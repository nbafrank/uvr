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

    // No total `.timeout(...)` here on purpose: R archives are 100-230 MB and
    // a slow-but-moving download must never be killed (#133). Instead,
    // `connect_timeout` bounds connection establishment and `read_timeout`
    // (per-read idle timeout) kills a genuinely stalled socket.
    let client = reqwest::Client::builder()
        .user_agent(concat!("uvr/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(30))
        .read_timeout(std::time::Duration::from_secs(60))
        .build()
        .context("Failed to build HTTP client")?;

    let manager = RManager::new(client);
    let start = ui::now();
    // May differ from the requested version: a partial `4.5` resolves to the
    // newest published `4.5.x` (#170).
    let resolved = manager
        .install(&version)
        .await
        .context("R installation failed")?;

    ui::summary(
        format!("R {} installed", palette::info(&resolved)),
        format!("in {}", palette::format_duration(start.elapsed())),
    );
    Ok(())
}
