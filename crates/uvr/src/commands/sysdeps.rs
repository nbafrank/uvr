//! `uvr sysdeps` — report a project's system dependencies without syncing.
//!
//! The sysreqs machinery already existed but ran only inside `uvr sync` on
//! Linux, so "what system libraries does this project need, and which are
//! missing?" had no standalone answer (#256). That question comes up when
//! writing a Dockerfile, when adding a CI setup step, and simply before
//! committing to a sync.
//!
//! Reads the lockfile rather than the manifest. System dependencies come
//! from resolved package DESCRIPTIONs, so answering from `uvr.toml` alone
//! would mean running a full resolution — exactly the cost the command
//! exists to avoid. Making one path instant and the other silently
//! expensive is worse than one clear error telling the user to run
//! `uvr lock`.

use anyhow::{Context, Result};

use uvr_core::project::Project;

use crate::ui;

/// Exit code when system dependencies are missing, so CI can gate on it
/// without parsing output. Mirrors `sync --frozen`'s "the check failed"
/// contract rather than inventing a second one.
///
/// Linux-gated with everything else it belongs to: `clippy -D warnings`
/// counts an unreachable constant as dead code on macOS and Windows.
#[cfg(target_os = "linux")]
const MISSING_DEPS_EXIT: i32 = 1;

pub async fn run(all: bool) -> Result<()> {
    let project = Project::find_cwd().context("Not inside a uvr project")?;

    let lockfile = project
        .load_lockfile()
        .context("Failed to read uvr.lock")?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "No lockfile found. Run `uvr lock` to generate one — system \
                 dependencies come from resolved packages, so there is nothing \
                 to report without a resolution."
            )
        })?;

    if lockfile.packages.is_empty() {
        ui::success("No packages in uvr.lock — no system dependencies.");
        return Ok(());
    }

    report(&lockfile, all).await
}

#[cfg(not(target_os = "linux"))]
async fn report(_lockfile: &uvr_core::lockfile::Lockfile, _all: bool) -> Result<()> {
    // Not an error: there is genuinely nothing to check. macOS resolves
    // system libraries through brew, which is outside uvr's scope, and
    // Windows source builds are an Rtools story. Exiting 0 keeps the
    // command usable in a cross-platform CI matrix.
    ui::info("System dependency checks are Linux-only today.");
    Ok(())
}

#[cfg(target_os = "linux")]
async fn report(lockfile: &uvr_core::lockfile::Lockfile, all: bool) -> Result<()> {
    use std::collections::BTreeSet;
    use uvr_core::sysreqs;

    use crate::ui::palette;

    let Some(distro) = sysreqs::detect_linux_distro() else {
        ui::warn("Could not identify this Linux distribution from /etc/os-release.");
        return Ok(());
    };

    let queries: Vec<sysreqs::PackageSysReqQuery> = lockfile
        .packages
        .iter()
        .map(|p| sysreqs::PackageSysReqQuery {
            name: p.name.clone(),
            system_requirements: p.system_requirements.clone(),
            bioc: matches!(p.source, uvr_core::lockfile::PackageSource::Bioconductor),
        })
        .collect();

    let client = reqwest::Client::new();
    let check = sysreqs::check_system_deps(&client, &queries, &distro).await;

    // `--all` answers "what does this project need", `missing` answers "what
    // is absent from this host". The first is the Dockerfile question, since
    // the image being built has none of it installed yet.
    let reported = if all { &check.resolved } else { &check.missing };

    // Say when the check could not see everything, rather than letting an
    // empty report read as a clean bill of health. Same honesty rule the
    // sync path follows.
    if check.unsupported_distro {
        ui::warn(format!(
            "Posit's sysreqs API does not serve {distro}; used the vendored rules only."
        ));
    } else if check.lookup_failed {
        ui::warn("Could not reach Posit's sysreqs API; used the vendored rules only.");
    }
    if check.local_unresolved > 0 {
        ui::warn(format!(
            "{} package(s) declare SystemRequirements no vendored rule matched.",
            check.local_unresolved
        ));
    }

    if reported.is_empty() {
        if all {
            ui::success("No system dependencies for this project.");
        } else {
            ui::success("All system dependencies are installed.");
        }
        return Ok(());
    }

    let mut rows: Vec<(&String, &Vec<sysreqs::SysReq>)> = reported.iter().collect();
    rows.sort_by(|a, b| a.0.cmp(b.0));

    ui::info(if all {
        format!("System dependencies for {} package(s):", rows.len())
    } else {
        format!("Missing system dependencies for {} package(s):", rows.len())
    });
    for (pkg, reqs) in &rows {
        let names: Vec<&str> = reqs.iter().map(|r| r.package.as_str()).collect();
        println!(
            "  {} {}",
            palette::pkg(pkg),
            palette::dim(&names.join(", "))
        );
    }

    // Deduplicated and sorted so the install line is stable across runs —
    // it ends up pasted into a Dockerfile, where churn shows up as a
    // rebuilt layer.
    let all_pkgs: Vec<&str> = rows
        .iter()
        .flat_map(|(_, reqs)| reqs.iter().map(|r| r.package.as_str()))
        .collect::<BTreeSet<&str>>()
        .into_iter()
        .collect();

    println!();
    for cmd in &check.pre_install {
        ui::hint(format!("First run: {}", cmd.command));
    }
    match sysreqs::PackageManager::detect() {
        Some(pm) => ui::hint(pm.install_command(&all_pkgs)),
        // Naming the packages and admitting uvr does not know the command
        // beats naming a command that is not installed (#226).
        None => ui::hint(format!(
            "No supported package manager found. Install: {}",
            all_pkgs.join(" ")
        )),
    }
    for cmd in &check.post_install {
        ui::hint(format!("Then run: {}", cmd.command));
    }

    if all {
        // A listing, not a check — nothing has failed.
        return Ok(());
    }
    std::process::exit(MISSING_DEPS_EXIT);
}
