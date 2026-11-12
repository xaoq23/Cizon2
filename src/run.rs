//! `cizon2 run`: build, then execute directly and forward arguments.
//!
//! Ships the exit-code translation table migrated from Cizon1 — the part genuinely useful to beginners:
//! crashes like segfault / heap corruption / missing DLL are explained in plain terms on the spot.

use std::io::Write;

use crate::compiler;
use crate::config::Config;
use crate::pipeline;

/// Execute `run`; returns the child process exit code (build/run failure returns Some(error code) or None, with the semantics handled by main).
pub fn run(
    dir: &std::path::Path,
    cfg: &Config,
    release: bool,
    cc: Option<&str>,
    bin: Option<&str>,
    app_args: &[String],
) -> Result<i32, String> {
    let (cxx, fallback_note) =
        compiler::pick_with_note(cc.or(cfg.explicit_compiler()))?;
    if let Some(note) = fallback_note {
        eprintln!("{note}");
    }
    let compiler_name = cxx.name.clone();
    let plan = pipeline::plan(dir, cfg, cxx, release)?;
    let result = pipeline::build_bins(dir, cfg, &plan)?;
    let exe = match bin {
        Some(name) => result
            .execs
            .iter()
            .find(|(bin_name, _)| bin_name.eq_ignore_ascii_case(name))
            .map(|(_, path)| path.clone())
            .ok_or_else(|| {
                let available = result
                    .execs
                    .iter()
                    .map(|(name, _)| name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("unknown bin target {name}. Available bins: {available}.")
            })?,
        None if result.execs.len() == 1 => result.execs[0].1.clone(),
        None if result.execs.is_empty() => {
            return Err(
                "no bin target to run. `cizon2 run` only runs kind = \"bin\" targets; use `cizon2 test` for kind = \"test\"."
                    .into(),
            );
        }
        None => {
            let available = result
                .execs
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "multiple bin targets; choose one: {available}. Usage: cizon2 run <bin-name> [-- <args>]."
            ));
        }
    };

    eprintln!(
        "\x1b[36m     Running\x1b[0m {}",
        pipeline::rel_display(dir, &exe)
    );
    eprintln!("\x1b[38;2;255;241;168m  {}\x1b[0m", "─".repeat(44));
    let mut cmd = std::process::Command::new(&exe);
    cmd.args(app_args);
    // Put the compiler's runtime DLLs first so an older libstdc++ elsewhere on PATH (e.g. Git's
    // bundled MinGW) cannot shadow them and break the child at startup.
    if let Some(path) = compiler::child_path(&compiler_name) {
        cmd.env("PATH", path);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", pipeline::rel_display(dir, &exe)))?;
    let status = child
        .wait()
        .map_err(|e| format!("failed to wait for child process: {e}"))?;

    if status.success() {
        return Ok(0);
    }
    let code = exit_code(status);
    let hint = describe_exit_code(code);
    let mut err = std::io::stderr();
    if let Some(hint) = hint {
        let _ = writeln!(
            err,
            "\n\x1b[0m\x1b[31m    Crash\x1b[0m Process exit code: {code} ({code:#x})"
        );
        let _ = writeln!(err, "\x1b[2m    {hint}\x1b[0m");
    } else {
        let _ = writeln!(err, "\n\x1b[0m    Exit code: {code} ({code:#x})");
    }
    Ok(code)
}

/// Exit code: on Windows take the NTSTATUS directly; on Unix, when killed by a signal, convert to 128 + signal.
pub(crate) fn exit_code(status: std::process::ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return 128 + sig;
        }
    }
    status.code().unwrap_or(-1)
}

