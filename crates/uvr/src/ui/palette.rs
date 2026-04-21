//! Semantic styling for uvr output.
//!
//! Central source of truth for every color decision in the CLI. Each helper
//! maps a semantic role (success, warn, info, pkg, hint, error, …) to a
//! concrete `console::Style`. Call sites should reach for these rather than
//! constructing styles inline, so that changing a role-color swaps everywhere.
//!
//! # Role map
//! - `success`  — green,   decisive positive outcomes (`✓ Ready`).
//! - `fail`     — red,     errors and failure glyphs (`✗ …`).
//! - `warn`     — amber,   warnings and caution (`⚠ …`). Uses xterm-256 208.
//! - `warn_badge` — inverse amber block (` WARN `) for loud multi-line headers.
//! - `info`     — cyan,    section leads and neutral highlights.
//! - `hint`     — cyan,    actionable next-step guidance. Bolded lead word.
//! - `added`    — green,   `+` rows.
//! - `removed`  — red,     `-` rows.
//! - `upgraded` — magenta, `↑` rows (reserved exclusively for upgrades).
//! - `pkg`      — cyan,    package names in-body.
//! - `version`  — dim,     version strings that trail a name.
//! - `dim`      — dim,     muted metadata (paths, counts, separators).

#![allow(dead_code)]

use console::{style, Style, StyledObject};
use std::fmt::Display;

/// Amber accent for warnings. Uses xterm-256 color 208 (a saturated orange)
/// so it's clearly distinguishable from both yellow-tinted info text and the
/// red of errors. Falls back cleanly to 16-color yellow via the 256→16 map.
const AMBER: u8 = 208;

/// Cyan accent for info and hints. 39 is a bright, slightly-blue cyan that
/// pops against most dark terminal backgrounds without being neon.
const CYAN_ACCENT: u8 = 39;

pub fn success<D: Display>(d: D) -> StyledObject<D> {
    style(d).green().bold()
}

pub fn fail<D: Display>(d: D) -> StyledObject<D> {
    style(d).red().bold()
}

/// Amber, bold. For the `⚠` glyph and the warning headline text.
pub fn warn<D: Display>(d: D) -> StyledObject<D> {
    style(d).color256(AMBER).bold()
}

/// Inverse amber block — ` WARN ` style badge. For loud multi-line headers
/// where the user must not miss that a warning is being raised.
pub fn warn_badge<D: Display>(d: D) -> StyledObject<D> {
    style(d).color256(AMBER).bold().reverse()
}

/// Dim amber for continuation lines under a warning — readable, but clearly
/// subordinate to the headline.
pub fn warn_body<D: Display>(d: D) -> StyledObject<D> {
    style(d).color256(AMBER)
}

pub fn info<D: Display>(d: D) -> StyledObject<D> {
    style(d).color256(CYAN_ACCENT).bold()
}

/// Dim cyan for info continuation bodies (under a `›` lead).
pub fn info_body<D: Display>(d: D) -> StyledObject<D> {
    style(d).color256(CYAN_ACCENT)
}

/// Inverse red badge — ` ERROR ` style. For error block headlines.
pub fn error_badge<D: Display>(d: D) -> StyledObject<D> {
    style(d).red().bold().reverse()
}

/// Hint accent: bright cyan, bold. Used on the literal "Hint:" label
/// preceding actionable next-step text.
pub fn hint_label<D: Display>(d: D) -> StyledObject<D> {
    style(d).color256(CYAN_ACCENT).bold()
}

/// Hint body text: cyan, not dim — we want the user to *read* the fix.
pub fn hint_body<D: Display>(d: D) -> StyledObject<D> {
    style(d).color256(CYAN_ACCENT)
}

/// Legacy name kept for back-compat at any stray call site; behaves like
/// `hint_body` now (colored, not dim).
pub fn hint<D: Display>(d: D) -> StyledObject<D> {
    hint_body(d)
}

pub fn dim<D: Display>(d: D) -> StyledObject<D> {
    style(d).dim()
}

pub fn pkg<D: Display>(d: D) -> StyledObject<D> {
    style(d).color256(CYAN_ACCENT)
}

pub fn version<D: Display>(d: D) -> StyledObject<D> {
    style(d).dim()
}

pub fn added<D: Display>(d: D) -> StyledObject<D> {
    style(d).green().bold()
}

pub fn removed<D: Display>(d: D) -> StyledObject<D> {
    style(d).red().bold()
}

pub fn upgraded<D: Display>(d: D) -> StyledObject<D> {
    style(d).magenta().bold()
}

pub fn bold<D: Display>(d: D) -> StyledObject<D> {
    style(d).bold()
}

/// Construct a free-form `Style`. Escape hatch for rare layouts (progress bar
/// templates, tree connectors) where a stored style is more ergonomic than a
/// role helper.
pub fn style_for(role: Role) -> Style {
    match role {
        Role::Success => Style::new().green().bold(),
        Role::Fail => Style::new().red().bold(),
        Role::Warn => Style::new().color256(AMBER).bold(),
        Role::Info => Style::new().color256(CYAN_ACCENT).bold(),
        Role::Hint => Style::new().color256(CYAN_ACCENT),
        Role::Dim => Style::new().dim(),
    }
}

/// Semantic role — used by `style_for` when a stored `Style` is needed.
pub enum Role {
    Success,
    Fail,
    Warn,
    Info,
    Hint,
    Dim,
}

/// Format a duration in a friendly way.
pub fn format_duration(d: std::time::Duration) -> String {
    let secs = d.as_secs_f64();
    if secs < 1.0 {
        format!("{}ms", d.as_millis())
    } else if secs < 60.0 {
        format!("{secs:.2}s")
    } else {
        let mins = (secs / 60.0).floor() as u64;
        let rem = secs - (mins as f64 * 60.0);
        format!("{mins}m{rem:.0}s")
    }
}

/// Format a byte count in a friendly way.
pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}
