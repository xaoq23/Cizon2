use std::path::{Path, PathBuf};

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
            std::fs::remove_file(dest)
                .map_err(|e| format!("Failed to delete old file before replacing {}: {e}", dest.display()))?;
            std::fs::rename(tmp, dest).map_err(|e| {
                let _ = std::fs::remove_file(tmp);
                format!("Failed to replace {}: {e}", dest.display())
            })
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
