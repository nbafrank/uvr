//! Progress indicators: single spinner + aggregate progress bar.
//!
//! Both respect TTY detection via `console::Term::stderr().is_term()` — in
//! pipes/CI they become hidden `ProgressBar` instances so `set_message` and
//! `inc` calls become no-ops, and no control sequences leak to the stream.
//!
//! Two escape hatches for environments where the TTY heuristic is wrong
//! (notably Positron's SSH terminal, which reports not-a-TTY but can render
//! ANSI sequences fine):
//!   - `UVR_PROGRESS=always` — force spinners on regardless of detection
//!   - `UVR_PROGRESS=never`  — force spinners off (useful for CI logs)

use super::{glyph, palette};
use console::Term;
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};

#[derive(Copy, Clone, PartialEq)]
enum ProgressMode {
    /// Hidden (no draws, no env-var noise — pipes/CI/explicitly-off).
    Off,
    /// Auto: stderr is a real TTY; let indicatif use its default target.
    Auto,
    /// Forced on via `UVR_PROGRESS=always`. We must use an explicit
    /// `term()` draw target — indicatif's default `stderr()` target does
    /// *its own* `is_terminal` check and silently drops draws on non-TTYs
    /// (notably Positron's SSH terminal, which reports not-a-TTY despite
    /// rendering ANSI fine — see #48). `term()` writes through `Term`
    /// unconditionally.
    ForceOn,
}

fn progress_mode() -> ProgressMode {
    match std::env::var("UVR_PROGRESS").ok().as_deref() {
        Some("always") | Some("1") | Some("true") => {
            // If stderr happens to be a real TTY, prefer Auto so indicatif's
            // own TTY-aware code path manages refresh rate and shutdown.
            // Only use ForceOn (with explicit term() target) when we'd
            // otherwise be hidden — that's the case the env var exists for.
            if Term::stderr().is_term() {
                ProgressMode::Auto
            } else {
                ProgressMode::ForceOn
            }
        }
        Some("never") | Some("0") | Some("false") => ProgressMode::Off,
        _ => {
            if Term::stderr().is_term() {
                ProgressMode::Auto
            } else {
                ProgressMode::Off
            }
        }
    }
}

/// Apply the right draw target for the current mode. Caller owns the bar.
fn apply_draw_target(pb: &ProgressBar, mode: ProgressMode) {
    if mode == ProgressMode::ForceOn {
        // 12 Hz is conservative relative to indicatif's default (20 Hz on
        // 0.17.x). The slower refresh produces less flicker over high-latency
        // SSH terminals, which is the exact scenario that lands us on this
        // path. `Term::stderr()` writes through console::Term unconditionally
        // (no internal `is_terminal()` re-check), bypassing the bail-out that
        // otherwise hides the spinner. Verified against indicatif 0.17.
        pb.set_draw_target(ProgressDrawTarget::term(Term::stderr(), 12));
    }
}

/// A slim cyan spinner for single long-running operations. Use for operations
/// whose total work is unknown up-front (dependency resolution, network round
/// trips). The spinner renders on stderr at a ~12fps tick so it feels alive
/// without burning CPU.
///
/// Styling: amber-bold spinner frames (`⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`) against cyan message
/// text — the spinner is the focal point; the message is secondary.
pub fn make_spinner(msg: &str) -> ProgressBar {
    let mode = progress_mode();
    if mode == ProgressMode::Off {
        return ProgressBar::hidden();
    }
    let pb = ProgressBar::new_spinner();
    apply_draw_target(&pb, mode);
    pb.set_style(
        // `{spinner:.cyan.bold}` renders the tick frames bold cyan, matching
        // the info-role accent in `palette.rs`. `{msg:.dim}` keeps the hint
        // text subordinate so the spinner is what the eye tracks.
        ProgressStyle::with_template("{spinner:.cyan.bold} {msg:.dim}")
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
    let mode = progress_mode();
    if mode == ProgressMode::Off {
        return ProgressBar::hidden();
    }
    let pb = ProgressBar::new(total);
    apply_draw_target(&pb, mode);
    let tmpl = format!(
        "  {{bar:28.cyan/blue.dim}} {{pos:>3}}/{{len}} {sep} {{msg:.dim}}",
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

// Touch `palette` to keep the module graph explicit — this file is the only
// one in `ui/` that doesn't go through `palette.rs` at compile time (the
// colors are baked into the indicatif template string). Leaving this import
// here signals that any future, programmatic coloring of progress should go
// through `palette` rather than adding more raw template strings.
#[allow(dead_code)]
fn _palette_anchor() -> console::Style {
    palette::style_for(palette::Role::Info)
}
