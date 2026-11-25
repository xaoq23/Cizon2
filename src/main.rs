//! cizon — declarative C++ build & run.
//!
//! See AGENTS.md for scope: single crate, recursive src/ sources, g++/clang++; git deps since v0.2.
//! Hand-written argument dispatch to avoid clap (dependency whitelist: serde, toml).

mod atomic;
mod compiler;
mod config;
mod deps;
mod pipeline;
mod run;
mod scaffold;
mod test;

use pipeline::output::{
    style_err, style_out_enabled, BOLD, CYAN, DIM, GREEN, RED, YELLOW,
};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const NAME: &str = env!("CARGO_PKG_NAME");

const USAGE: &str = "\
cizon — declarative C++ build & run manager

Usage:
  cizon [COMMAND] [OPTIONS] [ARGS]...

Options:
  -h, --help      Print help
  -V, -v, --version   Print version information

Commands:
  new       Create a project skeleton
  init      Initialize a project in the current directory
  build     Build bin and library targets
  run       Build and run a bin target
  test      Build and run test targets
  add       Add a git dependency
  update    Align the lock with cizon.toml
  sync      Restore dependencies from the lock
  clean     Remove build artifacts
  help      Print help for a command or cizon.toml
  version   Print version information

Run `cizon help <command>` or `cizon help toml` for details.";

const TOML_HELP: &str = r#"cizon.toml — configuration reference

[project]
  name = "app"                 # optional; defaults to the directory name
  version = "0.1.0"            # optional metadata only
  edition = "c++23"            # optional; default c++23; validated before compilation
  sources = ["src"]            # optional; only without [[target]]; files/dirs recursive
  defines = ["FEATURE"]        # optional; implicit single-bin only, rejected with [[target]]
  compile_commands = false     # optional; write compile_commands.json + missing VS Code config

  edition values: 98/03/11/14/17/20/23/26, optionally c++ or gnu++ prefixed;
  0x/1y/1z/2a/2b/2c aliases are also accepted.

[compiler]
  cpp = "g++"                  # optional; g++, clang++, or a full path; --compiler overrides
  strict = false               # -Wall -Wextra -Wpedantic for project sources
  werror = false               # -Werror for project sources
  flags = ["-Wall"]            # global compile flags for project sources, not dependencies
  link_flags = ["-lm"]         # extra link flags

[dependencies]
  fmt = { git = "https://...", tag = "11.0.2" }
  # key = dependency name and deps/<name> directory; names are case-insensitive, but every
  # file must use the same spelling. git is required, tag is optional. Flat layout: include/
  # headers plus src/*.cpp. Cizon compiles those sources directly, without dependency build/install.

[subproject]                   # local composition, not a workspace
  net = "libs/net"             # directory relative to the declaring cizon.toml
  # One root build/lock/compiler/edition; no independent subproject build or release.
  # The declaring file and every subproject must declare [[target]]. A subproject provides
  # static_lib, shared_lib and test targets only — no bin. Paths must stay inside the root;
  # sources, include and prefix resolve against the subproject's own directory.
  # Root compiler/edition win; a subproject [compiler] or differing edition is ignored with
  # a warning. [dependencies] merge into the root set with case-insensitive names but one key
  # spelling: case-only duplicates and conflicting git/tag declarations are errors; exact duplicates
  # deduplicate. Subproject git sources must be absolute paths or URL/SSH forms, not relative paths.
  # update writes the root cizon.lock; sync restores from it.
  # Nesting is allowed; cycles are errors and a diamond includes the directory once.

[[target]]
  name = "app"                 # optional for a single bin; required for libs, tests, multiple bins
  kind = "bin"                 # bin | static_lib | shared_lib | test
  sources = ["src"]            # bin defaults to scanning src/; libraries and tests must provide it
  include = "libs/net/include" # optional public include root; libraries propagate it through links
  defines = ["FEATURE", "LEVEL=3"] # NAME or NAME=VALUE; library definitions propagate through links
  links = ["storage"]          # project libraries; static_lib may publicly depend on static_lib
  prefix = "deps/.install/pkg" # optional installed package root; target-local -I/-L, not propagated
  extern_links = ["gtest_main", "gtest"] # test only; mapped to -l in declaration order

