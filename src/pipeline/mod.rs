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
use targets::{exec_obj_dir, lib_obj_dir, lib_obj_for, obj_for};



#[derive(Debug)]
struct LibTarget {
    name: String,
    sources: Vec<PathBuf>,
    /// Optional installed-package prefix; expands to -I<prefix>/include for this target.
    prefix: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub(super) struct BinTarget {
    pub(super) name: String,
    pub(super) sources: Vec<PathBuf>,
    pub(super) links: Vec<LinkTarget>,
    pub(super) out_name: String,
    /// Test targets only: external library names mapped verbatim to `-l<name>` at link time.
    /// Bins always keep this empty (config validation enforces it).
    pub(super) extern_links: Vec<String>,
    /// Optional installed-package prefix; expands to -I<prefix>/include (compile) and
    /// -L<prefix>/lib (link) for this target only.
    pub(super) prefix: Option<PathBuf>,
}

#[derive(Debug)]
struct SharedTarget {
    name: String,
    sources: Vec<PathBuf>,
    /// Static libraries built into this DLL, in declaration order.
    links: Vec<String>,
    /// Optional installed-package prefix; expands to -I<prefix>/include and -L<prefix>/lib.
    prefix: Option<PathBuf>,
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
pub struct BuildResult {
    /// Built executable targets: (target name, exe path). Bins for `build`/`run`, tests for
    /// `cizon test`.
    pub execs: Vec<(String, PathBuf)>,
}

#[derive(Debug)]
pub struct Plan {
    cxx: Cxx,
    /// executable targets; may be empty when the project only declares test targets
    bins: Vec<BinTarget>,
    /// test targets; built and run only by `cizon test`, ignored by build/run
    tests: Vec<BinTarget>,
    /// compile-time flags (edition + optimization level + user flags + dependency include paths)
    flags: Vec<String>,
    /// test-source compile flags: `flags` with every `-DNDEBUG` definition removed, so a test
    /// project that adds it cannot silently disable assert-based tests.
    test_flags: Vec<String>,
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
    /// "debug" / "release" — together with the compiler decides the artifact scope
    mode: &'static str,
}

impl Plan {
    /// Names of the planned test targets (`cizon test` validates its name filter against this).
    pub fn test_names(&self) -> Vec<String> {
        self.tests.iter().map(|t| t.name.clone()).collect()
    }

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
#[cfg(test)]
pub fn build(dir: &Path, cfg: &Config, plan: &Plan) -> Result<PathBuf, String> {
    let result = build_bins(dir, cfg, plan)?;
    result
        .execs
        .first()
        .map(|(_, path)| path.clone())
        .ok_or_else(|| "no bin target built.".to_string())
}

pub fn build_bins(dir: &Path, cfg: &Config, plan: &Plan) -> Result<BuildResult, String> {
    build_execs(dir, cfg, plan, ExecSet::Bins, &[])
}

/// Build the selected test targets (`cizon test` only). `filter` holds pre-validated target
/// names; empty means all tests.
pub fn build_tests(
    dir: &Path,
    cfg: &Config,
    plan: &Plan,
    filter: &[String],
) -> Result<BuildResult, String> {
    build_execs(dir, cfg, plan, ExecSet::Tests, filter)
}

/// Which executable list a build run operates on. Tests share the compile/archive/link pipeline
/// but live in separate object and state files, so `build`/`run` and `cizon test` never touch
/// each other's artifacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecSet {
    Bins,
    Tests,
}

fn build_execs(
    dir: &Path,
    cfg: &Config,
    plan: &Plan,
    set: ExecSet,
    filter: &[String],
) -> Result<BuildResult, String> {
    let all: &[BinTarget] = match set {
        ExecSet::Bins => &plan.bins,
        ExecSet::Tests => &plan.tests,
    };
    // Filter is only passed by `cizon test` (names pre-validated there); empty means all.
    let execs: Vec<BinTarget> = if filter.is_empty() {
        all.to_vec()
    } else {
        all.iter()
            .filter(|t| filter.iter().any(|f| t.name.eq_ignore_ascii_case(f)))
            .cloned()
            .collect()
    };
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
    let test_obj_root = scope_dir.join("test-obj");
    // Object layout version: after mirroring source-relative paths, objects in the old layout
    // must be invalidated as a whole batch.
    let layout_marker = scope_dir.join(".obj-layout");
    let layout = "mirror-v1";
    let layout_current = std::fs::read_to_string(&layout_marker)
        .map_or(false, |t| t.trim() == layout);
    if !layout_current {
        reset_dir(&obj_dir)?;
        reset_dir(&deps_obj_dir)?;
        reset_dir(&test_obj_root)?;
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
    let (execs_state_label, execs_state) = match set {
        ExecSet::Bins => ("bin", scope_dir.join(".bins-state")),
        ExecSet::Tests => ("test", scope_dir.join(".tests-state")),
    };
    let old_execs = read_execs_state(&execs_state, execs_state_label);
    let wanted_project = project_state_text(plan);
    let wanted_deps = deps_state_text(plan);
    let wanted_libs = libs_state_text(plan);
    let wanted_shared = shared_state_text(plan);
    // Record the full planned list, not the filtered subset: otherwise `cizon test one` would
    // treat every unselected test as removed and delete its exe/link state.
    let wanted_execs = execs_state_text(execs_state_label, all);
    let multi = plan.bins.len() > 1;
    let exec_flags = match set {
        ExecSet::Bins => &plan.flags,
        ExecSet::Tests => &plan.test_flags,
    };
    if !state_current(&project_state, &wanted_project)? {
        clear_project_objects(&obj_dir)?;
        reset_dir(&test_obj_root)?;
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
    for bin in &execs {
        let bin_obj = exec_obj_dir(&scope_dir, &bin.name, multi, set);
        std::fs::create_dir_all(&bin_obj)
            .map_err(|e| format!("Cannot create {}: {e}", bin_obj.display()))?;
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
    let mut stale_exes = stale_exec_paths(&scope_dir, &old_execs, all);
    if set == ExecSet::Bins && !multi && !plan.bins.is_empty() {
        stale_exes.extend(stale_exe_paths(
            &scope_dir,
            &plan.bins[0].out_name,
        ));
    }

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
    for bin in &execs {
        let bin_obj = exec_obj_dir(&scope_dir, &bin.name, multi, set);
        let flags = compile_flags_with_prefix(exec_flags, &bin.prefix);
        for src in &bin.sources {
            let obj = obj_for(dir, src, &bin_obj)?;
            if needs_recompile(src, &obj) {
                jobs.push(Job { src: src.clone(), obj, flags: flags.clone() });
            }
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
        let flags = compile_flags_with_prefix(&plan.flags, &lib.prefix);
        for src in &lib.sources {
            let obj = lib_obj_for(dir, &scope_dir, &lib.name, src)?;
            if needs_recompile(src, &obj) {
                jobs.push(Job {
                    src: src.clone(),
                    obj,
                    flags: flags.clone(),
                });
            }
        }
    }
    for shared in &plan.shared {
        let flags = compile_flags_with_prefix(&plan.flags, &shared.prefix);
        for src in &shared.sources {
            let obj = targets::shared_obj_for(dir, &scope_dir, &shared.name, src)?;
            if needs_recompile(src, &obj) {
                jobs.push(Job {
                    src: src.clone(),
                    obj,
                    flags: flags.clone(),
                });
            }
        }
    }
    let total = execs
        .iter()
        .map(|b| b.sources.len())
        .sum::<usize>()
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
                let mut shared_link_flags = plan.link_flags.clone();
                if let Some(p) = &shared.prefix {
                    shared_link_flags.push(format!("-L{}/lib", p.display()));
                }
                step_line("Linking", &rel_display(dir, &dll));
                let link_result = link_shared(
                    &plan.cxx.name,
                    &shared_objs,
                    &shared_archives,
                    &dll,
                    &plan.flags,
                    &shared_link_flags,
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

    let mut result_bins: Vec<(String, PathBuf)> = Vec::new();
    let mut any_bin_linked = false;
    for bin in &execs {
        let mut static_archives: Vec<PathBuf> = Vec::new();
        let mut import_archives: Vec<PathBuf> = Vec::new();
        let mut imports: Vec<(PathBuf, String)> = Vec::new();
        for link in &bin.links {
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

        let exe = scope_dir.join(&bin.out_name);
        let bin_obj = exec_obj_dir(&scope_dir, &bin.name, multi, set);
        let mut objs: Vec<PathBuf> = bin
            .sources
            .iter()
            .map(|src| obj_for(dir, src, &bin_obj))
            .collect::<Result<_, _>>()?;
        objs.extend(dep_jobs.iter().map(|(_, _, obj)| obj.clone()));
        // Per-target prefix contributes -L<prefix>/lib; test targets then append -l<name> for
        // their declared external libraries, after all project archives and import libraries.
        let mut extra_flags: Vec<String> = Vec::new();
        if let Some(p) = &bin.prefix {
            extra_flags.push(format!("-L{}/lib", p.display()));
        }
        extra_flags.extend(bin.extern_links.iter().map(|n| format!("-l{n}")));
        let link_state = match set {
            ExecSet::Bins => bin_link_state_path(&scope_dir, &bin.name, multi),
            ExecSet::Tests => test_link_state_path(&scope_dir, &bin.name),
        };
        let wanted_link = link_state_text(plan, bin, &objs, &archives, &imports);
        let link_current = std::fs::read_to_string(&link_state)
            .map_or(false, |t| t == wanted_link);
        if link_current && exe_is_fresh(&exe, &objs, &static_archives) {
            result_bins.push((bin.name.clone(), exe));
            continue;
        }
        step_line("Linking", &rel_display(dir, &exe));
        link(
            &plan.cxx.name,
            &objs,
            &archives,
            &exe,
            &plan.flags,
            &plan.link_flags,
            &extra_flags,
        )
        .map_err(|e| {
            let mut msg = format!(
                "Link failed: {}\n{}",
                rel_display(dir, &exe),
                relativize_diagnostics(dir, &e)
            );
            if !bin.extern_links.is_empty() {
                if let Some(hint) = missing_extern_lib_hint(&e, &bin.extern_links) {
                    msg.push_str(&hint);
                }
            }
            msg
        })?;
        std::fs::write(&link_state, &wanted_link)
            .map_err(|e| format!("Failed to write {}: {e}", link_state.display()))?;
        any_bin_linked = true;
        result_bins.push((bin.name.clone(), exe));
    }

    std::fs::write(&execs_state, &wanted_execs)
        .map_err(|e| format!("Failed to write {}: {e}", execs_state.display()))?;
    remove_artifacts(&stale_exes);

    let did_work = recompiled > 0 || libs_rebuilt || shared_rebuilt || any_bin_linked;
    if did_work {
        step_fin(started, recompiled, total);
    } else {
        step_line(
            "Finished",
            &format!("up to date ({:.2}s)", started.elapsed().as_secs_f32()),
        );
    }
    Ok(BuildResult { execs: result_bins })
}

/// Append a target's prefix include path to its base compile flags. The prefix is the installed
/// package root; cizon2 assumes the standard `<prefix>/include` layout.
fn compile_flags_with_prefix(base: &[String], prefix: &Option<PathBuf>) -> Vec<String> {
    let mut flags = base.to_vec();
    if let Some(p) = prefix {
        flags.push(format!("-I{}/include", p.display()));
    }
    flags
}

/// When a `extern_links` entry cannot be resolved (`cannot find -l<name>`), add a plain-language
/// hint: cizon2 neither downloads nor locates external libraries; installation is the user's move.
fn missing_extern_lib_hint(stderr_text: &str, extern_links: &[String]) -> Option<String> {
    if extern_links.is_empty() {
        return None;
    }
    if let Some(hit) = stderr_text.lines().find(|l| l.contains("cannot find -l")) {
        if let Some(name) = hit
            .split("cannot find -l")
            .nth(1)
            .and_then(|rest| rest.trim().split_whitespace().next())
            .map(|name| name.trim_end_matches(':'))
        {
            if extern_links.iter().any(|link| link.eq_ignore_ascii_case(name)) {
                return Some(format!(
                    "\nHint: library -l{name} was not found. cizon2 does not download or install libraries; install it with your system package manager (e.g. `pacman -S mingw-w64-ucrt-x86_64-gtest`, `apt install libgtest-dev`), or check the extern_links entry in cizon.toml."
                ));
            }
        }
    }
    // Some MinGW linkers only report `collect2: ld returned N` without naming the missing
    // library. Keep the same guidance instead of losing it entirely.
    if stderr_text.contains("ld returned") && !stderr_text.contains("undefined reference") {
        return Some(format!(
            "\nHint: the link failed. cizon2 does not download or install external libraries; verify that these extern_links are installed and discoverable by the compiler: {}.",
            extern_links.join(", ")
        ));
    }
    None
}

#[cfg(test)]
mod tests;
