use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::compile::{replace_file, temp_sibling};
use super::state::reset_dir;

pub(super) fn archive_path(scope_dir: &Path, name: &str) -> PathBuf {
    scope_dir
        .join("lib")
        .join(name)
        .join(format!("lib{name}.a"))
}

/// When project flags or the library source list change, invalidate library objects and archives together.
pub(super) fn clear_lib_artifacts(scope_dir: &Path) -> Result<(), String> {
    reset_dir(&scope_dir.join("lib"))
}

pub(super) fn archive_stale(archive: &Path, objs: &[PathBuf]) -> bool {
    let Ok(archive_time) = std::fs::metadata(archive).and_then(|m| m.modified()) else {
        return true;
    };
    objs.iter().any(|obj| {
        std::fs::metadata(obj)
            .and_then(|m| m.modified())
            .map_or(true, |t| t > archive_time)
    })
}

pub(super) fn archive_lib(ar: &str, archive: &Path, objs: &[PathBuf]) -> Result<(), String> {
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

pub(super) fn link_shared(
    cxx: &str,
    objs: &[PathBuf],
    archives: &[PathBuf],
    out: &Path,
    flags: &[String],
    link_flags: &[String],
    import_arg: &str,
    static_runtime: bool,
) -> Result<(), String> {
    let mut cmd = Command::new(cxx);
    cmd.arg("-shared");
    for obj in objs {
        cmd.arg(obj);
    }
    for archive in archives {
        cmd.arg(archive);
    }
    cmd.arg("-o").arg(out);
    for f in flags {
        cmd.arg(f);
    }
    if static_runtime {
        cmd.arg("-static");
    }
    cmd.arg(import_arg);
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

pub(super) fn link(
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

