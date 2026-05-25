use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

/// Validate a `--name <NAME>` value for `uvr import` (#77 follow-up to
/// the v0.3.4-batch review). CRAN's R-package convention: starts with a
/// letter, followed by letters / digits / dots only, length ≥ 2.
/// Rejecting up-front prevents silent corruption of `[project] name` in
/// uvr.toml when the user passes whitespace or TOML-special chars.
fn validate_project_name(s: &str) -> Result<()> {
    if s.len() < 2 {
        anyhow::bail!(
            "Invalid project name {:?}: must be at least 2 characters",
            s
        );
    }
    let mut chars = s.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphabetic() {
        anyhow::bail!(
            "Invalid project name {:?}: must start with an ASCII letter",
            s
        );
    }
    for c in chars {
        if !(c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-') {
            anyhow::bail!(
                "Invalid project name {:?}: only letters, digits, '.', '_', '-' allowed (offending char: {:?})",
                s,
                c
            );
        }
    }
    Ok(())
}

use uvr_core::manifest::{DependencySpec, DetailedDep, Manifest, PackageSource};
use uvr_core::project::{Project, DOT_UVR_DIR, LIBRARY_DIR, MANIFEST_FILE};
use uvr_core::r_version::detector::{find_r_binary, query_r_version};

use crate::commands::init;
use crate::ui;
use crate::ui::palette;

