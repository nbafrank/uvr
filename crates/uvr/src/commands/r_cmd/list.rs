use anyhow::Result;
use console::style;

use uvr_core::r_version::detector::find_all;

pub fn run(_all: bool) -> Result<()> {
    let installations = find_all();

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
