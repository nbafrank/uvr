use anyhow::Result;
use console::style;

use uvr_core::r_version::detector::find_all;
use uvr_core::r_version::downloader::{fetch_available_versions, Platform};

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

        println!("{}", style("Available R versions:").bold());
        for ver in available.iter().rev() {
            if installed.contains(ver.as_str()) {
                println!("  {} {}", style(ver).cyan(), style("[installed]").green());
            } else {
                println!("  {}", style(ver).dim());
            }
        }
        return Ok(());
    }

    if installations.is_empty() {
        println!("No R installations found.");
        println!("Install R with:  uvr r install <version>");
        return Ok(());
    }

    println!("{}", style("Installed R versions:").bold());
    for inst in &installations {
        let label = if inst.managed {
            format!("{} {}", style(&inst.version).cyan(), style("[uvr]").dim())
        } else {
            format!(
                "{} {}",
                style(&inst.version).cyan(),
                style(format!("[system: {}]", inst.binary.display())).dim()
            )
        };
        println!("  {label}");
    }

    Ok(())
}
