//! Glyph vocabulary for uvr output.
//!
//! Unicode by default, ASCII fallback when `UVR_ASCII=1` is set or when the
//! terminal can't render unicode (e.g. piped output).

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
g!(warn, "▲", "!");
g!(info, "›", ">");
g!(hint, "i", "i");

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
