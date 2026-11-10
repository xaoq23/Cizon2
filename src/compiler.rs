use std::path::{Path, PathBuf};
use std::process::Command;

/// Compiler detection priority (carried over from Cizon1's conclusion: g++ is most common, clang++ second).
pub const PREFERENCE: &[&str] = &["g++", "clang++"];

#[derive(Debug, Clone)]
pub struct Cxx {
    /// executable name, e.g. "g++"
    pub name: String,
    /// first line of `--version`, e.g. "g++ (MinGW-W64 ...) 14.2.0"
    pub version: String,
}

impl Cxx {
    /// Short version number: the first pure "digits+dots" token in the version line (g++ → "14.2.0",
    /// clang → "22.1.7"; the commit hash at the end of the clang line is not picked up by mistake).
    pub fn short_version(&self) -> &str {
        self.version
            .split_whitespace()
            .find(|t| !t.is_empty() && t.chars().all(|c| c.is_ascii_digit() || c == '.'))
            .unwrap_or(self.version.as_str())
    }
}

/// Normalize a spec alias: "gcc@14" → "g++", "clang" → "clang++".
/// The @version suffix is ignored outright (same behavior as Cizon1); a full path is kept as-is,
/// because the path itself may contain @ (e.g. /opt/gcc@14/bin/g++).
pub fn resolve_spec(spec: &str) -> &str {
    if spec.contains('/') || spec.contains('\\') {
        return spec;
    }
    let name = match spec.find('@') {
        Some(at) => &spec[..at],
        None => spec,
    };
    match name {
        "gcc" => "g++",
        "clang" => "clang++",
        n => n,
    }
}

/// Probe whether a compiler is usable: `--version` exits 0 and produces version output.
fn probe(name: &str) -> Option<Cxx> {
    let out = Command::new(name)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let version = stdout
        .lines()
        .chain(stderr.lines())
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .to_string();
    if version.is_empty() {
        return None;
    }
    Some(Cxx {
        name: name.to_string(),
        version,
    })
}

/// An explicit choice (`--compiler` / `[compiler].cpp`) takes priority, otherwise auto-detect by preference.
#[cfg(test)]
pub fn pick(explicit: Option<&str>) -> Result<Cxx, String> {
    pick_with_note(explicit).map(|(cxx, _)| cxx)
}

/// Like `pick`, but also reports when auto-detection had to skip a preferred compiler.
pub fn pick_with_note(explicit: Option<&str>) -> Result<(Cxx, Option<String>), String> {
    if let Some(spec) = explicit {
        let name = resolve_spec(spec);
        return probe(name)
            .map(|cxx| (cxx, None))
            .ok_or_else(|| {
                format!("The specified compiler {name} is not available in PATH. Check the spelling, or drop --compiler to let cizon2 auto-detect.")
            });
    }
    let mut missing = Vec::new();
    for name in PREFERENCE {
        if let Some(cxx) = probe(name) {
            let note = if missing.is_empty() {
                None
            } else {
                Some(format!(
                    "{} not found; using {} (compiler flavor and ABI may differ)",
                    missing.join(", "),
                    cxx.name
                ))
            };
            return Ok((cxx, note));
        }
        missing.push(*name);
    }
    Err("No usable C++ compiler found (tried g++, clang++).\nInstall MinGW-w64 or LLVM and add it to PATH, \
         or point [compiler] cpp = \"...\" in cizon.toml at a compiler."
        .into())
}

