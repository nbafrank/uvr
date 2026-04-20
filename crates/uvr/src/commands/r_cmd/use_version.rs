use anyhow::{Context, Result};

use uvr_core::project::Project;

use crate::ui;
use crate::ui::palette;

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

    if is_exact_version(&version) {
        project
            .write_r_version_pin(&version)
            .context("Failed to write .r-version")?;
        ui::success(format!(
            "Pinned R {} in {} and {}",
            palette::info(&version),
            palette::dim("uvr.toml"),
            palette::dim(".r-version"),
        ));
    } else {
        match old {
            Some(prev) => ui::success(format!(
                "R constraint updated: {} {} {}",
                palette::dim(&prev),
                palette::dim(ui::glyph::arrow()),
                palette::info(&version),
            )),
            None => ui::success(format!("R constraint set to {}", palette::info(&version))),
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
