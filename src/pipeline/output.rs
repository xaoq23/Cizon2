use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::compiler::Cxx;

/// `--quiet` (build/run/test): suppress cizon's own progress lines (Compiling/Linking/
/// Finished/Running/Passed) while keeping errors, warnings and the executed program's own
/// output. Process-global because the progress sites are spread across the pipeline, run and
/// test drivers; set once from the CLI flags.
static QUIET: AtomicBool = AtomicBool::new(false);

pub(crate) fn set_quiet(value: bool) {
    QUIET.store(value, Ordering::Relaxed);
}

pub(crate) fn quiet() -> bool {
    QUIET.load(Ordering::Relaxed)
}

/// Color follows each stream's terminal state; main calls this once before any output so
/// redirected stdout/stderr never receives ANSI escapes. Tests keep the default (enabled) so
/// formatting assertions stay meaningful.
static STDOUT_COLOR: AtomicBool = AtomicBool::new(true);
static STDERR_COLOR: AtomicBool = AtomicBool::new(true);

pub(crate) fn init_color() {
    STDOUT_COLOR.store(std::io::stdout().is_terminal(), Ordering::Relaxed);
    STDERR_COLOR.store(std::io::stderr().is_terminal(), Ordering::Relaxed);
}

pub(crate) fn stdout_color() -> bool {
    STDOUT_COLOR.load(Ordering::Relaxed)
}

pub(crate) fn stderr_color() -> bool {
    STDERR_COLOR.load(Ordering::Relaxed)
}

/// Output path: relative to the project root; falls back to the original path when outside it.
pub(crate) fn rel_display(base: &Path, path: &Path) -> String {
    path.strip_prefix(base).unwrap_or(path).display().to_string()
}

pub(crate) const GREEN: &str = "\x1b[32m";
pub(crate) const YELLOW: &str = "\x1b[33m";
pub(crate) const CYAN: &str = "\x1b[36m";
pub(crate) const RED: &str = "\x1b[31m";
pub(crate) const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

fn paint(text: &str, code: &str, enabled: bool) -> String {
    if enabled {
        format!("{code}{text}{RESET}")
    } else {
        text.to_string()
    }
}

pub(crate) fn style_out(text: &str, code: &str) -> String {
    paint(text, code, stdout_color())
}

pub(crate) fn style_err(text: &str, code: &str) -> String {
    paint(text, code, stderr_color())
}

pub(super) fn step_line(label: &str, detail: &str) {
    if quiet() {
        return;
    }
    eprintln!("{} {detail}", style_err(&format!("{label:>12}"), GREEN));
}

/// Compiler warning section: a blank line separates it from build status, then the source
/// file/compiler header and the untouched diagnostics. No decorative divider lines.
pub(super) fn warning_section_text(dir: &Path, cxx: &Cxx, warnings: &[(PathBuf, String)]) -> String {
    let mut out = String::from("\n");
    for (src, text) in warnings {
        out.push_str(&format!(
            "{} {} ({} {})\n",
            style_err(&format!("{:>12}", "Warning"), YELLOW),
            rel_display(dir, src),
            cxx.name,
            cxx.short_version()
        ));
        out.push_str(text);
        if !text.ends_with('\n') {
            out.push('\n');
        }
    }
    out.push('\n');
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

/// The unit stays inside the styled span: a reset between the number and `s` would make
/// escape-stripped logs read `0.12ms` (the final `m` of the reset code).
pub(super) fn elapsed_text(secs: f32, color: bool) -> String {
    paint(&format!("{secs:.2}s"), DIM, color)
}

pub(super) fn step_fin(started: Instant, recompiled: usize, total: usize) {
    let secs = started.elapsed().as_secs_f32();
    let note = finish_note(recompiled, total);
    step_line(
        "Finished",
        &format!("{} ({note})", elapsed_text(secs, stderr_color())),
    );
}
