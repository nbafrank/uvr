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
//!
//! `uvr add --script` and `uvr remove --script` edit the block through
//! [`upsert`] and [`remove`] (#184), which touch only its `dependencies`.

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

        let Some(content) = body_content(line) else {
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

/// The TOML on one header body line (without its line ending): the line less
/// its `# `, or nothing for a bare `#`. `None` if it is not such a line.
fn body_content(line: &str) -> Option<&str> {
    if line.trim_end() == "#" {
        Some("")
    } else {
        line.strip_prefix("# ")
    }
}

/// Add `add` to the header's dependencies — `uvr add --script`.
///
/// A package already listed has its spec replaced in place; a new one goes
/// into sorted position when the list is sorted, else at its end, so edits
/// never reorder what someone arranged by hand. A file with no header gets
/// one, directly after a shebang line if there is one, else at the top.
///
/// Only the `dependencies` lines change. Other keys, comments, and the rest
/// of the file stay byte for byte, line endings included. Entries keep the
/// spelling they were written with; new ones use [`dep_spec::format`]'s.
pub fn upsert(source: &str, add: &[(String, DependencySpec)]) -> Result<String> {
    edit(source, &[], add)
}

/// Drop every entry for the packages in `names` — `uvr remove --script`.
///
/// Once no entry and no other key is left, the whole header goes, with the
/// blank line after it that [`upsert`] adds, so removing what `upsert` added
/// to a headerless file restores it exactly. Names not in the header are
/// ignored; the caller says so.
pub fn remove(source: &str, names: &[String]) -> Result<String> {
    edit(source, names, &[])
}

/// The `dependencies` value, with the byte span of the array and of each
/// entry in the header's TOML.
#[derive(Deserialize)]
struct SpannedHeader {
    dependencies: Option<toml::Spanned<Vec<toml::Spanned<String>>>>,
}

/// One line inside the `dependencies` array.
struct Row<'a> {
    /// The entry on this line, or `None` for a comment or blank line.
    dep: Option<(String, DependencySpec)>,
    /// The TOML before the entry, the entry, and the TOML after it.
    indent: String,
    entry: String,
    rest: String,
    /// The source line, while the row is unchanged.
    verbatim: Option<&'a str>,
}

impl Row<'_> {
    fn name(&self) -> Option<&str> {
        self.dep.as_ref().map(|(name, _)| name.as_str())
    }
}

/// Where a new or updated entry is compared: R users sort package names
/// without regard to case.
fn sort_key(name: &str) -> String {
    name.to_ascii_lowercase()
}

/// A header body line, rendered with the block's line ending.
fn body_line(content: &str, eol: &str) -> String {
    if content.is_empty() {
        format!("#{eol}")
    } else {
        format!("# {content}{eol}")
    }
}

