use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::compiler::{self, Cxx};
use crate::config::{Config, TargetKind};

use super::output::rel_display;
use super::{BinTarget, LibTarget, LinkTarget, Plan, SharedTarget};

const SOURCE_EXTS: &[&str] = &["cpp", "cc", "cxx"];

pub fn plan(dir: &Path, cfg: &Config, cxx: Cxx, release: bool) -> Result<Plan, String> {
    let (bins, tests, libs, shared) = if let Some(targets) = cfg.targets() {
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
                prefix: resolve_prefix(dir, target.prefix.as_deref(), &format!("static library target {name}"))?,
            });
        }
        let mut shared = Vec::new();
        for target in targets.iter().filter(|t| t.kind == TargetKind::SharedLib) {
            let name = target.name.as_deref().expect("config resolves shared library name");
            let shared_sources = collect_target_sources(
                dir,
                target.sources.as_deref(),
                &format!("shared library target {name}"),
            )?;
            if shared_sources.is_empty() {
                return Err(format!(
                    "shared library target {name} has no source file to compile."
                ));
            }
            let shared_links = target
                .links
                .iter()
                .map(|link| {
                    libs.iter()
                        .find(|lib| lib.name.to_lowercase() == link.to_lowercase())
                        .map(|lib| lib.name.clone())
                        .unwrap_or_else(|| link.clone())
                })
                .collect();
            shared.push(SharedTarget {
                name: name.to_string(),
                sources: shared_sources,
                links: shared_links,
                prefix: resolve_prefix(dir, target.prefix.as_deref(), &format!("shared library target {name}"))?,
            });
        }
        let mut bins = Vec::new();
        for target in targets.iter().filter(|t| t.kind == TargetKind::Bin) {
            let name = target.name.as_deref().expect("config resolves bin name");
            let sources = collect_target_sources(
                dir,
                target.sources.as_deref(),
                &format!("bin target {name}"),
            )?;
            if sources.is_empty() {
                return Err(format!("bin target {name} has no source file to compile."));
            }
            let links = resolve_exec_links(target.links.as_slice(), &libs, &shared);
            bins.push(BinTarget {
                name: name.to_string(),
                sources,
                links,
                out_name: exe_name(name),
                extern_links: Vec::new(),
                prefix: resolve_prefix(dir, target.prefix.as_deref(), &format!("bin target {name}"))?,
            });
        }
        let mut tests = Vec::new();
        for target in targets.iter().filter(|t| t.kind == TargetKind::Test) {
            let name = target.name.as_deref().expect("config resolves test name");
            let sources = collect_target_sources(
                dir,
                target.sources.as_deref(),
                &format!("test target {name}"),
            )?;
            if sources.is_empty() {
                return Err(format!("test target {name} has no source file to compile."));
            }
            let links = resolve_exec_links(target.links.as_slice(), &libs, &shared);
            tests.push(BinTarget {
                name: name.to_string(),
                sources,
                links,
                out_name: exe_name(name),
                extern_links: target.extern_links.clone(),
                prefix: resolve_prefix(dir, target.prefix.as_deref(), &format!("test target {name}"))?,
            });
        }
        (bins, tests, libs, shared)
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
        let name = cfg.name(dir);
        let bins = vec![BinTarget {
            out_name: exe_name(&name),
            name,
            sources,
            links: Vec::new(),
            extern_links: Vec::new(),
            prefix: None,
        }];
        (bins, Vec::new(), Vec::new(), Vec::new())
    };

    let edition = compiler::edition_flag(cfg.edition());
    let optimization = if release { "-O2" } else { "-g" };
    let mut flags = vec![edition.clone(), optimization.into()];
    // Convenience warning switches expand to fixed flag bundles; user flags come after so they
    // can still override (e.g. -Wno-...). These apply to project sources only, never deps.
    if cfg.strict() {
        flags.extend(["-Wall", "-Wextra", "-Wpedantic"].map(String::from));
    }
    if cfg.werror() {
        flags.push("-Werror".into());
    }
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
    let test_flags = without_ndebug(&flags);

    for (src, names) in overlapping_exec_sources(&bins, &tests) {
        eprintln!(
            "Warning: source {} is compiled into multiple executable targets ({}). Recursive sources can pull in another target's files; multiple main() definitions will fail to link. Use explicit file paths or separate directories.",
            rel_display(dir, &src),
            names.join(", ")
        );
    }

    Ok(Plan {
        cxx,
        bins,
        tests,
        flags,
        test_flags,
        link_flags,
        dep_base,
        dep_includes,
        libs,
        shared,
        mode: if release { "release" } else { "debug" },
    })
}

