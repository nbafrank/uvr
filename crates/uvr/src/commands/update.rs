use anyhow::{Context, Result};

use uvr_core::lockfile::Lockfile;
use uvr_core::project::Project;

use crate::commands::lock::{resolve_and_lock, resolve_only_upgraded};
use crate::commands::sync::install_from_lockfile;
use crate::ui;
use crate::ui::palette;

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
        ui::info("Updating all packages");
    } else {
        ui::info(format!(
            "Updating {}",
            packages
                .iter()
                .map(|p| palette::pkg(p).to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));

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
    //
    // Selective update: non-targeted packages are injected into resolution as
    // pins at their locked versions, so the resolver validates the updated
    // packages' dependency constraints against the held-back set. The old
    // approach merged locked versions back AFTER resolution with no
    // re-validation, which could write a lockfile where an updated package
    // requires a newer version of a pinned dep than the one locked (#127) —
    // installing fine but failing at library() time. Now that combination is
    // an explicit resolution error instead.
    let effective_lockfile: Lockfile = if !packages.is_empty() {
        let mut pins = std::collections::HashMap::new();
        if let Some(old_lf) = &old_lockfile {
            for pkg in &old_lf.packages {
                if !packages.contains(&pkg.name) {
                    pins.insert(
                        pkg.name.clone(),
                        uvr_core::resolver::locked_to_package_info(pkg)?,
                    );
                }
            }
        }
        let resolved = resolve_only_upgraded(&project, pins)
            .await
            .with_context(|| {
                format!(
                    "Selective update failed with the non-targeted packages held at their locked \
                 versions. If the error above is a version conflict, the updated package needs \
                 a newer version of a held-back dependency — include that package too \
                 (`uvr update {} <conflicting-pkg>`) or update everything with `uvr update`.",
                    packages.join(" ")
                )
            })?;
        if !dry_run {
            project
                .save_lockfile(&resolved)
                .context("Failed to write uvr.lock")?;
        }
        resolved
    } else if dry_run {
        resolve_only_upgraded(&project, std::collections::HashMap::new()).await?
    } else {
        resolve_and_lock(&project, true).await?
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
        ui::success("All packages already at latest versions");
    } else {
        for (name, old, new) in &updated {
            ui::row_upgrade(name, old, new);
        }
        for (name, ver) in &added {
            ui::row_added(name, ver);
        }
        ui::success(format!(
            "{} package(s) updated",
            updated.len() + added.len()
        ));
    }

    if dry_run {
        println!();
        ui::warn("Dry run — no changes written");
    } else {
        // Install updated packages
        install_from_lockfile(&project, &effective_lockfile, jobs, None, None).await?;
    }

    Ok(())
}
