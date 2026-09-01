use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ignore::WalkBuilder;
use regex::Regex;

use uvr_core::project::Project;
use uvr_core::resolver::is_base_package;

use crate::ui;
use crate::ui::palette;

/// Scan `.R`, `.Rmd`, and `.Qmd` files in the project for package usage and
/// report deps that aren't declared in `uvr.toml` (#82).
///
/// Honours `.gitignore` and `.uvrignore` via the `ignore` crate. Detects
/// the common ways an R script names a package: `library(pkg)`,
/// `require(pkg)`, `pkg::fn`, `pkg:::fn`, roxygen2 `@import` /
/// `@importFrom`, and `box::use(pkg)` declarations.
///
/// `--all` reports every package referenced regardless of manifest
/// presence; without it we only report the missing set, which is the
/// signal for "you need to `uvr add` these".
pub fn run(all: bool) -> Result<()> {
    let project = Project::find_cwd().context("Not inside a uvr project")?;
    let manifest_deps: BTreeSet<String> = project
        .manifest
        .dependencies
        .keys()
        .chain(project.manifest.dev_dependencies.keys())
        .cloned()
        .collect();

    let detector = PackageDetector::new();
    let cwd = std::env::current_dir().context("Cannot determine current directory")?;

    let mut found: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    let mut files_scanned = 0usize;

    // ignore::WalkBuilder honours .gitignore / .ignore by default; we
    // additionally register .uvrignore so users can scope a separate
    // exclusion list when their .gitignore is shared with non-R tooling.
    let mut walker = WalkBuilder::new(&cwd);
    walker.add_custom_ignore_filename(".uvrignore");
    walker.hidden(true);

    for entry in walker.build().filter_map(std::result::Result::ok) {
        let path = entry.path();
        if !is_scannable(path) {
            continue;
        }
        files_scanned += 1;
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        for pkg in detector.extract(&content) {
            if is_base_package(&pkg) {
                continue;
            }
            found
                .entry(pkg)
                .or_default()
                .push(path.strip_prefix(&cwd).unwrap_or(path).to_path_buf());
        }
    }

    let missing: BTreeMap<&str, &Vec<PathBuf>> = found
        .iter()
        .filter(|(name, _)| !manifest_deps.contains(name.as_str()))
        .map(|(n, p)| (n.as_str(), p))
        .collect();

    let to_report: Vec<(&str, &Vec<PathBuf>)> = if all {
        found.iter().map(|(n, p)| (n.as_str(), p)).collect()
    } else {
        missing.iter().map(|(n, p)| (*n, *p)).collect()
    };

    if to_report.is_empty() {
        if all {
            ui::info(format!(
                "No package references found in {files_scanned} file(s)."
            ));
        } else {
            ui::success(format!(
                "All package references in {files_scanned} file(s) are declared in uvr.toml."
            ));
        }
        return Ok(());
    }

    let header = if all {
        format!("References found in {files_scanned} file(s):")
    } else {
        format!(
            "Found {} package(s) used but not declared in uvr.toml:",
            to_report.len()
        )
    };
    ui::info(header);
    for (pkg, files) in &to_report {
        let in_manifest = manifest_deps.contains(*pkg);
        let marker = if in_manifest {
            palette::dim("(declared)")
        } else {
            palette::warn("(missing)")
        };
        let first_file = files
            .first()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let extra = if files.len() > 1 {
            format!(" (+{} more)", files.len() - 1)
        } else {
            String::new()
        };
        println!(
            "  {} {} {}{}",
            palette::pkg(pkg),
            marker,
            palette::dim(&first_file),
            palette::dim(&extra)
        );
    }
    if !all && !to_report.is_empty() {
        println!();
        ui::hint(format!(
            "Run {} to add them.",
            palette::bold(&format!(
                "uvr add {}",
                to_report
                    .iter()
                    .map(|(n, _)| *n)
                    .collect::<Vec<_>>()
                    .join(" ")
            )),
        ));
    }

    Ok(())
}

