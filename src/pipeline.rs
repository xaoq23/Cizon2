//! Build pipeline: source collection → parallel compile (%.d incremental) → link.
//!
//! Minimal correct core migrated from Cizon1: mtime + `-MMD` depfile-level incremental,
//! std::thread parallelism, stderr passed through — PCH / fingerprint cache / error
//! formatter are not migrated. Since v0.2.1 collection follows modern C++ layout:
//! recursive src/, include/ automatically added with -I.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Instant;

use crate::compiler::{self, Cxx};
use crate::config::{Config, TargetKind};
use crate::deps;

const SOURCE_EXTS: &[&str] = &["cpp", "cc", "cxx"];

#[derive(Debug)]
struct LibTarget {
    name: String,
    sources: Vec<PathBuf>,
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
    /// static library names the bin links, in declaration order
    links: Vec<String>,
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

/// Assemble a plan for one build: abort on failure, do not enter the compile stage.
pub fn plan(dir: &Path, cfg: &Config, cxx: Cxx, release: bool) -> Result<Plan, String> {
    let (sources, libs, links, out_name) = if let Some(targets) = cfg.targets() {
        let bin = targets
            .iter()
            .find(|t| t.kind == TargetKind::Bin)
            .expect("config validation guarantees exactly one bin");
        let bin_name = bin.name.as_deref().expect("config resolves bin name");
        let sources = collect_target_sources(
            dir,
            bin.sources.as_deref(),
            &format!("bin target {bin_name}"),
        )?;
        if sources.is_empty() {
            return Err(format!("bin target {bin_name} has no source file to compile."));
        }
        let mut libs = Vec::new();
        for target in targets.iter().filter(|t| t.kind == TargetKind::StaticLib) {
            let name = target.name.as_deref().expect("config resolves lib name");
            let lib_sources = collect_target_sources(
                dir,
                target.sources.as_deref(),
                &format!("static library target {name}"),
            )?;
            if lib_sources.is_empty() {
                return Err(format!("static library target {name} has no source file to compile."));
            }
            libs.push(LibTarget {
                name: name.to_string(),
                sources: lib_sources,
            });
        }
        let links = bin
            .links
            .iter()
            .map(|link| {
                libs.iter()
                    .find(|lib| lib.name.to_lowercase() == link.to_lowercase())
                    .map(|lib| lib.name.clone())
                    .unwrap_or_else(|| link.clone())
            })
            .collect();
        (sources, libs, links, exe_name(bin_name))
    } else {
        let explicit = cfg.explicit_sources();
        let sources = collect_sources(dir, explicit)?;
        if sources.is_empty() {
            return Err(match explicit {
                Some(_) => "No .cpp file found under the paths listed in [project] sources. Check those paths, or remove sources to scan src/.".into(),
                None => format!(
                    "No .cpp file under {}. Put source code in src/, or list them explicitly with [project] sources = [...].",
                    rel_display(dir, &dir.join("src"))
                ),
            });
        }
        (sources, Vec::new(), Vec::new(), exe_name(&cfg.name(dir)))
    };

    let edition = compiler::edition_flag(cfg.edition());
    let optimization = if release { "-O2" } else { "-g" };
    let mut flags = vec![edition.clone(), optimization.into()];
    flags.extend(cfg.flags().iter().cloned());
    // Public header directory: modern-layout include/ is added with -I automatically; when the
    // directory is missing the compiler safely ignores it
    flags.push(format!("-I{}", dir.join("include").display()));
    // Dependency header paths enter flags unconditionally: when the directory is missing the
    // compiler safely ignores it, so planning needs no network operation
    let mut dep_includes: BTreeMap<String, Vec<String>> = BTreeMap::new();
    if let Some(map) = cfg.dependencies() {
        for name in map.keys() {
            let inc = dir.join("deps").join(name);
            let include = format!("-I{}", inc.join("include").display());
            let root = format!("-I{}", inc.display());
            flags.push(include.clone());
            flags.push(root.clone());
            dep_includes.insert(name.clone(), vec![include, root]);
        }
    }
    let dep_base = vec![edition, optimization.into()];

    let link_flags: Vec<String> = cfg.link_flags().to_vec();

    Ok(Plan {
        cxx,
        sources,
        flags,
        link_flags,
        dep_base,
        dep_includes,
        libs,
        links,
        out_name,
        mode: if release { "release" } else { "debug" },
    })
}

fn exe_name(name: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// Output path: relative to the project root; falls back to the original path when outside it.
pub(crate) fn rel_display(base: &Path, path: &Path) -> String {
    path.strip_prefix(base).unwrap_or(path).display().to_string()
}

/// Replace the project absolute-path prefix in compiler diagnostics with a project-relative path;
/// line numbers and the rest of the original text stay unchanged.
fn relativize_diagnostics(dir: &Path, text: &str) -> String {
    let root = dir.to_string_lossy();
    let roots = [root.replace('/', "\\"), root.replace('\\', "/")];
    let mut out = text.to_string();
    for root in roots {
        for sep in ['\\', '/'] {
            out = out.replace(&format!("{root}{sep}"), "");
        }
    }
    out
}

/// Remove a directory (missing counts as success); failure must be reported, otherwise stale
/// objects survive silently.
fn reset_dir(path: &Path) -> Result<(), String> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Cannot clear {}: {e}", path.display())),
    }
}

fn state_current(path: &Path, wanted: &str) -> Result<bool, String> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(t == wanted),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("Failed to read {}: {e}", path.display())),
    }
}

/// Bin objects mirror project-relative paths; obj/ belongs to the bin only.
fn clear_project_objects(obj_dir: &Path) -> Result<(), String> {
    reset_dir(obj_dir)
}

/// Dependency objects live separately under deps-obj/<dep>/.
fn clear_dep_objects(deps_obj_dir: &Path) -> Result<(), String> {
    reset_dir(deps_obj_dir)
}

/// The compiler name may be a full path (--compiler C:\...\g++.exe); the directory name keeps only safe characters.
fn scope_label(cxx: &str) -> String {
    cxx.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// The source list only accepts project-relative paths, avoiding compiling files outside the
/// project or scanning build/deps in.
fn resolve_source_path(dir: &Path, raw: &str, owner: &str) -> Result<PathBuf, String> {
    if raw.trim().is_empty() {
        return Err(format!("{owner} sources must not contain an empty path."));
    }
    let rel = Path::new(raw);
    let bad_component = rel.components().any(|c| {
        !matches!(
            c,
            std::path::Component::Normal(_) | std::path::Component::CurDir
        )
    });
    if rel.is_absolute() || bad_component {
        return Err(format!(
            "{owner} sources must be project-relative paths (no absolute path, no ..): {raw}"
        ));
    }
    Ok(dir.join(rel))
}

/// Source directories must be inside the project; reject symlink/junction directories pointing
/// outside the project or at the project root.
fn ensure_dir_inside(dir: &Path, path: &Path, owner: &str) -> Result<(), String> {
    let root = std::fs::canonicalize(dir)
        .map_err(|e| format!("Cannot resolve project root {}: {e}", dir.display()))?;
    let canonical = std::fs::canonicalize(path)
        .map_err(|e| format!("Cannot resolve {owner} directory {}: {e}", path.display()))?;
    if canonical == root {
        return Err(format!("{owner} must not point at the project root."));
    }
    if !canonical.starts_with(&root) {
        return Err(format!(
            "Directory of {owner} is outside the project (symlink/junction?): {}",
            path.display()
        ));
    }
    Ok(())
}

/// Explicit sources or recursive scan of src/. Same-name stem conflicts in the same directory
/// error out immediately (object files are named by path mirror).
fn collect_sources(dir: &Path, explicit: Option<&[String]>) -> Result<Vec<PathBuf>, String> {
    let mut out: Vec<PathBuf> = Vec::new();
    match explicit {
        Some(list) => {
            for raw in list {
                let path = resolve_source_path(dir, raw, "[project].sources")?;
                if path.is_dir() {
                    ensure_dir_inside(dir, &path, "[project].sources")?;
                    collect_recursive(&path, &mut out)?;
                } else if path.is_file() {
                    check_source_ext(&path)?;
                    out.push(path);
                } else {
                    return Err(format!(
                        "Path listed in [project].sources does not exist: {}",
                        path.display()
                    ));
                }
            }
            out.sort();
        }
        None => {
            let src = dir.join("src");
            if !src.is_dir() {
                return Err(format!("src/ directory not found (or not readable): {}", src.display()));
            }
            ensure_dir_inside(dir, &src, "src/")?;
            collect_recursive(&src, &mut out)?;
            out.sort();
        }
    }
    let owner = if explicit.is_some() {
        "[project].sources"
    } else {
        "src/"
    };
    check_object_paths(dir, &out, owner)?;
    Ok(out)
}

/// A target's sources: files or directories (directories scanned recursively); bin defaults to
/// scanning src/.
fn collect_target_sources(
    dir: &Path,
    list: Option<&[String]>,
    owner: &str,
) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    match list {
        None => {
            let src = dir.join("src");
            if !src.is_dir() {
                return Err(format!("src/ directory not found (or not readable): {}", src.display()));
            }
            ensure_dir_inside(dir, &src, owner)?;
            collect_recursive(&src, &mut out)?;
        }
        Some(entries) => {
            for raw in entries {
                let path = resolve_source_path(dir, raw, owner)?;
                if path.is_dir() {
                    ensure_dir_inside(dir, &path, owner)?;
                    collect_recursive(&path, &mut out)?;
                } else if path.is_file() {
                    check_source_ext(&path)?;
                    out.push(path);
                } else {
                    return Err(format!("Path listed in {owner} sources does not exist: {}", path.display()));
                }
            }
        }
    }
    out.sort();
    check_object_paths(dir, &out, owner)?;
    Ok(out)
}

