use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use console::style;
use serde::Deserialize;

use uvr_core::manifest::{DependencySpec, DetailedDep, Manifest, PackageSource};
use uvr_core::project::{Project, DOT_UVR_DIR, LIBRARY_DIR, MANIFEST_FILE};

pub async fn run(path: Option<String>, lock: bool, jobs: usize) -> Result<()> {
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
        existing
            .parse::<Manifest>()
            .context("Failed to parse existing uvr.toml")?
    } else {
        let project_name = cwd
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "imported-project".to_string());
        Manifest::new(&project_name, r_version.clone())
    };

    // Import packages — all become direct dependencies since renv.lock
    // doesn't distinguish direct vs transitive deps.
    let mut cran_count = 0;
    let mut bioc_count = 0;
    let mut github_count = 0;
    let mut skipped = Vec::new();

    let mut custom_sources: Vec<PackageSource> = Vec::new();

    for (name, pkg) in &renv_lock.packages {
        let spec = match pkg.source.as_str() {
            "Repository" => {
                // Check if this is a non-CRAN repository
                if let Some(ref repo_url) = pkg.repository {
                    let is_cran = repo_url.eq_ignore_ascii_case("CRAN")
                        || repo_url.contains("cran.r-project.org")
                        || repo_url.contains("cran.rstudio.com")
                        || repo_url.contains("packagemanager.posit.co")
                        || repo_url.contains("packagemanager.rstudio.com");
                    if !is_cran {
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
                cran_count += 1;
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
                DependencySpec::Detailed(DetailedDep {
                    git,
                    rev: pkg.remote_ref.clone(),
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

    if merge_mode {
        println!(
            "{} Merged from {} into existing uvr.toml",
            style("✓").green().bold(),
            style(renv_path.display()).cyan()
        );
    } else {
        println!(
            "{} Imported from {}",
            style("✓").green().bold(),
            style(renv_path.display()).cyan()
        );
    }
    println!(
        "  {} CRAN, {} Bioconductor, {} GitHub package(s)",
        cran_count, bioc_count, github_count
    );
    if !custom_sources.is_empty() {
        let names: Vec<_> = custom_sources.iter().map(|s| s.url.as_str()).collect();
        println!(
            "  {} Added {} custom source(s): {}",
            style("+").green().bold(),
            custom_sources.len(),
            names.join(", ")
        );
    }
    if !skipped.is_empty() {
        println!(
            "  {} Skipped {} package(s): {}",
            style("!").yellow().bold(),
            skipped.len(),
            skipped.join(", ")
        );
    }

    if let Some(ref ver) = r_version {
        println!("  R version: {ver}");
    }

    // Optionally resolve and lock
    if lock {
        println!("\n{} Resolving dependencies...", style("→").blue().bold());
        let project = Project::find_cwd().context("Failed to load imported project")?;
        let lockfile = crate::commands::lock::resolve_and_lock(&project, false).await?;
        crate::commands::sync::install_from_lockfile(&project, &lockfile, jobs).await?;
    } else {
        println!(
            "\n  Run {} to resolve and install packages",
            style("uvr lock && uvr sync").cyan()
        );
    }

    Ok(())
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
}
