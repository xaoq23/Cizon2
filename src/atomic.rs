use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Replace `dest` with `tmp`. On Unix this is a single atomic rename; on Windows the old file
/// must be removed first, so a crash can leave the destination missing but never truncated.
pub(crate) fn replace(tmp: &Path, dest: &Path) -> Result<(), String> {
    match std::fs::rename(tmp, dest) {
        Ok(()) => Ok(()),
        Err(first) => {
            if !dest.exists() {
                let _ = std::fs::remove_file(tmp);
                return Err(format!("Failed to replace {}: {first}", dest.display()));
            }
            remove_dest(dest).map_err(|e| {
                let hint = if cfg!(windows) {
                    ". Close the program holding this file (for example an exe started by `cizon run`) and retry"
                } else {
                    ""
                };
                format!("Failed to delete old file before replacing {}: {e}{hint}", dest.display())
            })?;
            std::fs::rename(tmp, dest).map_err(|e| {
                let _ = std::fs::remove_file(tmp);
                format!("Failed to replace {}: {e}", dest.display())
            })
        }
    }
}

/// On Windows a just-built exe can still be held by its previous process. Give the OS a
/// short grace period instead of failing the whole build with a bare sharing violation.
fn remove_dest(dest: &Path) -> Result<(), std::io::Error> {
    const ATTEMPTS: u32 = 10;
    let mut last = None;
    for attempt in 0..ATTEMPTS {
        match std::fs::remove_file(dest) {
            Ok(()) => return Ok(()),
            Err(e) => {
                let busy = cfg!(windows)
                    && (e.kind() == std::io::ErrorKind::PermissionDenied
                        || e.raw_os_error() == Some(32)); // ERROR_SHARING_VIOLATION
                if !busy || attempt + 1 == ATTEMPTS {
                    return Err(e);
                }
                last = Some(e);
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
    Err(last.expect("the loop always returns on the last attempt"))
}

/// Create `path` with `content` only if it does not exist yet. The final step is atomic
/// (hard link on Unix, rename on Windows), so an interrupted write never leaves a truncated
/// target. Returns whether this call created the file.
pub(crate) fn write_new(path: &Path, content: &[u8]) -> Result<bool, String> {
    if path.exists() {
        return Ok(false);
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = PathBuf::from(format!(
        "{}.tmp-new-{}-{}",
        path.display(),
        std::process::id(),
        nanos
    ));
    {
        let mut file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
        {
            Ok(file) => file,
            Err(e) => return Err(format!("Failed to create {}: {e}", tmp.display())),
        };
        if let Err(e) = file.write_all(content) {
            drop(file);
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("Failed to write {}: {e}", tmp.display()));
        }
    }
    // A hard link fails if the target already exists, on both Unix and Windows
    // (same-directory temp file, so the same volume). `rename` on Windows would overwrite.
    let installed = std::fs::hard_link(&tmp, path);
    match installed {
        Ok(()) => {
            let _ = std::fs::remove_file(&tmp);
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let _ = std::fs::remove_file(&tmp);
            Ok(false)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(format!("Failed to create {}: {e}", path.display()))
        }
    }
}

/// Write through a sibling temporary file, then replace the destination.
pub(crate) fn write(path: &Path, content: &[u8]) -> Result<(), String> {
    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
    if let Err(e) = std::fs::write(&tmp, content) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("Failed to write {}: {e}", tmp.display()));
    }
    replace(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_new_creates_once_and_preserves_existing_file() {
        let dir = std::env::temp_dir().join(format!(
            "cizon-atomic-new-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        assert!(write_new(&path, b"generated").unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"generated");
        assert!(!write_new(&path, b"other").unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"generated");
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "no temporary files should remain"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn replace_overwrites_an_existing_file() {
        let dir = std::env::temp_dir().join(format!(
            "cizon-atomic-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("out.txt");
        let tmp = dir.join("out.txt.tmp");
        std::fs::write(&dest, "old").unwrap();
        std::fs::write(&tmp, "new").unwrap();
        replace(&tmp, &dest).unwrap();
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "new");
        assert!(!tmp.exists());
        std::fs::remove_dir_all(dir).ok();
    }
}
