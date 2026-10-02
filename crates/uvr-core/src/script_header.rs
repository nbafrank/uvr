//! Inline dependency headers — a script that carries its own environment.
//!
//! An `.R` file can declare the packages it needs in a fenced comment block
//! near the top, so `uvr run script.R` provisions them and runs the file in
//! **any** directory, with no project and no setup. Send someone the script
//! and it runs.
//!
//! ```r
//! # /// script
//! # dependencies = [
//! #   "ggplot2>=3.4",
//! #   "DESeq2 (bioc)",
//! #   "tidyverse/ggplot2@main",
//! # ]
//! # ///
//!
//! library(ggplot2)
//! ```
//!
//! The shape mirrors Python's PEP 723, which solved the same problem: an
//! opening `# /// script` line, `#`-prefixed TOML, and a closing `# ///`.
//! R has no published standard here, so uvr defines the format rather than
//! inventing a second one later.
//!
//! Both fence lines must sit at column 0 with no leading whitespace — the
//! same rule PEP 723 sets. An indented `# /// script` is *not* a header, so
//! that a line inside a string or a nested comment cannot open a block.
//!
//! A file may declare at most one header: a second `# /// script` block is
//! a hard error, as in PEP 723's reference implementation. First-wins would
//! silently discard the later block — a stale header surviving a bad merge
//! is exactly how that bites.
//!
//! Each entry is a dependency spec in the grammar `uvr add` takes
//! ([`crate::dep_spec`]), so a header can pin versions and name
//! Bioconductor or git sources (#182). The `r` version pin arrives with
//! #183 and is only captured here.

use serde::Deserialize;

use crate::dep_spec;
use crate::error::{Result, UvrError};
use crate::manifest::DependencySpec;

/// Opens the block. Matched exactly, ignoring only trailing whitespace.
const FENCE_OPEN: &str = "# /// script";
/// Closes the block.
const FENCE_CLOSE: &str = "# ///";

/// The declarations parsed out of a script's inline header.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ScriptHeader {
    /// `(package name, spec)` pairs in the order written — the same shape
    /// `uvr add` produces.
    pub dependencies: Vec<(String, DependencySpec)>,

    /// R version constraint. Parsed but not yet acted on — see [`parse`].
    pub r: Option<String>,
}

/// The header's TOML as written, before its specs are parsed.
///
/// Unknown keys are ignored rather than rejected, so a script written for a
/// newer uvr still runs on an older one instead of failing on a key it hasn't
/// learned yet.
#[derive(Deserialize)]
struct RawHeader {
    #[serde(default)]
    dependencies: Vec<String>,
    #[serde(default)]
    r: Option<String>,
}

/// Escape control characters so header-derived text is safe to print.
///
/// A script is exactly the thing this feature invites users to accept from
/// strangers, and TOML's `\u001b` escape legally decodes to a live ESC —
/// enough for a header to repaint or overwrite uvr's own diagnostics once
/// it is interpolated into a warning or error. Control characters render
/// as their `\u{…}` escape instead. Newlines are kept: TOML's multi-line
/// parse errors stay readable, and a bare `\n` cannot forge a styled line.
pub fn sanitize_for_display(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_control() && c != '\n' {
            out.extend(c.escape_debug());
        } else {
            out.push(c);
        }
    }
    out
}

/// Build a parse error, sanitizing the message.
///
/// Every error this module produces quotes text from the script — the
/// offending spec, the stray line, TOML's own snippet of the source — so
/// the escaping lives here, at the one place errors are built, rather than
/// at each interpolation site.
fn parse_err(message: String) -> UvrError {
    UvrError::ScriptHeaderParse(sanitize_for_display(&message))
}