Rules:
  - The first [[target]] disables the implicit src/ bin; [project].sources is then rejected.
  - [project].defines is valid only in implicit single-bin mode. It uses the same NAME/NAME=VALUE
    syntax, follows [compiler].flags, and is overridden by temporary CLI -D/--define values.
  - At least one target is required. A library-only project is valid: build produces libraries,
    run reports no bin, and test reports no test targets.
  - include is added to the target's own compile; library includes propagate transitively through
    links. The project-root include/ remains automatic for every target.
  - defines accepts simple NAME or NAME=VALUE object macros without a -D prefix. Library definitions
    propagate transitively through links; bin/test definitions apply only to themselves. They follow
    [compiler].flags, so target definitions override global ones without redefinition warnings.
    Git dependency sources do not inherit them. Identical definitions deduplicate; conflicting values
    in one link closure are plan errors. Test sources strip NDEBUG after propagation.
  - Duplicate public-header paths across root include/, dependency include roots, and target/library
    include roots are plan errors; Windows compares case-insensitively. Dependency-vs-dependency
    collisions remain allowed. Include directory links must stay inside the project, are scanned,
    and use cycle guards to avoid repeated scanning.
  - A subproject's targets are merged into the plan by name; a subproject may not declare a
    bin, and target names must be globally unique and case-insensitive.
  - Static dependencies are ordered dependent-first (consumers before dependencies); cycles are errors.
  - shared_lib may link only static_lib; bin/test may link both; shared -> shared is not supported.
  - extern_links is accepted only on test targets; framework discovery/download is not handled.
  - Test targets are built and run only by `cizon test`; test sources never define NDEBUG.
  - Unknown sections and unknown keys inside known sections are hard errors.
"#;

fn paint_help(text: &str, code: &str, color: bool) -> String {
    style_out_enabled(text, code, color)
}

fn style_help_name(text: &str, color: bool) -> String {
    if let Some(pos) = text.find(|c: char| c.is_whitespace()) {
        format!("{}{}", paint_help(&text[..pos], CYAN, color), &text[pos..])
    } else {
        paint_help(text, CYAN, color)
    }
}

fn style_help_option(text: &str, color: bool) -> String {
    if let Some(pos) = text.find("  ") {
        format!("{}{}", paint_help(&text[..pos], CYAN, color), &text[pos..])
    } else {
        paint_help(text, CYAN, color)
    }
}

fn style_help_usage(text: &str, color: bool) -> String {
    if let Some(rest) = text.strip_prefix("cizon ") {
        if let Some(pos) = rest.find(' ') {
            return format!(
                "{} {}{}",
                paint_help("cizon", CYAN, color),
                paint_help(&rest[..pos], CYAN, color),
                &rest[pos..]
            );
        }
    }
    paint_help(text, YELLOW, color)
}

fn render_help(text: &str, color: bool) -> String {
    let mut out = String::new();
    let mut section = "";
    for (index, line) in text.lines().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        let trimmed = line.trim();
        if index == 0 {
            out.push_str(&paint_help(line, BOLD, color));
            continue;
        }
        if matches!(
            trimmed,
            "Usage:" | "Options:" | "Commands:" | "Examples:" | "Kinds:" | "Rules:"
        ) {
            out.push_str(&paint_help(line, BOLD, color));
            section = match trimmed {
                "Options:" => "options",
                "Commands:" => "commands",
                "Examples:" => "examples",
                "Kinds:" => "kinds",
                "Usage:" => "usage",
                _ => "",
            };
            continue;
        }
        if trimmed.is_empty() {
            out.push_str(line);
            section = "";
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            out.push_str(&paint_help(line, CYAN, color));
            continue;
        }
        let indent_len = line.len() - line.trim_start().len();
        let indent = &line[..indent_len];
        let rest = &line[indent_len..];
        let rendered = match section {
            "commands" | "kinds" => style_help_name(rest, color),
            "options" => style_help_option(rest, color),
            "usage" | "examples" => style_help_usage(rest, color),
            _ if trimmed.starts_with("Run `cizon help") => paint_help(rest, DIM, color),
            _ => rest.to_string(),
        };
        out.push_str(indent);
        out.push_str(&rendered);
    }
    out
}

fn print_help(text: &str) {
    println!(
        "{}",
        render_help(text, pipeline::output::stdout_color())
    );
}

fn main() {
    pipeline::output::init_color();
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(dispatch(&args));
}

