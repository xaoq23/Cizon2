//! cizon2 — declarative C++ build & run.
//!
//! See AGENTS.md for scope: single crate, recursive src/ sources, g++/clang++; git deps since v0.2.
//! Hand-written argument dispatch to avoid clap (dependency whitelist: serde, toml).

mod compiler;
mod config;
mod deps;
mod pipeline;
mod run;
mod scaffold;
mod test;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const NAME: &str = env!("CARGO_PKG_NAME");

const USAGE: &str = "\
cizon2 — declarative C++ build & run manager

Usage:
  cizon2 new <dir-name>              # create a new project
  cizon2 init                        # initialize in place in the current directory
  cizon2 add <git-url> [--tag <tag>] # add a git dependency (locked into cizon.lock)
  cizon2 update <dep-name>           # align the lock with cizon.toml intent (upgrade/downgrade)
  cizon2 sync                        # restore/verify deps from cizon.lock (no build)
  cizon2 build [--release] [--compiler <spec>]
  cizon2 run [--release] [--compiler <spec>] [<bin-name>] [-- <program args...>]
  cizon2 test [--release] [--compiler <spec>] [<test-name>...] [-- <test args...>]
  cizon2 help [<command>]            # general or per-command help
  cizon2 version                     # print version

Run `cizon2 help <command>` for details. Only reads the [project], [dependencies], \
[compiler] and [[target]] sections of cizon.toml; any other section is a hard error.";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(dispatch(&args));
}

fn dispatch(args: &[String]) -> i32 {
    if args.is_empty() {
        println!("{USAGE}");
        return 0;
    }
    let cmd = args[0].as_str();
    match cmd {
        "help" => {
            match args.get(1).map(|s| s.as_str()) {
                Some(sub) => match command_usage(sub) {
                    Some(text) => println!("{text}"),
                    None => {
                        eprint_top(&format!("Unknown command \"{sub}\".\n\n{USAGE}"));
                        return 2;
                    }
                },
                None => println!("{USAGE}"),
            }
            return 0;
        }
        "--help" | "-h" => {
            println!("{USAGE}");
            return 0;
        }
        "version" | "--version" | "-V" | "-v" => {
            println!("{NAME} v{VERSION}");
            return 0;
        }
        _ => {}
    }
    // Per-command help: `cizon2 <command> --help` / `-h`.
    if matches!(args.get(1).map(|s| s.as_str()), Some("--help" | "-h")) {
        if let Some(text) = command_usage(cmd) {
            println!("{text}");
            return 0;
        }
    }
    match cmd {
        "build" => top_err(build_cmd(&args[1..])),
        "run" => top_err(execute_run(&args[1..])),
        "test" => top_err(test_cmd(&args[1..])),
        "new" => top_err(scaffold::new_cmd(&args[1..])),
        "init" => top_err(scaffold::init_cmd(&args[1..])),
        "add" => top_err(deps::add_cmd(&args[1..])),
        "update" => top_err(deps::update_cmd(&args[1..])),
        "sync" => top_err(sync_cmd(&args[1..])),
        other => {
            eprint_top(&format!("Unknown command \"{other}\".\n\n{USAGE}"));
            2
        }
    }
}

fn sync_cmd(args: &[String]) -> Result<i32, String> {
    if !args.is_empty() {
        return Err("Usage: cizon2 sync (accepts no arguments)".into());
    }
    let (dir, cfg) = load_config()?;
    let n = deps::sync(&dir, &cfg)?;
    if n == 0 {
        eprintln!("no dependencies to sync");
    } else {
        eprintln!("\x1b[32m  Finished\x1b[0m synced {n} dependencies from cizon.lock");
    }
    Ok(0)
}