pub async fn run(
    path: Option<String>,
    name: Option<String>,
    lock: bool,
    jobs: usize,
    clean_renv: bool,
) -> Result<()> {
    // Validate `--name` before we touch any files. R package / project
    // names follow CRAN's rule: start with a letter, then letters /
    // digits / dots only (≥ 2 chars total). Empty or whitespace-laden
    // names slip silently into `[project]` name and corrupt downstream
    // TOML consumers — reject loudly.
    if let Some(n) = name.as_deref() {
        validate_project_name(n)?;
    }
    // Find the renv.lock file
    let renv_path = path.unwrap_or_else(|| "renv.lock".to_string());
    let renv_path = Path::new(&renv_path);

    if !renv_path.exists() {
        anyhow::bail!(
            "File not found: {}. Specify the path with `uvr import <path>`",
            renv_path.display()
        );
    }

    // Load existing manifest if present, otherwise create a new one
    let merge_mode = Path::new("uvr.toml").exists();
    let content = std::fs::read_to_string(renv_path)
        .with_context(|| format!("Failed to read {}", renv_path.display()))?;
    let renv_lock: RenvLock =
        serde_json::from_str(&content).context("Failed to parse renv.lock as JSON")?;

    // Extract R version
    let r_version = if renv_lock.r.version.is_empty() {
        None
    } else {
        Some(renv_lock.r.version.clone())
    };

    let cwd = std::env::current_dir().context("Cannot determine current directory")?;

    let mut manifest = if merge_mode {
        let manifest_path = cwd.join(MANIFEST_FILE);
        let existing =
            std::fs::read_to_string(&manifest_path).context("Failed to read existing uvr.toml")?;
        let mut m = existing
            .parse::<Manifest>()
            .context("Failed to parse existing uvr.toml")?;
        // #77 — `--name` overrides the existing manifest's project name
        // when merging. Without this, `uvr import --name foo` against a
        // pre-existing uvr.toml silently kept the old name.
        if let Some(n) = &name {
            m.project.name = n.clone();
        }
        m
    } else {
        // Project name: explicit `--name` (#77) wins over the cwd basename.
        let project_name = name.clone().unwrap_or_else(|| {
            cwd.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "imported-project".to_string())
        });
        Manifest::new(&project_name, r_version.clone())
    };

    // Import packages — all become direct dependencies since renv.lock
    // doesn't distinguish direct vs transitive deps.
    let mut cran_count = 0;
    let mut custom_count = 0;
    let mut bioc_count = 0;
    let mut github_count = 0;
    let mut skipped = Vec::new();

    let mut custom_sources: Vec<PackageSource> = Vec::new();

    for (name, pkg) in &renv_lock.packages {
        let spec = match pkg.source.as_str() {
            "Repository" => {
                // Check if this is a non-CRAN repository
                let mut is_custom = false;
                if let Some(ref repo_url) = pkg.repository {
                    let is_cran = repo_url.eq_ignore_ascii_case("CRAN")
                        || repo_url.eq_ignore_ascii_case("RSPM")
                        || repo_url.contains("cran.r-project.org")
                        || repo_url.contains("cran.rstudio.com")
                        || repo_url.contains("packagemanager.posit.co")
                        || repo_url.contains("packagemanager.rstudio.com");
                    if !is_cran {
                        is_custom = true;
                        // Extract hostname as source name (e.g. "https://rpolars.r-universe.dev" -> "rpolars.r-universe.dev")
                        let source_name = repo_url
                            .strip_prefix("https://")
                            .or_else(|| repo_url.strip_prefix("http://"))
                            .and_then(|s| s.split('/').next())
                            .unwrap_or(repo_url)
                            .to_string();
                        // Add to custom sources if not already present
                        if !custom_sources.iter().any(|s| s.url == *repo_url)
                            && !manifest.sources.iter().any(|s| s.url == *repo_url)
                        {
                            custom_sources.push(PackageSource {
                                name: source_name,
                                url: repo_url.clone(),
                            });
                        }
                    }
                }
                if is_custom {
                    custom_count += 1;
                } else {
                    cran_count += 1;
                }
                DependencySpec::Version("*".to_string())
            }
            "Bioconductor" => {
                bioc_count += 1;
                DependencySpec::Detailed(DetailedDep {
                    bioc: Some(true),
                    ..Default::default()
                })
            }
            "GitHub" => {
                github_count += 1;
                let git = match (&pkg.remote_username, &pkg.remote_repo) {
                    (Some(user), Some(repo)) => Some(format!("{user}/{repo}")),
                    _ => None,
                };
                if git.is_none() {
                    skipped.push(name.clone());
                    continue;
                }
                // Prefer RemoteSha (exact commit) over RemoteRef (branch name)
                // to preserve the exact pin from renv.lock.
                let rev = pkg
                    .remote_sha
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .or_else(|| pkg.remote_ref.clone());
                DependencySpec::Detailed(DetailedDep {
                    git,
                    rev,
                    ..Default::default()
                })
            }
            other => {
                skipped.push(format!("{name} (source: {other})"));
                continue;
            }
        };

        // In merge mode, don't overwrite deps the user already configured
        if merge_mode && manifest.dependencies.contains_key(name) {
            continue;
        }
        manifest.add_dep(name.clone(), spec, false);
    }

    // Add discovered custom sources to manifest
    if !custom_sources.is_empty() {
        manifest.sources.extend(custom_sources.clone());
    }

    // Write uvr.toml and create project structure
    let manifest_path = cwd.join(MANIFEST_FILE);
    manifest.write(&manifest_path)?;

    // Create .uvr/library/
    let library_path = cwd.join(DOT_UVR_DIR).join(LIBRARY_DIR);
    std::fs::create_dir_all(&library_path).context("Failed to create .uvr/library/")?;

    // Mirror the scaffolding `uvr init` writes — without these, a user
    // migrating from renv ends up with `uvr.toml` but no `.Rprofile`
    // block (so R startup never sets `.libPaths()` to `.uvr/library/`),
    // no `.gitignore` entry, and no Positron config. Functions are
    // idempotent so it's safe to call them in merge mode too.
    init::write_gitignore(&cwd).context("Failed to write .gitignore")?;
    if init::is_r_package_dir(&cwd) {
        init::write_rbuildignore(&cwd).context("Failed to write .Rbuildignore")?;
    }
    init::ensure_rprofile(&cwd).context("Failed to write .Rprofile")?;
    init::ensure_positron_settings(&cwd).context("Failed to write Positron settings")?;
    if let Ok(r_binary) = find_r_binary(manifest.project.r_version.as_deref()) {
        if let Some(r_ver) = query_r_version(&r_binary) {
            crate::commands::sync::ensure_companion_package(&library_path, &r_ver, &r_binary);
        }
    }

    // Handle leftover renv plumbing. The renv project layout puts a
    // `source("renv/activate.R")` hook into `.Rprofile`, which resets
    // `.libPaths()` to `renv/library/...` at every R startup —
    // completely bypassing uvr's library. Without cleanup the user
    // ends up with two side-by-side environments and the conflict
    // message renv prints when both are present.
    let renv_status = detect_renv_leftovers(&cwd);
    if renv_status.has_leftovers() {
        if clean_renv {
            clean_renv_leftovers(&cwd, &renv_status)?;
        } else {
            warn_renv_leftovers(&renv_status);
        }
    }

    if merge_mode {
        ui::success(format!(
            "Merged from {} into existing uvr.toml",
            palette::pkg(renv_path.display().to_string()),
        ));
    } else {
        ui::success(format!(
            "Imported from {}",
            palette::pkg(renv_path.display().to_string()),
        ));
    }
    let mut counts = format!("{cran_count} CRAN");
    if custom_count > 0 {
        counts.push_str(&format!(", {custom_count} custom"));
    }
    if bioc_count > 0 {
        counts.push_str(&format!(", {bioc_count} Bioconductor"));
    }
    if github_count > 0 {
        counts.push_str(&format!(", {github_count} GitHub"));
    }
    ui::bullet_dim(format!("{counts} package(s)"));
    if !custom_sources.is_empty() {
        let names: Vec<_> = custom_sources.iter().map(|s| s.url.as_str()).collect();
        println!(
            "  {} Added {} custom source(s): {}",
            palette::added(ui::glyph::add()),
            custom_sources.len(),
            names.join(", ")
        );
    }
    if !skipped.is_empty() {
        println!(
            "  {} Skipped {} package(s): {}",
            palette::warn(ui::glyph::warn()),
            skipped.len(),
            skipped.join(", ")
        );
    }

    if let Some(ref ver) = r_version {
        ui::bullet_dim(format!("R version: {ver}"));
    }

    if lock {
        println!();
        ui::info("Resolving dependencies");
        let project = Project::find_cwd().context("Failed to load imported project")?;
        let lockfile = crate::commands::lock::resolve_and_lock(&project, false).await?;
        crate::commands::sync::install_from_lockfile(&project, &lockfile, jobs, None, None).await?;
    } else {
        println!();
        ui::hint(format!(
            "Run {} to resolve and install packages",
            palette::bold("uvr lock && uvr sync"),
        ));
    }

    Ok(())
}