/// Object paths within one target must be unique; same stem across directories is allowed,
/// same stem within the same directory errors out.
fn check_object_paths(dir: &Path, sources: &[PathBuf], owner: &str) -> Result<(), String> {
    let mut seen: HashMap<String, PathBuf> = HashMap::new();
    for src in sources {
        let rel = project_relative(dir, src)?;
        let mut obj_rel = rel;
        obj_rel.set_extension("o");
        let key = obj_rel.to_string_lossy().to_lowercase();
        if let Some(prev) = seen.insert(key, src.clone()) {
            if prev == *src {
                return Err(format!("{owner} lists the same source more than once: {}", src.display()));
            }
            return Err(format!(
                "{owner} object path conflict:\n  {}\n  {}\nSource file stems within the same directory must be unique (same name across directories is allowed).",
                prev.display(),
                src.display()
            ));
        }
    }
    // An object file could also be treated as a parent directory by another source directory
    // (e.g. src/foo.o/bar.cpp).
    let mut artifacts: HashSet<String> = HashSet::new();
    for key in seen.keys() {
        artifacts.insert(key.clone());
        artifacts.insert(format!("{key}.d"));
        artifacts.insert(format!("{key}.tmp"));
        artifacts.insert(format!("{key}.d.tmp"));
    }
    for key in seen.keys() {
        for ancestor in Path::new(key).ancestors().skip(1) {
            if artifacts.contains(&ancestor.to_string_lossy().to_lowercase()) {
                return Err(format!(
                    "{owner} object/dep file path conflicts with a directory: {key}"
                ));
            }
        }
    }
    Ok(())
}

/// Path of a source file relative to the project root (used to mirror object paths).
fn project_relative(dir: &Path, src: &Path) -> Result<PathBuf, String> {
    let rel = src
        .strip_prefix(dir)
        .map_err(|_| format!("Source file is not inside the project: {}", src.display()))?;
    let mut out = PathBuf::new();
    for component in rel.components() {
        match component {
            std::path::Component::Normal(part) => out.push(part),
            std::path::Component::CurDir => {}
            _ => {
                return Err(format!(
                    "Source file path contains an illegal component (.. / root / prefix): {}",
                    src.display()
                ));
            }
        }
    }
    if out.as_os_str().is_empty() {
        return Err(format!("Source file path is empty: {}", src.display()));
    }
    Ok(out)
}

fn is_source(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| SOURCE_EXTS.iter().any(|ext| ext.eq_ignore_ascii_case(e)))
}

/// Recursively collect source files under a directory. Do not follow symlink/junction
/// directories (cycle guard); linked source files are still collected.
fn collect_recursive(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("Cannot read {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("Cannot read directory entries of {}: {e}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| format!("Cannot read type of {}: {e}", path.display()))?;
        if file_type.is_dir() {
            collect_recursive(&path, out)?;
        } else if is_source(&path) && path.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

fn check_source_ext(path: &Path) -> Result<(), String> {
    if is_source(path) {
        Ok(())
    } else {
        Err(format!(
            "sources list only accepts {:?}, rejected: {}",
            SOURCE_EXTS,
            path.display()
        ))
    }
}

// ── Build execution ──────────────────────────────────────────────

/// One compilation unit: project sources use plan.flags, dependency sources use their own dep
/// flags (user flags not applied).
struct Job {
    src: PathBuf,
    obj: PathBuf,
    flags: Vec<String>,
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
    }
    // Changes to compile parameters touch no mtime; without proactive invalidation the old
    // behavior's .o files would be reused silently.
    // Project, dependencies and libraries are recorded separately: user flags changes recompile
    // the project and libraries; dep flags only affect dependencies.
    let project_state = scope_dir.join(".project-state");
    let deps_state = scope_dir.join(".deps-state");
    let libs_state = scope_dir.join(".libs-state");
    let wanted_project = project_state_text(plan);
    let wanted_deps = deps_state_text(plan);
    let wanted_libs = libs_state_text(plan);
    if !state_current(&project_state, &wanted_project)? {
        clear_project_objects(&obj_dir)?;
        // Project flags apply to library sources too
        clear_lib_artifacts(&scope_dir)?;
    }
    if !state_current(&deps_state, &wanted_deps)? {
        clear_dep_objects(&deps_obj_dir)?;
    }
    if !state_current(&libs_state, &wanted_libs)? {
        clear_lib_artifacts(&scope_dir)?;
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
    std::fs::write(&project_state, &wanted_project)
        .map_err(|e| format!("Failed to write {}: {e}", project_state.display()))?;
    std::fs::write(&deps_state, &wanted_deps)
        .map_err(|e| format!("Failed to write {}: {e}", deps_state.display()))?;
    std::fs::write(&libs_state, &wanted_libs)
        .map_err(|e| format!("Failed to write {}: {e}", libs_state.display()))?;
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
    let total = plan.sources.len()
        + dep_jobs.len()
        + plan.libs.iter().map(|l| l.sources.len()).sum::<usize>();
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
    let archives: Vec<PathBuf> = plan
        .links
        .iter()
        .map(|name| archive_path(&scope_dir, name))
        .collect();

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
    let wanted_link = link_state_text(plan, &objs, &archives);
    let link_current = std::fs::read_to_string(&link_state)
        .map_or(false, |t| t == wanted_link);
    if recompiled == 0
        && !libs_rebuilt
        && link_current
        && exe_is_fresh(&exe, &objs, &archives)
    {
        remove_artifacts(&stale_exes);
        step_line(
            "Finished",
            &format!("up to date ({:.2}s)", started.elapsed().as_secs_f32()),
        );
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

/// Compile parameters for project sources. Line-based text is easy to diff by eye.
fn project_state_text(plan: &Plan) -> String {
    let mut s = String::from("project:\n");
    for f in &plan.flags {
        s.push_str(f);
        s.push('\n');
    }
    s
}

/// Compile parameters for dependency sources (base flags + each dependency's own include set).
fn deps_state_text(plan: &Plan) -> String {
    let mut s = String::from("dep-base:\n");
    for f in &plan.dep_base {
        s.push_str(f);
        s.push('\n');
    }
    for (name, includes) in &plan.dep_includes {
        s.push_str(&format!("dep {name}:\n"));
        for f in includes {
            s.push_str(f);
            s.push('\n');
        }
    }
    s
}

/// Source list of static library targets; when it changes, clear library objects and archives.
fn libs_state_text(plan: &Plan) -> String {
    let mut s = String::from("libs:\n");
    for lib in &plan.libs {
        s.push_str(&format!("lib {}:\n", lib.name));
        for src in &lib.sources {
            s.push_str(&src.display().to_string());
            s.push('\n');
        }
    }
    s
}

/// Link state: exe name, objects, archives participating in the link, link flags. Any change in
/// these requires a relink.
fn link_state_text(plan: &Plan, objs: &[PathBuf], archives: &[PathBuf]) -> String {
    let mut s = format!("exe: {}\n", plan.out_name);
    s.push_str("objects:\n");
    for obj in objs {
        s.push_str(&obj.display().to_string());
        s.push('\n');
    }
    s.push_str("archives:\n");
    for archive in archives {
        s.push_str(&archive.display().to_string());
        s.push('\n');
    }
    s.push_str("link-flags:\n");
    for f in &plan.link_flags {
        s.push_str(f);
        s.push('\n');
    }
    s
}

/// Write compile_commands.json: bin/library sources use plan.flags, dependency sources use their
/// own dep_flags_for. Hand-written JSON; serde_json is not introduced (dependency whitelist).
fn write_compile_db(
    dir: &Path,
    plan: &Plan,
    dep_jobs: &[(String, PathBuf, PathBuf)],
) -> Result<(), String> {
    let mut owned: Vec<(PathBuf, Vec<String>)> = plan
        .sources
        .iter()
        .map(|s| (s.clone(), plan.flags.clone()))
        .collect();
    for lib in &plan.libs {
        owned.extend(
            lib.sources
                .iter()
                .map(|s| (s.clone(), plan.flags.clone())),
        );
    }
    owned.extend(
        dep_jobs
            .iter()
            .map(|(name, src, _)| (src.clone(), plan.dep_flags_for(name))),
    );
    let entries: Vec<(&Path, &[String])> = owned
        .iter()
        .map(|(path, flags)| (path.as_path(), flags.as_slice()))
        .collect();
    let json = render_compile_db(dir, &plan.cxx.name, &entries);
    let path = dir.join("compile_commands.json");
    let unchanged = std::fs::read_to_string(&path).map_or(false, |old| old == json);
    if !unchanged {
        std::fs::write(&path, json)
            .map_err(|e| format!("Failed to write {}: {e}", rel_display(dir, &path)))?;
    }
    Ok(())
}

/// Get the C++ standard that actually takes effect: with repeated `-std=` the last one wins
/// (consistent with the compiler).
fn cpp_standard(flags: &[String]) -> &str {
    flags
        .iter()
        .rev()
        .find_map(|f| f.strip_prefix("-std="))
        .unwrap_or("c++23")
}

/// The Microsoft C/C++ extension does not read compile_commands.json automatically; when
/// c_cpp_properties.json is missing, add one. Existing files are never overwritten, to avoid
/// breaking the user's own IntelliSense configuration.
fn ensure_cpptools_config(dir: &Path, compiler: &str, standard: &str) -> Result<bool, String> {
    let vscode = dir.join(".vscode");
    let path = vscode.join("c_cpp_properties.json");
    if path.exists() {
        return Ok(false);
    }
    std::fs::create_dir_all(&vscode)
        .map_err(|e| format!("Failed to create {}: {e}", rel_display(dir, &vscode)))?;
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(e) => return Err(format!("Failed to write {}: {e}", rel_display(dir, &path))),
    };
    if let Err(e) = file.write_all(render_cpptools_config(compiler, standard).as_bytes()) {
        drop(file);
        let _ = std::fs::remove_file(&path);
        return Err(format!("Failed to write {}: {e}", rel_display(dir, &path)));
    }
    Ok(true)
}

fn render_cpptools_config(compiler: &str, standard: &str) -> String {
    let compiler = compiler.replace('\\', "/");
    format!(
        "{{\n  \"version\": 4,\n  \"configurations\": [\n    {{\n      \"name\": \"{}\",\n      \"compilerPath\": \"{}\",\n      \"intelliSenseMode\": \"{}\",\n      \"includePath\": [\"${{workspaceFolder}}/include\"],\n      \"cppStandard\": \"{}\",\n      \"compileCommands\": \"${{workspaceFolder}}/compile_commands.json\"\n    }}\n  ]\n}}\n",
        cpptools_config_name(),
        json_escape(&compiler),
        intelli_sense_mode(&compiler),
        json_escape(standard)
    )
}

fn cpptools_config_name() -> &'static str {
    if cfg!(windows) {
        "Win32"
    } else if cfg!(target_os = "macos") {
        "Mac"
    } else {
        "Linux"
    }
}

fn intelli_sense_mode(compiler: &str) -> &'static str {
    let clang = compiler.to_ascii_lowercase().contains("clang");
    if cfg!(windows) {
        if clang { "windows-clang-x64" } else { "windows-gcc-x64" }
    } else if cfg!(target_os = "macos") {
        if clang { "macos-clang-x64" } else { "macos-gcc-x64" }
    } else if clang {
        "linux-clang-x64"
    } else {
        "linux-gcc-x64"
    }
}

