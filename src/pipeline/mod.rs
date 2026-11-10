//! Build pipeline: source collection → parallel compile (%.d incremental) → link.
//!
//! Minimal correct core migrated from Cizon1: mtime + `-MMD` depfile-level incremental,
//! std::thread parallelism, stderr passed through — PCH / fingerprint cache / error
//! formatter are not migrated. Since v0.2.1 collection follows modern C++ layout:
//! recursive src/, include/ automatically added with -I.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use crate::compiler::{self, Cxx};
use crate::config::Config;
use crate::deps;

mod compile;
mod ide;
mod implib;
mod link;
mod output;
mod state;
mod targets;

pub use targets::plan;
pub(crate) use output::rel_display;

use compile::*;
use ide::*;
use implib::{
    adopt, dll_path, fingerprint, flavor, import_lib_arg, import_lib_path, temp_import_path,
    ImportLibFlavor,
};
use link::*;
use output::{step_fin, step_line, warning_section_text};
use state::*;
use targets::{lib_obj_dir, lib_obj_for, obj_for};



#[derive(Debug)]
struct LibTarget {
    name: String,
    sources: Vec<PathBuf>,
}

#[derive(Debug)]
struct SharedTarget {
    name: String,
    sources: Vec<PathBuf>,
    /// Static libraries built into this DLL, in declaration order.
    links: Vec<String>,
}

#[derive(Debug, Clone)]
pub(super) enum LinkTarget {
    Static(String),
    Shared(String),
}

#[derive(Debug)]
struct SharedArtifact {
    name: String,
    import_lib: PathBuf,
    fingerprint: String,
}

#[derive(Debug)]
pub struct Plan {
    cxx: Cxx,
    /// source files of the bin (relative to the project root)
    sources: Vec<PathBuf>,
    /// compile-time flags (edition + optimization level + user flags + dependency include paths)
    flags: Vec<String>,
    /// extra link-time flags (user link_flags; user flags already live in flags, not passed twice)
    link_flags: Vec<String>,
    /// base compile flags for dependency sources (edition + optimization level; user flags are
    /// not applied, so -Werror does not accidentally hit third-party code)
    dep_base: Vec<String>,
    /// each dependency's own -I, so dependency sources find their own headers first
    dep_includes: BTreeMap<String, Vec<String>>,
    /// static library targets; built in configuration order
    libs: Vec<LibTarget>,
    /// shared library targets; built in configuration order
    shared: Vec<SharedTarget>,
    /// bin link targets, in declaration order (static archives and import libraries are grouped
    /// at link time)
    links: Vec<LinkTarget>,
    out_name: String,
    /// "debug" / "release" — together with the compiler decides the artifact scope
    mode: &'static str,
}

impl Plan {
    /// Compile flags for dependency sources: base flags + own -I (first) + other deps' -I.
    fn dep_flags_for(&self, name: &str) -> Vec<String> {
        let mut flags = self.dep_base.clone();
        if let Some(own) = self.dep_includes.get(name) {
            flags.extend(own.iter().cloned());
        }
        for (other, includes) in &self.dep_includes {
            if other != name {
                flags.extend(includes.iter().cloned());
            }
        }
        flags
    }
}