fn dispatch(args: &[String]) -> i32 {
    if args.is_empty() {
        print_help(USAGE);
        return 0;
    }
    let cmd = args[0].as_str();
    // `--help` is equivalent to `help <command>` wherever it appears among the
    // command's options. Stop at `--`: after it the tokens belong to the child program.
    let wants_help = args[1..]
        .iter()
        .take_while(|arg| arg.as_str() != "--")
        .any(|arg| matches!(arg.as_str(), "--help" | "-h"));
    if wants_help {
        if let Some(text) = command_usage(cmd) {
            print_help(text);
            return 0;
        }
    }
    match cmd {
        "help" => {
            if args.len() > 2 {
                eprint_top("cizon help accepts at most one command name.");
                return 2;
            }
            match args.get(1).map(|s| s.as_str()) {
                Some(sub) => match command_usage(sub) {
                    Some(text) => print_help(text),
                    None => {
                        eprint_top(&format!("Unknown command \"{sub}\".\n\n{USAGE}"));
                        return 2;
                    }
                },
                None => print_help(USAGE),
            }
            return 0;
        }
        "--help" | "-h" => {
            if args.len() > 1 {
                eprint_top("cizon --help does not take arguments.");
                return 2;
            }
            print_help(USAGE);
            return 0;
        }
        "version" | "--version" | "-V" | "-v" => {
            if args.len() > 1 {
                eprint_top("cizon version does not take arguments.");
                return 2;
            }
            println!("{NAME} v{VERSION}");
            return 0;
        }
        _ => {}
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
        "clean" => top_err(clean_cmd(&args[1..])),
        other => {
            eprint_top(&format!("Unknown command \"{other}\".\n\n{USAGE}"));
            2
        }
    }
}

fn sync_cmd(args: &[String]) -> Result<i32, String> {
    let check = match args {
        [] => false,
        [arg] if arg == "--check" => true,
        _ => return Err("Usage:\n  cizon sync [OPTIONS]".into()),
    };
    let (dir, cfg) = load_config()?;
    if check {
        let n = deps::check(&dir, &cfg)?;
        if n == 0 {
            eprintln!("no dependencies to check");
        } else {
            eprintln!(
                "{} dependencies in sync ({n} checked, no changes made)",
                style_err("  Finished", GREEN)
            );
        }
        return Ok(0);
    }
    let n = deps::sync(&dir, &cfg)?;
    if n == 0 {
        eprintln!("no dependencies to sync");
    } else {
        eprintln!(
            "{} synced {n} dependencies from cizon.lock",
            style_err("  Finished", GREEN)
        );
    }
    Ok(0)
}

fn clean_cmd(args: &[String]) -> Result<i32, String> {
    if !args.is_empty() {
        return Err("Usage: cizon clean".into());
    }
    let dir = std::env::current_dir().map_err(|e| format!("cannot read current directory: {e}"))?;
    if !dir.join("cizon.toml").is_file() {
        return Err(
            "not found cizon.toml: this directory is not a cizon project. Run `cizon new <name>` first."
                .into(),
        );
    }
    let removed = pipeline::clean(&dir)?;
    if removed {
        eprintln!(
            "{} cleaned build/",
            style_err("  Finished", GREEN)
        );
    } else {
        eprintln!("no build directory to clean");
    }
    Ok(0)
}

