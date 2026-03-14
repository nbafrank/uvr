use anyhow::{Context, Result};
use console::style;

use uvr_core::project::Project;
use uvr_core::r_version::detector::{find_r_binary, query_r_version};

/// `uvr r pin [version]` — write an exact version to `.r-version`.
///
/// If no version is given, queries the currently active R binary.
pub fn run(version: Option<String>) -> Result<()> {
    let project = Project::find_cwd().context("Not inside a uvr project")?;

    let pinned = match version {
        Some(v) => v,
        None => {
            // Detect active R version
            let constraint = project.manifest.project.r_version.as_deref();
            let binary = find_r_binary(constraint)
                .context("R not found. Install R or use `uvr r install <version>`")?;
            query_r_version(&binary)
                .context("Could not determine R version from the active R binary")?
        }
    };

    project
        .write_r_version_pin(&pinned)
        .context("Failed to write .r-version")?;

    println!(
        "{} Pinned R {} → {}",
        style("✓").green().bold(),
        style(&pinned).cyan(),
        style(".r-version").dim(),
    );

    Ok(())
}
