use anyhow::Result;

use uvr_core::r_version::detector::find_all;
use uvr_core::r_version::downloader::{fetch_available_versions, Platform};

use crate::ui;
use crate::ui::palette;

pub async fn run(all: bool) -> Result<()> {
    let installations = find_all();

    if all {
        let client = crate::commands::util::build_client()?;
        let platform =
            Platform::detect().map_err(|e| anyhow::anyhow!("Unsupported platform: {e}"))?;
        let available = fetch_available_versions(&client, platform)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to fetch available R versions: {e}"))?;

        let installed: std::collections::HashSet<&str> =
            installations.iter().map(|i| i.version.as_str()).collect();

        println!("{}", palette::bold("Available R versions"));
        for ver in available.iter().rev() {
            if installed.contains(ver.as_str()) {
                println!(
                    "  {} {} {}",
                    palette::success(ui::glyph::success()),
                    palette::info(ver),
                    palette::dim("[installed]"),
                );
            } else {
                println!(
                    "  {} {}",
                    palette::dim(ui::glyph::bullet()),
                    palette::dim(ver),
                );
            }
        }
        return Ok(());
    }

    if installations.is_empty() {
        ui::warn("No R installations found.");
        ui::hint("Install R with: uvr r install <version>");
        return Ok(());
    }

    println!("{}", palette::bold("Installed R versions"));
    for inst in &installations {
        let tag = if inst.managed {
            palette::info("[uvr-managed]").to_string()
        } else {
            palette::dim(format!("[system: {}]", inst.binary.display())).to_string()
        };
        println!(
            "  {} {} {}",
            palette::success(ui::glyph::success()),
            palette::info(&inst.version),
            tag,
        );
    }

    Ok(())
}
