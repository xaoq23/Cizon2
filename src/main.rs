//! cizon2 —— 零配置 C++ build & run。
//!
//! 边界见 AGENTS.md：单 crate、src/ 递归源文件、g++/clang++；v0.2 起支持 git 依赖。
//! 手写参数分发，避免引入 clap（依赖白名单：serde、toml）。

mod compiler;
mod config;
mod deps;
mod pipeline;
mod run;
mod scaffold;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const NAME: &str = env!("CARGO_PKG_NAME");

const USAGE: &str = "\
cizon2 — 零配置 C++ 构建与运行管理器

用法:
  cizon2 new <目录名>             # 创建新项目
  cizon2 init                     # 在当前目录就地初始化
  cizon2 add <git-url> [--tag <tag>]  # 添加 git 依赖（锁进 cizon.lock）
  cizon2 update <依赖名>          # 按 cizon.toml 意图对齐锁（升/降通用）
  cizon2 build [--release] [--compiler <spec>]
  cizon2 run [--release] [--compiler <spec>] [-- <程序参数...>]
  cizon2 help | version

仅读取 cizon.toml 的 [project]、[dependencies] 与 [compiler]; \
其他配置段一律明确报错。";

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
            eprint_top(&format!("未知命令 \"{other}\"。\n\n{USAGE}"));
            2
        }
    }
}

/// build 与 run 的公共 flag 前缀解析: --release / --compiler <spec>
#[derive(Debug)]
struct Flags {
    release: bool,
    cc: Option<String>,
}

/// 解析至 `--` 为止的 flags，之后剩余的透传。
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
                _ => return Err("--compiler 需要一个参数，如 --compiler g++".into()),
            },
            "--" => {
                args = &args[1..];
                break;
            }
            other => return Err(format!("未知标志 {other}。\n\n{USAGE}")),
        }
        args = &args[1..];
    }
    Ok((flags, args))
}

fn load_config() -> Result<(std::path::PathBuf, config::Config), String> {
    let dir = std::env::current_dir().map_err(|e| format!("无法读取当前目录: {e}"))?;
    let cfg = config::load(&dir)?;
    Ok((dir, cfg))
}

fn build_cmd(args: &[String]) -> Result<i32, String> {
    let (flags, rest) = parse_flags(args)?;
    if !rest.is_empty() {
        return Err(format!("build 不接受程序参数: {}", rest.join(" ")));
    }
    let (dir, cfg) = load_config()?;
    let cxx = compiler::pick(flags.cc.as_deref().or(cfg.explicit_compiler()))?;
    let plan = pipeline::plan(&dir, &cfg, cxx, flags.release)?;
    pipeline::build(&dir, &cfg, &plan).map(|_| 0)
}

fn execute_run(args: &[String]) -> Result<i32, String> {
    let (flags, app_args) = parse_flags(args)?;
    let (dir, cfg) = load_config()?;
    run::run(&dir, &cfg, flags.release, flags.cc.as_deref(), app_args)
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
        assert!(err.contains("未知标志"), "got: {err}");
    }
}
