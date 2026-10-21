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
    let exe = pipeline::build(dir, &plan)?;

    println!("\x1b[36m     Running\x1b[0m {}", exe.display());
    println!("\x1b[38;2;255;241;168m  {}\x1b[0m", "─".repeat(44));
    let mut child = std::process::Command::new(&exe)
        .args(app_args)
        .spawn()
        .map_err(|e| format!("无法启动 {}: {e}", exe.display()))?;
    let status = child
        .wait()
        .map_err(|e| format!("等待子进程失败: {e}"))?;

    if status.success() {
        return Ok(0);
    }
    let code = status.code().unwrap_or(-1);
    let mut err = std::io::stderr();
    let _ = writeln!(err, "\r\n\x1b[0m\x1b[31m    危险\x1b[0m 进程退出码: {code} ({code:#x})");
    if let Some(hint) = describe_exit_code(code) {
        let _ = writeln!(err, "\x1b[2m    {hint}\x1b[0m");
    }
    Ok(code)
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

    // POSIX：退出码 = 128 + signal
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

    match code {
        1 => Some("GENERAL_ERROR"),
        2 => Some("MISUSE: 用法错误"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_crash_codes_named() {
        assert!(describe_exit_code(-1073741819).unwrap().contains("ACCESS_VIOLATION"));
    }

    #[test]
    fn posix_signals_named() {
        // 语义检查:139 = 128 + 11 (SIGSEGV)
        assert!(describe_exit_code(139).unwrap().contains("SIGSEGV"));
    }

    #[test]
    fn zero_ok_misc_unknown() {
        assert_eq!(describe_exit_code(0), None); // 正常路径不该进这个函数
        assert_eq!(describe_exit_code(77), None);
        assert!(describe_exit_code(1).expect("code 1 has a hint").contains("GENERAL"));
    }
}