fn edit(source: &str, drop: &[String], add: &[(String, DependencySpec)]) -> Result<String> {
    // A broken header is never edited: the user fixes it first.
    let Some(header) = parse(source)? else {
        return if add.is_empty() {
            Ok(source.to_string())
        } else {
            create(source, add)
        };
    };
    if add.is_empty() && !header.dependencies.iter().any(|(n, _)| drop.contains(n)) {
        return Ok(source.to_string());
    }

    let lines: Vec<&str> = source.split_inclusive('\n').collect();
    // `parse` accepted the block, so both fences exist and every body line
    // is a comment; the scan below is `parse`'s own.
    let open = lines
        .iter()
        .position(|l| l.trim_end() == FENCE_OPEN)
        .expect("parse found the opening fence");
    let close = open
        + 1
        + lines[open + 1..]
            .iter()
            .position(|l| l.trim_end() == FENCE_CLOSE)
            .expect("parse found the closing fence");
    let eol = if lines[open].ends_with("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let src_body = &lines[open + 1..close];
    let body: Vec<&str> = src_body
        .iter()
        .map(|l| {
            let l = l.strip_suffix('\n').unwrap_or(l);
            body_content(l.strip_suffix('\r').unwrap_or(l)).expect("parse accepted the body")
        })
        .collect();
    let text: String = body.iter().map(|c| format!("{c}\n")).collect();
    let toml_err = |e: toml::de::Error| parse_err(e.to_string());
    let spanned: SpannedHeader = toml::from_str(&text).map_err(toml_err)?;
    let has_other_keys = toml::from_str::<toml::Table>(&text)
        .map_err(toml_err)?
        .keys()
        .any(|k| k != "dependencies");

    // Byte offset in `text` -> (body line, column).
    let starts: Vec<usize> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    let at = |offset: usize| {
        let line = starts.partition_point(|&s| s <= offset) - 1;
        (line, offset - starts[line])
    };

    let mut out: String;
    let mut rows: Vec<Row> = Vec::new();
    let tail_from: usize; // first body line after the array
    let (open_line, close_line): (String, String);
    let mut collapsed = None; // the array as one `[]` line, once it empties

    match &spanned.dependencies {
        // No `dependencies` key yet: the array goes first in the body, where
        // it is in the root table whatever follows.
        None => {
            out = lines[..=open].concat();
            tail_from = 0;
            open_line = body_line("dependencies = [", eol);
            close_line = body_line("]", eol);
        }
        Some(array) => {
            let span = array.span();
            let entries = array.get_ref();
            let (ls, lc) = at(span.start);
            let (le, rc) = at(span.end - 1);
            let head = &body[ls][..lc];
            let open_rest = &body[ls][lc + 1..];
            let close_before = &body[le][..rc];
            let tail = &body[le][rc + 1..];

            // One entry per line, alone on it bar a comma and a comment:
            // then each line is kept as written, comments included.
            let mut entry_lines = Vec::with_capacity(entries.len());
            for entry in entries {
                let (line, col) = at(entry.span().start);
                let (end_line, end_col) = at(entry.span().end);
                entry_lines.push((line, col, end_col, end_line == line));
            }
            let one_per_line = ls < le
                && close_before.trim().is_empty()
                && (open_rest.trim().is_empty() || open_rest.trim_start().starts_with('#'))
                && entry_lines
                    .iter()
                    .enumerate()
                    .all(|(i, &(line, col, _, single))| {
                        single
                            && ls < line
                            && line < le
                            && body[line][..col].trim().is_empty()
                            && (i == 0 || entry_lines[i - 1].0 < line)
                    })
                && (ls + 1..le).all(|line| {
                    let t = body[line].trim();
                    entry_lines.iter().any(|e| e.0 == line) || t.is_empty() || t.starts_with('#')
                });

            let mut deps = header.dependencies.into_iter();
            if one_per_line {
                for line in ls + 1..le {
                    let content = body[line];
                    let row = match entry_lines.iter().find(|e| e.0 == line) {
                        Some(&(_, col, end, _)) => Row {
                            dep: deps.next(),
                            indent: content[..col].to_string(),
                            entry: content[col..end].to_string(),
                            rest: content[end..].to_string(),
                            verbatim: Some(src_body[line]),
                        },
                        None => Row {
                            dep: None,
                            indent: content.to_string(),
                            entry: String::new(),
                            rest: String::new(),
                            verbatim: Some(src_body[line]),
                        },
                    };
                    rows.push(row);
                }
                open_line = src_body[ls].to_string();
                close_line = src_body[le].to_string();
            } else {
                // Any other layout is rewritten one entry per line. A comment
                // in it has no line of its own to stay on, so refuse rather
                // than drop it.
                let mut pos = span.start + 1;
                for entry in entries
                    .iter()
                    .map(|e| e.span())
                    .chain(std::iter::once(span.end - 1..span.end))
                {
                    if text[pos..entry.start].contains('#') {
                        return Err(parse_err(
                            "the `dependencies` list has a comment on a line it \
                             shares with an entry or a bracket, which uvr cannot \
                             keep when it rewrites the list; put each entry on its \
                             own line and retry"
                                .to_string(),
                        ));
                    }
                    pos = entry.end;
                }
                for entry in entries {
                    rows.push(Row {
                        dep: deps.next(),
                        indent: "  ".to_string(),
                        entry: text[entry.span()].to_string(),
                        rest: ",".to_string(),
                        verbatim: None,
                    });
                }
                open_line = body_line(&format!("{head}["), eol);
                close_line = body_line(&format!("]{tail}"), eol);
            }
            out = lines[..open + 1 + ls].concat();
            tail_from = le + 1;
            // Not when a comment follows the `[`: it would be lost.
            if !one_per_line || open_rest.trim().is_empty() {
                collapsed = Some(body_line(&format!("{head}[]{tail}"), eol));
            }
        }
    }

    apply(&mut rows, drop, add)?;

    if !rows.iter().any(|r| r.dep.is_some()) && !has_other_keys {
        // Nothing left to declare: the header goes, with one blank line after.
        let after = close + 1;
        let skip = usize::from(lines.get(after).is_some_and(|l| l.trim().is_empty()));
        let out = [&lines[..open], &lines[after + skip..]].concat().concat();
        return verified(out, None);
    }

    match collapsed {
        Some(line) if rows.is_empty() => out.push_str(&line),
        _ => {
            out.push_str(&open_line);
            render_rows(&mut out, &rows, eol);
            out.push_str(&close_line);
        }
    }
    out.push_str(&src_body[tail_from..].concat());
    out.push_str(&lines[close..].concat());
    let expected = rows.into_iter().filter_map(|r| r.dep).collect();
    verified(out, Some(expected))
}

/// A new header holding `add`, after a shebang line if there is one.
fn create(source: &str, add: &[(String, DependencySpec)]) -> Result<String> {
    let lines: Vec<&str> = source.split_inclusive('\n').collect();
    let eol = match lines.first() {
        Some(l) if l.ends_with("\r\n") => "\r\n",
        _ => "\n",
    };
    // A BOM is kept first too: `parse` would not see a fence behind it.
    let after = usize::from(
        lines
            .first()
            .is_some_and(|l| l.starts_with("#!") || l.starts_with('\u{feff}')),
    );
    let mut out = lines[..after].concat();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push_str(eol);
    }

    let mut rows = Vec::new();
    apply(&mut rows, &[], add)?;
    out.push_str(&format!("{FENCE_OPEN}{eol}"));
    out.push_str(&body_line("dependencies = [", eol));
    render_rows(&mut out, &rows, eol);
    out.push_str(&body_line("]", eol));
    out.push_str(&format!("{FENCE_CLOSE}{eol}"));
    if after < lines.len() {
        out.push_str(eol);
        out.push_str(&lines[after..].concat());
    }
    let expected = rows.into_iter().filter_map(|r| r.dep).collect();
    verified(out, Some(expected))
}

