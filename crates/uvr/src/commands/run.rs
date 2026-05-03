use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use uvr_core::manifest::{DependencySpec, Manifest};
use uvr_core::project::{ManifestSource, Project};
use uvr_core::r_version::detector::{find_r_binary, query_r_version};

pub async fn run(
    script: Option<String>,
    r_version_override: Option<String>,
    with_packages: Vec<String>,
    args: Vec<String>,
) -> Result<()> {
    // Resolve project (optional — uvr run works outside a project too).
    let (project_library, project_r_constraint) = match Project::find_cwd() {
        Ok(p) => {
            p.ensure_library_dir()
                .context("Failed to create .uvr/library/")?;
            let lib = p.library_path();
            let rv = p.manifest.project.r_version.clone();
            (Some(lib), rv)
        }
        Err(_) => (None, None),
    };

    // --r-version flag takes priority over the project constraint.
    let effective_constraint = r_version_override
        .as_deref()
        .or(project_r_constraint.as_deref());

    let r_binary = find_r_binary(effective_constraint)
        .context("R not found. Install R or use `uvr r install <version>`")?;

    let library: PathBuf = project_library.unwrap_or_else(|| {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("uvr")
            .join("library")
    });

    // Handle --with packages: resolve + install into a cached env.
    let with_library = if !with_packages.is_empty() {
        let r_ver = query_r_version(&r_binary).unwrap_or_default();
        Some(ensure_with_env(&with_packages, &r_ver).await?)
    } else {
        None
    };

    // Build R_LIBS_USER: with-library (if any) prepended to project library,
    // plus any path supplied via the UVR_EXTRA_LIBS escape hatch. The escape
    // hatch covers cases where a controlled environment (a Docker image, a
    // shared lab machine, the bench harness in #40) needs to expose a system
    // library to `uvr run` without un-isolating the project — without it,
    // setting R_LIBS_SITE="" below would shadow the system library entirely
    // and any package installed there (pak / renv / cli / …) becomes
    // invisible.
    let path_sep = if cfg!(target_os = "windows") {
        ";"
    } else {
        ":"
    };
    let mut libs_user = match &with_library {
        Some(with_lib) => format!("{}{path_sep}{}", with_lib.display(), library.display()),
        None => library.to_string_lossy().into_owned(),
    };
    if let Ok(extra) = std::env::var("UVR_EXTRA_LIBS") {
        let extra = extra.trim();
        if !extra.is_empty() {
            libs_user.push_str(path_sep);
            libs_user.push_str(extra);
        }
    }

    // Derive R's lib directory for DYLD_LIBRARY_PATH so that compiled packages
    // (e.g. rlang) can find libR.dylib at runtime regardless of its embedded install-name.
    let r_lib_dir = r_binary
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.join("lib"))
        .unwrap_or_default();

    let mut cmd = Command::new(&r_binary);
    cmd.env("R_LIBS_USER", &libs_user);
    cmd.env("R_LIBS_SITE", "");
    cmd.env("R_LIBS", "");
    cmd.env("DYLD_LIBRARY_PATH", r_lib_dir.to_string_lossy().as_ref());
    cmd.env("LD_LIBRARY_PATH", r_lib_dir.to_string_lossy().as_ref());
    cmd.env("R_ENVIRON", "");
    cmd.arg("--no-environ");

    if let Some(script_path) = &script {
        // Script mode — suppress the R session intro banner. Without
        // --quiet, every `uvr run script.R` dumps the "R version 4.6.0
        // (2026-…) -- 'Because it was There'" preamble to stdout before
        // the script starts (#81). --quiet keeps prompts visible (so
        // interactive `browser()` still works) while dropping the banner.
        cmd.arg("--quiet");
        cmd.arg("--no-save");
        cmd.arg("--no-restore");
        cmd.arg(format!("--file={script_path}"));
        if !args.is_empty() {
            cmd.arg("--args");
            cmd.args(&args);
        }
    } else {
        // Interactive mode — keep the banner. It's part of the REPL
        // experience and a user typing `uvr run` (no script) expects
        // R's normal startup output.
        cmd.arg("--no-save");
    }

    let status = cmd.status().context("Failed to spawn R")?;
    if !status.success() {
        let code = status.code().unwrap_or(1);
        return Err(ScriptExitError(code).into());
    }

    Ok(())
}

/// Ensure the `--with` packages are installed in a cached environment.
/// Returns the path to the cached library directory.
async fn ensure_with_env(packages: &[String], r_version: &str) -> Result<PathBuf> {
    // Compute a stable cache key from the sorted package list + R version.
    let mut sorted = packages.to_vec();
    sorted.sort();
    let mut hasher = Sha256::new();
    hasher.update(r_version.as_bytes());
    for pkg in &sorted {
        hasher.update(b"\0");
        hasher.update(pkg.as_bytes());
    }
    let hash = format!("{:x}", hasher.finalize());
    let short_hash = &hash[..12];

    let cache_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".uvr")
        .join("cache")
        .join("with-envs")
        .join(short_hash);

    let lib_dir = cache_dir.join(".uvr").join("library");

    // Check if all requested packages are already installed.
    let all_installed = sorted
        .iter()
        .all(|pkg| lib_dir.join(pkg).join("DESCRIPTION").exists());

    if all_installed {
        return Ok(lib_dir);
    }

    // Build a temporary manifest with the --with packages.
    let mut manifest = Manifest::new("__with__", None);
    for pkg in &sorted {
        manifest.add_dep(pkg.clone(), DependencySpec::default(), false);
    }

    let project = Project {
        root: cache_dir.clone(),
        manifest,
        manifest_source: ManifestSource::Toml,
    };
    project
        .ensure_library_dir()
        .context("Failed to create --with cache library")?;
    project
        .save_manifest()
        .context("Failed to write --with manifest")?;

    // Resolve and install.
    let lockfile = crate::commands::lock::resolve_and_lock(&project, false).await?;
    crate::commands::sync::install_from_lockfile(&project, &lockfile, 4, None, None).await?;

    Ok(lib_dir)
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