/// Run the full build, return the executable path.
///
/// Artifact scope (migrated from the Cizon1 compiler-sandbox lesson): `build/<mode>-<compiler>/`.
/// Different compilers or debug/release are physically isolated, cross-linking is forbidden; when
/// the same-named compiler's version changes, the whole directory is invalidated based on the
/// `.compiler-version` fingerprint.
pub fn build(dir: &Path, cfg: &Config, plan: &Plan) -> Result<PathBuf, String> {
    let started = Instant::now();
    let build_dir = dir.join("build");
    let scope = format!("{}-{}", plan.mode, scope_label(&plan.cxx.name));
    let scope_dir = build_dir.join(&scope);
    let marker = scope_dir.join(".compiler-version");
    let marker_state = match std::fs::read_to_string(&marker) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("Failed to read {}: {e}", marker.display())),
    };
    if marker_state.as_deref() != Some(plan.cxx.version.as_str()) {
        reset_dir(&scope_dir)?;
    }
    let obj_dir = scope_dir.join("obj");
    let deps_obj_dir = scope_dir.join("deps-obj");
    // Object layout version: after mirroring source-relative paths, objects in the old layout
    // must be invalidated as a whole batch.
    let layout_marker = scope_dir.join(".obj-layout");
    let layout = "mirror-v1";
    let layout_current = std::fs::read_to_string(&layout_marker)
        .map_or(false, |t| t.trim() == layout);
    if !layout_current {
        reset_dir(&obj_dir)?;
        reset_dir(&deps_obj_dir)?;
        clear_lib_artifacts(&scope_dir)?;
        clear_shared_artifacts(&scope_dir)?;
    }
    // Changes to compile parameters touch no mtime; without proactive invalidation the old
    // behavior's .o files would be reused silently.
    // Project, dependencies and libraries are recorded separately: user flags changes recompile
    // the project and libraries; dep flags only affect dependencies.
    let project_state = scope_dir.join(".project-state");
    let deps_state = scope_dir.join(".deps-state");
    let libs_state = scope_dir.join(".libs-state");
    let shared_state = scope_dir.join(".shared-state");
    let wanted_project = project_state_text(plan);
    let wanted_deps = deps_state_text(plan);
    let wanted_libs = libs_state_text(plan);
    let wanted_shared = shared_state_text(plan);
    if !state_current(&project_state, &wanted_project)? {
        clear_project_objects(&obj_dir)?;
        // Project flags apply to library and shared-library sources too
        clear_lib_artifacts(&scope_dir)?;
        clear_shared_artifacts(&scope_dir)?;
    }
    if !state_current(&deps_state, &wanted_deps)? {
        clear_dep_objects(&deps_obj_dir)?;
    }
    if !state_current(&libs_state, &wanted_libs)? {
        clear_lib_artifacts(&scope_dir)?;
    }
    if !state_current(&shared_state, &wanted_shared)? {
        clear_shared_artifacts(&scope_dir)?;
    }
    std::fs::create_dir_all(&obj_dir)
        .map_err(|e| format!("Cannot create {}: {e}", obj_dir.display()))?;
    std::fs::create_dir_all(&deps_obj_dir)
        .map_err(|e| format!("Cannot create {}: {e}", deps_obj_dir.display()))?;
    for lib in &plan.libs {
        let lib_obj = lib_obj_dir(&scope_dir, &lib.name);
        std::fs::create_dir_all(&lib_obj)
            .map_err(|e| format!("Cannot create {}: {e}", lib_obj.display()))?;
    }
    for shared in &plan.shared {
        let shared_obj = targets::shared_obj_dir(&scope_dir, &shared.name);
        std::fs::create_dir_all(&shared_obj)
            .map_err(|e| format!("Cannot create {}: {e}", shared_obj.display()))?;
    }
    std::fs::write(&project_state, &wanted_project)
        .map_err(|e| format!("Failed to write {}: {e}", project_state.display()))?;
    std::fs::write(&deps_state, &wanted_deps)
        .map_err(|e| format!("Failed to write {}: {e}", deps_state.display()))?;
    std::fs::write(&libs_state, &wanted_libs)
        .map_err(|e| format!("Failed to write {}: {e}", libs_state.display()))?;
    std::fs::write(&shared_state, &wanted_shared)
        .map_err(|e| format!("Failed to write {}: {e}", shared_state.display()))?;
    std::fs::write(&layout_marker, layout)
        .map_err(|e| format!("Failed to write {}: {e}", layout_marker.display()))?;
    std::fs::write(&marker, &plan.cxx.version)
        .map_err(|e| format!("Failed to write {}: {e}", marker.display()))?;
    let stale_exes = stale_exe_paths(&scope_dir, plan);

    // 0) Dependency contract: restore / verify. Missing directory is restored per the lock; a
    // mismatch with the lock is a loud error.
    let dep_jobs = deps::prepare(dir, cfg, &deps_obj_dir)?;

    // 0.5) Editor support: only when explicitly enabled, write the compile database at the project
    // root and add the VS Code IntelliSense config
    if cfg.compile_commands() {
        write_compile_db(dir, plan, &dep_jobs)?;
        let compiler_path = compiler::executable_path(&plan.cxx.name)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| plan.cxx.name.clone());
        let standard = cpp_standard(&plan.flags);
        match ensure_cpptools_config(dir, &compiler_path, standard) {
            Ok(true) => step_line("Writing", ".vscode/c_cpp_properties.json"),
            Ok(false) => {}
            Err(e) => eprintln!("Warning: failed to generate .vscode/c_cpp_properties.json: {e}"),
        }
    }

    // 1) Determine which sources need recompiling
    let mut jobs: Vec<Job> = Vec::new(); // project sources
    for src in &plan.sources {
        let obj = obj_for(dir, src, &obj_dir)?;
        if needs_recompile(src, &obj) {
            jobs.push(Job { src: src.clone(), obj, flags: plan.flags.clone() });
        }
    }
    for (name, dsrc, dobj) in &dep_jobs {
        if needs_recompile(dsrc, dobj) {
            jobs.push(Job {
                src: dsrc.clone(),
                obj: dobj.clone(),
                flags: plan.dep_flags_for(name),
            });
        }
    }
    for lib in &plan.libs {
        for src in &lib.sources {
            let obj = lib_obj_for(dir, &scope_dir, &lib.name, src)?;
            if needs_recompile(src, &obj) {
                jobs.push(Job {
                    src: src.clone(),
                    obj,
                    flags: plan.flags.clone(),
                });
            }
        }
    }
    for shared in &plan.shared {
        for src in &shared.sources {
            let obj = targets::shared_obj_for(dir, &scope_dir, &shared.name, src)?;
            if needs_recompile(src, &obj) {
                jobs.push(Job {
                    src: src.clone(),
                    obj,
                    flags: plan.flags.clone(),
                });
            }
        }
    }
    let total = plan.sources.len()
        + dep_jobs.len()
        + plan.libs.iter().map(|l| l.sources.len()).sum::<usize>()
        + plan.shared.iter().map(|s| s.sources.len()).sum::<usize>();
    let recompiled = jobs.len();
    if recompiled > 0 {
        let label = if recompiled == total { "Compiling" } else { "Rebuilding" };
        step_line(
            label,
            &format!(
                "{} {} [{recompiled}/{total} files]",
                plan.cxx.name,
                plan.cxx.short_version()
            ),
        );
    }

    // 2) Parallel compile (work queue + n threads); collect output first and print it all at the
    // end to avoid interleaved scrambling
    let warnings = Mutex::new(Vec::<(PathBuf, String)>::new());
    let failures = Mutex::new(Vec::<(PathBuf, String)>::new());
    if !jobs.is_empty() {
        let queue = Mutex::new(jobs);
        let threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2)
            .max(1);
        std::thread::scope(|scope| {
            for _ in 0..threads {
                scope.spawn(|| loop {
                    let job = queue.lock().expect("queue poisoned").pop();
                    let Some(Job { src, obj, flags }) = job else { break };
                    match compile_one(&plan.cxx.name, &src, &obj, &flags) {
                        Ok(Some(stderr)) => {
                            warnings
                                .lock()
                                .expect("warn poisoned")
                                .push((src, relativize_diagnostics(dir, &stderr)));
                        }
                        Ok(None) => {}
                        Err(stderr) => {
                            failures
                                .lock()
                                .expect("fail poisoned")
                                .push((src, relativize_diagnostics(dir, &stderr)));
                        }
                    }
                });
            }
        });
    }

    let mut warnings = warnings.into_inner().expect("warn poisoned");
    warnings.sort_by(|a, b| a.0.cmp(&b.0));
    let failures = failures.into_inner().expect("fail poisoned");
    // Print warnings from successful translation units first: after a failure the next build only
    // recompiles the failed files, so warnings must not be lost
    if !warnings.is_empty() {
        let text = warning_section_text(dir, &plan.cxx, &warnings);
        let mut err = std::io::stderr();
        let _ = err.write_all(text.as_bytes());
    }
    if !failures.is_empty() {
        let mut msg = String::new();
        for (src, stderr) in &failures {
            msg.push_str(&format!("Compile failed: {}\n", rel_display(dir, src)));
            msg.push_str(stderr);
        }
        return Err(msg);
    }
    // 3) Static library archiving (rebuild when objects are newer or the archive is missing; when
    // everything is fresh the archiver tool is not required)
    let mut pending_archives: Vec<(PathBuf, Vec<PathBuf>)> = Vec::new();
    for lib in &plan.libs {
        let lib_objs: Vec<PathBuf> = lib
            .sources
            .iter()
            .map(|src| lib_obj_for(dir, &scope_dir, &lib.name, src))
            .collect::<Result<_, _>>()?;
        let archive = archive_path(&scope_dir, &lib.name);
        if archive_stale(&archive, &lib_objs) {
            pending_archives.push((archive, lib_objs));
        }
    }
    let libs_rebuilt = !pending_archives.is_empty();
    if libs_rebuilt {
        let ar = compiler::archiver(&plan.cxx)?;
        for (archive, lib_objs) in &pending_archives {
            step_line("Archiving", &rel_display(dir, archive));
            archive_lib(&ar, archive, lib_objs)?;
        }
    }
    // 3.5) Link shared libraries. The import library is generated explicitly; identical export
    // surfaces keep the old import library so the bin does not relink needlessly.
    let mut shared_artifacts: Vec<SharedArtifact> = Vec::new();
    let mut shared_rebuilt = false;
    if !plan.shared.is_empty() {
        let flavor = flavor(&plan.cxx)?;
        for shared in &plan.shared {
            let mut shared_objs: Vec<PathBuf> = shared
                .sources
                .iter()
                .map(|src| targets::shared_obj_for(dir, &scope_dir, &shared.name, src))
                .collect::<Result<_, _>>()?;
            // Dependencies are project-wide; their objects must be linked into every shared
            // library that references them, just like the bin.
            shared_objs.extend(dep_jobs.iter().map(|(_, _, obj)| obj.clone()));
            let shared_archives: Vec<PathBuf> = shared
                .links
                .iter()
                .map(|name| archive_path(&scope_dir, name))
                .collect();
            let dll = dll_path(&scope_dir, &shared.name);
            let import_lib = import_lib_path(&scope_dir, &shared.name, flavor);
            let state_path = shared_link_state_path(&scope_dir, &shared.name);
            let wanted_state =
                shared_link_state_text(plan, shared, &shared_objs, &shared_archives);
            let inputs_stale =
                archive_stale(&dll, &shared_objs) || archive_stale(&dll, &shared_archives);
            let mut current = std::fs::read_to_string(&state_path)
                .map_or(false, |t| t == wanted_state)
                && dll.exists()
                && import_lib.exists()
                && !inputs_stale;
            let mut import_fp = String::from("none");
            if current {
                match fingerprint(&import_lib) {
                    Ok(fp) => import_fp = fp,
                    Err(_) => current = false,
                }
            }
            if !current {
                let tmp_import = temp_import_path(&import_lib);
                let import_arg = import_lib_arg(&tmp_import, flavor);
                step_line("Linking", &rel_display(dir, &dll));
                let link_result = link_shared(
                    &plan.cxx.name,
                    &shared_objs,
                    &shared_archives,
                    &dll,
                    &plan.flags,
                    &plan.link_flags,
                    &import_arg,
                    matches!(flavor, ImportLibFlavor::Gnu),
                );
                if let Err(e) = link_result {
                    let _ = std::fs::remove_file(&tmp_import);
                    return Err(format!(
                        "Shared library link failed: {}\n{}",
                        rel_display(dir, &dll),
                        relativize_diagnostics(dir, &e)
                    ));
                }
                import_fp = adopt(&tmp_import, &import_lib)?;
                std::fs::write(&state_path, &wanted_state)
                    .map_err(|e| format!("Failed to write {}: {e}", state_path.display()))?;
                shared_rebuilt = true;
            }
            shared_artifacts.push(SharedArtifact {
                name: shared.name.clone(),
                import_lib,
                fingerprint: import_fp,
            });
        }
    }

    let mut static_archives: Vec<PathBuf> = Vec::new();
    let mut import_archives: Vec<PathBuf> = Vec::new();
    let mut imports: Vec<(PathBuf, String)> = Vec::new();
    for link in &plan.links {
        match link {
            LinkTarget::Static(name) => static_archives.push(archive_path(&scope_dir, name)),
            LinkTarget::Shared(name) => {
                let artifact = shared_artifacts
                    .iter()
                    .find(|a| a.name.eq_ignore_ascii_case(name))
                    .expect("plan validation guarantees a built shared target");
                import_archives.push(artifact.import_lib.clone());
                imports.push((artifact.import_lib.clone(), artifact.fingerprint.clone()));
            }
        }
    }
    let mut archives = static_archives.clone();
    archives.extend(import_archives);

    // 4) Link (skip when nothing recompiled, link inputs and parameters unchanged and the artifact is fresh)
    let exe = scope_dir.join(&plan.out_name);
    let mut objs: Vec<PathBuf> = plan
        .sources
        .iter()
        .map(|src| obj_for(dir, src, &obj_dir))
        .collect::<Result<_, _>>()?;
    objs.extend(dep_jobs.iter().map(|(_, _, obj)| obj.clone()));
    // Shrinking the object set (deleting a source) produces no mtime change; force a relink via
    // this state.
    let link_state = scope_dir.join(".link-state");
    let wanted_link = link_state_text(plan, &objs, &archives, &imports);
    let link_current = std::fs::read_to_string(&link_state)
        .map_or(false, |t| t == wanted_link);
    let did_work = recompiled > 0 || libs_rebuilt || shared_rebuilt;
    if link_current && exe_is_fresh(&exe, &objs, &static_archives) {
        remove_artifacts(&stale_exes);
        if did_work {
            step_fin(started, recompiled, total);
        } else {
            step_line(
                "Finished",
                &format!("up to date ({:.2}s)", started.elapsed().as_secs_f32()),
            );
        }
        return Ok(exe);
    }
    step_line("Linking", &rel_display(dir, &exe));
    link(
        &plan.cxx.name,
        &objs,
        &archives,
        &exe,
        &plan.flags,
        &plan.link_flags,
    )
    .map_err(|e| {
        format!(
            "Link failed: {}\n{}",
            rel_display(dir, &exe),
            relativize_diagnostics(dir, &e)
        )
    })?;
    std::fs::write(&link_state, &wanted_link)
        .map_err(|e| format!("Failed to write {}: {e}", link_state.display()))?;
    remove_artifacts(&stale_exes);

    step_fin(started, recompiled, total);
    Ok(exe)
}

#[cfg(test)]
mod tests;