fn render_compile_db(dir: &Path, cxx: &str, entries: &[(&Path, &[String])]) -> String {
    let directory = json_escape(&dir.display().to_string());
    let mut out = String::from("[\n");
    for (i, (src, flags)) in entries.iter().enumerate() {
        if i > 0 {
            out.push_str(",\n");
        }
        let file = json_escape(&src.display().to_string());
        out.push_str("  {\n");
        out.push_str(&format!("    \"directory\": \"{directory}\",\n"));
        out.push_str(&format!("    \"file\": \"{file}\",\n"));
        out.push_str(&format!(
            "    \"arguments\": [\"{}\", \"-c\", \"{file}\"",
            json_escape(cxx)
        ));
        for f in *flags {
            out.push_str(&format!(", \"{}\"", json_escape(f)));
        }
        out.push_str("]\n  }");
    }
    out.push_str("\n]\n");
    out
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// After a bin rename, clean up the old exe and Windows link-side artifacts.
/// Failure only warns: the old exe may still be running and should not block building the new artifact.
/// Collect the old exe and Windows link-side artifacts to clean up after a bin rename; the actual
/// removal is done only after the link succeeds.
fn stale_exe_paths(scope_dir: &Path, plan: &Plan) -> Vec<PathBuf> {
    let mut stale = Vec::new();
    let old = std::fs::read_to_string(scope_dir.join(".link-state"))
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| {
                line.strip_prefix("exe: ")
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_string)
            })
        });
    if let Some(old) = old {
        // Windows file names are case-insensitive: app.exe and APP.exe are the same file, do not delete as a stale artifact.
        let same_name = if cfg!(windows) {
            old.to_lowercase() == plan.out_name.to_lowercase()
        } else {
            old == plan.out_name
        };
        if !same_name {
            let path = scope_dir.join(&old);
            stale.push(path.clone());
            stale.push(path.with_extension("pdb"));
            stale.push(path.with_extension("ilk"));
        }
    }

    // Old versions of .link-state have no exe line: on Windows collect other top-level artifacts by extension.
    #[cfg(windows)]
    {
        let current_stem = Path::new(&plan.out_name)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();
        if let Ok(entries) = std::fs::read_dir(scope_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
                    continue;
                };
                if !["exe", "pdb", "ilk"]
                    .iter()
                    .any(|x| x.eq_ignore_ascii_case(ext))
                {
                    continue;
                }
                let stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                if stem != current_stem {
                    stale.push(path);
                }
            }
        }
    }
    stale
}

fn remove_artifacts(paths: &[PathBuf]) {
    for path in paths {
        remove_artifact(path);
    }
}

fn remove_artifact(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!("Warning: failed to clean up stale artifact {}: {e}", path.display()),
    }
}

/// Whether the exe is not older than all objects and static library archives.
fn exe_is_fresh(exe: &Path, objs: &[PathBuf], archives: &[PathBuf]) -> bool {
    let Ok(exe_time) = std::fs::metadata(exe).and_then(|m| m.modified()) else {
        return false;
    };
    objs.iter()
        .chain(archives.iter())
        .all(|input| {
            std::fs::metadata(input)
                .and_then(|m| m.modified())
                .map_or(false, |t| t <= exe_time)
        })
}

fn obj_for(dir: &Path, src: &Path, obj_dir: &Path) -> Result<PathBuf, String> {
    let mut rel = project_relative(dir, src)?;
    rel.set_extension("o");
    Ok(obj_dir.join(rel))
}

fn lib_obj_dir(scope_dir: &Path, name: &str) -> PathBuf {
    scope_dir.join("lib").join(name).join("obj")
}

fn lib_obj_for(
    dir: &Path,
    scope_dir: &Path,
    name: &str,
    src: &Path,
) -> Result<PathBuf, String> {
    let mut rel = project_relative(dir, src)?;
    rel.set_extension("o");
    Ok(lib_obj_dir(scope_dir, name).join(rel))
}

fn archive_path(scope_dir: &Path, name: &str) -> PathBuf {
    scope_dir
        .join("lib")
        .join(name)
        .join(format!("lib{name}.a"))
}

/// When project flags or the library source list change, invalidate library objects and archives together.
fn clear_lib_artifacts(scope_dir: &Path) -> Result<(), String> {
    reset_dir(&scope_dir.join("lib"))
}

fn archive_stale(archive: &Path, objs: &[PathBuf]) -> bool {
    let Ok(archive_time) = std::fs::metadata(archive).and_then(|m| m.modified()) else {
        return true;
    };
    objs.iter().any(|obj| {
        std::fs::metadata(obj)
            .and_then(|m| m.modified())
            .map_or(true, |t| t > archive_time)
    })
}

fn archive_lib(ar: &str, archive: &Path, objs: &[PathBuf]) -> Result<(), String> {
    if objs.is_empty() {
        return Err(format!("static library {} has no object file.", archive.display()));
    }
    let tmp = temp_sibling(archive);
    let _ = std::fs::remove_file(&tmp);
    let out = Command::new(ar)
        .arg("rcs")
        .arg(&tmp)
        .args(objs)
        .output()
        .map_err(|e| format!("Failed to start archiver {ar}: {e}"))?;
    if !out.status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!(
            "Archive failed: {} {}\n{}",
            ar,
            archive.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    replace_file(&tmp, archive)
}

/// Incremental decision: src, obj and .d all exist, and neither src nor any input listed by .d is
/// newer than obj. When the .d cannot be read, recompile rather than treat corrupt dependency
/// info as "all fresh".
fn needs_recompile(src: &Path, obj: &Path) -> bool {
    let dep_file = dep_file_for(obj);
    if !src.exists() || !obj.exists() || !dep_file.exists() {
        return true;
    }
    let Ok(obj_time) = std::fs::metadata(obj).and_then(|m| m.modified()) else {
        return true;
    };
    let stale = |p: &Path| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .map_or(true, |t| t > obj_time)
    };
    if stale(src) {
        return true;
    }
    match dep_inputs(&dep_file) {
        Ok(inputs) if !inputs.is_empty() => {
            // After switching the source list the stem may stay the same: when the old .d does not
            // contain the current src, recompile.
            if !inputs.iter().any(|p| same_path(p, src)) {
                return true;
            }
            inputs.iter().any(|p| stale(p))
        }
        _ => true,
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn dep_file_for(obj: &Path) -> PathBuf {
    PathBuf::from(format!("{}.d", obj.display()))
}

fn temp_sibling(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.tmp", path.display()))
}

fn replace_file(tmp: &Path, dest: &Path) -> Result<(), String> {
    if dest.exists() {
        std::fs::remove_file(dest)
            .map_err(|e| format!("Failed to delete old file before replacing {}: {e}", dest.display()))?;
    }
    std::fs::rename(tmp, dest).map_err(|e| {
        let _ = std::fs::remove_file(tmp);
        format!("Failed to replace {}: {e}", dest.display())
    })
}

/// Parse a gcc/clang depfile (`target: dep dep dep ...`): all inputs after the first-line target;
/// handle `\` line continuations, `\ ` escaped spaces and Windows path separators.
/// Orphan lines generated by `-MP` (e.g. `foo.h:`) are safely skipped because their token ends with ':'.
fn dep_inputs(dep_file: &Path) -> Result<Vec<PathBuf>, String> {
    let content = std::fs::read_to_string(dep_file)
        .map_err(|e| format!("Failed to read dependency file: {e}"))?;
    let mut joined = String::new();
    for line in content.lines() {
        if let Some(stripped) = line.strip_suffix('\\') {
            joined.push_str(stripped);
        } else {
            joined.push_str(line);
            joined.push(' ');
        }
    }
    let deps = joined
        .split_once(": ")
        .map(|(_, rest)| rest)
        .unwrap_or("");
    Ok(split_dep_tokens(deps)
        .into_iter()
        .filter(|s| !s.ends_with(':'))
        .map(|s| {
            if cfg!(target_os = "windows") {
                PathBuf::from(s.replace('/', "\\"))
            } else {
                PathBuf::from(s)
            }
        })
        .collect())
}

/// make-style token splitting: whitespace-separated, `\ ` means a literal space, other backslashes are kept.
fn split_dep_tokens(s: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&' ') => {
                current.push(' ');
                chars.next();
            }
            c if c.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Compile a single file. Ok means success (Some = warning output), Err = failure (full stderr).
/// Object and .d are first written to temp files, replaced only on success, so half-written
/// products are not treated as fresh artifacts by the next build.
fn compile_one(
    cxx: &str,
    src: &Path,
    obj: &Path,
    flags: &[String],
) -> Result<Option<String>, String> {
    let dep_file = dep_file_for(obj);
    if let Some(parent) = obj.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Cannot create {}: {e}", parent.display()))?;
    }
    let tmp_obj = temp_sibling(obj);
    let tmp_dep = temp_sibling(&dep_file);
    let _ = std::fs::remove_file(&tmp_obj);
    let _ = std::fs::remove_file(&tmp_dep);
    let mut cmd = Command::new(cxx);
    cmd.arg("-c").arg(src).arg("-o").arg(&tmp_obj);
    cmd.arg("-MMD").arg("-MF").arg(&tmp_dep).arg("-MP");
    for f in flags {
        cmd.arg(f);
    }
    let out = match cmd.output() {
        Ok(out) => out,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_obj);
            let _ = std::fs::remove_file(&tmp_dep);
            return Err(format!("Failed to start compiler {cxx}: {e}"));
        }
    };
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

    if !out.status.success() {
        let _ = std::fs::remove_file(&tmp_obj);
        let _ = std::fs::remove_file(&tmp_dep);
        return Err(stderr);
    }
    replace_file(&tmp_obj, obj)?;
    replace_file(&tmp_dep, &dep_file)?;
    if stderr.trim().is_empty() {
        Ok(None)
    } else {
        Ok(Some(stderr))
    }
}