/// Remove every `-DNDEBUG` spelling from test-source flags. Tests must not silently compile out
/// their own asserts: the exit-code contract is only meaningful while `assert` is live.
fn without_ndebug(flags: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(flags.len());
    let mut i = 0;
    while i < flags.len() {
        let flag = &flags[i];
        if flag == "-D"
            && flags
                .get(i + 1)
                .is_some_and(|v| v == "NDEBUG" || v.starts_with("NDEBUG="))
        {
            i += 2;
            continue;
        }
        if flag == "-DNDEBUG" || flag.starts_with("-DNDEBUG=") {
            i += 1;
            continue;
        }
        out.push(flag.clone());
        i += 1;
    }
    out
}

/// Resolve an optional installed-package prefix to an absolute directory inside the project.
/// cizon2 later expands it to -I<prefix>/include (compile) and -L<prefix>/lib (link) for the
/// owning target, so consuming an installed library needs no hand-written search-path flags.
fn resolve_prefix(dir: &Path, raw: Option<&str>, owner: &str) -> Result<Option<PathBuf>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let path = resolve_source_path(dir, raw, &format!("{owner} prefix"))?;
    if !path.is_dir() {
        return Err(format!(
            "{owner} prefix {} is not a directory. Install the package there first (e.g. cmake --install), or remove the prefix.",
            rel_display(dir, &path)
        ));
    }
    ensure_dir_inside(dir, &path, &format!("{owner} prefix"))?;
    Ok(Some(path))
}

/// Source files compiled into more than one executable target (bin/test). Recursive `sources`
/// directories can silently overlap; multiple main() definitions then fail to link.
pub(super) fn overlapping_exec_sources(
    bins: &[BinTarget],
    tests: &[BinTarget],
) -> Vec<(PathBuf, Vec<String>)> {
    let mut map: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    for target in bins.iter().chain(tests.iter()) {
        for src in &target.sources {
            map.entry(src.clone()).or_default().push(target.name.clone());
        }
    }
    map.into_iter().filter(|(_, names)| names.len() > 1).collect()
}

/// Resolve bin/test `links` entries against the planned libraries (bin and test targets link the
/// same artifact kinds: static archives and shared import libraries).
fn resolve_exec_links(
    links: &[String],
    libs: &[LibTarget],
    shared: &[SharedTarget],
) -> Vec<LinkTarget> {
    links
        .iter()
        .map(|link| {
            let lower = link.to_lowercase();
            if let Some(lib) = libs.iter().find(|lib| lib.name.to_lowercase() == lower) {
                LinkTarget::Static(lib.name.clone())
            } else if let Some(sh) = shared.iter().find(|sh| sh.name.to_lowercase() == lower) {
                LinkTarget::Shared(sh.name.clone())
            } else {
                LinkTarget::Static(link.clone())
            }
        })
        .collect()
}

