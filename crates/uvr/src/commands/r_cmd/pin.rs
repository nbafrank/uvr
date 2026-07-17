use anyhow::{bail, Context, Result};

use uvr_core::project::Project;
use uvr_core::r_version::detector::{
    find_all, find_r_binary, is_plausible_r_version, query_r_version, version_matches_prefix,
};

use crate::ui;
use crate::ui::palette;

/// `uvr r pin [version]` — write an exact version to `.r-version`.
///
/// If no version is given, queries the currently active R binary.
pub fn run(version: Option<String>) -> Result<()> {
    let project = Project::find_cwd().context("Not inside a uvr project")?;

    let pinned = match version {
        Some(v) => {
            // Validate before writing: the pin used to accept any string,
            // and garbage (`--`, `4-5-2`) produced a `.r-version` that could
            // never match an install (#171).
            if !is_plausible_r_version(&v) {
                bail!(
                    "`{v}` is not a valid R version to pin. Expected `X.Y.Z` (e.g. 4.5.1) \
                     or a partial `X.Y` (e.g. 4.5)."
                );
            }
            let installed = find_all();
            if !installed
                .iter()
                .any(|i| i.version == v || version_matches_prefix(&v, &i.version))
            {
                ui::warn(format!(
                    "R {v} is not installed yet — run `uvr r install {v}` to use this pin."
                ));
            }
            v
        }
        None => {
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

    ui::success(format!(
        "Pinned R {} {} {}",
        palette::info(&pinned),
        palette::dim(ui::glyph::arrow()),
        palette::dim(".r-version"),
    ));

    Ok(())
}