/// Detailed per-command help for `cizon2 help <command>` and `<command> --help`.
fn command_usage(cmd: &str) -> Option<&'static str> {
    Some(match cmd {
        "new" => "\
cizon2 new — create a new project

Usage: cizon2 new <dir-name>

Creates <dir-name>/ with cizon.toml, src/main.cpp and .gitignore. The project name is
the last path segment. Refuses an existing directory; does not run git init.

Example:
  cizon2 new hello && cd hello && cizon2 run",
        "init" => "\
cizon2 init — initialize a project in the current directory

Usage: cizon2 init

Writes cizon.toml, src/main.cpp and .gitignore into the current directory; the project
name is the directory name. Refuses when cizon.toml or any main.cpp already exists.
Accepts no arguments.",
        "build" => "\
cizon2 build — build all bin and library targets (not tests)

Usage: cizon2 build [--release] [--compiler <spec>]

Options:
  --release          optimize (-O2); the default debug build uses -g
  --compiler <spec>  use g++, clang++ or a full path for this build

Compiles only stale objects; artifacts live under build/<mode>-<compiler>/.
Test targets are ignored — use `cizon2 test` for those.",
        "run" => "\
cizon2 run — build, then run a bin target

Usage: cizon2 run [--release] [--compiler <spec>] [<bin-name>] [-- <program args...>]

The bin name is optional only when the project has exactly one bin. Everything after
-- is passed to the program, and the program's exit code is forwarded.

Examples:
  cizon2 run
  cizon2 run worker -- --port 8080",
        "test" => "\
cizon2 test — build and run test targets

Usage: cizon2 test [--release] [--compiler <spec>] [<test-name>...] [-- <test args...>]

Runs the selected test targets (all when none are named), in declaration order. A test
passes iff its process exits 0; any failure exits 1. Test output goes to stderr.

Examples:
  cizon2 test
  cizon2 test calc_test -- --gtest_filter=Add*",
        "add" => "\
cizon2 add — add a git dependency

Usage: cizon2 add <git-url> [--tag <tag>]

Shallow-clones into deps/<name>, appends to [dependencies] and writes cizon.lock. The
name is the last URL segment minus .git. --tag pins a tag or branch.

Example:
  cizon2 add https://github.com/fmtlib/fmt --tag 11.0.2",
        "update" => "\
cizon2 update — align the lock with cizon.toml

Usage: cizon2 update <dep-name>

Re-resolves a dependency from its git/tag in cizon.toml (upgrade or downgrade). Refuses
a dirty deps/<name>. Change the tag in cizon.toml first, then run update.",
        "sync" => "\
cizon2 sync — restore dependencies from the lock

Usage: cizon2 sync

Restores/verifies every dependency checkout from cizon.lock without building or
changing cizon.toml. Use it after cloning a project to materialize its dependencies.",
        "help" => "\
cizon2 help — show help

Usage: cizon2 help [<command>]

Without a command, prints general usage. With a command, prints its detailed help.
`cizon2 <command> --help` is equivalent.",
        "version" => "\
cizon2 version — print the version

Usage: cizon2 version

Aliases: --version, -V, -v.",
        _ => return None,
    })
}

/// Common flag prefix parsing for build and run: --release / --compiler <spec>
#[derive(Debug)]
struct Flags {
    release: bool,
    cc: Option<String>,
}

/// Parse flags up to `--`; everything after it is passed through.
fn parse_flags<'a>(mut args: &'a [String]) -> Result<(Flags, &'a [String]), String> {
    let mut flags = Flags { release: false, cc: None };
    while let Some(first) = args.first() {
        match first.as_str() {
            "--release" => flags.release = true,
            "--compiler" => match args.get(1) {
                Some(spec) if !spec.is_empty() && !spec.starts_with('-') => {
                    flags.cc = Some(spec.clone());
                    args = &args[2..];
                    continue;
                }
                _ => return Err("--compiler requires an argument, e.g. --compiler g++".into()),
            },
            "--" => {
                args = &args[1..];
                break;
            }
            other => return Err(format!("Unknown flag {other}.\n\n{USAGE}")),
        }
        args = &args[1..];
    }
    Ok((flags, args))
}