// ─── renv leftover detection + cleanup ─────────────────────────────

/// Snapshot of which renv artifacts are still present in the project
/// directory after import. Drives both the warn-only and `--clean-renv`
/// code paths.
#[derive(Debug, Default)]
struct RenvLeftovers {
    /// `<dir>/renv` exists.
    renv_dir: bool,
    /// `<dir>/.Rprofile` contains a line referencing `renv/activate.R`.
    /// Captures the line for the cleanup message.
    activate_hook_in_rprofile: bool,
}

impl RenvLeftovers {
    fn has_leftovers(&self) -> bool {
        self.renv_dir || self.activate_hook_in_rprofile
    }
}

fn detect_renv_leftovers(dir: &Path) -> RenvLeftovers {
    let mut out = RenvLeftovers::default();
    if dir.join("renv").is_dir() {
        out.renv_dir = true;
    }
    let rprofile = dir.join(".Rprofile");
    if let Ok(content) = std::fs::read_to_string(&rprofile) {
        if rprofile_has_renv_hook(&content) {
            out.activate_hook_in_rprofile = true;
        }
    }
    out
}

/// True if any non-comment line references `renv/activate.R`. Tolerant
/// of single vs double quotes and of `if (file.exists(...)) source(...)`
/// wrappers — all forms renv has shipped in the wild.
fn rprofile_has_renv_hook(content: &str) -> bool {
    content.lines().any(|line| {
        let trimmed = line.trim();
        !trimmed.starts_with('#') && trimmed.contains("renv/activate.R")
    })
}

fn warn_renv_leftovers(status: &RenvLeftovers) {
    let mut body: Vec<String> = Vec::new();
    if status.activate_hook_in_rprofile {
        body.push(
            "`.Rprofile` still sources `renv/activate.R`. R startup will reset .libPaths()"
                .to_string(),
        );
        body.push("  to renv/library/... and never see .uvr/library/.".to_string());
    }
    if status.renv_dir {
        body.push("`renv/` directory still present (will be ignored once the hook is gone, but takes disk space).".to_string());
    }
    body.push(String::new());
    body.push(
        "Re-run `uvr import --clean-renv` to remove these, or clean up manually:".to_string(),
    );
    if status.activate_hook_in_rprofile {
        body.push("  sed -i '/renv\\/activate.R/d' .Rprofile".to_string());
    }
    if status.renv_dir {
        body.push("  rm -rf renv/".to_string());
    }
    ui::warn_block("Leftover renv plumbing detected", body);
}

fn clean_renv_leftovers(dir: &Path, status: &RenvLeftovers) -> Result<()> {
    if status.activate_hook_in_rprofile {
        let rprofile = dir.join(".Rprofile");
        let content = std::fs::read_to_string(&rprofile)
            .with_context(|| format!("Failed to read {}", rprofile.display()))?;
        let stripped = strip_renv_hook(&content);
        // If the file becomes empty (only whitespace) after stripping,
        // delete it — leaving an empty .Rprofile means R prints
        // "empty .Rprofile sourced" on every start, which is noise.
        if stripped.trim().is_empty() {
            std::fs::remove_file(&rprofile)
                .with_context(|| format!("Failed to remove empty {}", rprofile.display()))?;
        } else {
            std::fs::write(&rprofile, stripped)
                .with_context(|| format!("Failed to write {}", rprofile.display()))?;
        }
        ui::bullet_dim("Stripped renv hook from .Rprofile");
    }
    if status.renv_dir {
        let renv_path = dir.join("renv");
        std::fs::remove_dir_all(&renv_path)
            .with_context(|| format!("Failed to remove {}", renv_path.display()))?;
        ui::bullet_dim("Removed renv/ directory");
    }
    Ok(())
}

