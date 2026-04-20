use anyhow::{Context, Result};

use uvr_core::project::Project;

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
