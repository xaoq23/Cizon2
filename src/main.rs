//! cizon2 v0.1 —— 零配置 C++ build & run。
//!
//! 边界见 AGENTS.md：只支持单 crate、src/ 平铺 .cpp、g++/clang++。
//! 手写参数分发，避免引入 clap（依赖白名单：serde、toml）。

mod compiler;
mod config;
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
  cizon2 build [--release] [--compiler <spec>]
  cizon2 run [--release] [--compiler <spec>] [-- <程序参数...>]
  cizon2 help | version

仅读取 cizon.toml 的 [project] 与 [compiler] 两段; \
依赖管理等超出边界的能力未实装。";

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
        "init" => top_err(scaffold::init_cmd()),
        "add" => {
            eprint_top("`add`（git 依赖管理）尚未实装，规划见 AGENTS.md 路线。");
            2
        }
        other => {
            eprint_top(&format!("未知命令 \"{other}\"。\n\n{USAGE}"));
            2
        }
    }
}

/// build 与 run 的公共 flag 前缀解析: --release / --compiler <spec>
struct Flags {
    release: bool,
    cc: Option<String>,
}

/// 解析至 `--` 为止的 flags，之后剩余的透传。
fn parse_flags(mut args: &[String]) -> (Flags, &[String]) {
    let mut flags = Flags { release: false, cc: None };
    while let Some(first) = args.first() {
        match first.as_str() {
            "--release" => flags.release = true,
            "--compiler" => {
                if let Some(spec) = args.get(1) {
                    flags.cc = Some(spec.clone());
                    args = &args[2..];
                    continue;
                }
                eprint_top("--compiler 需要一个参数，如 --compiler g++");
                std::process::exit(2);
            }
            "--" => {
                args = &args[1..];
                break;
            }
            other => {
                eprint_top(&format!("未知标志 {other}。\n\n{USAGE}"));
                std::process::exit(2);
            }
        }
        args = &args[1..];
    }
    (flags, args)
}

fn load_config() -> (std::path::PathBuf, config::Config) {
    let dir = std::env::current_dir().expect("无法读取当前目录");
    let cfg = match config::load(&dir) {
        Ok(c) => c,
        Err(e) => {
            eprint_top(&e);
            std::process::exit(1);
        }
    };
    (dir, cfg)
}

fn build_cmd(args: &[String]) -> Result<i32, String> {
    let (flags, _) = parse_flags(args);
    let (dir, cfg) = load_config();
    let cxx = compiler::pick(flags.cc.as_deref().or(cfg.explicit_compiler()))?;
    let plan = pipeline::plan(&dir, &cfg, cxx, flags.release)?;
    pipeline::build(&dir, &plan).map(|_| 0)
}

fn execute_run(args: &[String]) -> Result<i32, String> {
    let (flags, app_args) = parse_flags(args);
    let (dir, cfg) = load_config();
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
