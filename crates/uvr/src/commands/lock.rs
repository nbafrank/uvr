use std::collections::HashMap;

use anyhow::{Context, Result};
use console::style;

use uvr_core::lockfile::Lockfile;
use uvr_core::manifest::DependencySpec;
use uvr_core::project::Project;
use uvr_core::r_version::detector::{find_r_binary, query_r_version};
use uvr_core::registry::bioconductor::BiocRegistry;
use uvr_core::registry::cran::CranRegistry;
use uvr_core::registry::github::{parse_github_spec, resolve_github_package};
use uvr_core::registry::{PackageInfo, RegistryChain};
use uvr_core::resolver::{PackageRegistry, Resolver};

pub async fn run(upgrade: bool) -> Result<()> {
    let project = Project::find_cwd().context("Not inside a uvr project")?;
    let lockfile = resolve_and_lock(&project, upgrade).await?;
    println!(
        "{} Lockfile updated ({} packages)",
        style("✓").green().bold(),
        lockfile.packages.len()
    );
    Ok(())
}

/// Re-resolve all dependencies and write `uvr.lock`.
/// Called by `uvr lock`, `uvr add`, and `uvr remove`.
pub async fn resolve_and_lock(project: &Project, upgrade: bool) -> Result<Lockfile> {
    let client = build_client()?;
    let existing = load_existing_lockfile(project);
    let lockfile = resolve_lockfile(project, &client, upgrade, existing.as_ref()).await?;
    project
        .save_lockfile(&lockfile)
        .context("Failed to write uvr.lock")?;
    Ok(lockfile)
}

/// Resolve dependencies and return the lockfile WITHOUT writing it to disk.
/// Used by `uvr sync --frozen` to verify the existing lockfile is current.
pub async fn resolve_only(project: &Project) -> Result<Lockfile> {
    let client = build_client()?;
    let existing = load_existing_lockfile(project);
    resolve_lockfile(project, &client, false, existing.as_ref()).await
}

/// Resolve with upgrade=true WITHOUT writing the lockfile.
/// Used by `uvr update --dry-run`.
pub async fn resolve_only_upgraded(project: &Project) -> Result<Lockfile> {
    let client = build_client()?;
    // --upgrade: don't reuse locked bioc_version, re-detect fresh
    resolve_lockfile(project, &client, true, None).await
}

