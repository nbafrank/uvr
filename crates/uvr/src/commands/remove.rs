use std::path::Path;

use anyhow::{Context, Result};

use uvr_core::project::Project;
use uvr_core::script_header;

use crate::ui;
use crate::ui::palette;

pub async fn run(packages: Vec<String>) -> Result<()> {
    let mut project = Project::find_cwd().context("Not inside a uvr project")?;

    for name in &packages {
        if project.manifest.remove_dep(name) {
            println!(
                "{} {}",
                palette::removed(ui::glyph::remove()),
                palette::pkg(name),
            );
        } else {
            ui::warn(format!("Package '{name}' not in dependencies"));
        }
    }

    project
        .save_manifest()
        .context("Failed to write uvr.toml")?;

    let lockfile = crate::commands::lock::resolve_and_lock(&project, false)
        .await
        .context("Failed to update lockfile")?;

    ui::summary(
        format!("Lockfile updated — {} package(s)", lockfile.packages.len()),
        "Run `uvr sync` to remove unused packages from the library.",
    );

    Ok(())
}

/// `uvr remove --script`: drop `packages` from the script's inline header
/// (#184). As in a project, a name that is not there is a warning.
pub fn run_script(path: &Path, packages: &[String]) -> Result<()> {
    let (before, changed) = crate::commands::util::edit_script_header(path, |source| {
        script_header::remove(source, packages)
    })?;
    for name in packages {
        if before.contains(name) {
            println!(
                "{} {}",
                palette::removed(ui::glyph::remove()),
                palette::pkg(name),
            );
        } else {
            ui::warn(format!(
                "Package '{name}' not in the script header of {}",
                path.display()
            ));
        }
    }
    if let Some(after) = changed {
        let sub = if matches!(script_header::parse(&after), Ok(None)) {
            "No dependencies left, so the header was removed."
        } else {
            "The next `uvr run` uses the new list."
        };
        ui::summary(
            format!("Updated the script header in {}", path.display()),
            sub,
        );
    }
    Ok(())
}