/// Drop the `drop` entries from `rows`, then add or replace the `add` ones.
fn apply(rows: &mut Vec<Row>, drop: &[String], add: &[(String, DependencySpec)]) -> Result<()> {
    let names = || rows.iter().filter_map(Row::name).map(sort_key);
    let sorted = names().zip(names().skip(1)).all(|(a, b)| a <= b);
    rows.retain(|r| r.name().is_none_or(|n| !drop.iter().any(|d| d == n)));

    let indent = rows
        .iter()
        .find(|r| r.dep.is_some())
        .map_or_else(|| "  ".to_string(), |r| r.indent.clone());
    for (name, spec) in add {
        let Some(text) = dep_spec::format(name, spec) else {
            return Err(parse_err(format!(
                "the spec for `{name}` has no spelling a script header accepts"
            )));
        };
        let entry = toml::Value::String(text).to_string();
        let dep = Some((name.clone(), spec.clone()));

        let mut same = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.name() == Some(name));
        if let Some((first, _)) = same.next() {
            // Replaced where it stands; any duplicate of it goes.
            let later: Vec<usize> = same.map(|(i, _)| i).collect();
            for i in later.into_iter().rev() {
                rows.remove(i);
            }
            let row = &mut rows[first];
            if row.dep != dep {
                row.dep = dep;
                row.entry = entry;
                row.verbatim = None;
            }
            continue;
        }

        let key = sort_key(name);
        let last_entry = rows.iter().rposition(|r| r.dep.is_some());
        let mut index = last_entry.map_or(rows.len(), |i| i + 1);
        if sorted {
            if let Some(next) = rows
                .iter()
                .position(|r| r.name().is_some_and(|n| sort_key(n) > key))
            {
                // Before the entry it precedes and any comment lines above
                // it, which belong to that entry.
                index = next;
                while index > 0 && rows[index - 1].dep.is_none() {
                    index -= 1;
                }
            }
        }
        rows.insert(
            index,
            Row {
                dep,
                indent: indent.clone(),
                entry,
                rest: ",".to_string(),
                verbatim: None,
            },
        );
    }
    Ok(())
}

/// Append `rows`, giving every entry but the last a separating comma.
fn render_rows(out: &mut String, rows: &[Row], eol: &str) {
    let last_entry = rows.iter().rposition(|r| r.dep.is_some());
    for (i, row) in rows.iter().enumerate() {
        let needs_comma =
            row.dep.is_some() && Some(i) != last_entry && !row.rest.trim_start().starts_with(',');
        match row.verbatim {
            Some(line) if !needs_comma => out.push_str(line),
            _ => {
                let comma = if needs_comma { "," } else { "" };
                out.push_str(&body_line(
                    &format!("{}{}{comma}{}", row.indent, row.entry, row.rest),
                    eol,
                ));
            }
        }
    }
}