/// Core resolution logic shared by `resolve_and_lock` and `resolve_only`.
/// `existing` is the current lockfile on disk, used to preserve the locked
/// Bioconductor version across re-resolves (unless `upgrade` is true).
async fn resolve_lockfile(
    project: &Project,
    client: &reqwest::Client,
    upgrade: bool,
    existing: Option<&Lockfile>,
) -> Result<Lockfile> {
    // Query the actual running R version to pin in the lockfile.
    let r_constraint = project.manifest.project.r_version.as_deref();
    let actual_r_version = find_r_binary(r_constraint)
        .ok()
        .and_then(|r| query_r_version(&r));

    let spinner = make_spinner("Resolving dependencies...");
    let cran = CranRegistry::fetch(client, upgrade)
        .await
        .context("Failed to fetch CRAN index")?;

    // Fetch custom repository indices from manifest [[sources]].
    let mut custom_registries: Vec<CranRegistry> = Vec::new();
    for source in &project.manifest.sources {
        let reg = CranRegistry::fetch_custom(client, &source.name, &source.url, upgrade)
            .await
            .with_context(|| format!("Failed to fetch index for repository '{}'", source.name))?;
        custom_registries.push(reg);
    }

    // Fetch Bioconductor index only when the manifest has bioc deps.
    let has_bioc = project.manifest.dependencies.values().any(|s| s.is_bioc())
        || project
            .manifest
            .dev_dependencies
            .values()
            .any(|s| s.is_bioc());

    let bioc_opt: Option<BiocRegistry> = if has_bioc {
        let bioc = if let Some(ref bioc_ver) = project.manifest.project.bioc_version {
            // Explicit bioc_version in manifest — always use it.
            BiocRegistry::fetch_release(client, bioc_ver)
                .await
                .context("Failed to fetch Bioconductor index")?
        } else if let Some(locked_bioc) = existing.and_then(|lf| lf.r.bioc_version.as_deref()) {
            // Reuse the Bioconductor version from the existing lockfile to prevent
            // silent drift between resolves. Use `uvr lock --upgrade` to update.
            BiocRegistry::fetch_release(client, locked_bioc)
                .await
                .context("Failed to fetch Bioconductor index")?
        } else {
            // First resolve or no existing lockfile — auto-detect from R version.
            let r_ver = actual_r_version.as_deref().unwrap_or("4.4");
            BiocRegistry::fetch(client, r_ver)
                .await
                .context("Failed to fetch Bioconductor index")?
        };
        Some(bioc)
    } else {
        None
    };

    // Pre-resolve GitHub dependencies via the GitHub API (async).
    let pre_resolved = resolve_github_deps(client, &project.manifest).await?;

    // Build the registry chain: CRAN → custom sources → Bioconductor
    let mut lockfile = if !custom_registries.is_empty() || bioc_opt.is_some() {
        let mut chain: Vec<&dyn PackageRegistry> = Vec::new();
        chain.push(&cran);
        for reg in &custom_registries {
            chain.push(reg);
        }
        if let Some(ref bioc) = bioc_opt {
            chain.push(bioc);
        }
        let registry = RegistryChain::new(chain);
        Resolver::new(&registry)
            .resolve(&project.manifest, actual_r_version.as_deref(), pre_resolved)
            .context("Dependency resolution failed")?
    } else {
        Resolver::new(&cran)
            .resolve(&project.manifest, actual_r_version.as_deref(), pre_resolved)
            .context("Dependency resolution failed")?
    };

    // Record the Bioconductor release in the lockfile so it's fully self-describing.
    if let Some(ref bioc) = bioc_opt {
        lockfile.r.bioc_version = Some(bioc.release().to_string());
    }

    spinner.finish_with_message(format!("Resolved {} packages", lockfile.packages.len()));
    Ok(lockfile)
}

/// Load the existing lockfile, warning (not erroring) on parse failures.
/// A missing lockfile returns `None`; a corrupt lockfile logs a warning and
/// returns `None` so resolution can proceed without stale bioc pins.
fn load_existing_lockfile(project: &Project) -> Option<Lockfile> {
    match project.load_lockfile() {
        Ok(opt) => opt,
        Err(e) => {
            tracing::warn!("Failed to read existing lockfile, proceeding without it: {e}");
            None
        }
    }
}

/// Collect all GitHub dependencies from the manifest and resolve them via the
/// GitHub API. Returns a map from package name → PackageInfo that the resolver
/// can inject without going through the registry chain.
async fn resolve_github_deps(
    client: &reqwest::Client,
    manifest: &uvr_core::manifest::Manifest,
) -> Result<HashMap<String, PackageInfo>> {
    let mut github_specs: Vec<(String, String)> = Vec::new(); // (name, git_spec)

    let all_deps = manifest
        .dependencies
        .iter()
        .chain(manifest.dev_dependencies.iter());

    for (name, spec) in all_deps {
        if let DependencySpec::Detailed(d) = spec {
            if let Some(ref git) = d.git {
                let full_spec = if let Some(ref rev) = d.rev {
                    format!("{git}@{rev}")
                } else {
                    git.clone()
                };
                github_specs.push((name.clone(), full_spec));
            }
        }
    }

    let mut pre_resolved = HashMap::new();
    for (_name, spec) in &github_specs {
        if let Some((user, repo, git_ref)) = parse_github_spec(spec) {
            let info = resolve_github_package(client, &user, &repo, &git_ref)
                .await
                .with_context(|| format!("Failed to resolve GitHub package {spec}"))?;
            pre_resolved.insert(info.name.clone(), info);
        }
    }

    Ok(pre_resolved)
}

// Re-export from util for backward compatibility within this module
use super::util::{build_client, make_spinner};
