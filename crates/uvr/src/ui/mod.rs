//! Shared CLI presentation layer: glyphs, palette, printing helpers, progress.
//!
//! Design notes:
//! - Warm, inviting aesthetic: friendly verbs, soft bullets, no horizontal rules.
//! - Magenta is reserved exclusively for upgrades.
//! - All glyphs degrade to ASCII when `UVR_ASCII=1` or colors are disabled.

pub mod glyph;
pub mod palette;
pub mod print;
pub mod progress;

#[allow(unused_imports)]
pub use print::{
    bullet, bullet_dim, check, error_block, fail, hint, info, row, row_added, row_removed,
    row_upgrade, section, success, summary, warn, welcome, welcome_group,
};
#[allow(unused_imports)]
pub use progress::{make_aggregate_bar, make_spinner};

/// Start time, used by commands to print a `in Xs` trailer in their summary.
pub fn now() -> std::time::Instant {
    std::time::Instant::now()
}