/// Detailed per-command help for `cizon help <command>` and `<command> --help`.
fn command_usage(cmd: &str) -> Option<&'static str> {
    Some(match cmd {
        "new" => r#"cizon new — create a new project

Usage:
  cizon new <PATH> [--kind <KIND>]

Options:
  -h, --help       Print help
  --kind <KIND>    bin | static_lib | shared_lib (default: bin)

Creates <PATH> with cizon.toml and .gitignore plus the chosen template. The project
name is the last path segment. Existing directories are refused; git init is not run.

Examples:
  cizon new hello && cd hello && cizon run
  cizon new calc --kind static_lib && cd calc && cizon test"#,
        "init" => r#"cizon init — initialize a project in the current directory

Usage:
  cizon init [--kind <KIND>]

Options:
  -h, --help       Print help
  --kind <KIND>    bin | static_lib | shared_lib (default: bin)

Writes cizon.toml and .gitignore plus the chosen template. Existing configuration or
main sources are never overwritten."#,
        "build" => r#"cizon build — build all bin and library targets (not tests)

Usage:
  cizon build [OPTIONS]

Options:
  -h, --help             Print help
  --release              Optimize with -O2 -DNDEBUG; default debug uses -g
  --compiler <SPEC>      Select g++, clang++ or a full compiler path
  -D<NAME>[=<VALUE>]     Temporarily define or override a project macro (repeatable)
  -q, --quiet            Suppress progress lines
  --verbose              Print compiler, archiver and linker commands

-D and --define apply only to this invocation: they override TOML definitions,
do not modify cizon.toml or cizon.lock, and do not affect git dependencies.
Compiles only stale objects. Artifacts live under build/<mode>-<compiler>/, and test
targets are ignored — use `cizon test` for those."#,
        "run" => r#"cizon run — build, then run a bin target

Usage:
  cizon run [OPTIONS] [BIN] [-- <ARGS>...]

Options:
  -h, --help             Print help
  --release              Optimize with -O2 -DNDEBUG; default debug uses -g
  --compiler <SPEC>      Select g++, clang++ or a full compiler path
  -D<NAME>[=<VALUE>]     Temporarily define or override a project macro (repeatable)
  -q, --quiet            Suppress progress lines
  --verbose              Print build and selected program commands

-D and --define are temporary overrides for this build and run only.
The bin name is optional only when exactly one bin exists. Arguments after -- are
forwarded to the program, and its exit code is preserved.

Examples:
  cizon run
  cizon run worker -- --port 8080"#,
        "test" => r#"cizon test — build and run test targets

Usage:
  cizon test [OPTIONS] [TEST]... [-- <ARGS>...]

Options:
  -h, --help             Print help
  --release              Optimize with -O2 -DNDEBUG; default debug uses -g
  --compiler <SPEC>      Select g++, clang++ or a full compiler path
  -D<NAME>[=<VALUE>]     Temporarily define or override a project macro (repeatable)
  -q, --quiet            Suppress progress lines
  --verbose              Print build and test commands

-D and --define are temporary overrides for this build and test run only.
Runs selected test targets in declaration order, or all targets when none are named.
A test passes iff its process exits 0; any failure exits 1.

Examples:
  cizon test
  cizon test calc_test -- --gtest_filter=Add*"#,
        "add" => r#"cizon add — add a git dependency

Usage:
  cizon add <GIT_URL> [--tag <TAG>]

Options:
  -h, --help       Print help
  --tag <TAG>      Pin a tag or branch

Shallow-clones into deps/<name>, updates [dependencies], and writes cizon.lock. The
dependency name is the final URL segment minus .git.

Example:
  cizon add https://github.com/fmtlib/fmt --tag 11.0.2"#,
        "update" => r#"cizon update — align the lock with cizon.toml

Usage:
  cizon update <DEPENDENCY>

Options:
  -h, --help       Print help

Re-resolves a case-insensitive dependency name from the root or an included subproject,
then writes the root cizon.lock. A dirty deps/<name> checkout is refused."#,
        "sync" => r#"cizon sync — restore dependencies from the lock

Usage:
  cizon sync [OPTIONS]

Options:
  -h, --help    Print help
  --check       Verify the lock and checkouts without network access or writes

Restores every merged dependency checkout from the root cizon.lock. `--check` exits
non-zero on drift and is intended for CI."#,
        "clean" => r#"cizon clean — remove build artifacts

Usage:
  cizon clean

Options:
  -h, --help    Print help

Removes only this project's build/ directory. It does not delete cizon.toml,
cizon.lock, deps/, compile_commands.json, editor configuration or source files.
For a real build directory, an active cizon build lock is reported as an error.
If build is a symlink or junction, only the link is removed and its target kept."#,
        "toml" => TOML_HELP,
        "help" => r#"cizon help — show help

Usage:
  cizon help [COMMAND|toml]

Options:
  -h, --help    Print help

Without a command, prints general usage. With a command, prints its detailed help.
`cizon help toml` prints a reference for every cizon.toml key. `cizon <command> --help`
is equivalent to `cizon help <command>`."#,
        "version" => r#"cizon version — print the version

Usage:
  cizon version

Options:
  -h, --help    Print help

Aliases: --version, -V, -v."#,
        _ => return None,
    })
}

