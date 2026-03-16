use anyhow::{Context, Result};
use console::style;

use uvr_core::project::Project;

/// `uvr r use <version>` — sets the constraint in uvr.toml.
///
/// If `version` is an exact version (no `>=`, `^`, etc.) it also writes `.r-version`.
pub fn run(version: String) -> Result<()> {
    let mut project = Project::find_cwd().context("Not inside a uvr project")?;

    let old = project.manifest.project.r_version.clone();
    project.manifest.project.r_version = Some(version.clone());
    project
        .save_manifest()
        .context("Failed to write uvr.toml")?;

    // If the version looks exact (digits and dots/dashes only), also pin .r-version
    if is_exact_version(&version) {
        project
            .write_r_version_pin(&version)
            .context("Failed to write .r-version")?;
        println!(
            "{} Pinned R {} in {} and {}",
            style("✓").green().bold(),
            style(&version).cyan(),
            style("uvr.toml").dim(),
            style(".r-version").dim(),
        );
    } else {
        match old {
            Some(prev) => println!(
                "{} R version constraint updated: {} → {}",
                style("✓").green().bold(),
                style(&prev).dim(),
                style(&version).cyan()
            ),
            None => println!(
                "{} R version constraint set to {}",
                style("✓").green().bold(),
                style(&version).cyan()
            ),
        }
    }

    Ok(())
}

/// Returns true if `s` looks like a bare version number (`4.3.2`, `4.3-2`),
/// i.e. no comparison operators.
fn is_exact_version(s: &str) -> bool {
    s.chars()
        .all(|c| c.is_ascii_digit() || c == '.' || c == '-')
}
