use std::path::{Path, PathBuf};
use std::process::Command;

use crate::compiler::Cxx;

use super::compile::replace_file;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ImportLibFlavor {
    Gnu,
    Msvc,
}

/// Generated import libraries are archives whose header timestamps change on every link.
/// Hash member names and payloads only, so an unchanged export surface keeps the same fingerprint.
pub(super) fn fingerprint(path: &Path) -> Result<String, String> {
    let data = std::fs::read(path).map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
    if data.len() < 8 || &data[..8] != b"!<arch>\n" {
        return Err(format!("{} is not a GNU archive.", path.display()));
    }
    let mut hash = 0xcbf29ce484222325u64;
    let mut offset = 8usize;
    while offset + 60 <= data.len() {
        let header = &data[offset..offset + 60];
        let name = String::from_utf8_lossy(&header[..16]).trim().to_string();
        let size_text = String::from_utf8_lossy(&header[48..58]).trim().to_string();
        let size: usize = size_text
            .parse()
            .map_err(|_| format!("Malformed archive member size in {}.", path.display()))?;
        let start = offset + 60;
        let end = start
            .checked_add(size)
            .filter(|end| *end <= data.len())
            .ok_or_else(|| format!("Truncated archive member in {}.", path.display()))?;
        for byte in name.bytes().chain(data[start..end].iter().copied()) {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        offset = end + (size & 1);
    }
    Ok(format!("{hash:016x}"))
}

/// Keep the old import library untouched when the new one has the same semantic fingerprint.
pub(super) fn adopt(tmp: &Path, final_path: &Path) -> Result<String, String> {
    let new_fp = fingerprint(tmp)?;
    let same = fingerprint(final_path).map_or(false, |old| old == new_fp);
    if same {
        let _ = std::fs::remove_file(tmp);
    } else {
        replace_file(tmp, final_path)?;
    }
    Ok(new_fp)
}

pub(super) fn flavor(cxx: &Cxx) -> Result<ImportLibFlavor, String> {
    if !cfg!(windows) {
        return Err(
            "shared_lib is supported on Windows only (MinGW or clang++ MSVC target).".into(),
        );
    }
    let out = Command::new(&cxx.name)
        .arg("-dumpmachine")
        .output()
        .map_err(|e| format!("Failed to query compiler target for {}: {e}", cxx.name))?;
    if !out.status.success() {
        return Err(format!(
            "Failed to query compiler target for {}: {}",
            cxx.name,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let triple = String::from_utf8_lossy(&out.stdout).to_lowercase();
    if triple.contains("msvc") {
        Ok(ImportLibFlavor::Msvc)
    } else if triple.contains("mingw") || triple.contains("windows") {
        Ok(ImportLibFlavor::Gnu)
    } else {
        Err(format!(
            "Cannot determine a Windows target for {} (got {:?}); shared_lib needs a Windows toolchain.",
            cxx.name,
            triple.trim()
        ))
    }
}

pub(super) fn import_lib_path(
    scope_dir: &Path,
    name: &str,
    flavor: ImportLibFlavor,
) -> PathBuf {
    let dir = scope_dir.join(".shared").join(name);
    match flavor {
        ImportLibFlavor::Gnu => dir.join(format!("lib{name}.dll.a")),
        ImportLibFlavor::Msvc => dir.join(format!("{name}.lib")),
    }
}

pub(super) fn import_lib_arg(path: &Path, flavor: ImportLibFlavor) -> String {
    match flavor {
        ImportLibFlavor::Gnu => format!("-Wl,--out-implib,{}", path.display()),
        ImportLibFlavor::Msvc => format!("-Wl,/implib:{}", path.display()),
    }
}

pub(super) fn temp_import_path(final_path: &Path) -> PathBuf {
    let mut os = final_path.as_os_str().to_os_string();
    os.push(format!(".tmp-{}", std::process::id()));
    PathBuf::from(os)
}

pub(super) fn dll_path(scope_dir: &Path, name: &str) -> PathBuf {
    if cfg!(windows) {
        scope_dir.join(format!("{name}.dll"))
    } else if cfg!(target_os = "macos") {
        scope_dir.join(format!("lib{name}.dylib"))
    } else {
        scope_dir.join(format!("lib{name}.so"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cizon2-implib-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn archive(path: &Path, members: &[(&[u8], &[u8])], timestamp: &[u8]) {
        let mut out = Vec::from(&b"!<arch>\n"[..]);
        for (name, data) in members {
            let mut header = [b' '; 60];
            header[..name.len()].copy_from_slice(name);
            header[16..28].copy_from_slice(timestamp);
            let size = format!("{}", data.len());
            header[48..48 + size.len()].copy_from_slice(size.as_bytes());
            header[58..60].copy_from_slice(b"`\n");
            out.extend_from_slice(&header);
            out.extend_from_slice(data);
            if data.len() % 2 == 1 {
                out.push(b'\n');
            }
        }
        std::fs::write(path, out).unwrap();
    }

    #[test]
    fn fingerprint_ignores_archive_timestamps() {
        let dir = temp("stamp");
        let a = dir.join("a.a");
        let b = dir.join("b.a");
        archive(&a, &[(b"x.o/", b"payload")], b"1700000000  ");
        archive(&b, &[(b"x.o/", b"payload")], b"1800000000  ");
        assert_eq!(fingerprint(&a).unwrap(), fingerprint(&b).unwrap());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn import_lib_paths_and_flags_follow_the_toolchain() {
        let scope = Path::new("scope");
        let gnu = import_lib_path(scope, "core", ImportLibFlavor::Gnu);
        assert!(gnu.ends_with(".shared/core/libcore.dll.a"));
        assert!(import_lib_arg(&gnu, ImportLibFlavor::Gnu).contains("--out-implib"));

        let msvc = import_lib_path(scope, "core", ImportLibFlavor::Msvc);
        assert!(msvc.ends_with(".shared/core/core.lib"));
        assert!(import_lib_arg(&msvc, ImportLibFlavor::Msvc).contains("/implib:"));
    }

    #[test]
    fn fingerprint_changes_with_member_payload() {
        let dir = temp("payload");
        let a = dir.join("a.a");
        let b = dir.join("b.a");
        archive(&a, &[(b"x.o/", b"one")], b"1700000000  ");
        archive(&b, &[(b"x.o/", b"two")], b"1700000000  ");
        assert_ne!(fingerprint(&a).unwrap(), fingerprint(&b).unwrap());
        std::fs::remove_dir_all(dir).ok();
    }
}
