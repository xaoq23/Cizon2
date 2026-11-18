use std::path::{Path, PathBuf};
use std::process::Command;

/// Compiler detection priority (carried over from Cizon1's conclusion: g++ is most common, clang++ second).
/// cl (MSVC) is last: it is only probed when neither GCC-like compiler is present, so existing
/// MinGW/LLVM setups keep their behavior; a machine with only Visual Studio still auto-detects.
pub const PREFERENCE: &[&str] = &["g++", "clang++", "cl"];

/// Compiler family: drives flag spelling, object/archive extensions and the linker driver.
/// Derived from the executable name only (no extra state on `Cxx`), so test literals keep
/// working and the family is always consistent with the binary that was probed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompilerFamily {
    Gnu,
    Clang,
    Msvc,
}

impl CompilerFamily {
    pub fn is_msvc(self) -> bool {
        self == CompilerFamily::Msvc
    }
}

/// Classify a compiler executable name (bare name or full path) by its file stem:
/// `cl` / `cl.exe` and `clang-cl` speak MSVC-style flags, `clang*` is Clang, the rest is GNU.
pub fn family_of(name: &str) -> CompilerFamily {
    // Split on both separators first: a Windows path (backslashes) on a Linux host is one
    // component to std::path, so its file stem would be the whole path.
    let last = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name);
    let stem = last
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(last)
        .to_ascii_lowercase();
    if stem == "cl" || stem == "clang-cl" {
        CompilerFamily::Msvc
    } else if stem.contains("clang") {
        CompilerFamily::Clang
    } else {
        CompilerFamily::Gnu
    }
}

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

    /// Compiler family, derived from the executable name.
    pub fn family(&self) -> CompilerFamily {
        family_of(&self.name)
    }

    /// Object file extension for this compiler: `obj` for MSVC, `o` for GCC/Clang.
    pub fn obj_ext(&self) -> &'static str {
        if self.family().is_msvc() {
            "obj"
        } else {
            "o"
        }
    }

    /// Dependency-file extension for this compiler: MSVC writes `/sourceDependencies` JSON,
    /// GCC/Clang write `-MMD` make-style depfiles.
    pub fn dep_ext(&self) -> &'static str {
        if self.family().is_msvc() {
            "json"
        } else {
            "d"
        }
    }

    /// Archive file name for a static library target: `<name>.lib` for MSVC, `lib<name>.a`
    /// for GCC/Clang (including MinGW).
    pub fn archive_name(&self, name: &str) -> String {
        if self.family().is_msvc() {
            format!("{name}.lib")
        } else {
            format!("lib{name}.a")
        }
    }

    /// `-I<dir>` / `/I<dir>`.
    pub fn include_flag(&self, dir: &str) -> String {
        if self.family().is_msvc() {
            format!("/I{dir}")
        } else {
            format!("-I{dir}")
        }
    }


    /// `-L<dir>` / `/LIBPATH:<dir>`.
    pub fn lib_path_flag(&self, dir: &str) -> String {
        if self.family().is_msvc() {
            format!("/LIBPATH:{dir}")
        } else {
            format!("-L{dir}")
        }
    }

    /// Optimization/debug flags: `-O2` / `-g` for GCC/Clang, `/O2` / `/Od /Z7` for MSVC
    /// (/Z7 embeds debug info in the object, so no PDB sidecar is needed).
    pub fn opt_flags(&self, release: bool) -> Vec<String> {
        if self.family().is_msvc() {
            if release {
                vec!["/O2".to_string()]
            } else {
                vec!["/Od".to_string(), "/Z7".to_string()]
            }
        } else if release {
            vec!["-O2".to_string()]
        } else {
            vec!["-g".to_string()]
        }
    }

    /// Warning switches for `strict`: `-Wall -Wextra -Wpedantic` / `/W4`.
    pub fn strict_flags(&self) -> Vec<String> {
        if self.family().is_msvc() {
            vec!["/W4".to_string()]
        } else {
            vec!["-Wall".to_string(), "-Wextra".to_string(), "-Wpedantic".to_string()]
        }
    }

    /// Warnings-as-errors: `-Werror` / `/WX`.
    pub fn werror_flag(&self) -> Option<String> {
        Some(if self.family().is_msvc() {
            "/WX".to_string()
        } else {
            "-Werror".to_string()
        })
    }

    /// Language standard flag. MSVC has no `/std:c++23` (and no `gnu++` mode): C++23 and later
    /// map to `/std:c++latest`; `gnu++NN` maps to the matching `/std:c++NN`.
    pub fn edition_flag(&self, edition: Option<&str>) -> String {
        edition_flag(edition, self.family())
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
/// cl.exe does not understand `--version` (it takes no arguments and prints its banner), so it
/// goes through `probe_cl` instead.
fn probe(name: &str) -> Option<Cxx> {
    if family_of(name).is_msvc() && !name.to_ascii_lowercase().contains("clang-cl") {
        return probe_cl(name);
    }
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

/// Probe cl.exe: invoked with no arguments it prints its banner
/// ("Microsoft (R) C/C++ Optimizing Compiler Version 19.xx.xxxxx for x64") and usage text.
/// The exit code is not trusted (it varies by version); the banner line is the signal.
fn probe_cl(name: &str) -> Option<Cxx> {
    let out = Command::new(name).output().ok()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let version = stdout
        .lines()
        .chain(stderr.lines())
        .map(str::trim)
        .find(|line| line.contains("Microsoft (R) C/C++ Optimizing Compiler"))?;
    Some(Cxx {
        name: name.to_string(),
        version: version.to_string(),
    })
}

/// Locate cl.exe through vswhere (Visual Studio Installer) when cl is not on PATH.
/// Only meaningful on Windows; returns the full path to the newest MSVC cl.exe found.
#[cfg(windows)]
fn probe_vswhere() -> Option<Cxx> {
    let vswhere = Path::new(
        r"C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe",
    );
    let out = Command::new(vswhere)
        .args([
            "-latest",
            "-products",
            "*",
            "-requires",
            "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
            "-property",
            "installationPath",
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let install = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if install.is_empty() {
        return None;
    }
    let tools = Path::new(&install).join("VC").join("Tools").join("MSVC");
    let mut versions: Vec<PathBuf> = std::fs::read_dir(&tools)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    versions.sort();
    let cl = versions
        .pop()?
        .join("bin")
        .join("Hostx64")
        .join("x64")
        .join("cl.exe");
    if !cl.is_file() {
        return None;
    }
    probe_cl(&cl.to_string_lossy())
}

/// An explicit choice (`--compiler` / `[compiler].cpp`) takes priority, otherwise auto-detect by preference.
#[cfg(test)]
pub fn pick(explicit: Option<&str>, target: Option<&str>) -> Result<Cxx, String> {
    pick_with_note(explicit, target).map(|(cxx, _)| cxx)
}

/// Like `pick`, but also reports when auto-detection had to skip a preferred compiler.
///
/// With a cross-compilation `target`, a bare compiler name is prefixed with the triple
/// (`aarch64-linux-gnu-g++`); a full path is used as-is. Plain clang++ is also accepted
/// because plan() passes `--target=<triple>` to it.
pub fn pick_with_note(
    explicit: Option<&str>,
    target: Option<&str>,
) -> Result<(Cxx, Option<String>), String> {
    if let Some(triple) = target {
        if let Some(spec) = explicit {
            let name = resolve_spec(spec);
            if spec.contains('/') || spec.contains('\\') {
                return probe(name)
                    .map(|cxx| (cxx, None))
                    .ok_or_else(|| {
                        format!("The specified compiler {name} is not available. Check the spelling, or drop --compiler to let cizon2 auto-detect.")
                    });
            }
            let prefixed = format!("{triple}-{name}");
            if let Some(cxx) = probe(&prefixed) {
                return Ok((cxx, None));
            }
            if name.to_ascii_lowercase().contains("clang") {
                if let Some(cxx) = probe(name) {
                    return Ok((cxx, None));
                }
            }
            return Err(format!(
                "No cross compiler found for target {triple} (tried {prefixed}). Install the {triple} toolchain, or point [compiler] cpp at the cross compiler binary."
            ));
        }
        let mut missing: Vec<String> = Vec::new();
        for base in PREFERENCE {
            let prefixed = format!("{triple}-{base}");
            if let Some(cxx) = probe(&prefixed) {
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
            missing.push(prefixed);
        }
        if let Some(cxx) = probe("clang++") {
            let note = Some(format!(
                "{} not found; using {} with --target={triple} (compiler flavor and ABI may differ)",
                missing.join(", "),
                cxx.name
            ));
            return Ok((cxx, note));
        }
        return Err(format!(
            "No cross compiler found for target {triple} (tried {}). Install the {triple} toolchain, or point [compiler] cpp at the cross compiler binary.",
            missing.join(", ")
        ));
    }
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
    // On Windows, cl.exe is usually not on PATH outside a Developer Command Prompt; vswhere
    // can still locate the newest Visual Studio installation.
    #[cfg(windows)]
    if let Some(cxx) = probe_vswhere() {
        let note = Some(format!(
            "{} not found; using {} (compiler flavor and ABI may differ)",
            missing.join(", "),
            cxx.name
        ));
        return Ok((cxx, note));
    }
    Err("No usable C++ compiler found (tried g++, clang++, cl).\nInstall MinGW-w64, LLVM, or Visual Studio (MSVC) and add it to PATH, \
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

/// PATH for child processes (`run` / `test`): the compiler's own bin directory comes first so its
/// runtime DLLs (libstdc++ / libgcc / libwinpthread) win over older copies elsewhere on PATH
/// (notably Git for Windows' bundled MinGW). None when the compiler path cannot be resolved.
pub fn child_path(name: &str) -> Option<std::ffi::OsString> {
    let exe = executable_path(name)?;
    let bin = exe.parent()?.to_path_buf();
    let mut paths: Vec<PathBuf> = vec![bin];
    if let Some(current) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&current));
    }
    std::env::join_paths(paths).ok()
}

/// The tool prefix of a cross compiler binary: "aarch64-linux-gnu-g++" ->
/// "aarch64-linux-gnu-", "/usr/bin/aarch64-linux-gnu-clang++" ->
/// "/usr/bin/aarch64-linux-gnu-"; a host compiler yields "".
fn tool_prefix(name: &str) -> &str {
    for suffix in ["clang++", "g++", "clang", "gcc"] {
        if let Some(prefix) = name.strip_suffix(suffix) {
            return prefix;
        }
    }
    ""
}

/// Static library archiver: MSVC uses lib.exe; clang prefers llvm-ar, otherwise ar.
/// Look first in the compiler's own directory (when --compiler points to a full path, LLVM/MinGW may not be in PATH), then in PATH.
pub fn archiver(cxx: &Cxx) -> Result<String, String> {
    if cxx.family().is_msvc() {
        let names: &[&str] = &["lib.exe", "lib"];
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(parent) = Path::new(&cxx.name).parent() {
            if !parent.as_os_str().is_empty() {
                for name in names {
                    candidates.push(parent.join(name));
                }
            }
        }
        for name in names {
            candidates.push(PathBuf::from(name));
        }
        for candidate in candidates {
            // lib.exe has no --version; probe it by running it with no arguments (it prints its
            // banner and exits 0) and checking the output mentions the library manager.
            let ok = Command::new(&candidate)
                .output()
                .map(|o| {
                    let text = format!(
                        "{}\n{}",
                        String::from_utf8_lossy(&o.stdout),
                        String::from_utf8_lossy(&o.stderr)
                    );
                    text.contains("Library Manager")
                })
                .unwrap_or(false);
            if ok {
                return Ok(candidate.to_string_lossy().into_owned());
            }
        }
        return Err("No static library archiver found (lib.exe). Install Visual Studio (MSVC) and add it to PATH, or hand the static library over to CMake.".into());
    }
    let clang = cxx.name.to_ascii_lowercase().contains("clang");
    // Cross compilers come with a prefixed archiver (aarch64-linux-gnu-ar, llvm-ar for
    // clang); a host compiler has no prefix, so the list collapses to the old one.
    let prefix = tool_prefix(&cxx.name);
    // The unprefixed names stay in the list as a fallback: a compiler with an odd name
    // (tool_prefix strips any trailing "gcc"/"g++") must not lose plain `ar` entirely.
    let mut names: Vec<String> = if clang {
        vec![format!("{prefix}llvm-ar"), format!("{prefix}ar")]
    } else {
        vec![format!("{prefix}ar"), format!("{prefix}llvm-ar")]
    };
    for fallback in ["llvm-ar", "ar"] {
        if !names.iter().any(|n| n == fallback) {
            names.push(fallback.to_string());
        }
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(parent) = Path::new(&cxx.name).parent() {
        if !parent.as_os_str().is_empty() {
            for name in &names {
                candidates.push(parent.join(format!("{name}.exe")));
                candidates.push(parent.join(name));
            }
        }
    }
    for name in &names {
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

/// Generate the language-standard flag; when unset, follow Cizon1's template default of c++23.
/// Accepts `c++17` / `gnu++17` passed through as-is, and also a bare version number `17`.
/// MSVC has no `/std:c++23` and no `gnu++` mode: C++23 and later map to `/std:c++latest`,
/// and `gnu++NN` maps to the matching `/std:c++NN`.
pub fn edition_flag(edition: Option<&str>, family: CompilerFamily) -> String {
    if family.is_msvc() {
        let std = match edition {
            None => "c++latest",
            Some(e) => {
                let bare = e.strip_prefix("c++").or_else(|| e.strip_prefix("gnu++")).unwrap_or(e);
                match bare {
                    "17" => "c++17",
                    "20" => "c++20",
                    _ => "c++latest",
                }
            }
        };
        return format!("/std:{std}");
    }
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
        let (cxx, note) = pick_with_note(None, None).unwrap();
        if cxx.name == PREFERENCE[0] {
            assert!(note.is_none(), "unexpected fallback note: {note:?}");
        } else {
            let note = note.expect("a fallback should be explained");
            assert!(note.contains("not found"), "got: {note}");
        }
    }

    #[test]
    fn edition_flag_defaults_to_cxx23() {
        assert_eq!(edition_flag(None, CompilerFamily::Gnu), "-std=c++23");
        assert_eq!(edition_flag(Some("c++17"), CompilerFamily::Gnu), "-std=c++17");
        assert_eq!(edition_flag(Some("c++20"), CompilerFamily::Gnu), "-std=c++20");
        assert_eq!(edition_flag(Some("17"), CompilerFamily::Gnu), "-std=c++17");
        assert_eq!(edition_flag(Some("gnu++20"), CompilerFamily::Gnu), "-std=gnu++20");
        // MSVC: no /std:c++23 and no gnu++ mode; C++23 and later become /std:c++latest.
        assert_eq!(edition_flag(None, CompilerFamily::Msvc), "/std:c++latest");
        assert_eq!(edition_flag(Some("c++17"), CompilerFamily::Msvc), "/std:c++17");
        assert_eq!(edition_flag(Some("c++20"), CompilerFamily::Msvc), "/std:c++20");
        assert_eq!(edition_flag(Some("c++23"), CompilerFamily::Msvc), "/std:c++latest");
        assert_eq!(edition_flag(Some("gnu++20"), CompilerFamily::Msvc), "/std:c++20");
        assert_eq!(edition_flag(Some("20"), CompilerFamily::Msvc), "/std:c++20");
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
        assert!(pick(Some("cizon2-not-a-compiler-xyz"), None).is_err());
    }

    #[test]
    fn preference_order_is_documented() {
        // Changing the priority must be a deliberate decision, not silently followed by the test
        assert_eq!(PREFERENCE, &["g++", "clang++", "cl"]);
    }

    #[test]
    fn pick_follows_preference_order() {
        let expected = PREFERENCE.iter().find(|c| probe(c).is_some());
        let Some(expected) = expected else {
            return; // skip when no compiler is available
        };
        assert_eq!(pick(None, None).unwrap().name, *expected);
    }

    #[test]
    fn child_path_puts_compiler_bin_first() {
        let Some(exe) = executable_path("g++").or_else(|| executable_path("clang++")) else {
            return; // skip when no compiler is available
        };
        let name = exe.to_string_lossy().into_owned();
        let path = child_path(&name).expect("child PATH must be constructible");
        let first = std::env::split_paths(&path).next().unwrap();
        assert_eq!(first, exe.parent().unwrap());
    }

    #[test]
    fn tool_prefix_strips_compiler_suffix() {
        assert_eq!(tool_prefix("g++"), "");
        assert_eq!(tool_prefix("/usr/bin/clang++"), "/usr/bin/");
        assert_eq!(tool_prefix("aarch64-linux-gnu-g++"), "aarch64-linux-gnu-");
        assert_eq!(tool_prefix("aarch64-linux-gnu-clang++"), "aarch64-linux-gnu-");
        assert_eq!(tool_prefix("x86_64-w64-mingw32-g++"), "x86_64-w64-mingw32-");
    }

    #[test]
    fn pick_with_target_uses_prefixed_compiler() {
        // The real cross compiler installed in this environment, by full path.
        let path = "/usr/bin/aarch64-linux-gnu-g++";
        if !std::path::Path::new(path).is_file() {
            return;
        }
        let cxx = pick(Some(path), Some("aarch64-linux-gnu")).unwrap();
        assert_eq!(cxx.name, path);
    }

    #[test]
    fn pick_with_target_errors_when_no_cross_compiler_exists() {
        let err = pick(Some("g++"), Some("no-such-triple-xyz")).unwrap_err();
        assert!(err.contains("no-such-triple-xyz"), "got: {err}");
        assert!(err.contains("no-such-triple-xyz-g++"), "got: {err}");
    }

    #[test]
    fn family_of_classifies_by_executable_stem() {
        assert_eq!(family_of("g++"), CompilerFamily::Gnu);
        assert_eq!(family_of("/usr/bin/clang++"), CompilerFamily::Clang);
        assert_eq!(family_of("aarch64-linux-gnu-g++"), CompilerFamily::Gnu);
        assert_eq!(family_of("cl"), CompilerFamily::Msvc);
        assert_eq!(family_of("cl.exe"), CompilerFamily::Msvc);
        assert_eq!(family_of(r"C:\VS\VC\Tools\MSVC\14.44\bin\Hostx64\x64\cl.exe"), CompilerFamily::Msvc);
        assert_eq!(family_of("clang-cl"), CompilerFamily::Msvc);
        assert_eq!(family_of("clang-cl.exe"), CompilerFamily::Msvc);
    }

    #[test]
    fn msvc_flag_spelling() {
        let cl = Cxx {
            name: "cl".into(),
            version: "Microsoft (R) C/C++ Optimizing Compiler Version 19.44.35213 for x64".into(),
        };
        assert!(cl.family().is_msvc());
        assert_eq!(cl.obj_ext(), "obj");
        assert_eq!(cl.dep_ext(), "json");
        assert_eq!(cl.archive_name("core"), "core.lib");
        assert_eq!(cl.include_flag("/x/include"), "/I/x/include");

        assert_eq!(cl.lib_path_flag("/x/lib"), "/LIBPATH:/x/lib");
        assert_eq!(cl.opt_flags(true), ["/O2"]);
        assert_eq!(cl.opt_flags(false), ["/Od", "/Z7"]);
        assert_eq!(cl.strict_flags(), ["/W4"]);
        assert_eq!(cl.werror_flag().as_deref(), Some("/WX"));
        assert_eq!(cl.edition_flag(Some("c++20")), "/std:c++20");
        let gxx = Cxx {
            name: "g++".into(),
            version: "g++ 14.2.0".into(),
        };
        assert_eq!(gxx.obj_ext(), "o");
        assert_eq!(gxx.dep_ext(), "d");
        assert_eq!(gxx.archive_name("core"), "libcore.a");
        assert_eq!(gxx.include_flag("/x/include"), "-I/x/include");
    }

    /// A fake `cl` (a shell script printing the MSVC banner) exercises the probe path on any
    /// platform, including this Linux sandbox where no real MSVC exists.
    #[test]
    fn probe_cl_accepts_banner_output() {
        let dir = std::env::temp_dir().join(format!(
            "cizon2-fake-cl-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        let script = dir.join("cl");
        fs::write(
            &script,
            "#!/bin/sh\necho 'Microsoft (R) C/C++ Optimizing Compiler Version 19.44.35213 for x64'\necho 'usage: cl [ option... ] file...'\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let probed = probe_cl(&script.to_string_lossy());
        let cxx = probed.expect("the banner must be recognized as an MSVC compiler");
        assert!(cxx.family().is_msvc());
        assert!(cxx.version.contains("19.44.35213"));
        // A script without the banner is not a compiler.
        let other = dir.join("not-cl");
        fs::write(&other, "#!/bin/sh\necho hello\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&other, fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(probe_cl(&other.to_string_lossy()).is_none());
        fs::remove_dir_all(dir).ok();
    }
}
