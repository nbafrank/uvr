use std::process::{Command, Stdio};

use anyhow::{Context, Result};

use uvr_core::project::Project;
use uvr_core::r_version::detector::find_r_binary;

use crate::ui;
use crate::ui::palette;

/// `uvr r javareconf` — run `R CMD javareconf` against the project's uvr-managed R.
///
/// Registers the JVM with R so packages like rJava can compile. Requires sudo
/// because R writes to its own install dir under `~/.uvr/r-versions/<ver>/`.
pub fn run() -> Result<()> {
    let project = Project::find_cwd().context("Not inside a uvr project")?;
    let constraint = project.manifest.project.r_version.as_deref();
    let r_binary = find_r_binary(constraint)
        .context("R not found. Install R or use `uvr r install <version>`")?;

    ui::info(format!(
        "Running {} {} for {}",
        palette::info("sudo R CMD javareconf"),
        palette::dim("→"),
        palette::dim(r_binary.display().to_string()),
    ));

    let status = Command::new("sudo")
        .arg(&r_binary)
        .arg("CMD")
        .arg("javareconf")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("Failed to spawn sudo {}", r_binary.display()))?;

    if !status.success() {
        anyhow::bail!(
            "R CMD javareconf failed (exit {})",
            status.code().unwrap_or(-1)
        );
    }

    ui::success("JVM registered with R. You can now install rJava.");
    Ok(())
}
