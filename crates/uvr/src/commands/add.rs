use anyhow::{Context, Result};
use console::style;

use uvr_core::manifest::{DependencySpec, DetailedDep};
use uvr_core::project::Project;

/// Parse `"pkg@>=1.0.0"` or `"user/repo@ref"` into (name, spec).
fn parse_add_spec(raw: &str, bioc: bool) -> (String, DependencySpec) {
    // GitHub: contains '/'
    if raw.contains('/') {
        let (repo, git_ref) = if let Some(at) = raw.rfind('@') {
            (raw[..at].to_string(), Some(raw[at + 1..].to_string()))
        } else {
            (raw.to_string(), None)
        };
        // Use the repo name (after /) as the package name
        let name = repo
            .split('/')
            .last()
            .unwrap_or(&repo)
            .to_string();
        let spec = DependencySpec::Detailed(DetailedDep {
            git: Some(repo),
            rev: git_ref,
            ..Default::default()
        });
        return (name, spec);
    }

    // CRAN/Bioc with optional version: "pkg@>=1.0.0"
    let (name, version) = if let Some(at) = raw.find('@') {
        (raw[..at].to_string(), Some(raw[at + 1..].to_string()))
    } else {
        (raw.to_string(), None)
    };

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

    (name, spec)
}

pub async fn run(packages: Vec<String>, dev: bool, bioc: bool, jobs: usize) -> Result<()> {
    let mut project = Project::find_cwd().context("Not inside a uvr project")?;

    let parsed: Vec<(String, DependencySpec)> = packages
        .iter()
        .map(|p| parse_add_spec(p, bioc))
        .collect();

    for (name, spec) in &parsed {
        let is_new = project.manifest.add_dep(name.clone(), spec.clone(), dev);
        if is_new {
            println!("{} {} {}", style("+").green().bold(), style(name).cyan(), format_spec(spec));
        } else {
            println!("{} {} {} (updated)", style("~").yellow().bold(), style(name).cyan(), format_spec(spec));
        }
    }

    project.save_manifest().context("Failed to write uvr.toml")?;

    // Re-resolve → update lockfile → install new packages
    let lockfile = crate::commands::lock::resolve_and_lock(&project, false)
        .await
        .context("Failed to resolve dependencies after add")?;
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
        let (name, spec) = parse_add_spec("ggplot2@>=3.0.0", false);
        assert_eq!(name, "ggplot2");
        assert!(matches!(spec, DependencySpec::Version(v) if v == ">=3.0.0"));
    }

    #[test]
    fn parse_github() {
        let (name, spec) = parse_add_spec("tidyverse/ggplot2@main", false);
        assert_eq!(name, "ggplot2");
        assert!(spec.git().is_some());
    }

    #[test]
    fn parse_bioc() {
        let (name, spec) = parse_add_spec("DESeq2", true);
        assert_eq!(name, "DESeq2");
        assert!(spec.is_bioc());
    }
}
