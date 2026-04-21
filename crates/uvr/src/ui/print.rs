//! High-level printing helpers.
//!
//! Keep call sites short: `ui::success("Ready")`, `ui::warn("Stale lockfile")`.
//!
//! Every helper here goes through `palette` + `glyph`; nothing constructs a
//! style inline. If you need a new output shape, add a helper here first.

#![allow(dead_code)]

use super::{glyph, palette};

/// `✓ <text>` in green bold glyph, plain text.
pub fn success(text: impl std::fmt::Display) {
    println!("{} {text}", palette::success(glyph::success()));
}

/// `✗ <text>` in red bold.
pub fn fail(text: impl std::fmt::Display) {
    println!("{} {text}", palette::fail(glyph::fail()));
}

/// Single-line warning: `⚠ Warning: <text>`.
///
/// The `⚠` glyph is amber-bold, the word `Warning:` is amber-bold as well,
/// and the message body is amber (non-bold) so the whole line reads as a
/// single amber phrase against the default terminal text around it.
/// Goes to **stderr** — warnings are diagnostic output, not data.
pub fn warn(text: impl std::fmt::Display) {
    eprintln!(
        "{} {} {}",
        palette::warn(glyph::warn()),
        palette::warn("Warning:"),
        palette::warn_body(text),
    );
}

/// Multi-line warning with a loud inverse `⚠ WARN ` header band.
///
/// Use when the warning has structured context the user must read — e.g.
/// "these N system packages are missing, here's the install command".
/// `headline` is the one-liner; `body` lines appear dim-amber beneath.
/// Goes to stderr.
pub fn warn_block(headline: &str, body: impl IntoIterator<Item = String>) {
    eprintln!(
        "{} {}",
        palette::warn_badge(format!(" {} WARN ", glyph::warn())),
        palette::warn(headline),
    );
    for line in body {
        eprintln!("  {}", palette::warn_body(line));
    }
}

/// `› <text>` — informational headline.
pub fn info(text: impl std::fmt::Display) {
    println!("{} {text}", palette::info(glyph::info()));
}

/// Dedicated hint line — use after an error or warning to tell the user
/// what to do next. Renders as:
///
/// ```text
///   → Hint: Install the CRAN gfortran build from …
/// ```
///
/// Cyan label + cyan body; visually distinct from dim error context
/// and from amber warnings. Goes to stderr (it accompanies diagnostics).
pub fn hint(text: impl std::fmt::Display) {
    eprintln!(
        "  {} {} {}",
        palette::hint_label(glyph::hint()),
        palette::hint_label("Hint:"),
        palette::hint_body(text),
    );
}

/// Indented bullet: `  · <text>`.
pub fn bullet(text: impl std::fmt::Display) {
    println!("  {} {text}", palette::dim(glyph::bullet()));
}

/// Indented bullet with explicit dim body — for metadata under a header.
pub fn bullet_dim(text: impl std::fmt::Display) {
    println!("  {} {}", palette::dim(glyph::bullet()), palette::dim(text));
}

/// `<glyph> <pkg-name> <version>` — a single row in a change list.
pub fn row(glyph_str: console::StyledObject<&str>, name: &str, version: &str) {
    println!(
        "  {glyph_str} {} {}",
        palette::pkg(name),
        palette::version(version)
    );
}

/// Upgrade row: `↑ pkg old → new` with magenta accent on the new version.
pub fn row_upgrade(name: &str, from: &str, to: &str) {
    println!(
        "  {} {} {} {} {}",
        palette::upgraded(glyph::upgrade()),
        palette::pkg(name),
        palette::version(from),
        palette::dim(glyph::arrow()),
        palette::upgraded(to),
    );
}

pub fn row_added(name: &str, version: &str) {
    println!(
        "  {} {} {}",
        palette::added(glyph::add()),
        palette::pkg(name),
        palette::version(version)
    );
}

pub fn row_removed(name: &str, version: &str) {
    println!(
        "  {} {} {}",
        palette::removed(glyph::remove()),
        palette::pkg(name),
        palette::version(version)
    );
}

/// Section header with bold title. No horizontal rule — rule-less by choice.
pub fn section(title: &str) {
    println!();
    println!("{}", palette::bold(title));
}

/// Two-line summary: headline `✓ <headline>` and dim `  <sub>` subtitle.
pub fn summary(headline: impl std::fmt::Display, sub: impl std::fmt::Display) {
    println!("{} {headline}", palette::success(glyph::success()));
    println!("  {}", palette::dim(sub));
}