pub(super) fn exe_name(name: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// Output path: relative to the project root; falls back to the original path when outside it.

pub(super) fn resolve_source_path(dir: &Path, raw: &str, owner: &str) -> Result<PathBuf, String> {
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
pub(super) fn ensure_dir_inside(dir: &Path, path: &Path, owner: &str) -> Result<(), String> {
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
pub(super) fn collect_sources(dir: &Path, explicit: Option<&[String]>) -> Result<Vec<PathBuf>, String> {
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
pub(super) fn collect_target_sources(
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
pub(super) fn check_object_paths(dir: &Path, sources: &[PathBuf], owner: &str) -> Result<(), String> {
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
pub(super) fn project_relative(dir: &Path, src: &Path) -> Result<PathBuf, String> {
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

pub(super) fn is_source(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| SOURCE_EXTS.iter().any(|ext| ext.eq_ignore_ascii_case(e)))
}

/// Recursively collect source files under a directory. Do not follow symlink/junction
/// directories (cycle guard); linked source files are still collected.
pub(super) fn collect_recursive(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
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

pub(super) fn check_source_ext(path: &Path) -> Result<(), String> {
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

pub(super) fn obj_for(dir: &Path, src: &Path, obj_dir: &Path) -> Result<PathBuf, String> {
    let mut rel = project_relative(dir, src)?;
    rel.set_extension("o");
    Ok(obj_dir.join(rel))
}

pub(super) fn lib_obj_dir(scope_dir: &Path, name: &str) -> PathBuf {
    scope_dir.join("lib").join(name).join("obj")
}

pub(super) fn lib_obj_for(
    dir: &Path,
    scope_dir: &Path,
    name: &str,
    src: &Path,
) -> Result<PathBuf, String> {
    let mut rel = project_relative(dir, src)?;
    rel.set_extension("o");
    Ok(lib_obj_dir(scope_dir, name).join(rel))
}

pub(super) fn shared_obj_dir(scope_dir: &Path, name: &str) -> PathBuf {
    scope_dir.join(".shared").join(name).join("obj")
}

pub(super) fn bin_obj_dir(scope_dir: &Path, name: &str, multi: bool) -> PathBuf {
    if multi {
        scope_dir.join("obj").join(name)
    } else {
        scope_dir.join("obj")
    }
}

/// Test objects live in their own tree so `build`/`run` (obj/) and `cizon test` (test-obj/)
/// never invalidate each other's artifacts, even in a single-bin project where obj/ mirrors
/// src/ paths directly.
pub(super) fn test_obj_dir(scope_dir: &Path, name: &str) -> PathBuf {
    scope_dir.join("test-obj").join(name)
}

pub(super) fn exec_obj_dir(scope_dir: &Path, name: &str, multi: bool, set: super::ExecSet) -> PathBuf {
    match set {
        super::ExecSet::Bins => bin_obj_dir(scope_dir, name, multi),
        super::ExecSet::Tests => test_obj_dir(scope_dir, name),
    }
}

pub(super) fn shared_obj_for(
    dir: &Path,
    scope_dir: &Path,
    name: &str,
    src: &Path,
) -> Result<PathBuf, String> {
    let mut rel = project_relative(dir, src)?;
    rel.set_extension("o");
    Ok(shared_obj_dir(scope_dir, name).join(rel))
}

#[cfg(test)]
mod tests {
    use super::without_ndebug;

    #[test]
    fn test_flags_strip_every_ndebug_spelling() {
        let flags = vec![
            "-std=c++23".to_string(),
            "-DNDEBUG".to_string(),
            "-D".to_string(),
            "NDEBUG".to_string(),
            "-D".to_string(),
            "NDEBUG=1".to_string(),
            "-DNDEBUG=1".to_string(),
            "-Wall".to_string(),
        ];
        assert_eq!(without_ndebug(&flags), ["-std=c++23", "-Wall"]);
        // NDEBUG is case-sensitive; unrelated defines survive.
        let flags = vec!["-D".to_string(), "DEBUG".to_string()];
        assert_eq!(without_ndebug(&flags), flags);
    }

    #[test]
    fn overlapping_exec_sources_flags_shared_files() {
        use super::{overlapping_exec_sources, BinTarget};
        use std::path::PathBuf;
        let mk = |name: &str, src: &str| BinTarget {
            name: name.to_string(),
            sources: vec![PathBuf::from(src)],
            links: vec![],
            out_name: format!("{name}.exe"),
            extern_links: vec![],
            prefix: None,
        };
        let bins = vec![mk("app", "src/main.cpp")];
        let tests = vec![mk("t1", "tests/a.cpp"), mk("t2", "tests/a.cpp")];
        let overlaps = overlapping_exec_sources(&bins, &tests);
        assert_eq!(overlaps.len(), 1);
        assert_eq!(overlaps[0].0, PathBuf::from("tests/a.cpp"));
        assert_eq!(overlaps[0].1, vec!["t1".to_string(), "t2".to_string()]);
        // Disjoint sources produce no warning.
        let tests2 = vec![mk("t1", "tests/a.cpp"), mk("t2", "tests/b.cpp")];
        assert!(overlapping_exec_sources(&bins, &tests2).is_empty());
    }
}