fn link(
    cxx: &str,
    objs: &[PathBuf],
    archives: &[PathBuf],
    exe: &Path,
    flags: &[String],
    link_flags: &[String],
) -> Result<(), String> {
    let mut cmd = Command::new(cxx);
    for obj in objs {
        cmd.arg(obj);
    }
    for archive in archives {
        cmd.arg(archive);
    }
    cmd.arg("-o").arg(exe);
    for f in flags {
        cmd.arg(f);
    }
    for f in link_flags {
        cmd.arg(f);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("Failed to start linker {cxx}: {e}"))?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        return Err(stderr.to_string());
    }
    if !stderr.trim().is_empty() {
        let mut err = std::io::stderr();
        let _ = err.write_all(stderr.as_bytes());
    }
    Ok(())
}

// ── Output style (cargo-like) ─────────────────────────────────

const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const GOLD: &str = "\x1b[38;2;255;241;168m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

fn step_line(label: &str, detail: &str) {
    eprintln!("{GREEN}{label:>12}{RESET} {detail}", label = label, detail = detail);
}

/// Compiler warning section: gold top/bottom dividers + source file/compiler header, original
/// diagnostics are not rewritten.
fn warning_section_text(dir: &Path, cxx: &Cxx, warnings: &[(PathBuf, String)]) -> String {
    let sep = format!("  {GOLD}{}{RESET}\n", "─".repeat(44));
    let mut out = sep.clone();
    for (src, text) in warnings {
        out.push_str(&format!(
            "{YELLOW}{:>12}{RESET} {} ({} {})\n",
            "Warning",
            rel_display(dir, src),
            cxx.name,
            cxx.short_version()
        ));
        out.push_str(text);
        if !text.ends_with('\n') {
            out.push('\n');
        }
    }
    out.push_str(&sep);
    out
}

fn finish_note(recompiled: usize, total: usize) -> &'static str {
    if recompiled == 0 {
        "relinked"
    } else if recompiled == total {
        "fresh build"
    } else {
        "incremental rebuild"
    }
}