fn is_scannable(path: &Path) -> bool {
    path.is_file() && has_scannable_extension(path)
}

fn has_scannable_extension(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|s| s.to_str()),
        Some("R" | "r" | "Rmd" | "rmd" | "Qmd" | "qmd")
    )
}

/// Strip R `#` comments from source, preserving newlines so line structure
/// and the surrounding comma-separated structure survive for the box spec
/// regex. Used by the `box::use` pass; it does not model `#` inside string
/// literals, which is fine for the top-of-file/top-of-function statements
/// box uses.
fn strip_r_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '#' {
            for c in chars.by_ref() {
                if c == '\n' {
                    out.push('\n');
                    break;
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Compiled regexes for the R-package name patterns we recognise.
///
/// Patterns are intentionally conservative — we want few false positives
/// even at the cost of a few false negatives. Comment lines that contain
/// `library(pkg)` will still match (R doesn't have inline-comment escape
/// for code mid-string), but that's an acceptable trade-off for a tool
/// the user re-runs and visually scans.
struct PackageDetector {
    library_or_require: Regex,
    namespace_op: Regex,
    roxygen_import: Regex,
    box_use: Regex,
    box_use_spec: Regex,
}

impl PackageDetector {
    fn new() -> Self {
        // `library(pkg)` and `require(pkg)`. The package may be quoted
        // (`library("pkg")`) or bare (`library(pkg)`) — R accepts both
        // because `library()` uses NSE. `requireNamespace` and
        // `loadNamespace` use the same convention.
        let library_or_require = Regex::new(
            r#"\b(?:library|require|requireNamespace|loadNamespace)\s*\(\s*["']?([A-Za-z][A-Za-z0-9._]*)["']?"#,
        )
        .expect("library/require regex compiles");

        // `pkg::fn` / `pkg:::fn`. Word-boundary on the left avoids matching
        // inside identifiers; the package name follows R's allowed chars.
        let namespace_op =
            Regex::new(r"\b([A-Za-z][A-Za-z0-9._]*):{2,3}[A-Za-z._]").expect(":: regex compiles");

        // roxygen2 `#' @import pkg` and `#' @importFrom pkg fn1 fn2`. Real
        // package source trees often declare deps exclusively via these
        // tags — without this, `uvr scan` returns "no references found"
        // for any package that doesn't `library()` its own deps (post-
        // bundle review).
        let roxygen_import = Regex::new(r"#'\s*@import(?:From)?\s+([A-Za-z][A-Za-z0-9._]*)")
            .expect("roxygen import regex compiles");

        // `box::use(...)` imports packages and local modules. The scan
        // runs over comment-stripped source (see `extract`), so a simple
        // `[^)]*` capture suffices: a `)` inside a trailing `# comment (...)`
        // is already gone by the time this regex runs.
        let box_use =
            Regex::new(r#"\bbox\s*::\s*use\s*\(([^)]*)\)"#).expect("box::use regex compiles");

        // One import declaration inside a `box::use(...)` list. Captures
        // the raw package/module name (`pkg`, `alias = pkg`, `prefix/mod`,
        // `./mod`, `../mod`). The trailing `\[[^\]]*\]` consumes an attach
        // list so names inside it (`dplyr[filter, mutate]`) are never seen
        // as packages. Callers drop captures containing `/` (module paths)
        // and the `.`/`..` placeholders.
        let box_use_spec = Regex::new(
            r#"(?:^|,)\s*(?:[A-Za-z][A-Za-z0-9._]*\s*=\s*)?((?:[A-Za-z][A-Za-z0-9._]*|\.{1,2})(?:/[A-Za-z][A-Za-z0-9._]*)*)\s*(?:\[[^\]]*\])?"#,
        )
        .expect("box::use spec regex compiles");

        Self {
            library_or_require,
            namespace_op,
            roxygen_import,
            box_use,
            box_use_spec,
        }
    }

    fn extract(&self, content: &str) -> BTreeSet<String> {
        let mut found = BTreeSet::new();
        for cap in self.library_or_require.captures_iter(content) {
            if let Some(name) = cap.get(1) {
                found.insert(name.as_str().to_string());
            }
        }
        for cap in self.namespace_op.captures_iter(content) {
            if let Some(name) = cap.get(1) {
                found.insert(name.as_str().to_string());
            }
        }
        for cap in self.roxygen_import.captures_iter(content) {
            if let Some(name) = cap.get(1) {
                found.insert(name.as_str().to_string());
            }
        }
        // Scan `box::use` on comment-stripped source so a commented-out
        // import block can't sweep the live code after it into the
        // argument list. The other detectors keep running on raw content
        // (roxygen needs `#'`, and `library`/`::` in comments is an
        // accepted trade-off).
        let decommented = strip_r_comments(content);
        for cap in self.box_use.captures_iter(&decommented) {
            if let Some(args) = cap.get(1) {
                for spec in self.box_use_spec.captures_iter(args.as_str()) {
                    if let Some(name) = spec.get(1) {
                        let name = name.as_str();
                        if name.contains('/') || name == "." || name == ".." {
                            continue;
                        }
                        found.insert(name.to_string());
                    }
                }
            }
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_library_calls() {
        let detector = PackageDetector::new();
        let src = r#"
library(ggplot2)
library("dplyr")
require(tidyr)
require('data.table')
requireNamespace("rlang")
loadNamespace(stats)
"#;
        let found = detector.extract(src);
        assert!(found.contains("ggplot2"));
        assert!(found.contains("dplyr"));
        assert!(found.contains("tidyr"));
        assert!(found.contains("data.table"));
        assert!(found.contains("rlang"));
        assert!(found.contains("stats"));
    }

    #[test]
    fn extract_namespace_operators() {
        let detector = PackageDetector::new();
        let src = r#"
result <- jsonlite::fromJSON(x)
internal <- tools:::file_ext(p)
mixed <- dplyr::filter(df) |> tidyr::pivot_longer()
"#;
        let found = detector.extract(src);
        assert!(found.contains("jsonlite"));
        assert!(found.contains("tools"));
        assert!(found.contains("dplyr"));
        assert!(found.contains("tidyr"));
    }

    #[test]
    fn ignores_non_pkg_double_colons() {
        // `::` between non-identifier chars (literal scopes in some
        // packages, comments) shouldn't trip the regex.
        let detector = PackageDetector::new();
        let src = "# some comment with :: in it\nx <- 1::5\n"; // R doesn't parse `1::5` as a pkg ref
        let found = detector.extract(src);
        assert!(found.is_empty(), "got {found:?}");
    }

    #[test]
    fn extract_roxygen_imports() {
        // Package source trees often declare deps purely via roxygen2
        // tags, never calling library() in their own .R files. Without
        // this regex, `uvr scan` reports nothing for those packages.
        let detector = PackageDetector::new();
        let src = "\
#' @importFrom dplyr filter mutate
#' @importFrom rlang .data
#' @import ggplot2
#' @importFrom stats predict
my_fn <- function() NULL
";
        let found = detector.extract(src);
        assert!(found.contains("dplyr"), "got {found:?}");
        assert!(found.contains("rlang"), "got {found:?}");
        assert!(found.contains("ggplot2"), "got {found:?}");
        assert!(found.contains("stats"), "got {found:?}");
        // The function names after `@importFrom <pkg>` must NOT be
        // captured as packages.
        assert!(!found.contains("filter"), "got {found:?}");
        assert!(!found.contains("mutate"), "got {found:?}");
        assert!(!found.contains("predict"), "got {found:?}");
    }

    #[test]
    fn extract_box_use_imports() {
        let detector = PackageDetector::new();
        let src = r#"
# import statements can be written in a single line or broken:
box::use(rlang, dplyr[filter], gg = ggplot2, r/module,)

box::use(
  rlang,                   # whole rlang package
  dplyr[filter, mutate,],  # filter and mutate from dplyr package, trailing commas.
  dplyr[a = arrange,],     # aliased function from dplyr package.
  gg = ggplot2,            # ggplot2 package, aliased.
  r/module,                # a local module, should be ignored (not an R package).
  ../mod/utils[f,],        # a relative local module, also ignored.
)
"#;
        let found = detector.extract(src);
        assert!(found.contains("rlang"), "got {found:?}");
        assert!(found.contains("dplyr"), "got {found:?}");
        assert!(found.contains("ggplot2"), "got {found:?}");
        // Alias LHS, attach-list names and local module paths must not be
        // reported as packages.
        assert!(!found.contains("gg"), "got {found:?}");
        assert!(!found.contains("filter"), "got {found:?}");
        assert!(!found.contains("mutate"), "got {found:?}");
        assert!(!found.contains("arrange"), "got {found:?}");
        assert!(!found.contains("module"), "got {found:?}");
        assert!(!found.contains("utils"), "got {found:?}");
    }

    #[test]
    fn extract_box_use_single_and_wildcard() {
        let detector = PackageDetector::new();
        let src = "box::use(purrr, tbl = tibble, stats[st_filter = filter, ...])";
        let found = detector.extract(src);
        assert!(found.contains("purrr"), "got {found:?}");
        assert!(found.contains("tibble"), "got {found:?}");
        assert!(found.contains("stats"), "got {found:?}");
        assert!(!found.contains("tbl"), "got {found:?}");
        assert!(!found.contains("st_filter"), "got {found:?}");
        assert!(!found.contains("filter"), "got {found:?}");
    }

    #[test]
    fn extract_box_use_ignores_comment_parens() {
        // A `)` inside a trailing comment must not truncate the argument
        // list; both packages should still be found.
        let detector = PackageDetector::new();
        let src = "box::use(pkg, # comment (not a package)\n pkg2)";
        let found = detector.extract(src);
        assert!(found.contains("pkg"), "got {found:?}");
        assert!(found.contains("pkg2"), "got {found:?}");
    }

    #[test]
    fn extract_box_use_ignores_commented_out_block() {
        // A whole `box::use` block that is commented out must not leak its
        // package names, nor sweep the live code after it into the
        // argument list. (`box` itself still appears via the namespace
        // operator detector, which runs on raw source.)
        let detector = PackageDetector::new();
        let src = "# box::use(dplyr,\n#   tidyr)\nresult <- transform(df)\n";
        let found = detector.extract(src);
        assert!(!found.contains("dplyr"), "got {found:?}");
        assert!(!found.contains("tidyr"), "got {found:?}");
        assert!(!found.contains("result"), "got {found:?}");
    }

    #[test]
    fn extract_box_use_indented_function_body() {
        // `box::use` is valid inside a function body, so leading
        // indentation must not stop detection.
        let detector = PackageDetector::new();
        let src = "f <- function() {\n  box::use(dplyr[filter],\n    tidyr)\n}\n";
        let found = detector.extract(src);
        assert!(found.contains("dplyr"), "got {found:?}");
        assert!(found.contains("tidyr"), "got {found:?}");
        assert!(!found.contains("filter"), "got {found:?}");
    }

    #[test]
    fn extension_matching() {
        assert!(has_scannable_extension(Path::new("script.R")));
        assert!(has_scannable_extension(Path::new("script.r")));
        assert!(has_scannable_extension(Path::new("doc.Rmd")));
        assert!(has_scannable_extension(Path::new("doc.qmd")));
        assert!(!has_scannable_extension(Path::new("not_r.txt")));
        assert!(!has_scannable_extension(Path::new("uvr.toml")));
        assert!(!has_scannable_extension(Path::new("noext")));
    }
}
