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