/// Remove every non-comment line that references `renv/activate.R`.
/// Preserves all other content (the uvr-managed block included).
fn strip_renv_hook(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    for line in content.split_inclusive('\n') {
        let trimmed = line.trim();
        if !trimmed.starts_with('#') && trimmed.contains("renv/activate.R") {
            continue;
        }
        out.push_str(line);
    }
    out
}

// ─── renv.lock JSON types ───────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct RenvLock {
    #[serde(rename = "R")]
    r: RenvR,
    #[serde(rename = "Packages", default)]
    packages: HashMap<String, RenvPackage>,
}

#[derive(Debug, Deserialize)]
struct RenvR {
    #[serde(rename = "Version", default)]
    version: String,
}

#[derive(Debug, Deserialize)]
struct RenvPackage {
    #[serde(rename = "Source", default)]
    source: String,
    #[serde(rename = "Repository")]
    repository: Option<String>,
    #[serde(rename = "RemoteUsername")]
    remote_username: Option<String>,
    #[serde(rename = "RemoteRepo")]
    remote_repo: Option<String>,
    #[serde(rename = "RemoteRef")]
    remote_ref: Option<String>,
    #[serde(rename = "RemoteSha")]
    remote_sha: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{rprofile_has_renv_hook, strip_renv_hook, validate_project_name};

    #[test]
    fn validate_project_name_accepts_cran_style() {
        validate_project_name("ggplot2").unwrap();
        validate_project_name("data.table").unwrap();
        validate_project_name("Hmisc").unwrap();
        validate_project_name("a_b").unwrap();
        validate_project_name("my-project").unwrap();
        validate_project_name("R6").unwrap();
    }

    #[test]
    fn validate_project_name_rejects_invalid() {
        // Empty / whitespace
        assert!(validate_project_name("").is_err());
        assert!(validate_project_name(" ").is_err());
        assert!(validate_project_name("a").is_err()); // too short
        assert!(validate_project_name("foo bar").is_err()); // space mid
                                                            // Leading non-letter
        assert!(validate_project_name("1foo").is_err());
        assert!(validate_project_name(".foo").is_err());
        assert!(validate_project_name("_foo").is_err());
        // TOML-special / shell-special chars
        assert!(validate_project_name("foo=bar").is_err());
        assert!(validate_project_name("foo\"bar").is_err());
        assert!(validate_project_name("foo\nbar").is_err());
    }

    #[test]
    fn rprofile_hook_detected_in_common_forms() {
        assert!(rprofile_has_renv_hook("source(\"renv/activate.R\")\n"));
        assert!(rprofile_has_renv_hook("source('renv/activate.R')\n"));
        assert!(rprofile_has_renv_hook(
            "if (file.exists(\"renv/activate.R\")) source(\"renv/activate.R\")\n"
        ));
    }

    #[test]
    fn rprofile_hook_ignores_commented_lines() {
        assert!(!rprofile_has_renv_hook("# source(\"renv/activate.R\")\n"));
        assert!(!rprofile_has_renv_hook(
            "options(foo = 1)\n# renv/activate.R legacy hook\n"
        ));
    }

    #[test]
    fn strip_hook_preserves_other_lines() {
        let input = "options(foo = 1)\nsource(\"renv/activate.R\")\noptions(bar = 2)\n";
        let out = strip_renv_hook(input);
        assert_eq!(out, "options(foo = 1)\noptions(bar = 2)\n");
    }

    #[test]
    fn strip_hook_handles_wrapped_form() {
        let input =
            "if (file.exists(\"renv/activate.R\")) source(\"renv/activate.R\")\noptions(foo = 1)\n";
        let out = strip_renv_hook(input);
        assert_eq!(out, "options(foo = 1)\n");
    }

    #[test]
    fn strip_hook_preserves_uvr_block() {
        let input = "source(\"renv/activate.R\")\n# >>> uvr >>>\nlocal({})\n# <<< uvr <<<\n";
        let out = strip_renv_hook(input);
        assert!(!out.contains("renv/activate.R"));
        assert!(out.contains("# >>> uvr >>>"));
        assert!(out.contains("# <<< uvr <<<"));
    }

    #[test]
    fn strip_hook_can_leave_file_empty() {
        let input = "source(\"renv/activate.R\")\n";
        let out = strip_renv_hook(input);
        assert!(out.trim().is_empty());
    }
}
