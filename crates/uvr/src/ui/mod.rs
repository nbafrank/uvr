//! Shared CLI presentation layer: glyphs, palette, printing helpers, progress.
//!
//! Design notes:
//! - Roles are centralized in `palette.rs`; call sites never pick raw colors.
//! - Warnings use a distinct amber accent (xterm-256 208) and the `⚠` glyph;
//!   they cannot be confused with info (`›`, cyan) or errors (`✗`, red).
//! - Hints use a bright cyan `→ Hint:` lead so the next-step guidance stands
//!   apart from dim error context — the user's fix-path must be readable.
//! - Errors render with an inverse ` ERROR ` badge plus a red-bold headline
//!   so the top-level failure is unmistakable when scrolling back.
//! - All glyphs degrade to ASCII when `UVR_ASCII=1` or colors are disabled.

pub mod glyph;
pub mod palette;
pub mod print;
pub mod progress;

#[allow(unused_imports)]
pub use print::{
    bullet, bullet_dim, check, error_block, fail, hint, info, row, row_added, row_removed,
    row_upgrade, section, success, summary, warn, warn_block, welcome, welcome_group,
};
#[allow(unused_imports)]
pub use progress::{make_aggregate_bar, make_spinner};

/// Start time, used by commands to print a `in Xs` trailer in their summary.
pub fn now() -> std::time::Instant {
    std::time::Instant::now()
}
