use anyhow::{Context, Result};
use console::style;

use uvr_core::r_version::downloader::Platform;
use uvr_core::r_version::manager::RManager;

pub async fn run(version: String) -> Result<()> {
    let platform = Platform::detect().context("Unsupported platform")?;
    println!(
        "{} Installing R {} for {:?}...",
        style("→").blue().bold(),
        style(&version).cyan(),
        platform
    );

    let client = reqwest::Client::builder()
        .user_agent("uvr/0.1")
        .build()
        .context("Failed to build HTTP client")?;

    let manager = RManager::new(client);
    manager
        .install(&version)
        .await
        .context("R installation failed")?;

    println!(
        "{} R {} installed",
        style("✓").green().bold(),
        style(&version).cyan()
    );
    Ok(())
}
