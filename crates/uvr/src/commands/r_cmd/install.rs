use anyhow::{Context, Result};

use uvr_core::r_version::downloader::Platform;
use uvr_core::r_version::manager::RManager;

use crate::ui;
use crate::ui::palette;

pub async fn run(version: String, distribution: Option<String>) -> Result<()> {
    let platform = Platform::detect().context("Unsupported platform")?;

    // `--distribution` is deprecated: portable R builds are selected purely by
    // libc (glibc -> manylinux, musl -> musllinux) and architecture, so the
    // per-distro Posit CDN slug no longer affects R installation.
    if distribution
        .as_deref()
        .map(str::trim)
        .is_some_and(|s| !s.is_empty())
    {
        ui::bullet_dim(
            "`--distribution` is deprecated and ignored: portable R builds are \
             selected automatically by libc and architecture."
                .to_string(),
        );
    }

    ui::info(format!(
        "Installing R {} for {:?}",
        palette::info(&version),
        platform
    ));

    let client = reqwest::Client::builder()
        .user_agent("uvr/0.1")
        .build()
        .context("Failed to build HTTP client")?;

    let manager = RManager::new(client);
    let start = ui::now();
    manager
        .install(&version)
        .await
        .context("R installation failed")?;

    ui::summary(
        format!("R {} installed", palette::info(&version)),
        format!("in {}", palette::format_duration(start.elapsed())),
    );
    Ok(())
}