fn step_fin(started: Instant, recompiled: usize, total: usize) {
    let secs = started.elapsed().as_secs_f32();
    let note = finish_note(recompiled, total);
    step_line("Finished", &format!("{DIM}{secs:.2}{RESET}s ({note})"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cizon2-bl-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(windows)]
    fn make_dir_link(link: &Path, target: &Path) -> bool {
        Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[cfg(unix)]
    fn make_dir_link(link: &Path, target: &Path) -> bool {
        std::os::unix::fs::symlink(target, link).is_ok()
    }

    fn remove_dir_link(path: &Path) {
        #[cfg(windows)]
        let _ = fs::remove_dir(path);
        #[cfg(unix)]
        let _ = fs::remove_file(path);
    }

    #[test]
    fn user_flags_are_linked_once() {
        let dir = temp_dir("linkflags");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}").unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname=\"t\"\n\n[compiler]\nflags=[\"-Wall\"]\nlink_flags=[\"-lm\"]\n",
        )
        .unwrap();
        let cfg = crate::config::load(&dir).unwrap();
        let cxx = Cxx { name: "g++".into(), version: "test".into() };
        let p = plan(&dir, &cfg, cxx, false).unwrap();
        assert_eq!(p.flags.iter().filter(|f| *f == "-Wall").count(), 1);
        assert!(!p.flags.iter().any(|f| f == "-lm"));
        assert_eq!(p.link_flags, vec!["-lm".to_string()]);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn compiler_path_is_sanitized_for_scope_dir() {
        assert_eq!(scope_label("g++"), "g++");
        assert_eq!(scope_label(r"C:\mingw64\bin\g++.exe"), "C__mingw64_bin_g++.exe");
        assert_eq!(scope_label("/usr/bin/clang++"), "_usr_bin_clang++");
    }

    #[test]
    fn warning_section_has_gold_divider_and_header() {
        let dir = Path::new("proj");
        let cxx = Cxx { name: "g++".into(), version: "g++ 14.2.0".into() };
        let warnings = vec![(
            dir.join("src").join("main.cpp"),
            "main.cpp:54: warning: ignored\n".to_string(),
        )];
        let text = warning_section_text(dir, &cxx, &warnings);
        assert!(text.starts_with("  \x1b[38;2;255;241;168m"));
        assert_eq!(text.matches('─').count(), 88, "two 44-wide gold dividers, top and bottom");
        assert!(text.contains("Warning"), "got: {text}");
        assert!(text.contains("main.cpp"), "got: {text}");
        assert!(text.contains("g++ 14.2.0"), "got: {text}");
        assert!(text.contains("warning: ignored"), "got: {text}");
    }

    #[test]
    fn finish_note_distinguishes_relink() {
        assert_eq!(finish_note(0, 3), "relinked");
        assert_eq!(finish_note(3, 3), "fresh build");
        assert_eq!(finish_note(1, 3), "incremental rebuild");
    }

    #[test]
    fn compiler_diagnostics_paths_are_relativized() {
        let dir = Path::new("proj");
        let sep = std::path::MAIN_SEPARATOR;
        let raw = format!("{}{sep}src{sep}main.cpp:1: warning: x", dir.display());
        assert_eq!(
            relativize_diagnostics(dir, &raw),
            format!("src{sep}main.cpp:1: warning: x")
        );
        let forward = format!(
            "{}/src/main.cpp:1: error: y",
            dir.display().to_string().replace('\\', "/")
        );
        assert_eq!(
            relativize_diagnostics(dir, &forward),
            "src/main.cpp:1: error: y"
        );
        assert_eq!(relativize_diagnostics(dir, "other.cpp:1: warning"), "other.cpp:1: warning");
    }

    #[test]
    fn rel_display_strips_project_root() {
        let base = Path::new("proj");
        let inside = base.join("build").join("app.exe");
        assert_eq!(
            rel_display(base, &inside),
            Path::new("build").join("app.exe").display().to_string()
        );
        let outside = Path::new("elsewhere").join("app.exe");
        assert_eq!(rel_display(base, &outside), outside.display().to_string());
    }

    #[test]
    fn reset_dir_reports_failure() {
        let dir = temp_dir("reset-file");
        let file = dir.join("not-a-dir");
        fs::write(&file, "").unwrap();
        let err = reset_dir(&file).unwrap_err();
        assert!(err.contains("Cannot clear"), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn split_state_clears_only_its_objects() {
        let dir = temp_dir("split-clear");
        let bin_obj = dir.join("obj");
        let deps_obj = dir.join("deps-obj");
        fs::create_dir_all(bin_obj.join("src")).unwrap();
        fs::create_dir_all(deps_obj.join("dep")).unwrap();
        fs::write(bin_obj.join("src/main.o"), "").unwrap();
        fs::write(deps_obj.join("dep/dep.o"), "").unwrap();
        clear_project_objects(&bin_obj).unwrap();
        assert!(!bin_obj.exists());
        assert!(deps_obj.join("dep/dep.o").exists(), "project flags change must not delete dependency objects");
        clear_dep_objects(&deps_obj).unwrap();
        assert!(!deps_obj.exists());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn dep_include_order_prefers_own_dependency() {
        let mut includes = BTreeMap::new();
        includes.insert("a".to_string(), vec!["-I/a".to_string()]);
        includes.insert(
            "b".to_string(),
            vec!["-I/b/include".to_string(), "-I/b".to_string()],
        );
        let p = Plan {
            cxx: Cxx { name: "g++".into(), version: "test".into() },
            sources: vec![],
            flags: vec![],
            link_flags: vec![],
            dep_base: vec!["-std=c++23".to_string()],
            dep_includes: includes,
            libs: vec![],
            links: vec![],
            out_name: "t".into(),
            mode: "debug",
        };
        let flags = p.dep_flags_for("b");
        let pos = |needle: &str| flags.iter().position(|f| f == needle).unwrap();
        assert!(pos("-I/b/include") < pos("-I/a"), "own headers must precede other dependencies");
        assert!(pos("-I/b") < pos("-I/a"));
    }

    #[test]
    fn collect_sources_from_src_dir() {
        let dir = temp_dir("collect");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}").unwrap();
        fs::write(dir.join("src/util.cc"), "//").unwrap();
        fs::write(dir.join("src/notes.txt"), "ignore").unwrap();
        let s = collect_sources(&dir, None).unwrap();
        let names: Vec<String> = s
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["main.cpp", "util.cc"]);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn collects_uppercase_source_extension() {
        let dir = temp_dir("upper-ext");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/Util.CPP"), "//").unwrap();
        let s = collect_sources(&dir, None).unwrap();
        assert_eq!(s.len(), 1, "uppercase extension must not be missed on Windows");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn explicit_sources_resolve_and_validate() {
        let dir = temp_dir("explicit");
        fs::create_dir_all(dir.join("third_party")).unwrap();
        fs::write(dir.join("third_party/x.cpp"), "//").unwrap();
        let list = vec!["third_party/x.cpp".to_string()];
        let s = collect_sources(&dir, Some(&list)).unwrap();
        assert_eq!(s, vec![dir.join("third_party/x.cpp")]);

        let missing = vec!["missing.cpp".to_string()];
        let err = collect_sources(&dir, Some(&missing)).unwrap_err();
        assert!(err.contains("does not exist"), "got: {err}");

        fs::write(dir.join("notes.txt"), "").unwrap();
        let bad_ext = vec!["notes.txt".to_string()];
        let err = collect_sources(&dir, Some(&bad_ext)).unwrap_err();
        assert!(err.contains("only accepts"), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn explicit_sources_accept_directory() {
        let dir = temp_dir("explicit-dir");
        fs::create_dir_all(dir.join("app/sub")).unwrap();
        fs::write(dir.join("app/a.cpp"), "//").unwrap();
        fs::write(dir.join("app/sub/b.cc"), "//").unwrap();
        fs::write(dir.join("app/notes.txt"), "ignore").unwrap();
        let list = vec!["app".to_string()];
        let s = collect_sources(&dir, Some(&list)).unwrap();
        let names: Vec<String> = s
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["a.cpp", "b.cc"]);
        let root = vec![".".to_string()];
        let err = collect_sources(&dir, Some(&root)).unwrap_err();
        assert!(err.contains("project root"), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn empty_default_src_message_is_relative() {
        let dir = temp_dir("empty-src-message");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();
        let cfg = crate::config::load(&dir).unwrap();
        let cxx = Cxx { name: "g++".into(), version: "test".into() };
        let err = plan(&dir, &cfg, cxx, false).unwrap_err();
        assert!(err.contains("No .cpp file under src"), "got: {err}");
        assert!(!err.contains(&dir.display().to_string()), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn empty_explicit_sources_message_points_at_sources() {
        let dir = temp_dir("explicit-empty");
        fs::create_dir_all(dir.join("code")).unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\nsources = [\"code\"]\n",
        )
        .unwrap();
        let cfg = crate::config::load(&dir).unwrap();
        let cxx = Cxx { name: "g++".into(), version: "test".into() };
        let err = plan(&dir, &cfg, cxx, false).unwrap_err();
        assert!(err.contains("[project] sources"), "got: {err}");
        assert!(!err.contains("Put source code in src/"), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn same_directory_stem_conflict_detected() {
        let dir = temp_dir("same-dir-conflict");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/util.cpp"), "//").unwrap();
        fs::write(dir.join("src/util.cc"), "//").unwrap();
        let err = collect_sources(&dir, None).unwrap_err();
        assert!(err.contains("object path conflict"), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn object_path_cannot_be_parent_directory() {
        let dir = temp_dir("obj-parent");
        fs::create_dir_all(dir.join("src/foo.o")).unwrap();
        fs::write(dir.join("src/foo.cpp"), "//").unwrap();
        fs::write(dir.join("src/foo.o/bar.cpp"), "//").unwrap();
        let err = collect_sources(&dir, None).unwrap_err();
        assert!(err.contains("conflicts with a directory"), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn duplicate_explicit_source_reported_as_duplicate() {
        let dir = temp_dir("dup-source");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "//").unwrap();
        let list = vec!["src/main.cpp".to_string(), "src/main.cpp".to_string()];
        let err = collect_sources(&dir, Some(&list)).unwrap_err();
        assert!(err.contains("more than once"), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn depfile_parsing_with_continuation() {
        let dir = temp_dir("depfile");
        let f = dir.join("main.d");
        fs::write(
            &f,
            "build/obj/main.o: \\\n src/main.cpp src/util.hpp \\\n third_party/x.h\n\nsrc/util.hpp:\n",
        )
        .unwrap();
        let inputs = dep_inputs(&f).unwrap();
        let names: Vec<String> = inputs
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        assert!(names.iter().any(|p| p.contains("main.cpp")));
        assert_eq!(names.len(), 3, "should skip -MP phony line: {names:?}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn depfile_parsing_handles_escaped_spaces() {
        let dir = temp_dir("depfile-space");
        let f = dir.join("main.d");
        fs::write(
            &f,
            "main.o: C:\\path\\ with\\ space\\src\\main.cpp \\\n  C:\\path\\ with\\ space\\src\\h.h\n\n\
             C:\\path\\ with\\ space\\src\\h.h:\n",
        )
        .unwrap();
        let inputs = dep_inputs(&f).unwrap();
        let names: Vec<String> = inputs
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        assert_eq!(names.len(), 2, "space must stay inside the token: {names:?}");
        assert!(
            names.iter().any(|p| p.contains("with space")),
            "escaped space should be restored: {names:?}"
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn obj_missing_means_recompile() {
        let dir = temp_dir("objmissing");
        let src = dir.join("a.cpp");
        fs::write(&src, "//").unwrap();
        let obj = dir.join("nope.o");
        assert!(needs_recompile(&src, &obj));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn unreadable_depfile_triggers_recompile() {
        let dir = temp_dir("baddep");
        let src = dir.join("a.cpp");
        let obj = dir.join("a.o");
        fs::write(&src, "//").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&obj, "").unwrap();
        // A directory masquerading as .d: exists but unreadable — must be treated as "needs recompile"
        fs::create_dir(dir.join("a.o.d")).unwrap();
        assert!(needs_recompile(&src, &obj));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn malformed_depfile_triggers_recompile() {
        let dir = temp_dir("garbagedep");
        let src = dir.join("a.cpp");
        let obj = dir.join("a.o");
        fs::write(&src, "//").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&obj, "").unwrap();
        fs::write(dir.join("a.o.d"), "garbage without target separator").unwrap();
        assert!(needs_recompile(&src, &obj));
        fs::remove_dir_all(dir).ok();
    }

    // ── P0: incremental build correctness (real-compiler end-to-end) ───────────────
    // Shared premise: after a config or source-set change, the artifact must change accordingly;
    // when nothing changed, objects and exe must not move a single byte.

    fn have_compiler() -> bool {
        crate::compiler::pick(None).is_ok()
    }

    fn build_project(dir: &Path) -> Result<PathBuf, String> {
        let cfg = crate::config::load(dir)?;
        let cxx = crate::compiler::pick(None)?;
        let plan = plan(dir, &cfg, cxx, false)?;
        build(dir, &cfg, &plan)
    }

    /// Find stem.o under build/<scope>/obj/ (scope name varies by platform, tests do not hardcode it).
    fn find_named(dir: &Path, name: &str) -> Option<PathBuf> {
        for entry in fs::read_dir(dir).ok()?.flatten() {
            let path = entry.path();
            if path.is_file() && path.file_name().is_some_and(|n| n == name) {
                return Some(path);
            }
            if path.is_dir() {
                if let Some(found) = find_named(&path, name) {
                    return Some(found);
                }
            }
        }
        None
    }

    fn obj_path(dir: &Path, stem: &str) -> PathBuf {
        let target = format!("{stem}.o");
        for scope in fs::read_dir(dir.join("build")).unwrap().flatten() {
            if let Some(path) = find_named(&scope.path().join("obj"), &target) {
                return path;
            }
        }
        panic!("did not find {target}");
    }

    fn scope_of(dir: &Path, path: &Path) -> PathBuf {
        for scope in fs::read_dir(dir.join("build")).unwrap().flatten() {
            if path.starts_with(scope.path()) {
                return scope.path();
            }
        }
        panic!("did not find a build scope for {}", path.display());
    }

    fn mtime(p: &Path) -> std::time::SystemTime {
        fs::metadata(p).unwrap().modified().unwrap()
    }

    fn run_code(exe: &Path) -> i32 {
        Command::new(exe)
            .status()
            .unwrap()
            .code()
            .unwrap_or(-1)
    }

    fn run_stdout(exe: &Path) -> String {
        let out = Command::new(exe).output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[test]
    fn no_op_rebuild_keeps_objects_and_exe() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("noop");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}\n").unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();

        let exe = build_project(&dir).unwrap();
        let obj = obj_path(&dir, "main");
        let (obj_t, exe_t) = (mtime(&obj), mtime(&exe));
        std::thread::sleep(std::time::Duration::from_millis(50));
        build_project(&dir).unwrap();
        assert_eq!(mtime(&obj), obj_t, "a no-change build must not recompile objects");
        assert_eq!(mtime(&exe), exe_t, "a no-change build must not relink the exe");
        fs::remove_dir_all(dir).ok();
    }

    /// A compiler flags change touches no source file, but must invalidate objects:
    /// use -DVALUE=n so the old object's and old exe's return value is observable.
    #[test]
    fn compile_flag_change_rebuilds_objects() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("cflags");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(
            dir.join("src/main.cpp"),
            "#ifndef VALUE\n#define VALUE 0\n#endif\nint main(){return VALUE;}\n",
        )
        .unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n[compiler]\nflags = [\"-DVALUE=1\"]\n",
        )
        .unwrap();

        let exe = build_project(&dir).unwrap();
        // Runtime copy: on Windows a just-executed exe may be briefly held by the OS, blocking relink
        let first = dir.join("first.exe");
        fs::copy(&exe, &first).unwrap();
        assert_eq!(run_code(&first), 1, "the first build must compile with VALUE=1");
        let obj = obj_path(&dir, "main");
        let obj_t = mtime(&obj);
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n[compiler]\nflags = [\"-DVALUE=2\"]\n",
        )
        .unwrap();

        let exe = build_project(&dir).unwrap();
        assert_eq!(
            run_code(&exe),
            2,
            "a flags change must recompile objects and relink the exe (otherwise still the old VALUE=1)"
        );
        assert_ne!(mtime(&obj), obj_t, "a flags change must recompile objects");
        fs::remove_dir_all(dir).ok();
    }

    /// link_flags only affect the link: the exe must update, objects must stay untouched.
    #[test]
    fn link_flag_change_relinks_without_recompiling() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("lflags");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}\n").unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n[compiler]\nlink_flags = []\n",
        )
        .unwrap();

        let exe = build_project(&dir).unwrap();
        let obj = obj_path(&dir, "main");
        let (obj_t, exe_t) = (mtime(&obj), mtime(&exe));
        std::thread::sleep(std::time::Duration::from_millis(50));
        // -g is accepted on both GNU and MSVC targets; -lm only exists on MinGW/GNU toolchains
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n[compiler]\nlink_flags = [\"-g\"]\n",
        )
        .unwrap();

        let exe = build_project(&dir).unwrap();
        assert_ne!(mtime(&exe), exe_t, "a link_flags change must relink the exe");
        assert_eq!(mtime(&obj), obj_t, "a link_flags change must not recompile objects");
        fs::remove_dir_all(dir).ok();
    }

    /// After deleting a source file the object set shrinks: even if all remaining objects are older
    /// than the exe, a relink is still required, otherwise the old exe still hides the deleted
    /// translation unit's code.
    #[test]
    fn deleting_source_forces_relink() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("delsrc");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(
            dir.join("src/main.cpp"),
            "#include <cstdio>\nint main(){ std::printf(\"main\\n\"); return 0; }\n",
        )
        .unwrap();
        // Standalone static-initialization translation unit: main never references it, so after
        // deletion the old object is not rejected by the linker
        fs::write(
            dir.join("src/helper.cpp"),
            "#include <cstdio>\nstruct Helper { Helper(){ std::printf(\"helper\\n\"); } };\nstatic Helper h;\n",
        )
        .unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();

        let exe = build_project(&dir).unwrap();
        let first = dir.join("first.exe");
        fs::copy(&exe, &first).unwrap();
        assert!(run_stdout(&first).contains("helper"), "the first build must include helper");
        let main_obj_t = mtime(&obj_path(&dir, "main"));
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::remove_file(dir.join("src/helper.cpp")).unwrap();

        let exe = build_project(&dir).unwrap();
        let out = run_stdout(&exe);
        assert!(out.contains("main"), "the rebuilt program must run normally: {out:?}");
        assert!(
            !out.contains("helper"),
            "after deleting a source a relink is required; the old exe still contains the deleted translation unit: {out:?}"
        );
        assert_eq!(run_code(&exe), 0);
        assert_eq!(mtime(&obj_path(&dir, "main")), main_obj_t, "an unchanged source must not recompile");
        fs::remove_dir_all(dir).ok();
    }

    // ── v0.2.1: modern C++ layout (recursive src/ + include/ auto -I) ──

    #[test]
    fn collect_sources_recurses_nested_dirs() {
        let dir = temp_dir("nested");
        fs::create_dir_all(dir.join("src/cli")).unwrap();
        fs::create_dir_all(dir.join("src/core/deep")).unwrap();
        fs::write(dir.join("src/cli/main.cpp"), "//").unwrap();
        fs::write(dir.join("src/core/util.cc"), "//").unwrap();
        fs::write(dir.join("src/core/deep/x.cxx"), "//").unwrap();
        fs::write(dir.join("src/notes.txt"), "ignore").unwrap();
        let s = collect_sources(&dir, None).unwrap();
        assert_eq!(s.len(), 3, "should recursively collect three levels of source files: {s:?}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn same_stem_in_different_dirs_allowed() {
        let dir = temp_dir("nested-same-stem");
        fs::create_dir_all(dir.join("src/a")).unwrap();
        fs::create_dir_all(dir.join("src/b")).unwrap();
        fs::write(dir.join("src/a/core.cpp"), "//").unwrap();
        fs::write(dir.join("src/b/core.cpp"), "//").unwrap();
        let sources = collect_sources(&dir, None).unwrap();
        assert_eq!(sources.len(), 2, "same stem across directories must be allowed");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn same_stem_in_different_dirs_builds() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("nested-same-stem-build");
        fs::create_dir_all(dir.join("src/a")).unwrap();
        fs::create_dir_all(dir.join("src/b")).unwrap();
        fs::write(dir.join("src/a/util.cpp"), "int a(){ return 1; }\n").unwrap();
        fs::write(dir.join("src/b/util.cpp"), "int b(){ return 2; }\n").unwrap();
        fs::write(
            dir.join("src/main.cpp"),
            "int a(); int b();\nint main(){ return a() + b() == 3 ? 0 : 1; }\n",
        )
        .unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();
        let exe = build_project(&dir).unwrap();
        assert_eq!(run_code(&exe), 0);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn default_src_link_outside_project_rejected() {
        let dir = temp_dir("link-src");
        let outside = temp_dir("link-src-outside");
        fs::write(outside.join("x.cpp"), "//").unwrap();
        if !make_dir_link(&dir.join("src"), &outside) {
            fs::remove_dir_all(dir).ok();
            fs::remove_dir_all(outside).ok();
            return;
        }
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();
        let cfg = crate::config::load(&dir).unwrap();
        let cxx = Cxx { name: "g++".into(), version: "test".into() };
        let err = plan(&dir, &cfg, cxx, false).unwrap_err();
        assert!(err.contains("outside the project"), "got: {err}");
        remove_dir_link(&dir.join("src"));
        fs::remove_dir_all(dir).ok();
        fs::remove_dir_all(outside).ok();
    }

    #[test]
    fn target_source_link_outside_project_rejected() {
        let dir = temp_dir("link-target");
        let outside = temp_dir("link-target-outside");
        fs::write(outside.join("x.cpp"), "//").unwrap();
        if !make_dir_link(&dir.join("applink"), &outside) {
            fs::remove_dir_all(dir).ok();
            fs::remove_dir_all(outside).ok();
            return;
        }
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"applink\"]\n",
        )
        .unwrap();
        let cfg = crate::config::load(&dir).unwrap();
        let cxx = Cxx { name: "g++".into(), version: "test".into() };
        let err = plan(&dir, &cfg, cxx, false).unwrap_err();
        assert!(err.contains("outside the project"), "got: {err}");
        remove_dir_link(&dir.join("applink"));
        fs::remove_dir_all(dir).ok();
        fs::remove_dir_all(outside).ok();
    }

    #[test]
    fn include_dir_is_on_include_path() {
        let dir = temp_dir("incflags");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join("include")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}").unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname=\"t\"\n").unwrap();
        let cfg = crate::config::load(&dir).unwrap();
        let cxx = Cxx { name: "g++".into(), version: "test".into() };
        let p = plan(&dir, &cfg, cxx, false).unwrap();
        let expected = format!("-I{}", dir.join("include").display());
        assert!(p.flags.contains(&expected), "flags missing {expected}: {:?}", p.flags);
        fs::remove_dir_all(dir).ok();
    }

    /// Modern-layout end-to-end: sources in a subdirectory, public headers in include/; must compile
    /// successfully and run correctly.
    #[test]
    fn nested_sources_use_include_dir() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("nested-e2e");
        fs::create_dir_all(dir.join("src/app")).unwrap();
        fs::create_dir_all(dir.join("include")).unwrap();
        fs::write(dir.join("include/greet.h"), "#pragma once\nint greet();\n").unwrap();
        fs::write(
            dir.join("src/app/main.cpp"),
            "#include \"greet.h\"\nint main(){ return greet() == 7 ? 0 : 1; }\n",
        )
        .unwrap();
        fs::write(
            dir.join("src/app/greet.cpp"),
            "#include \"greet.h\"\nint greet(){ return 7; }\n",
        )
        .unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();

        let exe = build_project(&dir).unwrap();
        assert_eq!(run_code(&exe), 0, "subdirectory sources + include/ must build and run normally");
        fs::remove_dir_all(dir).ok();
    }

    // ── v0.3: compile_commands.json (off by default, explicitly enabled via toml) ──

    #[test]
    fn json_escape_handles_windows_paths_and_quotes() {
        assert_eq!(json_escape(r"C:\a\b"), r"C:\\a\\b");
        assert_eq!(json_escape("a\"b"), r#"a\"b"#);
        assert_eq!(json_escape("a\nb"), r"a\nb");
    }

    #[test]
    fn render_cpptools_config_uses_compiler_and_standard() {
        let json = render_cpptools_config(r"C:\MinGW\bin\g++.exe", "c++23");
        assert!(json.contains("\"version\": 4"), "got: {json}");
        assert!(
            json.contains("\"compilerPath\": \"C:/MinGW/bin/g++.exe\""),
            "got: {json}"
        );
        assert!(json.contains("\"cppStandard\": \"c++23\""), "got: {json}");
        assert!(
            json.contains("\"compileCommands\": \"${workspaceFolder}/compile_commands.json\""),
            "got: {json}"
        );
        if cfg!(windows) {
            assert!(
                json.contains("\"intelliSenseMode\": \"windows-gcc-x64\""),
                "got: {json}"
            );
        }
    }

    #[test]
    fn cpp_standard_uses_effective_last_flag() {
        let flags = vec!["-std=c++17".to_string(), "-std=c++20".to_string()];
        assert_eq!(cpp_standard(&flags), "c++20");
        assert_eq!(cpp_standard(&[]), "c++23");
    }

    #[test]
    fn cpptools_config_is_created_once_and_never_overwritten() {
        let dir = temp_dir("vscode-cfg");
        assert!(
            ensure_cpptools_config(&dir, r"C:\MinGW\bin\g++.exe", "c++23").unwrap(),
            "should generate config when missing"
        );
        let path = dir.join(".vscode").join("c_cpp_properties.json");
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("compile_commands.json"), "got: {text}");

        fs::write(&path, "{\"sentinel\": true}\n").unwrap();
        assert!(
            !ensure_cpptools_config(&dir, r"C:\LLVM\bin\clang++.exe", "c++20").unwrap(),
            "existing file must not be rewritten"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"sentinel\": true}\n");

        fs::remove_file(&path).unwrap();
        assert!(
            ensure_cpptools_config(&dir, r"C:\MinGW\bin\g++.exe", "c++23").unwrap(),
            "should regenerate after deletion"
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn cpptools_config_reports_vscode_file() {
        let dir = temp_dir("vscode-file");
        fs::write(dir.join(".vscode"), "x").unwrap();
        let err = ensure_cpptools_config(&dir, "g++", "c++23").unwrap_err();
        assert!(err.contains(".vscode"), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn render_compile_db_emits_entries() {
        let flags_a = vec!["-std=c++23".to_string(), "-IX".to_string()];
        let flags_b = vec!["-O2".to_string()];
        let entries = [
            (Path::new("src/a.cpp"), flags_a.as_slice()),
            (Path::new("src/b.cpp"), flags_b.as_slice()),
        ];
        let json = render_compile_db(Path::new("proj"), "g++", &entries);
        assert!(json.starts_with("[\n  {"), "got: {json}");
        assert_eq!(json.matches("\"file\"").count(), 2);
        assert!(json.contains(
            "\"arguments\": [\"g++\", \"-c\", \"src/a.cpp\", \"-std=c++23\", \"-IX\"]"
        ));
        assert!(json.contains("},\n  {"), "entries must be separated by a comma: {json}");
        assert!(json.trim_end().ends_with(']'));
    }

    #[test]
    fn compile_db_absent_by_default() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("db-off");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}\n").unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();

        build_project(&dir).unwrap();
        assert!(
            !dir.join("compile_commands.json").exists(),
            "when not enabled, compile_commands.json must not be generated"
        );
        assert!(
            !dir.join(".vscode").join("c_cpp_properties.json").exists(),
            "when not enabled, the VS Code IntelliSense config must not be generated"
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn compile_db_written_when_enabled() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("db-on");
        fs::create_dir_all(dir.join("src/cli")).unwrap();
        fs::create_dir_all(dir.join("include")).unwrap();
        fs::write(dir.join("include/greet.h"), "#pragma once\n").unwrap();
        fs::write(
            dir.join("src/cli/main.cpp"),
            "#include \"greet.h\"\nint main(){return 0;}\n",
        )
        .unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\nedition = \"c++17\"\ncompile_commands = true\n\n\
             [compiler]\nflags = [\"-Wall\"]\n",
        )
        .unwrap();

        build_project(&dir).unwrap();
        let text = fs::read_to_string(dir.join("compile_commands.json")).unwrap();
        assert!(text.trim_start().starts_with('['), "should be a JSON array: {text}");
        assert_eq!(text.matches("\"file\"").count(), 1, "got: {text}");
        assert_eq!(text.matches("\"arguments\"").count(), 1, "got: {text}");
        let file = json_escape(
            &dir.join("src")
                .join("cli")
                .join("main.cpp")
                .display()
                .to_string(),
        );
        assert!(text.contains(&format!("\"file\": \"{file}\"")), "got: {text}");
        assert!(text.contains("-std=c++17"), "got: {text}");
        assert!(text.contains("-Wall"), "got: {text}");
        let include = json_escape(&format!("-I{}", dir.join("include").display()));
        assert!(text.contains(&format!("\"{include}\"")), "got: {text}");
        if cfg!(windows) {
            assert!(text.contains(r"\\"), "Windows path backslashes must be escaped: {text}");
        }
        let vscode_cfg = dir.join(".vscode").join("c_cpp_properties.json");
        let vscode_text = fs::read_to_string(&vscode_cfg).unwrap();
        assert!(
            vscode_text.contains("\"cppStandard\": \"c++17\""),
            "got: {vscode_text}"
        );
        assert!(
            vscode_text
                .contains("\"compileCommands\": \"${workspaceFolder}/compile_commands.json\""),
            "got: {vscode_text}"
        );
        assert!(
            vscode_text.contains("\"compilerPath\": \""),
            "got: {vscode_text}"
        );
        let db = dir.join("compile_commands.json");
        let db_t = mtime(&db);
        let cfg_t = mtime(&vscode_cfg);
        std::thread::sleep(std::time::Duration::from_millis(50));
        build_project(&dir).unwrap();
        assert_eq!(mtime(&db), db_t, "content unchanged, must not rewrite compile_commands.json");
        assert_eq!(mtime(&vscode_cfg), cfg_t, "existing c_cpp_properties.json must not be rewritten");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn compile_failure_then_fix_recovers() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("recover");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(
            dir.join("src/main.cpp"),
            "int main(){ return does_not_exist; }\n",
        )
        .unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();

        let err = build_project(&dir).unwrap_err();
        assert!(err.contains("Compile failed"), "got: {err}");

        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}\n").unwrap();
        let exe = build_project(&dir).unwrap();
        assert_eq!(run_code(&exe), 0);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn header_change_recompiles_only_dependent_tu() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("header-inc");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join("include")).unwrap();
        fs::write(dir.join("include/util.h"), "int value();\n").unwrap();
        fs::write(
            dir.join("src/main.cpp"),
            "#include \"util.h\"\nint main(){ return value(); }\n",
        )
        .unwrap();
        fs::write(dir.join("src/util.cpp"), "int value(){ return 0; }\n").unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();

        build_project(&dir).unwrap();
        let main_obj = obj_path(&dir, "main");
        let util_obj = obj_path(&dir, "util");
        let (main_t, util_t) = (mtime(&main_obj), mtime(&util_obj));
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(
            dir.join("include/util.h"),
            "// changed\nint value();\n",
        )
        .unwrap();
        build_project(&dir).unwrap();
        assert_ne!(mtime(&main_obj), main_t, "a TU including the header must recompile");
        assert_eq!(mtime(&util_obj), util_t, "a TU not including that header must not recompile");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn missing_version_marker_invalidates_scope() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("marker-missing");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}\n").unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();
        build_project(&dir).unwrap();
        let obj = obj_path(&dir, "main");
        let obj_t = mtime(&obj);
        let scope = scope_of(&dir, &obj);
        fs::remove_file(scope.join(".compiler-version")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        build_project(&dir).unwrap();
        assert_ne!(mtime(&obj), obj_t, "when the version fingerprint is missing, rebuild");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn build_aborts_when_object_dir_cannot_be_reset() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("obj-blocked");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}\n").unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n[compiler]\nflags = [\"-DVALUE=1\"]\n",
        )
        .unwrap();
        build_project(&dir).unwrap();
        let obj = obj_path(&dir, "main");
        let obj_dir = scope_of(&dir, &obj).join("obj");
        fs::remove_dir_all(&obj_dir).unwrap();
        fs::write(&obj_dir, "").unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n[compiler]\nflags = [\"-DVALUE=2\"]\n",
        )
        .unwrap();
        let err = build_project(&dir).unwrap_err();
        assert!(
            err.contains("Cannot clear") || err.contains("Cannot read"),
            "got: {err}"
        );
        fs::remove_file(&obj_dir).ok();
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn old_object_layout_is_cleared_once() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("obj-layout");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}\n").unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();
        build_project(&dir).unwrap();
        let obj = obj_path(&dir, "main");
        let scope = scope_of(&dir, &obj);
        fs::remove_file(scope.join(".obj-layout")).unwrap();
        let stale_obj = scope.join("obj/legacy.o");
        let stale_dep = scope.join("obj/dep/dep.o");
        let stale_archive = scope.join("lib/x/libx.a");
        fs::create_dir_all(stale_dep.parent().unwrap()).unwrap();
        fs::create_dir_all(stale_archive.parent().unwrap()).unwrap();
        fs::write(&stale_obj, "").unwrap();
        fs::write(&stale_dep, "").unwrap();
        fs::write(&stale_archive, "").unwrap();
        build_project(&dir).unwrap();
        assert!(!stale_obj.exists(), "the old object layout must be cleared as a whole batch");
        assert!(!stale_dep.exists(), "stale dependency objects must be cleaned up");
        assert!(!stale_archive.exists(), "stale library archives must be cleaned up");
        assert!(obj.exists(), "objects in the new layout should be rebuilt");

        let obj_t = mtime(&obj);
        std::thread::sleep(std::time::Duration::from_millis(50));
        build_project(&dir).unwrap();
        assert_eq!(mtime(&obj), obj_t, "the layout migration should happen only once");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn compiler_version_change_invalidates_scope() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("compiler-version");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}\n").unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();
        let cfg = crate::config::load(&dir).unwrap();

        let mut cxx = crate::compiler::pick(None).unwrap();
        cxx.version = "test-v1".into();
        let p1 = plan(&dir, &cfg, cxx.clone(), false).unwrap();
        build(&dir, &cfg, &p1).unwrap();
        let obj = obj_path(&dir, "main");
        let obj_t = mtime(&obj);

        std::thread::sleep(std::time::Duration::from_millis(50));
        cxx.version = "test-v2".into();
        let p2 = plan(&dir, &cfg, cxx, false).unwrap();
        build(&dir, &cfg, &p2).unwrap();
        assert_ne!(mtime(&obj), obj_t, "a compiler version change must invalidate the whole scope");
        fs::remove_dir_all(dir).ok();
    }

    // ── v0.4: single bin + multiple static libraries ──────────────────────────

    fn target_toml(flags: Option<&str>) -> String {
        let compiler = flags
            .map(|f| format!("[compiler]\nflags = {f}\n\n"))
            .unwrap_or_default();
        format!(
            "[project]\nname = \"t\"\nedition = \"c++23\"\ncompile_commands = true\n\n\
             {compiler}[[target]]\nname = \"foo\"\nkind = \"static_lib\"\nsources = [\"lib\"]\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [\"foo\"]\n"
        )
    }

    fn target_project(tag: &str) -> PathBuf {
        let dir = temp_dir(tag);
        for sub in ["src", "lib", "include"] {
            fs::create_dir_all(dir.join(sub)).unwrap();
        }
        fs::write(dir.join("include/foo.h"), "#pragma once\nint foo();\n").unwrap();
        fs::write(
            dir.join("lib/foo.cpp"),
            "#include \"foo.h\"\nint foo(){ return 1; }\n",
        )
        .unwrap();
        fs::write(
            dir.join("src/main.cpp"),
            "#include \"foo.h\"\nint main(){ return foo() == 1 ? 0 : 1; }\n",
        )
        .unwrap();
        fs::write(dir.join("cizon.toml"), target_toml(None)).unwrap();
        dir
    }

    fn lib_archive_path(dir: &Path, lib: &str) -> PathBuf {
        for scope in fs::read_dir(dir.join("build")).unwrap().flatten() {
            let path = scope
                .path()
                .join("lib")
                .join(lib)
                .join(format!("lib{lib}.a"));
            if path.exists() {
                return path;
            }
        }
        panic!("did not find lib{lib}.a");
    }

    fn libs_supported() -> bool {
        have_compiler()
            && crate::compiler::pick(None)
                .ok()
                .is_some_and(|cxx| crate::compiler::archiver(&cxx).is_ok())
    }

    #[test]
    fn targets_build_static_lib_and_link() {
        if !libs_supported() {
            return;
        }
        let dir = target_project("lib-e2e");
        let exe = build_project(&dir).unwrap();
        assert_eq!(run_code(&exe), 0);
        assert!(lib_archive_path(&dir, "foo").is_file());
        let db = fs::read_to_string(dir.join("compile_commands.json")).unwrap();
        assert_eq!(db.matches("\"file\"").count(), 2, "bin+lib two sources: {db}");
        assert!(db.contains("foo.cpp"), "got: {db}");
        assert!(db.contains("main.cpp"), "got: {db}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn lib_source_change_rebuilds_archive_without_recompiling_bin() {
        if !libs_supported() {
            return;
        }
        let dir = target_project("lib-inc");
        let exe = build_project(&dir).unwrap();
        let archive = lib_archive_path(&dir, "foo");
        let main_obj = obj_path(&dir, "main");
        let (archive_t, main_t, exe_t) = (mtime(&archive), mtime(&main_obj), mtime(&exe));
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(
            dir.join("lib/foo.cpp"),
            "#include \"foo.h\"\nint foo(){ return 2; }\n",
        )
        .unwrap();
        let exe = build_project(&dir).unwrap();
        assert_ne!(mtime(&archive), archive_t, "a library source change must rebuild the archive");
        assert_eq!(mtime(&main_obj), main_t, "a library source change must not recompile bin sources");
        assert_ne!(mtime(&exe), exe_t, "an archive change must relink");
        // main still checks foo()==1, the new library returns 2 → exit code 1, proving the link used the new code
        assert_eq!(run_code(&exe), 1, "the exe must contain the new library code");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn bin_source_change_keeps_archive() {
        if !libs_supported() {
            return;
        }
        let dir = target_project("bin-inc");
        let exe = build_project(&dir).unwrap();
        let archive = lib_archive_path(&dir, "foo");
        let (archive_t, exe_t) = (mtime(&archive), mtime(&exe));
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(
            dir.join("src/main.cpp"),
            "#include \"foo.h\"\n// changed\nint main(){ return foo() == 1 ? 0 : 1; }\n",
        )
        .unwrap();
        let exe = build_project(&dir).unwrap();
        assert_eq!(mtime(&archive), archive_t, "changing only the bin must not rebuild the archive");
        assert_ne!(mtime(&exe), exe_t);
        fs::remove_dir_all(dir).ok();
    }

    fn rename_config(name: &str) -> String {
        format!(
            "[project]\nname = \"t\"\nedition = \"c++23\"\n\n\
             [[target]]\nname = {name:?}\nkind = \"bin\"\nsources = [\"src\"]\n"
        )
    }

    #[test]
    fn library_allows_same_stem_across_dirs() {
        if !libs_supported() {
            return;
        }
        let dir = temp_dir("lib-same-stem");
        fs::create_dir_all(dir.join("lib/a")).unwrap();
        fs::create_dir_all(dir.join("lib/b")).unwrap();
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("lib/a/util.cpp"), "int a(){ return 1; }\n").unwrap();
        fs::write(dir.join("lib/b/util.cpp"), "int b(){ return 2; }\n").unwrap();
        fs::write(
            dir.join("src/main.cpp"),
            "int a(); int b();\nint main(){ return a() + b() == 3 ? 0 : 1; }\n",
        )
        .unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n\
             [[target]]\nname = \"foo\"\nkind = \"static_lib\"\nsources = [\"lib\"]\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [\"foo\"]\n",
        )
        .unwrap();
        let exe = build_project(&dir).unwrap();
        assert_eq!(run_code(&exe), 0);
        assert!(lib_archive_path(&dir, "foo").is_file());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn renaming_bin_target_removes_old_exe() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("rename-bin");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}\n").unwrap();
        fs::write(dir.join("cizon.toml"), rename_config("old")).unwrap();
        let old_exe = build_project(&dir).unwrap();
        assert!(old_exe.is_file());
        let old_pdb = old_exe.with_extension("pdb");
        fs::write(&old_pdb, "").unwrap();

        fs::write(dir.join("cizon.toml"), rename_config("new")).unwrap();
        let new_exe = build_project(&dir).unwrap();
        assert!(!old_exe.exists(), "after renaming the bin, the old exe must be cleaned up");
        assert!(!old_pdb.exists(), "the old exe's .pdb should be cleaned up too");
        assert!(new_exe.is_file());
        assert_eq!(run_code(&new_exe), 0);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn case_only_bin_rename_keeps_new_exe() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("rename-case");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}\n").unwrap();
        fs::write(dir.join("cizon.toml"), rename_config("case")).unwrap();
        let _old_exe = build_project(&dir).unwrap();

        fs::write(dir.join("cizon.toml"), rename_config("CASE")).unwrap();
        let new_exe = build_project(&dir).unwrap();
        assert!(new_exe.is_file(), "a case-only rename must not delete the new exe");
        assert_eq!(run_code(&new_exe), 0);
        #[cfg(unix)]
        assert!(!_old_exe.exists(), "on Unix the old exe with a different case should be deleted");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn failed_build_after_rename_keeps_old_exe() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("rename-fail");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}\n").unwrap();
        fs::write(dir.join("cizon.toml"), rename_config("old")).unwrap();
        let old_exe = build_project(&dir).unwrap();

        fs::write(
            dir.join("src/main.cpp"),
            "int main(){ return does_not_exist; }\n",
        )
        .unwrap();
        fs::write(dir.join("cizon.toml"), rename_config("new")).unwrap();
        let err = build_project(&dir).unwrap_err();
        assert!(err.contains("Compile failed"), "got: {err}");
        assert!(old_exe.exists(), "on build failure, the last runnable artifact must not be deleted");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn project_flag_change_rebuilds_library() {
        if !libs_supported() {
            return;
        }
        let dir = target_project("lib-flags");
        build_project(&dir).unwrap();
        let archive = lib_archive_path(&dir, "foo");
        let lib_obj = find_named(&archive.parent().unwrap().join("obj"), "foo.o").unwrap();
        let main_obj = obj_path(&dir, "main");
        let (archive_t, obj_t, main_t) = (mtime(&archive), mtime(&lib_obj), mtime(&main_obj));
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(dir.join("cizon.toml"), target_toml(Some("[\"-Wall\"]"))).unwrap();
        build_project(&dir).unwrap();
        assert_ne!(mtime(&lib_obj), obj_t, "a project flags change should recompile library sources");
        assert_ne!(mtime(&main_obj), main_t, "a project flags change should recompile bin sources");
        assert_ne!(mtime(&archive), archive_t, "a library object change should rebuild the archive");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn target_sources_reject_empty_entry() {
        let dir = target_project("lib-empty-src");
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"\"]\n",
        )
        .unwrap();
        let cfg = crate::config::load(&dir).unwrap();
        let cxx = Cxx { name: "g++".into(), version: "test".into() };
        let err = plan(&dir, &cfg, cxx, false).unwrap_err();
        assert!(err.contains("empty path"), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn target_sources_must_stay_inside_project() {
        let dir = target_project("lib-escape");
        let absolute = dir.join("src").to_string_lossy().into_owned();
        for bad in ["../outside", ".", absolute.as_str()] {
            fs::write(
                dir.join("cizon.toml"),
                format!(
                    "[project]\nname = \"t\"\n\n[[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [{bad:?}]\n"
                ),
            )
            .unwrap();
            let cfg = crate::config::load(&dir).unwrap();
            let cxx = Cxx { name: "g++".into(), version: "test".into() };
            let err = plan(&dir, &cfg, cxx, false).unwrap_err();
            assert!(
                err.contains("project-relative path") || err.contains("project root"),
                "source={bad:?} got: {err}"
            );
        }
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn source_switch_with_same_object_path_recompiles() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("bin-switch");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){ return 0; }\n").unwrap();
        fs::write(dir.join("src/main.cc"), "int main(){ return 1; }\n").unwrap();
        let config = |sources: &str| {
            format!(
                "[project]\nname = \"t\"\n\n[[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [{sources:?}]\n"
            )
        };
        fs::write(dir.join("cizon.toml"), config("src/main.cpp")).unwrap();
        let exe = build_project(&dir).unwrap();
        assert_eq!(run_code(&exe), 0);

        // Same object path obj/src/main.o; main.cc is older than the old object and the old .d does
        // not list it either, so it must recompile
        fs::write(dir.join("cizon.toml"), config("src/main.cc")).unwrap();
        let exe = build_project(&dir).unwrap();
        assert_eq!(run_code(&exe), 1, "switching the source at the same object path must recompile via .d source verification");
        fs::remove_dir_all(dir).ok();
    }
}
