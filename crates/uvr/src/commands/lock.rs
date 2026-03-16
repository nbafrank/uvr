use anyhow::{Context, Result};
use console::style;
use indicatif::{ProgressBar, ProgressStyle};

use uvr_core::lockfile::Lockfile;
use uvr_core::project::Project;
use uvr_core::r_version::detector::{find_r_binary, query_r_version};
use uvr_core::registry::bioconductor::BiocRegistry;
use uvr_core::registry::cran::CranRegistry;
use uvr_core::registry::CompositeRegistry;
use uvr_core::resolver::Resolver;

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
    let lockfile = resolve_lockfile(project, &client, upgrade).await?;
    project
        .save_lockfile(&lockfile)
        .context("Failed to write uvr.lock")?;
    Ok(lockfile)
}

/// Resolve dependencies and return the lockfile WITHOUT writing it to disk.
/// Used by `uvr sync --frozen` to verify the existing lockfile is current.
pub async fn resolve_only(project: &Project) -> Result<Lockfile> {
    let client = build_client()?;
    resolve_lockfile(project, &client, false).await
}

/// Core resolution logic shared by `resolve_and_lock` and `resolve_only`.
async fn resolve_lockfile(
    project: &Project,
    client: &reqwest::Client,
    upgrade: bool,
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

    // Fetch Bioconductor index only when the manifest has bioc deps.
    let has_bioc = project.manifest.dependencies.values().any(|s| s.is_bioc())
        || project.manifest.dev_dependencies.values().any(|s| s.is_bioc());

    let bioc_opt: Option<BiocRegistry> = if has_bioc {
        let r_ver = actual_r_version.as_deref().unwrap_or("4.4");
        let bioc = BiocRegistry::fetch(client, r_ver)
            .await
            .context("Failed to fetch Bioconductor index")?;
        Some(bioc)
    } else {
        None
    };

    let lockfile = if let Some(ref bioc) = bioc_opt {
        let composite = CompositeRegistry::new(&cran, bioc);
        Resolver::new(&composite)
            .resolve(&project.manifest, actual_r_version.as_deref())
            .context("Dependency resolution failed")?
    } else {
        Resolver::new(&cran)
            .resolve(&project.manifest, actual_r_version.as_deref())
            .context("Dependency resolution failed")?
    };

    spinner.finish_with_message(format!("Resolved {} packages", lockfile.packages.len()));
    Ok(lockfile)
}

pub fn build_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("uvr/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("Failed to build HTTP client")
}

pub fn make_spinner(msg: &str) -> ProgressBar {
    // In non-TTY environments (CI, piped output) suppress the spinner to avoid
    // ANSI escape codes polluting logs.
    if !console::Term::stderr().is_term() {
        return ProgressBar::hidden();
    }
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template("{spinner:.cyan} {msg}")
            .unwrap()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
    );
    pb.enable_steady_tick(std::time::Duration::from_millis(80));
    pb.set_message(msg.to_string());
    pb
}
