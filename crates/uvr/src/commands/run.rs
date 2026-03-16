use std::process::Command;

use anyhow::{Context, Result};

use uvr_core::project::Project;
use uvr_core::r_version::detector::find_r_binary;

pub fn run(script: Option<String>, args: Vec<String>) -> Result<()> {
    let project = Project::find_cwd().context("Not inside a uvr project")?;
    project
        .ensure_library_dir()
        .context("Failed to create .uvr/library/")?;

    let library = project.library_path();
    let r_constraint = project.manifest.project.r_version.as_deref();
    let r_binary = find_r_binary(r_constraint)
        .context("R not found. Install R or use `uvr r install <version>`")?;

    // Derive R's lib directory for DYLD_LIBRARY_PATH so that compiled packages
    // (e.g. rlang) can find libR.dylib at runtime regardless of its embedded install-name.
    let r_lib_dir = r_binary
        .parent() // …/bin/
        .and_then(|p| p.parent()) // …/r-versions/4.4.2/
        .map(|p| p.join("lib"))
        .unwrap_or_default();

    let mut cmd = Command::new(&r_binary);
    cmd.env("R_LIBS_USER", library.to_string_lossy().as_ref());
    cmd.env("R_LIBS_SITE", "");
    cmd.env("R_LIBS", "");
    cmd.env("DYLD_LIBRARY_PATH", r_lib_dir.to_string_lossy().as_ref());
    cmd.env("LD_LIBRARY_PATH", r_lib_dir.to_string_lossy().as_ref());
    // Suppress ALL Renviron files so our R_LIBS_USER is not overwritten:
    //  - R_ENVIRON=""   → skips $R_HOME/etc/Renviron (which sets R_LIBS_USER to
    //                     ~/Library/R/4.5/library, overriding our value)
    //  - --no-environ   → skips ~/.Renviron (user customisations)
    cmd.env("R_ENVIRON", "");
    cmd.arg("--no-environ");

    if let Some(script_path) = &script {
        cmd.arg("--no-save");
        cmd.arg("--no-restore");
        cmd.arg("--file");
        cmd.arg(script_path);
        if !args.is_empty() {
            cmd.arg("--args");
            cmd.args(&args);
        }
    } else {
        cmd.arg("--no-save");
    }

    let status = cmd.status().context("Failed to spawn R")?;
    if !status.success() {
        // Return the exit code to main so it can call process::exit with the
        // correct code. This preserves the code for the shell while allowing
        // Drop impls (e.g. TempDir) to run.
        let code = status.code().unwrap_or(1);
        return Err(ScriptExitError(code).into());
    }

    Ok(())
}

/// Sentinel error that carries an R script's exit code.
/// `main` matches on this to forward the exact code to the shell.
#[derive(Debug)]
pub struct ScriptExitError(pub i32);

impl std::fmt::Display for ScriptExitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "R script exited with code {}", self.0)
    }
}

impl std::error::Error for ScriptExitError {}
