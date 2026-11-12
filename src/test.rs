//! `cizon2 test`: build and run test targets.
//!
//! Framework-agnostic upper layer aligned with CTest: the only pass contract is the process
//! exit code (0 = pass). Test output is replayed untouched on stderr; no framework parsing, no
//! test-case discovery, no dependency download. `build` / `run` ignore test targets entirely.

use std::io::Write;
use std::path::Path;

use crate::compiler;
use crate::config::Config;
use crate::pipeline;
use crate::run;

/// Execute `test [name...] [-- <args>]`; returns the exit code for main.
pub fn test(
    dir: &Path,
    cfg: &Config,
    release: bool,
    cc: Option<&str>,
    names: &[String],
    app_args: &[String],
) -> Result<i32, String> {
    let has_tests = cfg.targets().is_some_and(|targets| {
        targets
            .iter()
            .any(|t| t.kind == crate::config::TargetKind::Test)
    });
    if !has_tests {
        if names.is_empty() {
            eprintln!("no test targets");
            return Ok(0);
        }
        return Err(format!(
            "no kind = \"test\" target found in cizon.toml; cannot select {}. Add one, e.g.\n\
             [[target]]\nname = \"my_test\"\nkind = \"test\"\nsources = [\"tests\"]",
            names.join(", ")
        ));
    }
    // Validate the filter against the planned tests up front: a typo must fail before building.
    let (cxx, fallback_note) = compiler::pick_with_note(cc.or(cfg.explicit_compiler()))?;
    if let Some(note) = fallback_note {
        eprintln!("{note}");
    }
    let compiler_name = cxx.name.clone();
    let plan = pipeline::plan(dir, cfg, cxx, release)?;
    let test_names = plan.test_names();
    for name in names {
        if !test_names.iter().any(|t| t.eq_ignore_ascii_case(name)) {
            return Err(format!(
                "unknown test target {name}. Available tests: {}. Usage: cizon2 test [name...] [-- <args>].",
                test_names.join(", ")
            ));
        }
    }
    let result = pipeline::build_tests(dir, cfg, &plan, names)?;

    let mut failed: Vec<String> = Vec::new();
    let mut passed = 0usize;
    for (name, exe) in &result.execs {
        eprintln!(
            "\x1b[36m     Running\x1b[0m {}",
            pipeline::rel_display(dir, exe)
        );
        eprintln!("\x1b[38;2;255;241;168m  {}\x1b[0m", "─".repeat(44));
        let start = std::time::Instant::now();
        let mut cmd = std::process::Command::new(exe);
        cmd.args(app_args);
        // Same runtime-DLL guard as `run`: the compiler's bin directory leads the child PATH.
        if let Some(path) = compiler::child_path(&compiler_name) {
            cmd.env("PATH", path);
        }
        let output = cmd
            .output()
            .map_err(|e| format!("cannot start {}: {e}", pipeline::rel_display(dir, exe)))?;
        let status = output.status;
        let elapsed = start.elapsed().as_secs_f32();
        // All test-process output goes to stderr so cizon's stdout stays empty; the bytes are
        // replayed untouched (frameworks control their own formatting/colors).
        {
            let mut err = std::io::stderr();
            let _ = err.write_all(&output.stdout);
            let _ = err.write_all(&output.stderr);
        }
        if status.success() {
            passed += 1;
            eprintln!("\x1b[32m   Passed\x1b[0m {name} ({elapsed:.2}s)");
        } else {
            let code = run::exit_code(status);
            failed.push(name.clone());
            if let Some(hint) = run::describe_exit_code(code) {
                eprintln!(
                    "\x1b[0m\x1b[31m   Failed\x1b[0m {name} — exit code {code} ({code:#x}, {elapsed:.2}s)"
                );
                eprintln!("\x1b[2m    {hint}\x1b[0m");
            } else {
                eprintln!(
                    "\x1b[0m\x1b[31m   Failed\x1b[0m {name} — exit code {code} ({code:#x}, {elapsed:.2}s)"
                );
            }
        }
    }

    let total = result.execs.len();
    if failed.is_empty() {
        eprintln!("\x1b[32m  Finished\x1b[0m test result: ok. {passed} passed; {total} total");
        Ok(0)
    } else {
        eprintln!(
            "\x1b[31m  Finished\x1b[0m test result: FAILED. {passed} passed; {} failed; {total} total",
            failed.len()
        );
        eprintln!("\x1b[31m     Failed\x1b[0m {}", failed.join(", "));
        Ok(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cizon2-test-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_project(tag: &str, pass: bool) -> std::path::PathBuf {
        let dir = temp_dir(tag);
        fs::create_dir_all(dir.join("tests")).unwrap();
        fs::create_dir_all(dir.join("src")).unwrap();
        // A library target under test, linked by both the bin and the test
        fs::create_dir_all(dir.join("lib")).unwrap();
        fs::create_dir_all(dir.join("include")).unwrap();
        fs::write(
            dir.join("include/calc.h"),
            "#pragma once\nint add(int a, int b);\n",
        )
        .unwrap();
        fs::write(
            dir.join("lib/calc.cpp"),
            "#include \"calc.h\"\nint add(int a, int b){ return a + b; }\n",
        )
        .unwrap();
        fs::write(
            dir.join("src/main.cpp"),
            "#include \"calc.h\"\nint main(){ return add(1, 2) == 3 ? 0 : 1; }\n",
        )
        .unwrap();
        let ret = if pass { "0" } else { "1" };
        fs::write(
            dir.join("tests/calc_test.cpp"),
            format!(
                "#include \"calc.h\"\n#include <cassert>\nint main(){{ assert(add(2, 3) == 5); return {ret}; }}\n"
            ),
        )
        .unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"calc\"\n\n\
             [[target]]\nname = \"calc\"\nkind = \"static_lib\"\nsources = [\"lib\"]\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [\"calc\"]\n\n\
             [[target]]\nname = \"calc_test\"\nkind = \"test\"\nsources = [\"tests\"]\nlinks = [\"calc\"]\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn test_command_passes_and_fails_by_exit_code() {
        if compiler::pick(None).is_err() {
            return;
        }
        let dir = write_project("pass", true);
        let cfg = crate::config::load(&dir).unwrap();
        assert_eq!(test(&dir, &cfg, false, None, &[], &[]).unwrap(), 0);
        fs::remove_dir_all(dir).ok();

        let dir = write_project("fail", false);
        let cfg = crate::config::load(&dir).unwrap();
        let code = test(&dir, &cfg, false, None, &[], &[]).unwrap();
        assert_eq!(code, 1, "a failing test target must exit non-zero");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn test_command_filters_and_rejects_unknown_names() {
        if compiler::pick(None).is_err() {
            return;
        }
        let dir = write_project("filter", true);
        let cfg = crate::config::load(&dir).unwrap();
        // Exact filter still works
        assert_eq!(
            test(&dir, &cfg, false, None, &["calc_test".to_string()], &[]).unwrap(),
            0
        );
        // Unknown name fails before building
        let err = test(&dir, &cfg, false, None, &["nope".to_string()], &[]).unwrap_err();
        assert!(err.contains("unknown test target"), "got: {err}");
        assert!(err.contains("calc_test"), "available tests listed: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn test_command_forwards_args_to_the_test_process() {
        if compiler::pick(None).is_err() {
            return;
        }
        let dir = temp_dir("args");
        fs::create_dir_all(dir.join("tests")).unwrap();
        fs::write(
            dir.join("tests/main.cpp"),
            "#include <cstring>\nint main(int argc, char** argv){ return (argc == 2 && std::strcmp(argv[1], \"x\") == 0) ? 0 : 1; }\n",
        )
        .unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n\
             [[target]]\nname = \"t_test\"\nkind = \"test\"\nsources = [\"tests\"]\n",
        )
        .unwrap();
        let cfg = crate::config::load(&dir).unwrap();
        assert_eq!(
            test(&dir, &cfg, false, None, &[], &["x".to_string()]).unwrap(),
            0
        );
        assert_eq!(
            test(&dir, &cfg, false, None, &[], &["y".to_string()]).unwrap(),
            1
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn filtering_one_test_keeps_the_other_test_artifact() {
        if compiler::pick(None).is_err() {
            return;
        }
        let dir = temp_dir("filter-state");
        fs::create_dir_all(dir.join("tests")).unwrap();
        fs::write(dir.join("tests/a.cpp"), "int main(){return 0;}").unwrap();
        fs::write(dir.join("tests/b.cpp"), "int main(){return 0;}").unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n\
             [[target]]\nname = \"a_test\"\nkind = \"test\"\nsources = [\"tests/a.cpp\"]\n\n\
             [[target]]\nname = \"b_test\"\nkind = \"test\"\nsources = [\"tests/b.cpp\"]\n",
        )
        .unwrap();
        let cfg = crate::config::load(&dir).unwrap();
        let cxx = compiler::pick(None).unwrap();
        let plan = pipeline::plan(&dir, &cfg, cxx, false).unwrap();
        let all = pipeline::build_tests(&dir, &cfg, &plan, &[]).unwrap();
        let b_exe = all
            .execs
            .iter()
            .find(|(name, _)| name == "b_test")
            .unwrap()
            .1
            .clone();
        assert!(b_exe.exists());

        let one = pipeline::build_tests(&dir, &cfg, &plan, &["a_test".to_string()]).unwrap();
        assert_eq!(one.execs.len(), 1);
        assert_eq!(one.execs[0].0, "a_test");
        assert!(
            b_exe.exists(),
            "filtering a_test must not delete the unselected b_test artifact"
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn test_command_without_test_targets_is_a_noop() {
        let dir = temp_dir("none");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){}").unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();
        let cfg = crate::config::load(&dir).unwrap();
        // No tests is not a failure (CTest/cargo-like); explicitly selecting one is.
        assert_eq!(test(&dir, &cfg, false, None, &[], &[]).unwrap(), 0);
        let err = test(&dir, &cfg, false, None, &["nope".to_string()], &[]).unwrap_err();
        assert!(err.contains("no kind = \"test\" target"), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn build_ignores_test_targets_and_test_only_project_build_fails_loudly() {
        if compiler::pick(None).is_err() {
            return;
        }
        let dir = write_project("ignore", true);
        let cfg = crate::config::load(&dir).unwrap();
        let cxx = compiler::pick(None).unwrap();
        let plan = pipeline::plan(&dir, &cfg, cxx, false).unwrap();
        // build must produce exactly the bin, not the test exe
        let result = pipeline::build_bins(&dir, &cfg, &plan).unwrap();
        assert_eq!(result.execs.len(), 1, "build ignores test targets");
        assert_eq!(result.execs[0].0, "app");
        // the test exe is not produced by build
        let test_exe = result.execs[0].1.with_file_name("calc_test.exe");
        assert!(
            !test_exe.exists(),
            "build must not link test targets (found {})",
            test_exe.display()
        );
        // test-obj tree is created only by the test command
        let scope = result.execs[0].1.parent().unwrap().to_path_buf();
        assert!(!scope.join("test-obj").join("calc_test").exists());
        // `cizon test` then builds and runs it
        assert_eq!(test(&dir, &cfg, false, None, &[], &[]).unwrap(), 0);
        assert!(scope.join("test-obj").join("calc_test").exists());
        fs::remove_dir_all(dir).ok();

        // test-only project: build succeeds with no bin, test works
        let dir = temp_dir("only");
        fs::create_dir_all(dir.join("tests")).unwrap();
        fs::write(dir.join("tests/main.cpp"), "int main(){return 0;}").unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n\
             [[target]]\nname = \"t_test\"\nkind = \"test\"\nsources = [\"tests\"]\n",
        )
        .unwrap();
        let cfg = crate::config::load(&dir).unwrap();
        let cxx = compiler::pick(None).unwrap();
        let plan = pipeline::plan(&dir, &cfg, cxx, false).unwrap();
        let built = pipeline::build_bins(&dir, &cfg, &plan).unwrap();
        assert!(
            built.execs.is_empty(),
            "test-only build must succeed without producing a bin"
        );
        assert_eq!(test(&dir, &cfg, false, None, &[], &[]).unwrap(), 0);
        fs::remove_dir_all(dir).ok();
    }
}