/// Parse a script's inline dependency header.
///
/// Returns `Ok(None)` when the file has no header at all — the overwhelming
/// majority of scripts, which must keep running exactly as they do today.
/// A header that is present but broken is an error, never a silent `None`:
/// a typo in the fence would otherwise drop every declared dependency and
/// fail later as a confusing missing-package error.
///
/// `r` is parsed but not applied — R-version pinning lands in #183. Callers
/// should tell the user it was ignored rather than silently running against
/// whichever R they happen to have.
///
/// A file may declare at most one block: a second `# /// script` fence after
/// the first block closes is an error, never silently discarded.
pub fn parse(source: &str) -> Result<Option<ScriptHeader>> {
    let mut lines = source.lines();

    // Scan for the opening fence. Anything before it is ordinary script
    // content — a shebang, a licence banner, a roxygen block.
    if !lines.any(|line| line.trim_end() == FENCE_OPEN) {
        return Ok(None);
    }

    let mut body = String::new();
    let mut header: Option<RawHeader> = None;
    for line in lines.by_ref() {
        // Checked before the comment strip below, which would otherwise
        // consume the closing fence as a body line: `# ///` less its `# `
        // prefix is `///`, which TOML would then reject as a syntax error
        // rather than the block ending cleanly.
        if line.trim_end() == FENCE_CLOSE {
            header = Some(toml::from_str(&body).map_err(|e| parse_err(e.to_string()))?);
            break;
        }

        let content = if line.trim_end() == "#" {
            ""
        } else if let Some(rest) = line.strip_prefix("# ") {
            rest
        } else {
            // Not "unterminated" — the block may well be closed further down.
            // This line simply cannot be part of it, which is almost always a
            // missing `# ///` above it.
            return Err(parse_err(format!(
                "`{}` is not a `#` comment, so it cannot be inside the \
                 `{FENCE_OPEN}` block — is the closing `{FENCE_CLOSE}` missing?",
                line.trim()
            )));
        };

        body.push_str(content);
        body.push('\n');
    }

    let Some(header) = header else {
        return Err(parse_err(format!(
            "unterminated `{FENCE_OPEN}` block: no closing `{FENCE_CLOSE}` line"
        )));
    };

    let mut dependencies = Vec::with_capacity(header.dependencies.len());
    for spec in &header.dependencies {
        // No part of any spec (name, version, host, ref) can hold a control
        // character. Refusing them here keeps header text that *is* accepted
        // from carrying a live escape sequence into later diagnostics — a
        // resolver error echoes a spec's ref, for one.
        if spec.chars().any(char::is_control) {
            return Err(parse_err(format!("`{spec}` contains a control character")));
        }
        dependencies.push(dep_spec::parse(spec, false).map_err(|e| parse_err(e.to_string()))?);
    }

    // The rest of the file gets the same scan the opening fence did, so a
    // second block is refused rather than silently ignored — PEP 723's
    // reference implementation errors here too.
    if lines.any(|line| line.trim_end() == FENCE_OPEN) {
        return Err(parse_err(format!(
            "multiple `{FENCE_OPEN}` blocks — a script may declare only one \
             header; remove the extra block"
        )));
    }

    Ok(Some(ScriptHeader {
        dependencies,
        r: header.r,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The package names a header declares, in order.
    fn deps(source: &str) -> Vec<String> {
        parse(source)
            .expect("should parse")
            .expect("should have a header")
            .dependencies
            .into_iter()
            .map(|(name, _)| name)
            .collect()
    }

    /// Parse a header holding the single entry `spec`.
    fn one(spec: &str) -> Result<(String, DependencySpec)> {
        let source = format!("# /// script\n# dependencies = [{spec:?}]\n# ///\n");
        Ok(parse(&source)?.expect("a header").dependencies.remove(0))
    }

    #[test]
    fn a_header_yields_its_dependencies_in_order() {
        let source = "\
# /// script
# dependencies = [
#   \"ggplot2\",
#   \"dplyr\",
# ]
# ///

library(ggplot2)
";
        assert_eq!(deps(source), vec!["ggplot2", "dplyr"]);
    }

    #[test]
    fn a_single_line_array_works_too() {
        let source = "# /// script\n# dependencies = [\"jsonlite\"]\n# ///\n";
        assert_eq!(deps(source), vec!["jsonlite"]);
    }

    #[test]
    fn a_file_with_no_header_has_none() {
        assert_eq!(parse("library(ggplot2)\nprint(1)\n").unwrap(), None);
    }

    #[test]
    fn an_empty_file_has_none() {
        assert_eq!(parse("").unwrap(), None);
    }

    #[test]
    fn a_decorative_comment_divider_does_not_open_a_block() {
        // `# ///` on its own is a plausible section divider in a real script.
        // Only the full `# /// script` opens a header, so a divider must not
        // turn the rest of the file into a parse error.
        let source = "# ///\n# a divider, not a header\nprint(1)\n";
        assert_eq!(parse(source).unwrap(), None);
    }

    #[test]
    fn the_fence_must_sit_at_column_zero() {
        // Indented, so not a header — matching PEP 723's rule.
        let source = "  # /// script\n  # dependencies = [\"x\"]\n  # ///\n";
        assert_eq!(parse(source).unwrap(), None);
    }

    #[test]
    fn trailing_whitespace_on_a_fence_is_tolerated() {
        let source = "# /// script  \n# dependencies = []\n# ///\t\n";
        assert_eq!(deps(source), Vec::<String>::new());
    }

    #[test]
    fn an_empty_dependency_list_is_a_header_not_an_absence() {
        // Distinct from `Ok(None)`: this script asked for an isolated
        // environment with nothing in it, and must get one.
        let source = "# /// script\n# dependencies = []\n# ///\n";
        assert_eq!(parse(source).unwrap(), Some(ScriptHeader::default()));
    }

    #[test]
    fn a_header_with_no_keys_at_all_is_valid() {
        let source = "# /// script\n# ///\n";
        assert_eq!(parse(source).unwrap(), Some(ScriptHeader::default()));
    }

    #[test]
    fn a_bare_hash_is_a_blank_body_line() {
        let source = "# /// script\n#\n# dependencies = [\"withr\"]\n#\n# ///\n";
        assert_eq!(deps(source), vec!["withr"]);
    }

    #[test]
    fn an_unterminated_block_is_an_error() {
        let source = "# /// script\n# dependencies = [\"ggplot2\"]\n";
        let err = parse(source).unwrap_err().to_string();
        assert!(err.contains("unterminated"), "got: {err}");
        assert!(err.contains("no closing"), "got: {err}");
    }

    #[test]
    fn script_code_before_the_closing_fence_is_an_error() {
        // The likeliest real typo: the author forgot the closing line, so
        // the first line of actual code lands inside the block. The block
        // *is* closed further down, so the message must point at the stray
        // line rather than claim the block was never terminated.
        let source = "# /// script\n# dependencies = [\"ggplot2\"]\nlibrary(ggplot2)\n# ///\n";
        let err = parse(source).unwrap_err().to_string();
        assert!(err.contains("library(ggplot2)"), "got: {err}");
        assert!(err.contains(FENCE_CLOSE), "got: {err}");
        assert!(!err.contains("unterminated"), "misleading wording: {err}");
    }

    #[test]
    fn malformed_toml_in_the_body_is_an_error() {
        let source = "# /// script\n# dependencies = [\n# ///\n";
        assert!(parse(source).is_err());
    }

    #[test]
    fn a_wrongly_typed_dependencies_key_is_an_error() {
        let source = "# /// script\n# dependencies = \"ggplot2\"\n# ///\n";
        assert!(parse(source).is_err());
    }

    #[test]
    fn an_unknown_key_is_ignored_for_forward_compatibility() {
        // A script written against a newer uvr must still run here rather
        // than failing on a key this version has never heard of.
        let source = "# /// script\n# future-knob = 7\n# dependencies = [\"cli\"]\n# ///\n";
        assert_eq!(deps(source), vec!["cli"]);
    }

    #[test]
    fn an_r_pin_is_captured_even_though_it_is_not_applied_yet() {
        // Captured so the caller can say it was ignored (#183). Dropping it
        // silently would run the script against the wrong R with no signal.
        let source = "# /// script\n# r = \">=4.3\"\n# dependencies = [\"cli\"]\n# ///\n";
        let header = parse(source).unwrap().unwrap();
        assert_eq!(header.r.as_deref(), Some(">=4.3"));
        assert_eq!(
            header.dependencies,
            vec![("cli".to_string(), DependencySpec::default())]
        );
    }

    #[test]
    fn dotted_package_names_are_plain_names() {
        // `data.table`, `org.Hs.eg.db` — dots are legal in R package names,
        // so the plain-name check must not mistake them for a spec grammar.
        let source = "# /// script\n# dependencies = [\"data.table\", \"org.Hs.eg.db\"]\n# ///\n";
        assert_eq!(deps(source), vec!["data.table", "org.Hs.eg.db"]);
    }

    #[test]
    fn a_plain_name_is_the_default_spec() {
        // What `--with` passes too — and what the with-env cache key hashes
        // as a bare name, so existing environments keep their keys (#182).
        assert_eq!(
            one("jsonlite").unwrap(),
            ("jsonlite".to_string(), DependencySpec::default())
        );
    }

    #[test]
    fn a_version_constraint_is_kept() {
        for spec in ["ggplot2>=3.4", "ggplot2 >=3.4", "ggplot2@>=3.4"] {
            assert_eq!(
                one(spec).unwrap(),
                (
                    "ggplot2".to_string(),
                    DependencySpec::Version(">=3.4".to_string())
                ),
                "{spec}"
            );
        }
    }

    #[test]
    fn a_bioc_entry_is_what_uvr_add_bioc_produces() {
        let (name, spec) = one("DESeq2 (bioc)").unwrap();
        assert_eq!(name, "DESeq2");
        assert!(spec.is_bioc());
        assert_eq!((name, spec), dep_spec::parse("DESeq2", true).unwrap());

        let (_, spec) = one("DESeq2>=1.40 (bioc)").unwrap();
        assert!(spec.is_bioc());
        assert_eq!(spec.version_req(), Some(">=1.40"));
    }

    #[test]
    fn git_entries_are_what_uvr_add_produces() {
        for (spec, name, git, rev) in [
            (
                "tidyverse/ggplot2@main",
                "ggplot2",
                "tidyverse/ggplot2",
                Some("main"),
            ),
            ("rladies/praise", "praise", "rladies/praise", None),
            (
                "forgejo::codeberg.org/owner/pkg@v1.0",
                "pkg",
                "forgejo::codeberg.org/owner/pkg",
                Some("v1.0"),
            ),
            (
                "gitlab::gitlab.com/group/sub/pkg@abc123",
                "pkg",
                "gitlab::gitlab.com/group/sub/pkg",
                Some("abc123"),
            ),
        ] {
            let parsed = one(spec).unwrap();
            assert_eq!(parsed, dep_spec::parse(spec, false).unwrap(), "{spec}");
            let (got_name, DependencySpec::Detailed(d)) = parsed else {
                panic!("{spec}: expected a detailed spec");
            };
            assert_eq!(got_name, name, "{spec}");
            assert_eq!(d.git.as_deref(), Some(git), "{spec}");
            assert_eq!(d.rev.as_deref(), rev, "{spec}");
        }

        let (name, spec) = one("owner/repo@v2#subdirectory=pkgs/inner").unwrap();
        assert_eq!(name, "inner");
        assert_eq!(spec.subdirectory(), Some("pkgs/inner"));
    }

    #[test]
    fn a_spec_uvr_add_refuses_is_a_header_error_saying_why() {
        for (spec, why) in [
            ("", "Invalid package name"),
            ("ggplot2>>3", "Invalid version constraint"),
            ("gitlab.com/user/repo", "Unsupported git host"),
            ("user/repo (bioc)", "git source"),
        ] {
            let err = match one(spec) {
                Err(e) => e.to_string(),
                Ok(ok) => panic!("`{spec}` should be rejected, got {ok:?}"),
            };
            assert!(err.contains(why), "`{spec}`: {err}");
            assert!(err.contains(spec), "`{spec}` not named: {err}");
        }
    }

    #[test]
    fn an_accepted_spec_cannot_carry_a_control_character() {
        // A git ref is free text as far as the spec grammar goes, and it is
        // echoed back by resolver errors. Refused at the header instead.
        let source = "# /// script\n# dependencies = [\"user/repo@\\u001b[31mmain\"]\n# ///\n";
        let err = parse(source).unwrap_err().to_string();
        assert!(err.contains("control character"), "{err}");
        assert!(!err.contains('\u{1b}'), "raw ESC leaked: {err:?}");
    }

    #[test]
    fn the_header_may_follow_a_shebang_or_banner() {
        let source = "\
#!/usr/bin/env Rscript
# Copyright someone
# /// script
# dependencies = [\"cli\"]
# ///
";
        assert_eq!(deps(source), vec!["cli"]);
    }

    #[test]
    fn a_second_block_is_a_hard_error() {
        // First-wins would silently discard the later block — a stale header
        // surviving a bad merge is exactly how that bites, and #181 promises
        // a broken header is "never silently ignored". PEP 723's reference
        // implementation errors on multiple blocks; so does this one.
        let source = "\
# /// script
# dependencies = [\"cli\"]
# ///
print(1)
# /// script
# dependencies = [\"nope\"]
# ///
";
        let err = parse(source).unwrap_err().to_string();
        assert!(err.contains("only one"), "got: {err}");
    }

    #[test]
    fn later_dividers_and_indented_fences_are_not_a_second_block() {
        // The duplicate scan applies the same rules as the opening scan: a
        // bare `# ///` divider and an indented `# /// script` never open a
        // block, so they must not be refused as one either.
        let source = "\
# /// script
# dependencies = [\"cli\"]
# ///
# ///
  # /// script
print(1)
";
        assert_eq!(deps(source), vec!["cli"]);
    }

    #[test]
    fn header_text_in_errors_cannot_smuggle_control_characters() {
        // Every parse error quotes text from the script — a file this
        // feature invites users to accept from strangers. TOML forbids raw
        // control bytes inside strings, but `\u001b` is a legal escape
        // that decodes to a live ESC; printed raw, it could repaint or
        // overwrite uvr's own diagnostics. It must surface as the escaped
        // rendering `\u{1b}` instead.
        for source in [
            // Decoded by TOML, quoted by the rejected-spec message.
            "# /// script\n# dependencies = [\"pkg\\u001b[31mFAKE\"]\n# ///\n",
            // Raw ESC on a stray line, quoted by the stray-line message.
            "# /// script\n\u{1b}[31mFAKE\n# ///\n",
        ] {
            let err = parse(source).unwrap_err().to_string();
            assert!(!err.contains('\u{1b}'), "raw ESC leaked: {err:?}");
            assert!(err.contains("\\u{1b}"), "escaped form missing: {err:?}");
        }
    }

    #[test]
    fn toml_error_snippets_cannot_smuggle_control_characters_either() {
        // A raw ESC in the body is refused by TOML itself, whose error
        // message quotes the offending source line — the same trust
        // boundary, reached through toml's renderer instead of ours.
        let source = "# /// script\n# x = \"\u{1b}[31mFAKE\"\n# ///\n";
        let err = parse(source).unwrap_err().to_string();
        assert!(!err.contains('\u{1b}'), "raw ESC leaked: {err:?}");
    }
}