/// Single-line padded check for doctor: `{glyph} {label:<width}} {status}`.
pub fn check(ok: bool, label: &str, status: impl std::fmt::Display, width: usize) {
    let glyph_str = if ok {
        palette::success(glyph::success()).to_string()
    } else {
        palette::fail(glyph::fail()).to_string()
    };
    println!("  {glyph_str} {label:<width$} {status}");
}

/// Submenu welcome: `uvr <group> · <tagline>` followed by a flat list of
/// commands. Used when a grouping subcommand (e.g. `uvr r`) is invoked
/// without a child.
pub fn welcome_group(group: &str, tagline: &str, items: &[(&str, &str)]) {
    println!();
    println!(
        "  {} {} {}",
        palette::bold(format!("uvr {group}")),
        palette::dim(glyph::bullet()),
        palette::dim(tagline),
    );

    let cmd_width = items.iter().map(|(c, _)| c.len()).max().unwrap_or(20);

    println!();
    for (cmd, desc) in items {
        let pad = cmd_width.saturating_sub(cmd.len());
        println!(
            "    {}{:pad$}   {}",
            palette::info(*cmd),
            "",
            palette::dim(*desc),
        );
    }
    println!();
}

/// Welcome screen printed when `uvr` is run with no subcommand.
pub fn welcome(version: &str) {
    let tagline = "Fast, reproducible R package management";
    println!();
    println!(
        "  {} {} {}",
        palette::bold("uvr"),
        palette::dim(glyph::bullet()),
        palette::dim(tagline),
    );
    println!("  {}", palette::dim(format!("v{version}")));

    let groups: [(&str, &[(&str, &str)]); 3] = [
        (
            "Get started",
            &[
                ("uvr init", "Create a new project here"),
                ("uvr add <pkg>", "Add a package"),
                ("uvr sync", "Install everything from the lockfile"),
            ],
        ),
        (
            "Everyday",
            &[
                ("uvr run <script>", "Run R in the project environment"),
                ("uvr update", "Update packages to the latest allowed"),
                ("uvr tree", "Show the dependency tree"),
            ],
        ),
        (
            "Tooling",
            &[
                ("uvr r install <ver>", "Install an R version"),
                ("uvr doctor", "Diagnose environment issues"),
                ("uvr help", "Full command reference"),
            ],
        ),
    ];

    let cmd_width = groups
        .iter()
        .flat_map(|(_, items)| items.iter())
        .map(|(cmd, _)| cmd.len())
        .max()
        .unwrap_or(20);

    for (title, items) in groups {
        println!();
        println!("  {}", palette::bold(title));
        for (cmd, desc) in items {
            let pad = cmd_width.saturating_sub(cmd.len());
            println!(
                "    {}{:pad$}   {}",
                palette::info(*cmd),
                "",
                palette::dim(*desc),
            );
        }
    }
    println!();
}

/// Three-part error block rendered to stderr:
///
/// ```text
///  ERROR  Failed to install packages after add
///    Failed to install RcppArmadillo
///    R CMD INSTALL failed for 'RcppArmadillo' (exit 1):
///    …
///
///   → Hint: Missing Fortran toolchain on macOS. Install the CRAN gfortran
///           build from https://mac.r-project.org/tools/ or run
///           `brew install gcc`.
/// ```
///
/// - **Headline** is a red-inverse ` ERROR ` badge followed by the red-bold
///   top-level message; impossible to miss scanning a terminal.
/// - **Context** lines are indented and dim — they recede behind the badge.
/// - **Hint** (if any) is separated by a blank line, cyan-accented, and
///   prefixed with `→ Hint:` so the user's fix-path is visually distinct
///   from the error context itself.
pub fn error_block(headline: &str, context: Option<&str>, hint_text: Option<&str>) {
    eprintln!(
        "{} {}",
        palette::error_badge(format!(" {} ERROR ", glyph::fail())),
        palette::fail(headline),
    );
    if let Some(ctx) = context {
        for line in ctx.lines() {
            eprintln!("  {}", palette::dim(line));
        }
    }
    if let Some(h) = hint_text {
        // Blank line separates guidance from error context — guidance is
        // not more error noise, it's the path forward.
        eprintln!();
        hint(h);
    }
}
