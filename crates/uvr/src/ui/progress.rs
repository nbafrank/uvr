//! Progress indicators: single spinner + aggregate progress bar.

use super::glyph;
use indicatif::{ProgressBar, ProgressStyle};

/// A slim, cyan spinner for single long-running operations.
pub fn make_spinner(msg: &str) -> ProgressBar {
    if !console::Term::stderr().is_term() {
        return ProgressBar::hidden();
    }
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template("{spinner:.cyan} {msg}")
            .unwrap()
            .tick_strings(glyph::spinner_ticks()),
    );
    pb.enable_steady_tick(std::time::Duration::from_millis(80));
    pb.set_message(msg.to_string());
    pb
}

/// Aggregate progress bar that shows `{bar} {pos}/{len} · {msg}`.
/// Use when installing a known number of packages.
pub fn make_aggregate_bar(total: u64) -> ProgressBar {
    if !console::Term::stderr().is_term() {
        return ProgressBar::hidden();
    }
    let pb = ProgressBar::new(total);
    let tmpl = format!(
        "  {{bar:28.cyan/{dim}}} {{pos:>3}}/{{len}} {sep} {{msg}}",
        dim = "blue.dim",
        sep = glyph::bullet(),
    );
    pb.set_style(
        ProgressStyle::with_template(&tmpl)
            .unwrap()
            .progress_chars(&format!(
                "{}{}{}",
                glyph::bar_filled(),
                glyph::bar_filled(),
                glyph::bar_empty()
            )),
    );
    pb
}
