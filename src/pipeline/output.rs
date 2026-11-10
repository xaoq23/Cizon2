use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::compiler::Cxx;

/// Output path: relative to the project root; falls back to the original path when outside it.
pub(crate) fn rel_display(base: &Path, path: &Path) -> String {
    path.strip_prefix(base).unwrap_or(path).display().to_string()
}

const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const GOLD: &str = "\x1b[38;2;255;241;168m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

pub(super) fn step_line(label: &str, detail: &str) {
    eprintln!("{GREEN}{label:>12}{RESET} {detail}", label = label, detail = detail);
}

/// Compiler warning section: gold top/bottom dividers + source file/compiler header, original
/// diagnostics are not rewritten.
pub(super) fn warning_section_text(dir: &Path, cxx: &Cxx, warnings: &[(PathBuf, String)]) -> String {
    let sep = format!("  {GOLD}{}{RESET}\n", "─".repeat(44));
    let mut out = sep.clone();
    for (src, text) in warnings {
        out.push_str(&format!(
            "{YELLOW}{:>12}{RESET} {} ({} {})\n",
            "Warning",
            rel_display(dir, src),
            cxx.name,
            cxx.short_version()
        ));
        out.push_str(text);
        if !text.ends_with('\n') {
            out.push('\n');
        }
    }
    out.push_str(&sep);
    out
}

pub(super) fn finish_note(recompiled: usize, total: usize) -> &'static str {
    if recompiled == 0 {
        "relinked"
    } else if recompiled == total {
        "fresh build"
    } else {
        "incremental rebuild"
    }
}

pub(super) fn step_fin(started: Instant, recompiled: usize, total: usize) {
    let secs = started.elapsed().as_secs_f32();
    let note = finish_note(recompiled, total);
    step_line("Finished", &format!("{DIM}{secs:.2}{RESET}s ({note})"));
}
