use anyhow::{Context, Result};
use console::style;
use serde::Serialize;
use std::collections::HashMap;

use uvr_core::lockfile::{Lockfile, PackageSource};
use uvr_core::project::Project;

pub fn run(format: ExportFormat, output: Option<String>) -> Result<()> {
    let project = Project::find_cwd().context("Not inside a uvr project")?;
    let lockfile = project
        .load_lockfile()
        .context("Failed to read uvr.lock")?
        .ok_or_else(|| anyhow::anyhow!("No lockfile found. Run `uvr lock` first."))?;

    let content = match format {
        ExportFormat::Renv => export_renv(&lockfile)?,
    };

    match output {
        Some(path) => {
            std::fs::write(&path, &content).with_context(|| format!("Failed to write {path}"))?;
            println!(
                "{} Exported {} package(s) to {}",
                style("✓").green().bold(),
                lockfile.packages.len(),
                style(&path).cyan()
            );
        }
        None => {
            print!("{content}");
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub enum ExportFormat {
    Renv,
}

impl std::str::FromStr for ExportFormat {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "renv" | "renv.lock" => Ok(ExportFormat::Renv),
            _ => Err(format!("Unknown format: '{s}'. Supported: renv")),
        }
    }
}

impl std::fmt::Display for ExportFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExportFormat::Renv => write!(f, "renv"),
        }
    }
}

/// Export to renv.lock format.
///
/// renv.lock is a JSON file with structure:
/// ```json
/// {
///   "R": { "Version": "4.4.2", "Repositories": [...] },
///   "Packages": {
///     "ggplot2": { "Package": "ggplot2", "Version": "3.5.1", "Source": "Repository", "Repository": "CRAN" },
///     ...
///   }
/// }
/// ```
fn export_renv(lockfile: &Lockfile) -> Result<String> {
    let r_section = RenvR {
        version: lockfile.r.version.clone(),
        repositories: vec![RenvRepo {
            name: "CRAN".into(),
            url: "https://cloud.r-project.org".into(),
        }],
    };

    let mut packages = HashMap::new();
    for pkg in &lockfile.packages {
        let (source, repository) = match pkg.source {
            PackageSource::Cran => ("Repository".to_string(), Some("CRAN".to_string())),
            PackageSource::Bioconductor => ("Bioconductor".to_string(), None),
            PackageSource::GitHub => ("GitHub".to_string(), None),
            PackageSource::Local => ("Local".to_string(), None),
        };

        let version = pkg
            .raw_version
            .as_deref()
            .unwrap_or(&pkg.version)
            .to_string();

        let requirements = if pkg.requires.is_empty() {
            None
        } else {
            Some(pkg.requires.clone())
        };

        let remote_info = if pkg.source == PackageSource::GitHub {
            pkg.url.as_ref().and_then(|u| parse_github_remote(u))
        } else {
            None
        };

        let entry = RenvPackage {
            package: pkg.name.clone(),
            version,
            source,
            repository,
            requirements,
            remote_username: remote_info.as_ref().map(|(user, _, _)| user.clone()),
            remote_repo: remote_info.as_ref().map(|(_, repo, _)| repo.clone()),
            remote_ref: remote_info.as_ref().and_then(|(_, _, r)| r.clone()),
        };
        packages.insert(pkg.name.clone(), entry);
    }

    let renv_lock = RenvLock {
        r: r_section,
        packages,
    };

    serde_json::to_string_pretty(&renv_lock).context("Failed to serialize renv.lock")
}

fn parse_github_remote(url: &str) -> Option<(String, String, Option<String>)> {
    // URL like "https://api.github.com/repos/user/repo/tarball/ref"
    // or "user/repo"
    if url.contains("github.com") {
        let parts: Vec<&str> = url.split('/').collect();
        // Find "repos" index or parse user/repo from the URL
        if let Some(pos) = parts.iter().position(|&p| p == "repos") {
            let user = parts.get(pos + 1)?.to_string();
            let repo = parts.get(pos + 2)?.to_string();
            let git_ref = parts.get(pos + 4).map(|s| s.to_string());
            return Some((user, repo, git_ref));
        }
    }
    None
}

#[derive(Serialize)]
struct RenvLock {
    #[serde(rename = "R")]
    r: RenvR,
    #[serde(rename = "Packages")]
    packages: HashMap<String, RenvPackage>,
}

#[derive(Serialize)]
struct RenvR {
    #[serde(rename = "Version")]
    version: String,
    #[serde(rename = "Repositories")]
    repositories: Vec<RenvRepo>,
}

#[derive(Serialize)]
struct RenvRepo {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "URL")]
    url: String,
}

#[derive(Serialize)]
struct RenvPackage {
    #[serde(rename = "Package")]
    package: String,
    #[serde(rename = "Version")]
    version: String,
    #[serde(rename = "Source")]
    source: String,
    #[serde(rename = "Repository", skip_serializing_if = "Option::is_none")]
    repository: Option<String>,
    #[serde(rename = "Requirements", skip_serializing_if = "Option::is_none")]
    requirements: Option<Vec<String>>,
    #[serde(rename = "RemoteUsername", skip_serializing_if = "Option::is_none")]
    remote_username: Option<String>,
    #[serde(rename = "RemoteRepo", skip_serializing_if = "Option::is_none")]
    remote_repo: Option<String>,
    #[serde(rename = "RemoteRef", skip_serializing_if = "Option::is_none")]
    remote_ref: Option<String>,
}