/// Parse `run` flags, the optional bin name, and program args after `--`.
fn parse_run_args<'a>(
    mut args: &'a [String],
) -> Result<(Flags, Option<String>, &'a [String]), String> {
    let mut flags = Flags { release: false, cc: None };
    let mut bin = None;
    while let Some(first) = args.first() {
        match first.as_str() {
            "--release" => flags.release = true,
            "--compiler" => match args.get(1) {
                Some(spec) if !spec.is_empty() && !spec.starts_with('-') => {
                    flags.cc = Some(spec.clone());
                    args = &args[2..];
                    continue;
                }
                _ => return Err("--compiler requires an argument, e.g. --compiler g++".into()),
            },
            "--" => return Ok((flags, bin, &args[1..])),
            other if !other.starts_with('-') => {
                if bin.is_some() {
                    return Err(format!(
                        "run accepts at most one bin name, got both {} and {other}. Usage: cizon2 run <bin-name> [-- <args>].",
                        bin.as_deref().unwrap_or("")
                    ));
                }
                bin = Some(other.to_string());
            }
            other => {
                return Err(format!(
                    "Unknown flag {other}.\n\n{USAGE}\n\nTo pass arguments to your program, use `cizon2 run <bin-name> -- <args>`; a PowerShell function wrapper may swallow a bare `--`, so call the exe directly or use Set-Alias."
                ));
            }
        }
        args = &args[1..];
    }
    Ok((flags, bin, args))
}

fn load_config() -> Result<(std::path::PathBuf, config::Config), String> {    let dir = std::env::current_dir().map_err(|e| format!("cannot read current directory: {e}"))?;
    let cfg = config::load(&dir)?;
    Ok((dir, cfg))
}

fn build_cmd(args: &[String]) -> Result<i32, String> {
    let (flags, rest) = parse_flags(args)?;
    if !rest.is_empty() {
        return Err(format!("build does not accept program arguments: {}", rest.join(" ")));
    }
    let (dir, cfg) = load_config()?;
    let (cxx, fallback_note) =
        compiler::pick_with_note(flags.cc.as_deref().or(cfg.explicit_compiler()))?;
    if let Some(note) = fallback_note {
        eprintln!("{note}");
    }
    let plan = pipeline::plan(&dir, &cfg, cxx, flags.release)?;
    // A test-only project has no bin to return; building its libraries is still useful and must
    // not be reported as an error.
    pipeline::build_bins(&dir, &cfg, &plan).map(|_| 0)
}

fn execute_run(args: &[String]) -> Result<i32, String> {
    let (flags, bin, app_args) = parse_run_args(args)?;
    let (dir, cfg) = load_config()?;
    run::run(
        &dir,
        &cfg,
        flags.release,
        flags.cc.as_deref(),
        bin.as_deref(),
        app_args,
    )
}

/// Parse `test` flags, any number of target names, and test args after `--`.
fn parse_test_args<'a>(
    mut args: &'a [String],
) -> Result<(Flags, Vec<String>, &'a [String]), String> {
    let mut flags = Flags { release: false, cc: None };
    let mut names = Vec::new();
    while let Some(first) = args.first() {
        match first.as_str() {
            "--release" => flags.release = true,
            "--compiler" => match args.get(1) {
                Some(spec) if !spec.is_empty() && !spec.starts_with('-') => {
                    flags.cc = Some(spec.clone());
                    args = &args[2..];
                    continue;
                }
                _ => return Err("--compiler requires an argument, e.g. --compiler g++".into()),
            },
            "--" => return Ok((flags, names, &args[1..])),
            other if !other.starts_with('-') => names.push(other.to_string()),
            other => {
                return Err(format!(
                    "Unknown flag {other}.\n\n{USAGE}\n\nTo pass arguments to your test binary, use `cizon2 test <name> -- <args>`."
                ));
            }
        }
        args = &args[1..];
    }
    Ok((flags, names, args))
}

fn test_cmd(args: &[String]) -> Result<i32, String> {
    let (flags, names, app_args) = parse_test_args(args)?;
    let (dir, cfg) = load_config()?;
    test::test(
        &dir,
        &cfg,
        flags.release,
        flags.cc.as_deref(),
        &names,
        app_args,
    )
}

fn top_err(result: Result<i32, String>) -> i32 {
    match result {
        Ok(code) => code,
        Err(msg) => {
            eprint_top(&msg);
            1
        }
    }
}

