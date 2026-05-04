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
/// the four common ways an R script names a package:
/// `library(pkg)`, `require(pkg)`, `pkg::fn`, `pkg:::fn`.
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

/// Compiled regexes for the four R-package name patterns.
///
/// Patterns are intentionally conservative — we want few false positives
/// even at the cost of a few false negatives. Comment lines that contain
/// `library(pkg)` will still match (R doesn't have inline-comment escape
/// for code mid-string), but that's an acceptable trade-off for a tool
/// the user re-runs and visually scans.
struct PackageDetector {
    library_or_require: Regex,
    namespace_op: Regex,
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

        Self {
            library_or_require,
            namespace_op,
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