/// Return `out` only if [`parse`] reads back exactly the `expected`
/// dependencies (`None`: no header), so an edit can never leave a script
/// that `uvr run` refuses or reads differently.
fn verified(out: String, expected: Option<Vec<(String, DependencySpec)>>) -> Result<String> {
    let got = parse(&out)?.map(|h| h.dependencies);
    if got != expected {
        return Err(parse_err(
            "uvr could not rewrite this header so that it reads back as intended; \
             the file was left unchanged — please report this"
                .to_string(),
        ));
    }
    Ok(out)
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

    /// `uvr add`'s parse of each spec.
    fn specs(raw: &[&str]) -> Vec<(String, DependencySpec)> {
        raw.iter()
            .map(|s| dep_spec::parse(s, false).unwrap())
            .collect()
    }

    fn add(source: &str, raw: &[&str]) -> String {
        upsert(source, &specs(raw)).unwrap()
    }

    fn drop(source: &str, names: &[&str]) -> String {
        let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
        remove(source, &names).unwrap()
    }

    #[test]
    fn upsert_creates_a_header_above_the_code() {
        let out = add("library(jsonlite)\n", &["jsonlite", "cli"]);
        assert_eq!(
            out,
            "\
# /// script
# dependencies = [
#   \"cli\",
#   \"jsonlite\",
# ]
# ///

library(jsonlite)
"
        );
        assert_eq!(deps(&out), vec!["cli", "jsonlite"]);
    }

    #[test]
    fn upsert_creates_a_header_in_an_empty_file() {
        assert_eq!(
            add("", &["cli"]),
            "# /// script\n# dependencies = [\n#   \"cli\",\n# ]\n# ///\n"
        );
    }

    #[test]
    fn a_new_header_goes_after_a_shebang() {
        let source = "#!/usr/bin/env -S uvr run\n# Copyright someone\nprint(1)\n";
        let out = add(source, &["cli"]);
        assert!(
            out.starts_with("#!/usr/bin/env -S uvr run\n# /// script\n"),
            "{out}"
        );
        assert!(
            out.ends_with("# ///\n\n# Copyright someone\nprint(1)\n"),
            "{out}"
        );
        assert_eq!(deps(&out), vec!["cli"]);

        // A shebang with no line ending still gets its own line.
        let out = add("#!/usr/bin/env Rscript", &["cli"]);
        assert!(
            out.starts_with("#!/usr/bin/env Rscript\n# /// script\n"),
            "{out}"
        );
        assert_eq!(drop(&out, &["cli"]), "#!/usr/bin/env Rscript\n");
    }

    #[test]
    fn adding_then_removing_restores_a_headerless_file_exactly() {
        for source in [
            "library(cli)\n",
            "#!/usr/bin/env Rscript\n\nprint(1)",
            "x <- 1\r\ny <- 2\r\n",
            "\u{feff}print(1)\n",
        ] {
            let out = add(source, &["cli", "ggplot2>=3.4"]);
            assert_eq!(deps(&out), vec!["cli", "ggplot2"], "{source:?}");
            assert_eq!(drop(&out, &["ggplot2", "cli"]), source, "{source:?}");
        }
    }

    #[test]
    fn upsert_adds_one_line_to_an_existing_list() {
        let source = "\
# /// script
# dependencies = [
#   \"cli\",
#   \"jsonlite\",
# ]
# ///
print(1)
";
        assert_eq!(
            add(source, &["glue"]),
            "\
# /// script
# dependencies = [
#   \"cli\",
#   \"glue\",
#   \"jsonlite\",
# ]
# ///
print(1)
"
        );
    }

    #[test]
    fn repeated_edits_keep_a_stable_sorted_order() {
        // Whatever order packages arrive in, a list uvr maintains stays
        // sorted (ignoring case), and each edit changes only its own line.
        let mut source = String::from("print(1)\n");
        for pkg in ["zoo", "DESeq2 (bioc)", "cli", "ggplot2", "abind"] {
            let before: Vec<String> = source.lines().map(str::to_string).collect();
            source = add(&source, &[pkg]);
            let after: Vec<&str> = source.lines().collect();
            let changed = after
                .iter()
                .filter(|l| !before.contains(&l.to_string()))
                .count();
            assert!(changed <= 1 || before.len() == 1, "{pkg}: {source}");
        }
        assert_eq!(
            deps(&source),
            vec!["abind", "cli", "DESeq2", "ggplot2", "zoo"]
        );
        // Adding what is already there changes nothing.
        assert_eq!(add(&source, &["cli", "zoo"]), source);
    }

    #[test]
    fn a_hand_ordered_list_is_appended_to_not_reordered() {
        let source = "# /// script\n# dependencies = [\n#   \"zoo\",\n#   \"cli\",\n# ]\n# ///\n";
        assert_eq!(
            add(source, &["abind"]),
            "# /// script\n# dependencies = [\n#   \"zoo\",\n#   \"cli\",\n#   \"abind\",\n# ]\n# ///\n"
        );
    }

    #[test]
    fn upsert_replaces_an_existing_spec_in_place() {
        let source = "\
# /// script
# dependencies = [
#   \"cli\",
#   'ggplot2',  # plots
#   \"zoo\",
# ]
# ///
";
        let out = add(source, &["ggplot2@>=3.4"]);
        assert_eq!(
            out,
            "\
# /// script
# dependencies = [
#   \"cli\",
#   \"ggplot2>=3.4\",  # plots
#   \"zoo\",
# ]
# ///
"
        );
        // A duplicate entry collapses into the replacement.
        let dup = "# /// script\n# dependencies = [\n#   \"cli\",\n#   \"cli>=3\",\n# ]\n# ///\n";
        let out = add(dup, &["cli>=3.6"]);
        assert_eq!(
            out,
            "# /// script\n# dependencies = [\n#   \"cli>=3.6\",\n# ]\n# ///\n"
        );
    }

    #[test]
    fn remove_drops_only_the_named_lines() {
        let source = "\
# /// script
# dependencies = [
#   \"cli\",       # console
#   # plotting
#   \"ggplot2\",
#   \"zoo\"
# ]
# ///
";
        assert_eq!(
            drop(source, &["zoo"]),
            "\
# /// script
# dependencies = [
#   \"cli\",       # console
#   # plotting
#   \"ggplot2\",
# ]
# ///
"
        );
        assert_eq!(
            drop(source, &["cli"]),
            "\
# /// script
# dependencies = [
#   # plotting
#   \"ggplot2\",
#   \"zoo\"
# ]
# ///
"
        );
        // A name that is not there leaves the file alone.
        assert_eq!(drop(source, &["nope"]), source);
        assert_eq!(drop("print(1)\n", &["nope"]), "print(1)\n");
    }

    #[test]
    fn removing_the_last_entry_deletes_the_block() {
        let source =
            "#!/usr/bin/env Rscript\n# /// script\n# dependencies = [\"cli\"]\n# ///\n\nprint(1)\n";
        assert_eq!(drop(source, &["cli"]), "#!/usr/bin/env Rscript\nprint(1)\n");
        // No blank line to take: only the block goes.
        let source = "# /// script\n# dependencies = [\"cli\"]\n# ///\nprint(1)\n";
        assert_eq!(drop(source, &["cli"]), "print(1)\n");
    }

    #[test]
    fn other_keys_and_comments_survive_every_edit() {
        let source = "\
# Banner
# /// script
# r = \">=4.3\"
#
# # why these packages
# dependencies = [
#   \"cli\",
# ]  # trailing
# future-knob = { x = 1 }
#
# [tool.later]
# y = 2
# ///
print(1)
";
        let out = add(source, &["zoo"]);
        assert_eq!(
            out,
            source.replace("\"cli\",\n", "\"cli\",\n#   \"zoo\",\n")
        );
        let header = parse(&out).unwrap().unwrap();
        assert_eq!(header.r.as_deref(), Some(">=4.3"));

        // With a key left, emptying the list keeps the block.
        let out = drop(&out, &["cli", "zoo"]);
        assert_eq!(out, source.replace("[\n#   \"cli\",\n# ]", "[]"));
        assert_eq!(parse(&out).unwrap().unwrap().dependencies, vec![]);
        // And adding again fills it where it stands.
        assert_eq!(add(&out, &["cli"]), source);
    }

    #[test]
    fn a_header_without_a_dependencies_key_gains_one_in_the_root_table() {
        let source = "# /// script\n# r = \">=4.3\"\n# [tool.x]\n# y = 1\n# ///\n";
        let out = add(source, &["cli"]);
        assert_eq!(
            out,
            "# /// script\n# dependencies = [\n#   \"cli\",\n# ]\n# r = \">=4.3\"\n# [tool.x]\n# y = 1\n# ///\n"
        );
        assert_eq!(parse(&out).unwrap().unwrap().r.as_deref(), Some(">=4.3"));

        let out = add("# /// script\n# ///\n", &["cli"]);
        assert_eq!(
            out,
            "# /// script\n# dependencies = [\n#   \"cli\",\n# ]\n# ///\n"
        );
    }

    #[test]
    fn a_single_line_list_is_rewritten_one_entry_per_line() {
        let source = "# /// script\n# dependencies = ['zoo', \"cli\"] # keep\n# ///\n";
        assert_eq!(
            add(source, &["abind"]),
            "# /// script\n# dependencies = [\n#   'zoo',\n#   \"cli\",\n#   \"abind\",\n# ] # keep\n# ///\n"
        );
        let with_r = "# /// script\n# r = \"4.4\"\n# dependencies = [\"cli\"]\n# ///\n";
        assert_eq!(
            drop(with_r, &["cli"]),
            "# /// script\n# r = \"4.4\"\n# dependencies = []\n# ///\n"
        );
    }

    #[test]
    fn a_comment_that_cannot_be_kept_is_an_error_not_a_loss() {
        let source = "# /// script\n# dependencies = [\"zoo\", # why\n#   \"cli\"]\n# ///\n";
        let err = upsert(source, &specs(&["abind"])).unwrap_err().to_string();
        assert!(err.contains("comment"), "{err}");
    }

    #[test]
    fn crlf_files_stay_crlf() {
        let source = "#!/usr/bin/env Rscript\r\nprint(1)\r\n";
        let out = add(source, &["cli"]);
        assert_eq!(
            out,
            "#!/usr/bin/env Rscript\r\n# /// script\r\n# dependencies = [\r\n#   \"cli\",\r\n# ]\r\n# ///\r\n\r\nprint(1)\r\n"
        );
        let out = add(&out, &["abind"]);
        assert!(
            out.contains("# dependencies = [\r\n#   \"abind\",\r\n#   \"cli\",\r\n"),
            "{out:?}"
        );
        assert!(!out.replace("\r\n", "").contains('\n'), "{out:?}");
        assert_eq!(drop(&out, &["cli", "abind"]), source);
    }

    #[test]
    fn every_spec_kind_reads_back_as_written() {
        // What `uvr add --script` writes is what `uvr run` gets.
        let all = [
            ("ggplot2@>=3.4", false),
            ("jsonlite", false),
            ("DESeq2", true),
            ("limma@>=3.50", true),
            ("rladies/praise@v1.0.0", false),
            ("owner/repo#subdirectory=pkgs/inner", false),
            ("forgejo::codeberg.org/owner/fpkg@main", false),
            ("gitlab::gitlab.com/group/sub/gpkg", false),
        ];
        let wanted: Vec<_> = all
            .iter()
            .map(|(s, bioc)| dep_spec::parse(s, *bioc).unwrap())
            .collect();
        let out = upsert("print(1)\n", &wanted).unwrap();
        let mut got = parse(&out).unwrap().unwrap().dependencies;
        let mut wanted = wanted;
        got.sort_by(|a, b| a.0.cmp(&b.0));
        wanted.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(got, wanted);
        for written in [
            "\"ggplot2>=3.4\"",
            "\"DESeq2 (bioc)\"",
            "\"limma>=3.50 (bioc)\"",
            "\"rladies/praise@v1.0.0\"",
        ] {
            assert!(out.contains(written), "{written} missing:\n{out}");
        }
    }

    #[test]
    fn a_spec_the_header_cannot_hold_is_refused() {
        let (_, spec) = dep_spec::parse("nbafrank/uvr-r", false).unwrap();
        let err = upsert("", &[("uvr".to_string(), spec)])
            .unwrap_err()
            .to_string();
        assert!(err.contains("uvr"), "{err}");
    }

    #[test]
    fn a_broken_header_is_not_edited() {
        let source = "# /// script\n# dependencies = [\"cli\"]\n";
        assert!(upsert(source, &specs(&["zoo"])).is_err());
        assert!(remove(source, &["cli".to_string()]).is_err());
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