fn eprint_top(msg: &str) {
    let mut err = std::io::stderr();
    use std::io::Write;
    let _ = writeln!(err, "\x1b[31m   cizon2:\x1b[0m {msg}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_flags_defaults_and_passthrough() {
        let input = args(&["--", "a", "b"]);
        let (flags, rest) = parse_flags(&input).unwrap();
        assert!(!flags.release);
        assert!(flags.cc.is_none());
        assert_eq!(rest, &args(&["a", "b"]));
    }

    #[test]
    fn parse_flags_accepts_release_and_compiler() {
        let input = args(&["--release", "--compiler", "clang++"]);
        let (flags, rest) = parse_flags(&input).unwrap();
        assert!(flags.release);
        assert_eq!(flags.cc.as_deref(), Some("clang++"));
        assert!(rest.is_empty());
    }

    #[test]
    fn parse_flags_rejects_missing_or_ambiguous_compiler_value() {
        for input in [
            args(&["--compiler"]),
            args(&["--compiler", ""]),
            args(&["--compiler", "--release"]),
        ] {
            let err = parse_flags(&input).unwrap_err();
            assert!(err.contains("--compiler"), "got: {err}");
        }
    }

    #[test]
    fn parse_flags_rejects_unknown_flag() {
        let err = parse_flags(&args(&["--nope"])).unwrap_err();
        assert!(err.contains("Unknown flag"), "got: {err}");
        assert!(!err.contains("Set-Alias"), "build errors should not show the run hint: {err}");
    }

    #[test]
    fn run_unknown_flag_mentions_powershell_wrapper() {
        let err = parse_run_args(&args(&["--hello"])).unwrap_err();
        assert!(err.contains("Unknown flag"), "got: {err}");
        assert!(err.contains("cizon2 run"), "got: {err}");
        assert!(err.contains("Set-Alias"), "got: {err}");
    }

    #[test]
    fn parse_run_args_selects_bin_and_passthrough() {
        let input = args(&["server", "--", "a", "b"]);
        let (flags, bin, rest) = parse_run_args(&input).unwrap();
        assert!(!flags.release);
        assert_eq!(bin.as_deref(), Some("server"));
        assert_eq!(rest, &args(&["a", "b"]));

        let input = args(&["--release", "server"]);
        let (flags, bin, rest) = parse_run_args(&input).unwrap();
        assert!(flags.release);
        assert_eq!(bin.as_deref(), Some("server"));
        assert!(rest.is_empty());
    }

    #[test]
    fn parse_run_args_rejects_two_bins() {
        let err = parse_run_args(&args(&["one", "two"])).unwrap_err();
        assert!(err.contains("at most one bin name"), "got: {err}");
    }

    #[test]
    fn parse_test_args_accepts_multiple_names_and_passthrough() {
        let input = args(&["--release", "a", "b", "--", "arg1", "arg2"]);
        let (flags, names, rest) = parse_test_args(&input).unwrap();
        assert!(flags.release);
        assert_eq!(names, args(&["a", "b"]));
        assert_eq!(rest, &args(&["arg1", "arg2"]));

        let (flags, names, rest) = parse_test_args(&[]).unwrap();
        assert!(!flags.release);
        assert!(names.is_empty());
        assert!(rest.is_empty());
    }

    #[test]
    fn parse_test_args_rejects_unknown_flag() {
        let err = parse_test_args(&args(&["--hello"])).unwrap_err();
        assert!(err.contains("Unknown flag"), "got: {err}");
        assert!(err.contains("cizon2 test"), "got: {err}");
    }

    #[test]
    fn command_help_and_version_aliases() {
        assert_eq!(dispatch(&args(&["-v"])), 0);
        assert_eq!(dispatch(&args(&["--version"])), 0);
        assert_eq!(dispatch(&args(&["-V"])), 0);
        assert_eq!(dispatch(&args(&["help", "build"])), 0);
        assert_eq!(dispatch(&args(&["build", "--help"])), 0);
        assert_eq!(dispatch(&args(&["test", "-h"])), 0);
        assert_eq!(dispatch(&args(&["help", "nope"])), 2);
        assert_eq!(dispatch(&args(&["sync", "extra"])), 1);
        assert!(command_usage("sync").is_some());
        assert!(command_usage("nope").is_none());
    }
}
