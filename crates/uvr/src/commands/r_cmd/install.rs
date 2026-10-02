use std::path::Path;

use anyhow::{Context, Result};

use uvr_core::r_version::downloader::Platform;
use uvr_core::r_version::manager::RManager;

use crate::ui;
use crate::ui::palette;

pub async fn run(
    version: String,
    distribution: Option<String>,
    install_dir: Option<std::path::PathBuf>,
) -> Result<()> {
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

    install(&version, install_dir.as_deref(), false).await?;
    Ok(())
}

/// Install R `version`, reporting progress as `uvr r install` does, and
/// return the full version installed.
///
/// Also the backend `uvr run` uses to provision the R a script header asks
/// for (#183). It passes `to_stderr`, so the script's stdout carries only
/// what the script itself prints.
pub async fn install(version: &str, install_dir: Option<&Path>, to_stderr: bool) -> Result<String> {
    let platform = Platform::detect().context("Unsupported platform")?;

    let headline = format!("Installing R {} for {platform:?}", palette::info(version));
    if to_stderr {
        ui::info_err(headline);
    } else {
        ui::info(headline);
    }

    // A channel is a moving target: the build behind `devel` today is not the
    // one behind it tomorrow, so a project that pins it is not reproducible.
    // Say so at install time — by the time it is in uvr.toml nobody re-reads
    // the docs.
    //
    // Careful about the wording: an install already present short-circuits, so
    // running this again does *not* fetch a newer build. What moves is what the
    // name means, not what is on disk.
    if uvr_core::r_version::downloader::is_rolling_channel(version) {
        ui::warn(format!(
            "{version} is a rolling channel, not a release: it names whatever was \
             built most recently, so this install is a snapshot of today's {version}."
        ));
        ui::hint(format!(
            "Pin a numbered version for anything you need to reproduce. To move this \
             one forward later: uvr r uninstall {version} && uvr r install {version}."
        ));
    }

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
        .install(version, install_dir)
        .await
        .context("R installation failed")?;

    let headline = format!("R {} installed", palette::info(&resolved));
    let sub = format!("in {}", palette::format_duration(start.elapsed()));
    if to_stderr {
        ui::summary_err(headline, sub);
    } else {
        ui::summary(headline, sub);
    }
    Ok(resolved)
}
