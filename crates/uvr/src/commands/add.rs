use anyhow::{Context, Result};
use console::style;

use uvr_core::manifest::{DependencySpec, DetailedDep};
use uvr_core::project::Project;

/// Parse `"pkg@>=1.0.0"` or `"user/repo@ref"` into (name, spec).
fn parse_add_spec(raw: &str, bioc: bool) -> Result<(String, DependencySpec)> {
    // GitHub: contains '/'
    if raw.contains('/') {
        let (repo, git_ref) = if let Some(at) = raw.rfind('@') {
            (raw[..at].to_string(), Some(raw[at + 1..].to_string()))
        } else {
            (raw.to_string(), None)
        };

        // Validate user/repo format
        let parts: Vec<&str> = repo.split('/').collect();
        if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
            anyhow::bail!(
                "Invalid GitHub spec '{raw}'. Expected format: user/repo or user/repo@ref"
            );
        }

        let name = parts[1].to_string();

        // Validate package name characters
        if !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '.' || c == '-' || c == '_')
        {
            anyhow::bail!("Invalid package name '{name}' extracted from GitHub spec '{raw}'");
        }

        let spec = DependencySpec::Detailed(DetailedDep {
            git: Some(repo),
            rev: git_ref,
            ..Default::default()
        });
        return Ok((name, spec));
    }

    // CRAN/Bioc with optional version: "pkg@>=1.0.0"
    let (name, version) = if let Some(at) = raw.find('@') {
        (raw[..at].to_string(), Some(raw[at + 1..].to_string()))
    } else {
        (raw.to_string(), None)
    };

    // Validate CRAN/Bioc package name
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '.' || c == '-' || c == '_')
    {
        anyhow::bail!("Invalid package name '{name}'");
    }

    let spec = if bioc {
        DependencySpec::Detailed(DetailedDep {
            bioc: Some(true),
            version,
            ..Default::default()
        })
    } else {
        match version {
            Some(v) => DependencySpec::Version(v),
            None => DependencySpec::Version("*".to_string()),
        }
    };

    Ok((name, spec))
}

pub async fn run(packages: Vec<String>, dev: bool, bioc: bool, jobs: usize) -> Result<()> {
    let mut project = Project::find_cwd().context("Not inside a uvr project")?;

    let parsed: Vec<(String, DependencySpec)> = packages
        .iter()
        .map(|p| parse_add_spec(p, bioc))
        .collect::<Result<Vec<_>>>()?;

    for (name, spec) in &parsed {
        let is_new = project.manifest.add_dep(name.clone(), spec.clone(), dev);
        if is_new {
            println!(
                "{} {} {}",
                style("+").green().bold(),
                style(name).cyan(),
                format_spec(spec)
            );
        } else {
            println!(
                "{} {} {} (updated)",
                style("~").yellow().bold(),
                style(name).cyan(),
                format_spec(spec)
            );
        }
    }

    // Save the original manifest so we can roll back on resolution failure
    let manifest_path = project.manifest_path();
    let original_manifest = std::fs::read_to_string(&manifest_path).ok();

    project
        .save_manifest()
        .context("Failed to write uvr.toml")?;

    // Re-resolve → update lockfile → install new packages
    let resolve_result = crate::commands::lock::resolve_and_lock(&project, false).await;
    if let Err(e) = resolve_result {
        // Roll back the manifest to its original state
        if let Some(original) = original_manifest {
            let _ = std::fs::write(&manifest_path, original);
        }
        return Err(e).context("Failed to resolve dependencies after add");
    }
    let lockfile = resolve_result.unwrap();

    crate::commands::sync::install_from_lockfile(&project, &lockfile, jobs)
        .await
        .context("Failed to install packages after add")?;

    Ok(())
}

fn format_spec(spec: &DependencySpec) -> String {
    match spec {
        DependencySpec::Version(v) => v.clone(),
        DependencySpec::Detailed(d) => {
            if let Some(git) = &d.git {
                let rev = d.rev.as_deref().unwrap_or("HEAD");
                format!("{git}@{rev}")
            } else if d.bioc.unwrap_or(false) {
                "[bioc]".to_string()
            } else {
                d.version.as_deref().unwrap_or("*").to_string()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cran() {
        let (name, spec) = parse_add_spec("ggplot2@>=3.0.0", false).unwrap();
        assert_eq!(name, "ggplot2");
        assert!(matches!(spec, DependencySpec::Version(v) if v == ">=3.0.0"));
    }

    #[test]
    fn parse_github() {
        let (name, spec) = parse_add_spec("tidyverse/ggplot2@main", false).unwrap();
        assert_eq!(name, "ggplot2");
        assert!(spec.git().is_some());
    }

    #[test]
    fn parse_bioc() {
        let (name, spec) = parse_add_spec("DESeq2", true).unwrap();
        assert_eq!(name, "DESeq2");
        assert!(spec.is_bioc());
    }

    #[test]
    fn parse_invalid_github() {
        assert!(parse_add_spec("/", false).is_err());
        assert!(parse_add_spec("a//b", false).is_err());
        assert!(parse_add_spec("user/repo/extra", false).is_err());
    }

    #[test]
    fn parse_empty_name() {
        assert!(parse_add_spec("", false).is_err());
    }
}
