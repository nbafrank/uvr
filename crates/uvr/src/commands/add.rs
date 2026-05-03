use anyhow::{Context, Result};

use uvr_core::manifest::{DependencySpec, DetailedDep};
use uvr_core::project::Project;
use uvr_core::resolver::is_base_package;

use crate::ui;
use crate::ui::palette;

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

#[allow(clippy::too_many_arguments)]
pub async fn run(
    packages: Vec<String>,
    dev: bool,
    bioc: bool,
    source: Option<String>,
    jobs: usize,
    timeout: Option<std::time::Duration>,
    no_lock: bool,
    no_install: bool,
) -> Result<()> {
    let mut project = Project::find_cwd().context("Not inside a uvr project")?;

    // If --source is provided, ensure it's in the manifest's [[sources]]
    if let Some(ref url) = source {
        let url_trimmed = url.trim_end_matches('/');
        let already_exists = project
            .manifest
            .sources
            .iter()
            .any(|s| s.url.trim_end_matches('/') == url_trimmed);
        if !already_exists {
            // Derive a short name from the URL hostname
            let name = url_trimmed
                .strip_prefix("https://")
                .or_else(|| url_trimmed.strip_prefix("http://"))
                .and_then(|s| s.split('/').next())
                .unwrap_or("custom")
                .to_string();
            project
                .manifest
                .sources
                .push(uvr_core::manifest::PackageSource {
                    name: name.clone(),
                    url: url_trimmed.to_string(),
                });
            println!(
                "{} Added source {} {}",
                palette::added(ui::glyph::add()),
                palette::pkg(&name),
                palette::dim(url_trimmed)
            );
        }
    }

    let mut parsed: Vec<(String, DependencySpec)> = packages
        .iter()
        .map(|p| parse_add_spec(p, bioc))
        .collect::<Result<Vec<_>>>()?;

    // For GitHub specs (`user/repo@ref`), the URL-derived basename is only a
    // provisional package name. R's actual package name lives in the
    // remote's DESCRIPTION's `Package:` field — and for some packages
    // those don't match (the `nbafrank/uvr-r` repo ships package `uvr`,
    // see uvr-r #8). Fetch the DESCRIPTION up-front so the manifest entry
    // is keyed by the real package name and matches what the resolver
    // will produce in the lockfile.
    if let Err(e) = resolve_github_pkg_names(&mut parsed).await {
        // Don't bail — fall through with the URL-derived names if the
        // network is unreachable. The user can edit uvr.toml manually.
        ui::warn(format!(
            "Could not resolve GitHub package names from DESCRIPTION ({e}). Using repo basenames; you may need to edit uvr.toml if the package name differs."
        ));
    }

    // Reject base/recommended packages that ship with R — they can't be installed from CRAN.
    for (name, _) in &parsed {
        if is_base_package(name) {
            anyhow::bail!(
                "'{}' is a base R package (ships with R itself) and cannot be installed separately.",
                name
            );
        }
    }

    for (name, spec) in &parsed {
        let is_new = project.manifest.add_dep(name.clone(), spec.clone(), dev);
        if is_new {
            println!(
                "{} {} {}",
                palette::added(ui::glyph::add()),
                palette::pkg(name),
                palette::version(format_spec(spec))
            );
        } else {
            println!(
                "{} {} {} {}",
                palette::upgraded(ui::glyph::change()),
                palette::pkg(name),
                palette::version(format_spec(spec)),
                palette::dim("(updated)"),
            );
        }
    }

    // Save the original manifest so we can roll back on resolution failure
    let manifest_path = project.manifest_path();
    let original_manifest = std::fs::read_to_string(&manifest_path).ok();

    project
        .save_manifest()
        .context("Failed to write uvr.toml")?;

    // #76 — `--no-lock` short-circuits before resolution; useful for
    // building uvr.toml programmatically (e.g. from a script generating
    // multiple `uvr add` calls in a row, then a single explicit
    // `uvr lock` + `uvr sync` at the end). `--no-install` keeps the
    // resolution but skips the install — same use case at a coarser
    // grain. `--no-lock` implies `--no-install` since there's no
    // lockfile to install from.
    if no_lock {
        ui::bullet_dim("Skipped lock + install (--no-lock).");
        return Ok(());
    }

    // Re-resolve → update lockfile (and roll back manifest on failure).
    let resolve_result = crate::commands::lock::resolve_and_lock(&project, false).await;
    if let Err(e) = resolve_result {
        // Roll back the manifest to its original state
        if let Some(original) = original_manifest {
            let _ = std::fs::write(&manifest_path, original);
            ui::warn("Rolled back uvr.toml — resolution failed.");
        }
        return Err(e).context("Failed to resolve dependencies after add");
    }
    let lockfile = resolve_result.unwrap();

    if no_install {
        ui::bullet_dim("Skipped install (--no-install). Run `uvr sync` to install.");
        return Ok(());
    }

    crate::commands::sync::install_from_lockfile(&project, &lockfile, jobs, None, timeout)
        .await
        .context("Failed to install packages after add")?;

    Ok(())
}

/// For each GitHub-sourced dep in `parsed`, fetch the remote DESCRIPTION
/// and replace the URL-derived name with the actual `Package:` field
/// (uvr-r #8). Mutates in place. Best-effort — if any individual fetch
/// fails, that entry keeps its URL-derived name and we warn at the call
/// site.
async fn resolve_github_pkg_names(parsed: &mut [(String, DependencySpec)]) -> Result<()> {
    use uvr_core::registry::github::parse_github_spec;

    let needs_resolve: Vec<usize> = parsed
        .iter()
        .enumerate()
        .filter_map(|(i, (_, spec))| match spec {
            DependencySpec::Detailed(d) if d.git.is_some() => Some(i),
            _ => None,
        })
        .collect();
    if needs_resolve.is_empty() {
        return Ok(());
    }

    let client = crate::commands::util::build_client()?;
    for idx in needs_resolve {
        let (provisional_name, spec) = &parsed[idx];
        let DependencySpec::Detailed(d) = spec else {
            continue;
        };
        let Some(git) = d.git.as_deref() else {
            continue;
        };
        let git_ref = d.rev.as_deref().unwrap_or("HEAD").to_string();
        let spec_str = format!("{git}@{git_ref}");
        let Some((user, repo, resolved_ref)) = parse_github_spec(&spec_str) else {
            continue;
        };
        // Cheap path: hit raw.githubusercontent.com directly for the
        // DESCRIPTION at the requested ref. Avoids the commit-resolution
        // round-trip that the full resolver does — we only need the
        // Package: field.
        let url =
            format!("https://raw.githubusercontent.com/{user}/{repo}/{resolved_ref}/DESCRIPTION");
        match client
            .get(&url)
            .header("User-Agent", concat!("uvr/", env!("CARGO_PKG_VERSION")))
            .send()
            .await
            .and_then(|r| r.error_for_status())
        {
            Ok(resp) => {
                let text = resp.text().await.unwrap_or_default();
                let fields = uvr_core::dcf::parse_dcf_fields(&text);
                if let Some(actual) = fields.get("Package") {
                    let actual = actual.trim().to_string();
                    if !actual.is_empty() && actual != *provisional_name {
                        ui::bullet_dim(format!(
                            "{} → {} (Package: field in DESCRIPTION)",
                            palette::dim(provisional_name),
                            palette::pkg(&actual)
                        ));
                        parsed[idx].0 = actual;
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    "DESCRIPTION fetch failed for {git}@{resolved_ref}: {e}; using {provisional_name} as the package name"
                );
            }
        }
    }
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