/// Resolve the compiler executable path: a spec containing a path separator becomes absolute, a bare name is looked up in PATH.
/// On Windows an explicit path may omit `.exe` (`Command` adds it automatically), so we add it here too.
pub fn executable_path(name: &str) -> Option<PathBuf> {
    let path = Path::new(name);
    if path.components().count() > 1 {
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir().ok()?.join(path)
        };
        if path.is_file() {
            return Some(path);
        }
        if cfg!(windows) && path.extension().is_none() {
            let mut with_ext = path.as_os_str().to_os_string();
            with_ext.push(".exe");
            let with_ext = PathBuf::from(with_ext);
            if with_ext.is_file() {
                return Some(with_ext);
            }
        }
        return Some(path);
    }
    let exts: &[&str] = if cfg!(windows) { &[".exe", ""] } else { &[""] };
    for dir in std::env::split_paths(&std::env::var_os("PATH")?) {
        for ext in exts {
            let candidate = dir.join(format!("{name}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Static library archiver: clang prefers llvm-ar, otherwise ar.
/// Look first in the compiler's own directory (when --compiler points to a full path, LLVM/MinGW may not be in PATH), then in PATH.
pub fn archiver(cxx: &Cxx) -> Result<String, String> {
    let clang = cxx.name.to_ascii_lowercase().contains("clang");
    let names: &[&str] = if clang {
        &["llvm-ar", "ar"]
    } else {
        &["ar", "llvm-ar"]
    };
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(parent) = Path::new(&cxx.name).parent() {
        if !parent.as_os_str().is_empty() {
            for name in names {
                candidates.push(parent.join(format!("{name}.exe")));
                candidates.push(parent.join(name));
            }
        }
    }
    for name in names {
        candidates.push(PathBuf::from(name));
    }
    for candidate in candidates {
        let ok = Command::new(&candidate)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            return Ok(candidate.to_string_lossy().into_owned());
        }
    }
    Err("No static library archiver found (ar / llvm-ar). Install binutils or LLVM, or hand the static library over to CMake.".into())
}

/// Generate `-std=...`; when unset, follow Cizon1's template default of c++23.
/// Accepts `c++17` / `gnu++17` passed through as-is, and also a bare version number `17`.
pub fn edition_flag(edition: Option<&str>) -> String {
    match edition {
        None => "-std=c++23".into(),
        Some(e) if e.starts_with("c++") || e.starts_with("gnu++") => format!("-std={e}"),
        Some(e) => format!("-std=c++{e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn resolve_spec_passthrough() {
        assert_eq!(resolve_spec("g++"), "g++");
        assert_eq!(resolve_spec("g++@14"), "g++");
        assert_eq!(resolve_spec("gcc@14"), "g++");
        assert_eq!(resolve_spec("clang@18"), "clang++");
        assert_eq!(resolve_spec("clang++"), "clang++");
        // The @ in a full path is part of the path and must not be truncated as a version suffix
        assert_eq!(resolve_spec(r"C:\gcc@14\bin\g++.exe"), r"C:\gcc@14\bin\g++.exe");
        assert_eq!(resolve_spec("/opt/gcc@14/bin/g++"), "/opt/gcc@14/bin/g++");
    }

    #[test]
    fn executable_path_keeps_explicit_paths() {
        #[cfg(windows)]
        let raw = r"C:\gcc@14\bin\g++.exe";
        #[cfg(not(windows))]
        let raw = "/opt/gcc/bin/clang++";
        let path = Path::new(raw);
        assert_eq!(executable_path(raw).as_deref(), Some(path));
    }

    #[cfg(windows)]
    #[test]
    fn executable_path_appends_exe_for_explicit_path() {
        let dir = std::env::temp_dir().join(format!("cizon2-exe-path-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("g++.exe");
        fs::write(&exe, "").unwrap();        assert_eq!(
            executable_path(&dir.join("g++").to_string_lossy()).as_deref(),
            Some(exe.as_path())
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn pick_with_note_reports_fallback_consistently() {
        let (cxx, note) = pick_with_note(None).unwrap();
        if cxx.name == PREFERENCE[0] {
            assert!(note.is_none(), "unexpected fallback note: {note:?}");
        } else {
            let note = note.expect("a fallback should be explained");
            assert!(note.contains("not found"), "got: {note}");
        }
    }

    #[test]
    fn edition_flag_defaults_to_cxx23() {
        assert_eq!(edition_flag(None), "-std=c++23");
        assert_eq!(edition_flag(Some("c++17")), "-std=c++17");
        assert_eq!(edition_flag(Some("c++20")), "-std=c++20");
        assert_eq!(edition_flag(Some("17")), "-std=c++17");
        assert_eq!(edition_flag(Some("gnu++20")), "-std=gnu++20");
    }

    #[test]
    fn short_version_extracts_dot_number_token() {
        let gxx = Cxx {
            name: "g++".into(),
            version: "g++ (x86_64-posix-seh-rev0, Built by MinGW-Builds project) 14.2.0".into(),
        };
        assert_eq!(gxx.short_version(), "14.2.0");
        let clang = Cxx {
            name: "clang++".into(),
            version:
                "clang version 22.1.7 (https://github.com/llvm/llvm-project a255c1ed36a1d06f79bd2633ba9f8d900153007c)"
                    .into(),
        };
        assert_eq!(clang.short_version(), "22.1.7");
        let odd = Cxx { name: "c++".into(), version: "unknown compiler".into() };
        assert_eq!(odd.short_version(), "unknown compiler");
    }

    #[test]
    fn probe_rejects_missing_binary() {
        assert!(probe("cizon2-not-a-compiler-xyz").is_none());
    }

    #[test]
    fn pick_rejects_bad_explicit_spec() {
        assert!(pick(Some("cizon2-not-a-compiler-xyz")).is_err());
    }

    #[test]
    fn preference_order_is_documented() {
        // Changing the priority must be a deliberate decision, not silently followed by the test
        assert_eq!(PREFERENCE, &["g++", "clang++"]);
    }

    #[test]
    fn pick_follows_preference_order() {
        let expected = PREFERENCE.iter().find(|c| probe(c).is_some());
        let Some(expected) = expected else {
            return; // skip when no compiler is available
        };
        assert_eq!(pick(None).unwrap().name, *expected);
    }
}
