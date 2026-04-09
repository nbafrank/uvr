use anyhow::{Context, Result};
use console::style;

use uvr_core::lockfile::Lockfile;
use uvr_core::project::Project;

use crate::commands::lock::{resolve_and_lock, resolve_only_upgraded};
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
    // For --dry-run or selective update, resolve WITHOUT writing — the final
    // lockfile is computed after merging and written once at the end.
    let new_lockfile: Lockfile = if dry_run || !packages.is_empty() {
        resolve_only_upgraded(&project).await?
    } else {
        resolve_and_lock(&project, true).await?
    };

    // When specific packages are requested, merge back old locked versions for
    // packages NOT in the update set. This keeps non-targeted packages pinned.
    let effective_lockfile = if let (false, Some(old_lf)) = (packages.is_empty(), &old_lockfile) {
        let old_pkg_map: std::collections::HashMap<&str, &uvr_core::lockfile::LockedPackage> =
            old_lf
                .packages
                .iter()
                .map(|p| (p.name.as_str(), p))
                .collect();

        let mut merged_packages = Vec::new();
        for pkg in &new_lockfile.packages {
            if packages.contains(&pkg.name) {
                // Requested for update — use the new version
                merged_packages.push(pkg.clone());
            } else if let Some(old_pkg) = old_pkg_map.get(pkg.name.as_str()) {
                // Not requested — keep the old version
                merged_packages.push((*old_pkg).clone());
            } else {
                // New transitive dep — keep it
                merged_packages.push(pkg.clone());
            }
        }

        let mut merged = Lockfile {
            r: new_lockfile.r.clone(),
            packages: merged_packages,
        };
        merged.packages.sort_by(|a, b| a.name.cmp(&b.name));

        // Write the merged lockfile (unless dry-run, which doesn't reach here)
        if !dry_run {
            project
                .save_lockfile(&merged)
                .context("Failed to write uvr.lock")?;
        }

        merged
    } else {
        new_lockfile
    };

    // Compute diff
    let mut updated = Vec::new();
    let mut added = Vec::new();
    for pkg in &effective_lockfile.packages {
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
            "\n{} Dry run — no changes written",
            style("!").yellow().bold()
        );
    } else {
        // Install updated packages
        install_from_lockfile(&project, &effective_lockfile, jobs).await?;
    }

    Ok(())
}
