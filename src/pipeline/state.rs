use std::path::{Path, PathBuf};

use super::{Plan, SharedTarget};

/// Remove a directory (missing counts as success); failure must be reported, otherwise stale
/// objects survive silently.
pub(super) fn reset_dir(path: &Path) -> Result<(), String> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Cannot clear {}: {e}", path.display())),
    }
}

pub(super) fn state_current(path: &Path, wanted: &str) -> Result<bool, String> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(t == wanted),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("Failed to read {}: {e}", path.display())),
    }
}

/// Bin objects mirror project-relative paths; obj/ belongs to the bin only.
pub(super) fn clear_project_objects(obj_dir: &Path) -> Result<(), String> {
    reset_dir(obj_dir)
}

/// Dependency objects live separately under deps-obj/<dep>/.
pub(super) fn clear_dep_objects(deps_obj_dir: &Path) -> Result<(), String> {
    reset_dir(deps_obj_dir)
}

/// The compiler name may be a full path (--compiler C:\...\g++.exe); the directory name keeps only safe characters.
pub(super) fn scope_label(cxx: &str) -> String {
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

/// Compile parameters for project sources. Line-based text is easy to diff by eye.
pub(super) fn project_state_text(plan: &Plan) -> String {
    let mut s = String::from("project:\n");
    for f in &plan.flags {
        s.push_str(f);
        s.push('\n');
    }
    s
}

/// Compile parameters for dependency sources (base flags + each dependency's own include set).
pub(super) fn deps_state_text(plan: &Plan) -> String {
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

/// Source list of static library targets; when it changes, clear static objects and archives.
pub(super) fn libs_state_text(plan: &Plan) -> String {
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

/// Shared target names, sources and private static links; changes here invalidate only `.shared`.
pub(super) fn shared_state_text(plan: &Plan) -> String {
    let mut s = String::from("shared:\n");
    for shared in &plan.shared {
        s.push_str(&format!("shared {}:\n", shared.name));
        for src in &shared.sources {
            s.push_str(&src.display().to_string());
            s.push('\n');
        }
        for link in &shared.links {
            s.push_str(&format!("link {link}\n"));
        }
    }
    s
}

/// Link state: exe name, objects, archives, import libraries, link flags. Any change requires a
/// relink; import libraries carry their semantic fingerprint rather than mtime.
pub(super) fn link_state_text(
    plan: &Plan,
    objs: &[PathBuf],
    archives: &[PathBuf],
    imports: &[(PathBuf, String)],
) -> String {
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
    s.push_str("import-libs:\n");
    for (path, fingerprint) in imports {
        s.push_str(&format!("{} {fingerprint}\n", path.display()));
    }
    s.push_str("link-flags:\n");
    for f in &plan.link_flags {
        s.push_str(f);
        s.push('\n');
    }
    s
}

/// State for one shared target: its objects, static archives built into it, and link flags.
pub(super) fn shared_link_state_text(
    plan: &Plan,
    shared: &SharedTarget,
    objs: &[PathBuf],
    archives: &[PathBuf],
) -> String {
    let mut s = format!("shared: {}\n", shared.name);
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

pub(super) fn shared_link_state_path(scope_dir: &Path, name: &str) -> PathBuf {
    scope_dir.join(".shared").join(name).join(".link-state")
}

/// Invalidate all shared artifacts when the library target list changes; stale DLLs at the scope
/// root must disappear together with their objects and import libraries.
pub(super) fn clear_shared_artifacts(scope_dir: &Path) -> Result<(), String> {
    reset_dir(&scope_dir.join(".shared"))?;
    let entries = match std::fs::read_dir(scope_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("Cannot read {}: {e}", scope_dir.display())),
    };
    for entry in entries {
        let entry = entry.map_err(|e| format!("Cannot read {}: {e}", scope_dir.display()))?;
        let path = entry.path();
        if path.is_file()
            && matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("dll" | "so" | "dylib")
            )
        {
            std::fs::remove_file(&path)
                .map_err(|e| format!("Cannot remove stale {}: {e}", path.display()))?;
        }
    }
    Ok(())
}

/// After a bin rename, clean up the old exe and Windows link-side artifacts.
/// Failure only warns: the old exe may still be running and should not block building the new artifact.
/// Collect the old exe and Windows link-side artifacts to clean up after a bin rename; the actual
/// removal is done only after the link succeeds.
pub(super) fn stale_exe_paths(scope_dir: &Path, plan: &Plan) -> Vec<PathBuf> {
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
    if let Some(ref old) = old {
        // Windows file names are case-insensitive: app.exe and APP.exe are the same file, do not delete as a stale artifact.
        let same_name = if cfg!(windows) {
            old.to_lowercase() == plan.out_name.to_lowercase()
        } else {
            old.as_str() == plan.out_name.as_str()
        };
        if !same_name {
            let path = scope_dir.join(old);
            stale.push(path.clone());
            stale.push(path.with_extension("pdb"));
            stale.push(path.with_extension("ilk"));
        }
    }

    // Old versions of .link-state have no exe line: on Windows collect other top-level artifacts by extension.
    #[cfg(windows)]
    if old.is_none() {
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

pub(super) fn remove_artifacts(paths: &[PathBuf]) {
    for path in paths {
        remove_artifact(path);
    }
}

pub(super) fn remove_artifact(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!("Warning: failed to clean up stale artifact {}: {e}", path.display()),
    }
}

/// Whether the exe is not older than all objects and static library archives.
pub(super) fn exe_is_fresh(exe: &Path, objs: &[PathBuf], archives: &[PathBuf]) -> bool {
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
