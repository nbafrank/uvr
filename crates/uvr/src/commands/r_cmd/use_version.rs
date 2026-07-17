use anyhow::{Context, Result};

use uvr_core::project::Project;

use crate::ui;
use crate::ui::palette;

/// `uvr r use <version>` — sets the constraint in uvr.toml.
///
/// If `version` is an exact version (no `>=`, `^`, etc.) it also writes `.r-version`.
pub fn run(version: String) -> Result<()> {
    let mut project = Project::find_cwd().context("Not inside a uvr project")?;

    let exact = is_exact_version(&version);
    if !exact {
        // Constraint form (`>=4.2`, `^4.3`): validate before touching
        // uvr.toml so garbage like `--` never lands in the manifest (#171).
        uvr_core::resolver::parse_version_req(&version).map_err(|e| {
            anyhow::anyhow!("`{version}` is not a valid R version or constraint: {e}")
        })?;
    }

    let old = project.manifest.project.r_version.clone();
    project.manifest.project.r_version = Some(version.clone());
    project
        .save_manifest()
        .context("Failed to write uvr.toml")?;

    if exact {
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

/// Returns true if `s` is a bare pinnable version (`4.5`, `4.3.2`).
///
/// The old digits/dots/dashes character check accepted garbage like `--`
/// and `4-5-2`, which was then written verbatim to `.r-version` and could
/// never match an install (#171). Dash forms now fall through to the
/// constraint branch, where R's `-`/`.` equivalence is handled by
/// `parse_version_req`.
fn is_exact_version(s: &str) -> bool {
    uvr_core::r_version::detector::is_plausible_r_version(s)
}
