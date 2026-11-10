use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::compiler::{self, Cxx};
use crate::config::{Config, TargetKind};

use super::output::rel_display;
use super::{BinTarget, LibTarget, LinkTarget, Plan, SharedTarget};

const SOURCE_EXTS: &[&str] = &["cpp", "cc", "cxx"];

pub fn plan(dir: &Path, cfg: &Config, cxx: Cxx, release: bool) -> Result<Plan, String> {
    let (bins, libs, shared) = if let Some(targets) = cfg.targets() {
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
            let links = target
                .links
                .iter()
                .map(|link| {
                    let lower = link.to_lowercase();
                    if let Some(lib) = libs.iter().find(|lib| lib.name.to_lowercase() == lower) {
                        LinkTarget::Static(lib.name.clone())
                    } else if let Some(sh) =
                        shared.iter().find(|sh| sh.name.to_lowercase() == lower)
                    {
                        LinkTarget::Shared(sh.name.clone())
                    } else {
                        LinkTarget::Static(link.clone())
                    }
                })
                .collect();
            bins.push(BinTarget {
                name: name.to_string(),
                sources,
                links,
                out_name: exe_name(name),
            });
        }
        (bins, libs, shared)
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
        }];
        (bins, Vec::new(), Vec::new())
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
        bins,
        flags,
        link_flags,
        dep_base,
        dep_includes,
        libs,
        shared,
        mode: if release { "release" } else { "debug" },
    })
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
