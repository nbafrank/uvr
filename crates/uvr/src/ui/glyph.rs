//! Glyph vocabulary for uvr output.
//!
//! Unicode by default, ASCII fallback when `UVR_ASCII=1` is set or when the
//! terminal can't render unicode (e.g. piped output).
//!
//! # Vocabulary
//! - `success` `✓` — positive outcome.
//! - `fail`    `✗` — error glyph.
//! - `warn`    `⚠` — warning glyph (distinct from `✗` and the `i` hint).
//! - `info`    `›` — section lead, inline info.
//! - `hint`    `→` — next-step pointer; paired with a bold "Hint:" label.
//! - `add`     `+`, `remove` `−`, `upgrade` `↑` — change rows.
//! - `bullet`  `·` — separator / unstyled leader under a header.
//! - `arrow`   `→` — inline transition (version old → new).
//! - `change`  `~` — manifest tweak.

#![allow(dead_code)]

fn ascii_only() -> bool {
    std::env::var_os("UVR_ASCII").is_some()
        || !console::Term::stderr().features().colors_supported()
}

macro_rules! g {
    ($name:ident, $unicode:expr, $ascii:expr) => {
        pub fn $name() -> &'static str {
            if ascii_only() {
                $ascii
            } else {
                $unicode
            }
        }
    };
}

g!(success, "✓", "v");
g!(fail, "✗", "x");
g!(warn, "⚠", "!");
g!(info, "›", ">");
g!(hint, "→", "->");

g!(add, "+", "+");
g!(remove, "−", "-");
g!(upgrade, "↑", "^");
g!(bullet, "·", ".");
g!(arrow, "→", "->");
g!(change, "~", "~");

g!(tree_branch, "├── ", "+-- ");
g!(tree_last, "└── ", "`-- ");
g!(tree_vert, "│   ", "|   ");
g!(tree_space, "    ", "    ");

pub fn spinner_ticks() -> &'static [&'static str] {
    if ascii_only() {
        &["-", "\\", "|", "/"]
    } else {
        &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
    }
}

pub fn bar_filled() -> &'static str {
    if ascii_only() {
        "#"
    } else {
        "━"
    }
}

pub fn bar_empty() -> &'static str {
    if ascii_only() {
        "-"
    } else {
        "─"
    }
}
