use anyhow::{Context, Result};
use console::style;
use indicatif::{ProgressBar, ProgressStyle};

use uvr_core::lockfile::Lockfile;
use uvr_core::project::Project;
use uvr_core::r_version::detector::{find_r_binary, query_r_version};
use uvr_core::registry::cran::CranRegistry;
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
/// Called by `uvr lock`, `uvr add`, and `uvr remove --prune`.
///
/// `upgrade = true` bypasses the CRAN index cache, forcing a fresh download.
pub async fn resolve_and_lock(project: &Project, upgrade: bool) -> Result<Lockfile> {
    let client = build_client()?;

    // Query the actual running R version to pin in the lockfile.
    let r_constraint = project.manifest.project.r_version.as_deref();
    let actual_r_version = find_r_binary(r_constraint)
        .ok()
        .and_then(|r| query_r_version(&r));

    let spinner = make_spinner("Resolving dependencies...");
    let cran = CranRegistry::fetch(&client, upgrade)
        .await
        .context("Failed to fetch CRAN index")?;

    let resolver = Resolver::new(&cran);
    let lockfile = resolver
        .resolve(&project.manifest, actual_r_version.as_deref())
        .context("Dependency resolution failed")?;

    spinner.finish_with_message(format!("Resolved {} packages", lockfile.packages.len()));

    project
        .save_lockfile(&lockfile)
        .context("Failed to write uvr.lock")?;

    Ok(lockfile)
}

pub fn build_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("uvr/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("Failed to build HTTP client")
}

pub fn make_spinner(msg: &str) -> ProgressBar {
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