/// Common flag prefix parsing for build and run: --release / --compiler <spec>
#[derive(Debug)]
struct Flags {
    release: bool,
    cc: Option<String>,
    /// Suppress cizon's own progress lines (errors, warnings and program output still print).
    quiet: bool,
    /// Print each compiler, archiver and linker command line.
    verbose: bool,
    /// Temporary project-wide macro overrides (`-DNAME[=VALUE]`).
    defines: Vec<String>,
}

fn validate_flags(flags: &Flags) -> Result<(), String> {
    if flags.quiet && flags.verbose {
        return Err("--quiet and --verbose cannot be used together.".into());
    }
    Ok(())
}

fn push_cli_define(flags: &mut Flags, define: &str) -> Result<(), String> {
    config::validate_define(define).map_err(|e| format!("invalid -D value: {e}"))?;
    flags.defines.push(define.to_string());
    Ok(())
}

/// Parse flags up to `--`; everything after it is passed through.
fn parse_flags<'a>(mut args: &'a [String]) -> Result<(Flags, &'a [String]), String> {
    let mut flags = Flags {
        release: false,
        cc: None,
        quiet: false,
        verbose: false,
        defines: Vec::new(),
    };
    while let Some(first) = args.first() {
        match first.as_str() {
            "--release" => flags.release = true,
            "--quiet" | "-q" => flags.quiet = true,
            "--verbose" => flags.verbose = true,
            "--compiler" => match args.get(1) {
                Some(spec) if !spec.is_empty() && !spec.starts_with('-') => {
                    flags.cc = Some(spec.clone());
                    args = &args[2..];
                    continue;
                }
                _ => return Err("--compiler requires an argument, e.g. --compiler g++".into()),
            },
            "--define" => match args.get(1) {
                Some(define) if !define.is_empty() && !define.starts_with('-') => {
                    push_cli_define(&mut flags, define)?;
                    args = &args[2..];
                    continue;
                }
                _ => return Err("--define requires NAME or NAME=VALUE".into()),
            },
            "-D" => match args.get(1) {
                Some(define) if !define.is_empty() && !define.starts_with('-') => {
                    push_cli_define(&mut flags, define)?;
                    args = &args[2..];
                    continue;
                }
                _ => return Err("-D requires NAME or NAME=VALUE".into()),
            },
            other if other.starts_with("-D") => {
                push_cli_define(&mut flags, &other[2..])?;
            }
            "--" => {
                args = &args[1..];
                break;
            }
            other => return Err(format!("Unknown flag {other}.\n\n{USAGE}")),
        }
        args = &args[1..];
    }
    validate_flags(&flags)?;
    Ok((flags, args))
}

/// Parse `run` flags, the optional bin name, and program args after `--`.
fn parse_run_args<'a>(
    mut args: &'a [String],
) -> Result<(Flags, Option<String>, &'a [String]), String> {
    let mut flags = Flags {
        release: false,
        cc: None,
        quiet: false,
        verbose: false,
        defines: Vec::new(),
    };
    let mut bin = None;
    while let Some(first) = args.first() {
        match first.as_str() {
            "--release" => flags.release = true,
            "--quiet" | "-q" => flags.quiet = true,
            "--verbose" => flags.verbose = true,
            "--compiler" => match args.get(1) {
                Some(spec) if !spec.is_empty() && !spec.starts_with('-') => {
                    flags.cc = Some(spec.clone());
                    args = &args[2..];
                    continue;
                }
                _ => return Err("--compiler requires an argument, e.g. --compiler g++".into()),
            },
            "--define" => match args.get(1) {
                Some(define) if !define.is_empty() && !define.starts_with('-') => {
                    push_cli_define(&mut flags, define)?;
                    args = &args[2..];
                    continue;
                }
                _ => return Err("--define requires NAME or NAME=VALUE".into()),
            },
            "-D" => match args.get(1) {
                Some(define) if !define.is_empty() && !define.starts_with('-') => {
                    push_cli_define(&mut flags, define)?;
                    args = &args[2..];
                    continue;
                }
                _ => return Err("-D requires NAME or NAME=VALUE".into()),
            },
            other if other.starts_with("-D") => {
                push_cli_define(&mut flags, &other[2..])?;
            }
            "--" => {
                validate_flags(&flags)?;
                return Ok((flags, bin, &args[1..]));
            }
            other if !other.starts_with('-') => {
                if bin.is_some() {
                    return Err(format!(
                        "run accepts at most one bin name, got both {} and {other}. Usage: cizon run <bin-name> [-- <args>].",
                        bin.as_deref().unwrap_or("")
                    ));
                }
                bin = Some(other.to_string());
            }
            other => {
                return Err(format!(
                    "Unknown flag {other}.\n\n{USAGE}\n\nTo pass arguments to your program, use `cizon run <bin-name> -- <args>`; a PowerShell function wrapper may swallow a bare `--`, so call the exe directly or use Set-Alias."
                ));
            }
        }
        args = &args[1..];
    }
    validate_flags(&flags)?;
    Ok((flags, bin, args))
}

