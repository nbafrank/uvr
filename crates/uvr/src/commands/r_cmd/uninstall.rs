use anyhow::{Context, Result};

use uvr_core::r_version::manager::RManager;

use crate::ui;
use crate::ui::palette;

pub fn run(version: String) -> Result<()> {
    let removed = RManager::uninstall(&version)
        .with_context(|| format!("Failed to uninstall R {version}"))?;

    ui::success(format!("Removed R {}", palette::info(&version)));
    ui::bullet_dim(removed.display().to_string());
    Ok(())
}
