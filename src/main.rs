//! cizon2 — zero-config C++ build & run.
//!
//! See AGENTS.md for scope: single crate, recursive src/ sources, g++/clang++; git deps since v0.2.
//! Hand-written argument dispatch to avoid clap (dependency whitelist: serde, toml).

mod compiler;
mod config;
mod deps;
mod pipeline;
mod run;
mod scaffold;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const NAME: &str = env!("CARGO_PKG_NAME");

const USAGE: &str = "\
cizon2 — zero-config C++ build & run manager

Usage:
  cizon2 new <dir-name>           # create a new project
  cizon2 init                     # initialize in place in the current directory
  cizon2 add <git-url> [--tag <tag>]  # add a git dependency (locked into cizon.lock)
  cizon2 update <dep-name>        # align the lock with cizon.toml intent (upgrade/downgrade)
  cizon2 build [--release] [--compiler <spec>]
  cizon2 run [--release] [--compiler <spec>] [<bin-name>] [-- <program args...>]
  cizon2 help | version

Only reads the [project], [dependencies], [compiler] and [[target]] sections of cizon.toml; \
any other config section is a hard error.";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(dispatch(&args));
}

fn dispatch(args: &[String]) -> i32 {
    if args.is_empty() {
        println!("{USAGE}");
        return 0;
    }
    match args[0].as_str() {
        "build" => top_err(build_cmd(&args[1..])),
        "run" => top_err(execute_run(&args[1..])),
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            0
        }
        "version" | "--version" | "-V" => {
            println!("{NAME} v{VERSION}");
            0
        }
        "new" => top_err(scaffold::new_cmd(&args[1..])),
        "init" => top_err(scaffold::init_cmd(&args[1..])),
        "add" => top_err(deps::add_cmd(&args[1..])),
        "update" => top_err(deps::update_cmd(&args[1..])),
        other => {
            eprint_top(&format!("Unknown command \"{other}\".\n\n{USAGE}"));
            2
        }
    }
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
    let cxx = compiler::pick(flags.cc.as_deref().or(cfg.explicit_compiler()))?;
    let plan = pipeline::plan(&dir, &cfg, cxx, flags.release)?;
    pipeline::build(&dir, &cfg, &plan).map(|_| 0)
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
}