fn load_config() -> Result<(std::path::PathBuf, config::Config), String> {    let dir = std::env::current_dir().map_err(|e| format!("cannot read current directory: {e}"))?;
    // Resolved load: the root project plus its [subproject] graph, merged into one
    // config. For a project without subprojects this is exactly `config::load`.
    let cfg = config::load_resolved(&dir)?;
    Ok((dir, cfg))
}

fn build_cmd(args: &[String]) -> Result<i32, String> {
    let (flags, rest) = parse_flags(args)?;
    if !rest.is_empty() {
        return Err(format!("build does not accept program arguments: {}", rest.join(" ")));
    }
    pipeline::output::set_quiet(flags.quiet);
    pipeline::output::set_verbose(flags.verbose);
    let (dir, cfg) = load_config()?;
    let (cxx, fallback_note) =
        compiler::pick_with_note(flags.cc.as_deref().or(cfg.explicit_compiler()))?;
    if let Some(note) = fallback_note {
        if !pipeline::output::quiet() {
            eprintln!("{note}");
        }
    }
    let plan = pipeline::plan_with_defines(&dir, &cfg, cxx, flags.release, &flags.defines)?;
    // A test-only project has no bin to return; building its libraries is still useful and must
    // not be reported as an error.
    pipeline::build_bins(&dir, &cfg, &plan).map(|_| 0)
}

fn execute_run(args: &[String]) -> Result<i32, String> {
    let (flags, bin, app_args) = parse_run_args(args)?;
    pipeline::output::set_quiet(flags.quiet);
    pipeline::output::set_verbose(flags.verbose);
    let (dir, cfg) = load_config()?;
    run::run_with_defines(
        &dir,
        &cfg,
        flags.release,
        flags.cc.as_deref(),
        &flags.defines,
        bin.as_deref(),
        app_args,
    )
}

/// Parse `test` flags, any number of target names, and test args after `--`.
fn parse_test_args<'a>(
    mut args: &'a [String],
) -> Result<(Flags, Vec<String>, &'a [String]), String> {
    let mut flags = Flags {
        release: false,
        cc: None,
        quiet: false,
        verbose: false,
        defines: Vec::new(),
    };
    let mut names = Vec::new();
    while let Some(first) = args.first() {
        match first.as_str() {
            "--release" => flags.release = true,
            "--quiet" | "-q" => flags.quiet = true,
            "--verbose" => flags.verbose = true,
            "--compiler" => match args.get(1) {
                Some(spec) if !spec.is_empty() && !spec.starts_with('-') => {
                    flags.cc = Some(spec.clone());
                    args = &args[2..];
                    continue;
                }
                _ => return Err("--compiler requires an argument, e.g. --compiler g++".into()),
            },
            "--define" => match args.get(1) {
                Some(define) if !define.is_empty() && !define.starts_with('-') => {
                    push_cli_define(&mut flags, define)?;
                    args = &args[2..];
                    continue;
                }
                _ => return Err("--define requires NAME or NAME=VALUE".into()),
            },
            "-D" => match args.get(1) {
                Some(define) if !define.is_empty() && !define.starts_with('-') => {
                    push_cli_define(&mut flags, define)?;
                    args = &args[2..];
                    continue;
                }
                _ => return Err("-D requires NAME or NAME=VALUE".into()),
            },
            other if other.starts_with("-D") => {
                push_cli_define(&mut flags, &other[2..])?;
            }
            "--" => {
                validate_flags(&flags)?;
                return Ok((flags, names, &args[1..]));
            }
            other if !other.starts_with('-') => names.push(other.to_string()),
            other => {
                return Err(format!(
                    "Unknown flag {other}.\n\n{USAGE}\n\nTo pass arguments to your test binary, use `cizon test <name> -- <args>`."
                ));
            }
        }
        args = &args[1..];
    }
    validate_flags(&flags)?;
    Ok((flags, names, args))
}

