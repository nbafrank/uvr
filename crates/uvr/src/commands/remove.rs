use anyhow::{Context, Result};
use console::style;

use uvr_core::project::Project;

pub fn run(packages: Vec<String>) -> Result<()> {
    let mut project = Project::find_cwd().context("Not inside a uvr project")?;

    for name in &packages {
        if project.manifest.remove_dep(name) {
            println!("{} {}", style("-").red().bold(), style(name).cyan());
        } else {
            eprintln!(
                "{} Package '{}' not in dependencies",
                style("warning:").yellow().bold(),
                name
            );
        }
    }

    project.save_manifest().context("Failed to write uvr.toml")?;

    // Recompute lockfile (without re-installing; orphans will be removed on next sync)
    println!(
        "{} Run `uvr sync` to remove unused packages from the library.",
        style("hint:").dim()
    );

    Ok(())
}
