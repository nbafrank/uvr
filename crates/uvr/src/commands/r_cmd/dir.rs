use anyhow::{Context, Result};

/// `uvr r dir` — print the directory uvr-managed R versions are installed in
/// (`UVR_R_INSTALL_DIR`, or `~/.uvr/r-versions`), and nothing else, so
/// `$(uvr r dir)` works in scripts.
pub fn run() -> Result<()> {
    let dir = uvr_core::env_vars::r_install_dir().context(
        "Cannot determine the R install directory: HOME is not set. Set UVR_R_INSTALL_DIR.",
    )?;
    println!("{}", dir.display());
    Ok(())
}