fn test_cmd(args: &[String]) -> Result<i32, String> {
    let (flags, names, app_args) = parse_test_args(args)?;
    pipeline::output::set_quiet(flags.quiet);
    pipeline::output::set_verbose(flags.verbose);
    let (dir, cfg) = load_config()?;
    test::test_with_defines(
        &dir,
        &cfg,
        flags.release,
        flags.cc.as_deref(),
        &flags.defines,
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
    let _ = writeln!(err, "{} {msg}", style_err("   cizon:", RED));
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
        assert!(!flags.quiet);
        assert!(!flags.verbose);
        assert_eq!(rest, &args(&["a", "b"]));
    }

    #[test]
    fn parse_flags_accepts_release_and_compiler() {
        let input = args(&["--release", "--compiler", "clang++"]);
        let (flags, rest) = parse_flags(&input).unwrap();
        assert!(flags.release);
        assert_eq!(flags.cc.as_deref(), Some("clang++"));
        assert!(!flags.verbose);
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
    fn parse_flags_accepts_verbose_and_rejects_quiet_conflict() {
        let binding = args(&["--verbose"]);
        let (flags, rest) = parse_flags(&binding).unwrap();
        assert!(flags.verbose);
        assert!(!flags.quiet);
        assert!(rest.is_empty());

        let err = parse_flags(&args(&["--quiet", "--verbose"])).unwrap_err();
        assert!(err.contains("--quiet and --verbose"), "got: {err}");
        let err = parse_run_args(&args(&["--quiet", "--verbose"])).unwrap_err();
        assert!(err.contains("--quiet and --verbose"), "got: {err}");
        let err = parse_run_args(&args(&["--quiet", "--verbose", "--", "arg"])).unwrap_err();
        assert!(err.contains("--quiet and --verbose"), "got: {err}");
        let err = parse_test_args(&args(&["--quiet", "--verbose"])).unwrap_err();
        assert!(err.contains("--quiet and --verbose"), "got: {err}");
    }

    #[test]
    fn parse_flags_accepts_temporary_defines() {
        let input = args(&[
            "-DFOO",
            "-D",
            "LEVEL=3",
            "--define",
            "TEXT=hello world",
        ]);
        let (flags, rest) = parse_flags(&input).unwrap();
        assert_eq!(flags.defines, ["FOO", "LEVEL=3", "TEXT=hello world"]);
        assert!(rest.is_empty());

        let input = args(&["-DRUN=1", "server"]);
        let (flags, bin, rest) = parse_run_args(&input).unwrap();
        assert_eq!(flags.defines, ["RUN=1"]);
        assert_eq!(bin.as_deref(), Some("server"));
        assert!(rest.is_empty());

        let input = args(&["--define=TEST=1"]);
        assert!(parse_test_args(&input).is_err(), "unsupported syntax must be rejected");
        for input in [
            args(&["-D"]),
            args(&["-D1BAD"]),
            args(&["--define"]),
            args(&["--define", "-1"]),
        ] {
            let err = parse_flags(&input).unwrap_err();
            assert!(err.contains("-D") || err.contains("--define"), "got: {err}");
        }
    }

    #[test]
    fn run_unknown_flag_mentions_powershell_wrapper() {
        let err = parse_run_args(&args(&["--hello"])).unwrap_err();
        assert!(err.contains("Unknown flag"), "got: {err}");
        assert!(err.contains("cizon run"), "got: {err}");
        assert!(err.contains("Set-Alias"), "got: {err}");
    }

    #[test]
    fn parse_run_args_selects_bin_and_passthrough() {
        let input = args(&["server", "--", "a", "b"]);
        let (flags, bin, rest) = parse_run_args(&input).unwrap();
        assert!(!flags.release);
        assert!(!flags.verbose);
        assert_eq!(bin.as_deref(), Some("server"));
        assert_eq!(rest, &args(&["a", "b"]));

        let input = args(&["--release", "server"]);
        let (flags, bin, rest) = parse_run_args(&input).unwrap();
        assert!(flags.release);
        assert!(!flags.verbose);
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
        assert!(!flags.verbose);
        assert_eq!(names, args(&["a", "b"]));
        assert_eq!(rest, &args(&["arg1", "arg2"]));

        let (flags, names, rest) = parse_test_args(&[]).unwrap();
        assert!(!flags.release);
        assert!(!flags.verbose);
        assert!(names.is_empty());
        assert!(rest.is_empty());
    }

    #[test]
    fn parse_test_args_rejects_unknown_flag() {
        let err = parse_test_args(&args(&["--hello"])).unwrap_err();
        assert!(err.contains("Unknown flag"), "got: {err}");
        assert!(err.contains("cizon test"), "got: {err}");
    }

    #[test]
    fn clean_rejects_arguments() {
        let err = clean_cmd(&args(&["--force"])).unwrap_err();
        assert_eq!(err, "Usage: cizon clean");
    }

    #[test]
    fn help_flag_is_equivalent_after_other_options() {
        // `cizon <command> --help` must mirror `cizon help <command>` regardless of
        // where the help flag appears among the command's options.
        for input in [
            args(&["build", "--help"]),
            args(&["build", "--release", "--help"]),
            args(&["run", "--verbose", "-h"]),
            args(&["test", "--quiet", "--help"]),
        ] {
            assert_eq!(dispatch(&input), 0, "input={input:?}");
        }
    }

    #[test]
    fn help_and_version_reject_extra_arguments() {
        assert_eq!(dispatch(&args(&["help", "build", "extra"])), 2);
        assert_eq!(dispatch(&args(&["version", "foo"])), 2);
        assert_eq!(dispatch(&args(&["--version", "foo"])), 2);
    }

    #[test]
    fn help_rendering_respects_color_mode() {
        assert_eq!(render_help(USAGE, false), USAGE, "redirected help must stay plain text");
        let colored = render_help(USAGE, true);
        assert!(colored.contains("\x1b["), "TTY help should contain ANSI styling");
        assert!(colored.contains("Usage:"), "got: {colored}");
        assert!(colored.contains("Commands:"), "got: {colored}");
        let build = command_usage("build").unwrap();
        assert_eq!(render_help(build, false), build, "command help must stay plain when redirected");
        assert!(render_help(build, true).contains("--verbose"));
        assert_eq!(
            render_help(TOML_HELP, false),
            TOML_HELP.trim_end(),
            "TOML help indentation must be preserved without color"
        );
    }

    #[test]
    fn command_help_and_version_aliases() {
        assert_eq!(dispatch(&args(&["-v"])), 0);
        assert_eq!(dispatch(&args(&["--version"])), 0);
        assert_eq!(dispatch(&args(&["-V"])), 0);
        assert_eq!(dispatch(&args(&["help", "build"])), 0);
        assert_eq!(dispatch(&args(&["help", "toml"])), 0);
        let toml = command_usage("toml").expect("toml reference");
        for needle in ["[project]", "[compiler]", "[dependencies]", "[subproject]", "[[target]]", "edition", "defines", "extern_links"] {
            assert!(toml.contains(needle), "toml help is missing {needle}");
        }
        assert_eq!(dispatch(&args(&["build", "--help"])), 0);
        assert_eq!(dispatch(&args(&["version", "--help"])), 0);
        assert_eq!(dispatch(&args(&["help", "--help"])), 0);
        assert_eq!(dispatch(&args(&["test", "-h"])), 0);
        assert_eq!(dispatch(&args(&["help", "nope"])), 2);
        assert_eq!(dispatch(&args(&["sync", "extra"])), 1);
        assert!(USAGE.contains("Options:"), "top-level help should use a Cargo-style Options section");
        assert!(USAGE.contains("Commands:"), "top-level help should use a Cargo-style Commands section");
        assert!(USAGE.contains("  clean     Remove build artifacts"), "top-level usage is missing clean");
        assert!(command_usage("build").unwrap().contains("--verbose"));
        assert!(command_usage("sync").is_some());
        let clean = command_usage("clean").expect("clean help");
        assert!(clean.contains("Removes only this project's build/ directory"), "got: {clean}");
        assert!(command_usage("build").unwrap().contains("--verbose"));
        assert!(command_usage("build").unwrap().contains("-D<NAME>"));
        assert!(command_usage("run").unwrap().contains("--verbose"));
        assert!(command_usage("run").unwrap().contains("-D<NAME>"));
        assert!(command_usage("test").unwrap().contains("--verbose"));
        assert!(command_usage("test").unwrap().contains("-D<NAME>"));
        assert!(command_usage("nope").is_none());
    }
}
