use anyhow::{Context, Result};
use console::style;

use uvr_core::project::Project;

use crate::commands::lock::resolve_and_lock;
use crate::commands::sync::install_from_lockfile;

pub async fn run(packages: Vec<String>, dry_run: bool, jobs: usize) -> Result<()> {
    let project = Project::find_cwd().context("Not inside a uvr project")?;

    // Load the current lockfile to compare versions after re-resolution.
    let old_lockfile = project.load_lockfile().context("Failed to read uvr.lock")?;
    let old_versions: std::collections::HashMap<String, String> = old_lockfile
        .as_ref()
        .map(|lf| {
            lf.packages
                .iter()
                .map(|p| (p.name.clone(), p.version.clone()))
                .collect()
        })
        .unwrap_or_default();

    if packages.is_empty() {
        // Update all: full re-resolve with upgrade=true
        println!("{} Updating all packages...", style("→").blue().bold());
    } else {
        println!(
            "{} Updating {}...",
            style("→").blue().bold(),
            packages
                .iter()
                .map(|p| style(p).cyan().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );

        // Verify requested packages are actually dependencies
        for pkg in &packages {
            if !project.manifest.dependencies.contains_key(pkg)
                && !project.manifest.dev_dependencies.contains_key(pkg)
            {
                anyhow::bail!(
                    "Package '{}' is not in the manifest. Use `uvr add {0}` to add it.",
                    pkg
                );
            }
        }
    }

    // Re-resolve with upgrade=true (fetches fresh index, ignores locked versions).
    let new_lockfile = resolve_and_lock(&project, true).await?;

    // Compute diff
    let mut updated = Vec::new();
    let mut added = Vec::new();
    for pkg in &new_lockfile.packages {
        let dominated = !packages.is_empty() && !packages.contains(&pkg.name);
        if dominated {
            continue;
        }
        match old_versions.get(&pkg.name) {
            Some(old_ver) if old_ver != &pkg.version => {
                updated.push((&pkg.name, old_ver.as_str(), pkg.version.as_str()));
            }
            None => {
                added.push((&pkg.name, pkg.version.as_str()));
            }
            _ => {}
        }
    }

    // Report changes
    if updated.is_empty() && added.is_empty() {
        println!(
            "{} All packages already at latest versions",
            style("✓").green().bold()
        );
    } else {
        for (name, old, new) in &updated {
            println!(
                "  {} {} → {}",
                style(name).cyan(),
                style(old).dim(),
                style(new).green()
            );
        }
        for (name, ver) in &added {
            println!(
                "  {} {} {}",
                style("+").green(),
                style(name).cyan(),
                style(ver).dim()
            );
        }
        println!(
            "{} {} package(s) updated",
            style("✓").green().bold(),
            updated.len() + added.len()
        );
    }

    if dry_run {
        println!(
            "\n{} Dry run — lockfile updated but packages not installed",
            style("!").yellow().bold()
        );
    } else {
        // Install updated packages
        install_from_lockfile(&project, &new_lockfile, jobs).await?;
    }

    Ok(())
}