/// Exit code → plain language. Windows NTSTATUS exception codes + POSIX signal convention (ported from Cizon1).
#[allow(clippy::manual_map)]
pub(crate) fn describe_exit_code(code: i32) -> Option<&'static str> {
    // Windows NTSTATUS (usually negative in unsigned form)
    if cfg!(windows) {
        match code as u32 {
            0xC0000005 => {
                return Some("ACCESS_VIOLATION: null pointer dereference or use-after-free");
            }
            0xC0000374 => {
                return Some("HEAP_CORRUPTION: heap corruption (double free or out-of-bounds write)");
            }
            0xC0000094 => return Some("INTEGER_DIVIDE_BY_ZERO: division by zero"),
            0xC00000FD => return Some("STACK_OVERFLOW: stack overflow (possibly infinite recursion)"),
            0xC000001D => return Some("ILLEGAL_INSTRUCTION: illegal CPU instruction"),
            0xC0000135 => {
                return Some(
                    "DLL_NOT_FOUND: missing runtime DLL; add the compiler's bin directory to PATH or link with -static",
                );
            }
            0xC0000139 => {
                return Some(
                    "ENTRYPOINT_NOT_FOUND: a runtime DLL was found but lacks a required symbol; an older libstdc++/libgcc earlier in PATH (e.g. Git's bundled MinGW) is shadowing the compiler's runtime. Put the compiler's bin directory first in PATH, or link with -static",
                );
            }
            0xC000013A => return Some("CONTROL_C_EXIT: terminated by Ctrl+C"),
            0xC0000142 => return Some("DLL_INIT_FAILED: dynamic library initialization failed"),
            _ => {}
        }
    }

    // POSIX: exit code = 128 + signal (Unix only; on Windows 139 is an ordinary exit code)
    #[cfg(unix)]
    if code > 128 && code < 160 {
        let sig = code - 128;
        return match sig {
            4 => Some("SIGILL: illegal instruction"),
            6 => Some("SIGABRT: abort() — assertion failure or unrecoverable error"),
            7 => Some("SIGBUS: bus error — address alignment problem"),
            8 => Some("SIGFPE: floating-point exception (e.g. division by zero)"),
            11 => Some("SIGSEGV: segmentation fault — null pointer or illegal address"),
            15 => Some("SIGTERM: terminated"),
            _ => None,
        };
    }

    // An ordinary non-zero exit (e.g. a test failure returning 1) is not treated as a crash and is not translated
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn run_forwards_args_and_exit_code() {
        if crate::compiler::pick(None).is_err() {
            return;
        }
        let dir = std::env::temp_dir().join(format!(
            "cizon2-run-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(
            dir.join("src/main.cpp"),
            "#include <cstring>\nint main(int argc, char** argv){ return (argc == 3 && std::strcmp(argv[1], \"a\") == 0 && std::strcmp(argv[2], \"b\") == 0) ? 7 : 1; }\n",
        )
        .unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();
        let cfg = crate::config::load(&dir).unwrap();
        let args = vec!["a".to_string(), "b".to_string()];
        assert_eq!(run(&dir, &cfg, false, None, None, &args).unwrap(), 7);
        fs::remove_dir_all(dir).ok();
    }

    #[cfg(windows)]
    #[test]
    fn windows_normal_exit_139_is_not_sigsegv() {
        // On Windows 139 is an ordinary exit code and must not be treated as a POSIX signal
        assert_eq!(describe_exit_code(139), None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_crash_codes_named() {
        assert!(describe_exit_code(-1073741819).unwrap().contains("ACCESS_VIOLATION"));
        let dll_not_found = describe_exit_code(0xC0000135u32 as i32).unwrap();
        assert!(dll_not_found.contains("PATH"), "got: {dll_not_found}");
        assert!(dll_not_found.contains("-static"), "got: {dll_not_found}");
        let entrypoint = describe_exit_code(0xC0000139u32 as i32).unwrap();
        assert!(entrypoint.contains("ENTRYPOINT_NOT_FOUND"), "got: {entrypoint}");
        assert!(entrypoint.contains("PATH"), "got: {entrypoint}");
    }

    #[cfg(unix)]
    #[test]
    fn posix_signals_named() {
        // Semantic check: 139 = 128 + 11 (SIGSEGV)
        assert!(describe_exit_code(139).unwrap().contains("SIGSEGV"));
    }

    #[test]
    fn run_rejects_a_test_only_project() {
        if crate::compiler::pick(None).is_err() {
            return;
        }
        let dir = std::env::temp_dir().join(format!(
            "cizon2-run-test-only-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        fs::create_dir_all(dir.join("tests")).unwrap();
        fs::write(dir.join("tests/main.cpp"), "int main(){return 0;}").unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n\
             [[target]]\nname = \"t_test\"\nkind = \"test\"\nsources = [\"tests\"]\n",
        )
        .unwrap();
        let cfg = crate::config::load(&dir).unwrap();
        let err = run(&dir, &cfg, false, None, None, &[]).unwrap_err();
        assert!(err.contains("no bin target"), "got: {err}");
        assert!(err.contains("cizon2 test"), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn normal_exit_is_not_a_crash() {
        assert_eq!(describe_exit_code(0), None);
        assert_eq!(describe_exit_code(1), None);
        assert_eq!(describe_exit_code(77), None);
    }

    #[test]
    fn exit_code_passes_through_normal_codes() {
        #[cfg(windows)]
        let status = std::process::Command::new("cmd")
            .args(["/C", "exit 7"])
            .status()
            .unwrap();
        #[cfg(unix)]
        let status = std::process::Command::new("sh")
            .args(["-c", "exit 7"])
            .status()
            .unwrap();
        assert_eq!(exit_code(status), 7);
    }
}
