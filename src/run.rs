//! `cizon2 run`：构建后直接执行并转发参数。
//!
//! 附带从 Cizon1 迁移的退出码翻译表 —— 对初学者真正有用的部分：
//! segfault / heap corruption / DLL 缺失这类崩溃当场说人话。

use std::io::Write;

use crate::compiler;
use crate::config::Config;
use crate::pipeline;

/// 执行 `run`；返回子进程退出码（构建/运行失败返回 Some(错误码) 或 None 语义由 main 处理）。
pub fn run(dir: &std::path::Path, cfg: &Config, release: bool, cc: Option<&str>, app_args: &[String]) -> Result<i32, String> {
    let cxx = compiler::pick(cc.or(cfg.explicit_compiler()))?;
    let plan = pipeline::plan(dir, cfg, cxx, release)?;
    let exe = pipeline::build(dir, cfg, &plan)?;

    eprintln!(
        "\x1b[36m     Running\x1b[0m {}",
        pipeline::rel_display(dir, &exe)
    );
    eprintln!("\x1b[38;2;255;241;168m  {}\x1b[0m", "─".repeat(44));
    let mut child = std::process::Command::new(&exe)
        .args(app_args)
        .spawn()
        .map_err(|e| format!("无法启动 {}: {e}", pipeline::rel_display(dir, &exe)))?;
    let status = child
        .wait()
        .map_err(|e| format!("等待子进程失败: {e}"))?;

    if status.success() {
        return Ok(0);
    }
    let code = exit_code(status);
    let hint = describe_exit_code(code);
    let mut err = std::io::stderr();
    if let Some(hint) = hint {
        let _ = writeln!(
            err,
            "\n\x1b[0m\x1b[31m    危险\x1b[0m 进程退出码: {code} ({code:#x})"
        );
        let _ = writeln!(err, "\x1b[2m    {hint}\x1b[0m");
    } else {
        let _ = writeln!(err, "\n\x1b[0m    退出码: {code} ({code:#x})");
    }
    Ok(code)
}

/// 退出码：Windows 直接取 NTSTATUS；Unix 被信号杀死时换算成 128 + signal。
fn exit_code(status: std::process::ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return 128 + sig;
        }
    }
    status.code().unwrap_or(-1)
}

/// 退出码 → 人话。Windows NTSTATUS 异常码 + POSIX signal 约定（移植自 Cizon1）。
#[allow(clippy::manual_map)]
fn describe_exit_code(code: i32) -> Option<&'static str> {
    // Windows NTSTATUS（以无符号形式通常为负）
    if cfg!(windows) {
        match code as u32 {
            0xC0000005 => {
                return Some("ACCESS_VIOLATION: 空指针解引用或 use-after-free");
            }
            0xC0000374 => {
                return Some("HEAP_CORRUPTION: 堆被破坏（double free 或越界写）");
            }
            0xC0000094 => return Some("INTEGER_DIVIDE_BY_ZERO: 除以零"),
            0xC00000FD => return Some("STACK_OVERFLOW: 栈溢出（可能是无限递归）"),
            0xC000001D => return Some("ILLEGAL_INSTRUCTION: 非法 CPU 指令"),
            0xC0000135 => return Some("DLL_NOT_FOUND: 缺少动态库（检查 PATH）"),
            0xC000013A => return Some("CONTROL_C_EXIT: 被 Ctrl+C 终止"),
            0xC0000142 => return Some("DLL_INIT_FAILED: 动态库初始化失败"),
            _ => {}
        }
    }

    // POSIX：退出码 = 128 + signal（仅 Unix；Windows 上 139 是普通退出码）
    #[cfg(unix)]
    if code > 128 && code < 160 {
        let sig = code - 128;
        return match sig {
            4 => Some("SIGILL: 非法指令"),
            6 => Some("SIGABRT: abort() — 断言失败或不可恢复错误"),
            7 => Some("SIGBUS: 总线错误 — 地址对齐问题"),
            8 => Some("SIGFPE: 浮点异常（如除以零）"),
            11 => Some("SIGSEGV: 段错误 — 空指针或非法地址"),
            15 => Some("SIGTERM: 被终止"),
            _ => None,
        };
    }

    // 普通非零退出（如测试失败返回 1）不当作崩溃，不翻译
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
        assert_eq!(run(&dir, &cfg, false, None, &args).unwrap(), 7);
        fs::remove_dir_all(dir).ok();
    }

    #[cfg(windows)]
    #[test]
    fn windows_normal_exit_139_is_not_sigsegv() {
        // Windows 上 139 是普通退出码，不能被当成 POSIX 信号
        assert_eq!(describe_exit_code(139), None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_crash_codes_named() {
        assert!(describe_exit_code(-1073741819).unwrap().contains("ACCESS_VIOLATION"));
    }

    #[cfg(unix)]
    #[test]
    fn posix_signals_named() {
        // 语义检查:139 = 128 + 11 (SIGSEGV)
        assert!(describe_exit_code(139).unwrap().contains("SIGSEGV"));
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
