use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use console::style;
use serde::Deserialize;

use uvr_core::manifest::{DependencySpec, DetailedDep, Manifest};
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

    // Check we're not overwriting an existing project
    if Path::new("uvr.toml").exists() {
        anyhow::bail!(
            "uvr.toml already exists. Remove it first or run from a different directory."
        );
    }

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

    // Determine project name from directory
    let project_name = std::env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_else(|| "imported-project".to_string());

    let mut manifest = Manifest::new(&project_name, r_version.clone());

    // Import packages — all become direct dependencies since renv.lock
    // doesn't distinguish direct vs transitive deps.
    let mut cran_count = 0;
    let mut bioc_count = 0;
    let mut github_count = 0;
    let mut skipped = Vec::new();

    for (name, pkg) in &renv_lock.packages {
        let spec = match pkg.source.as_str() {
            "Repository" => {
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

        manifest.add_dep(name.clone(), spec, false);
    }

    // Write uvr.toml and create project structure
    let cwd = std::env::current_dir().context("Cannot determine current directory")?;
    let manifest_path = cwd.join(MANIFEST_FILE);
    manifest.write(&manifest_path)?;

    // Create .uvr/library/
    let library_path = cwd.join(DOT_UVR_DIR).join(LIBRARY_DIR);
    std::fs::create_dir_all(&library_path).context("Failed to create .uvr/library/")?;

    println!(
        "{} Imported from {}",
        style("✓").green().bold(),
        style(renv_path.display()).cyan()
    );
    println!(
        "  {} CRAN, {} Bioconductor, {} GitHub package(s)",
        cran_count, bioc_count, github_count
    );
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
    #[serde(rename = "RemoteUsername")]
    remote_username: Option<String>,
    #[serde(rename = "RemoteRepo")]
    remote_repo: Option<String>,
    #[serde(rename = "RemoteRef")]
    remote_ref: Option<String>,
}
