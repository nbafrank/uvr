use std::path::PathBuf;

use anyhow::{Context, Result};
use console::style;

use uvr_core::installer::binary_install::{install_binary_package, patch_installed_so_files};
use uvr_core::installer::download::Downloader;
use uvr_core::installer::r_cmd_install::RCmdInstall;
use uvr_core::lockfile::{LockedPackage, Lockfile};
use uvr_core::project::Project;
use uvr_core::r_version::detector::{find_r_binary, query_r_version};
use uvr_core::r_version::downloader::{patch_r_dylibs, patch_renviron_site, Platform};
use uvr_core::registry::p3m::P3MBinaryIndex;
use uvr_core::resolver::topological_install_order;

use crate::commands::lock::make_spinner;

pub async fn run(frozen: bool, jobs: usize) -> Result<()> {
    let project = Project::find_cwd().context("Not inside a uvr project")?;
    run_inner(&project, frozen, jobs).await
}

/// Install all packages from the existing lockfile.
///
/// Does NOT re-resolve — the lockfile is the source of truth.
/// Use `uvr lock` or `uvr add` to update the lockfile.
///
/// With `frozen = true` (CI mode): first verify that the lockfile is consistent
/// with the current manifest. If the manifest has diverged, exit with an error
/// rather than silently installing a stale environment.
pub async fn run_inner(project: &Project, frozen: bool, jobs: usize) -> Result<()> {
    project
        .ensure_library_dir()
        .context("Failed to create .uvr/library/")?;

    let lockfile = project
        .load_lockfile()
        .context("Failed to read uvr.lock")?
        .ok_or_else(|| anyhow::anyhow!("No lockfile found. Run `uvr lock` to generate one."))?;

    if frozen {
        let fresh = crate::commands::lock::resolve_only(project)
            .await
            .context("Failed to re-resolve dependencies for --frozen check")?;
        if !lockfiles_equivalent(&lockfile, &fresh) {
            anyhow::bail!(
                "Lockfile is out of date with the current manifest.\n\
                 Run `uvr lock` to update it, then commit the result."
            );
        }
    }

    install_from_lockfile(project, &lockfile, jobs).await
}

/// Download and install any packages in `lockfile` not yet present in the project library.
///
/// Prefers pre-built binary packages from Posit Package Manager (P3M) — no compilation
/// or system library dependencies required. Falls back to CRAN source + `R CMD INSTALL`
/// for packages that don't have a binary available.
///
/// If the currently active R version differs from the one in the lockfile (major.minor),
/// the project library is wiped and all packages are reinstalled from scratch to avoid
/// ABI incompatibilities.
pub async fn install_from_lockfile(
    project: &Project,
    lockfile: &Lockfile,
    jobs: usize,
) -> Result<()> {
    let library = project.library_path();

    // Detect R version mismatch: compare lockfile major.minor against current R.
    let r_constraint = project.manifest.project.r_version.as_deref();
    if let Ok(r_binary) = find_r_binary(r_constraint) {
        if let Some(current_r) = query_r_version(&r_binary) {
            let locked_r = &lockfile.r.version;
            let current_minor = r_minor(&current_r);
            let locked_minor = r_minor(locked_r);
            if looks_like_version(locked_r) && current_minor != locked_minor {
                println!(
                    "{} R version changed ({} → {}), wiping library and reinstalling...",
                    style("!").yellow().bold(),
                    style(&locked_minor).dim(),
                    style(&current_minor).cyan(),
                );
                if library.exists() {
                    std::fs::remove_dir_all(&library).context("Failed to wipe project library")?;
                }
                project
                    .ensure_library_dir()
                    .context("Failed to recreate .uvr/library/")?;
            }
        }
    }

    let all_ordered = topological_install_order(&lockfile.packages);
    let to_install: Vec<&LockedPackage> = all_ordered
        .into_iter()
        .filter(|p| !is_installed(p, &library))
        .collect();

    if to_install.is_empty() {
        println!("{} All packages up to date", style("✓").green().bold());
        return Ok(());
    }

    println!(
        "{} Installing {} package(s)...",
        style("→").blue().bold(),
        to_install.len()
    );

    let client = crate::commands::lock::build_client()?;

    // On Linux, check for missing system dependencies before installing.
    #[cfg(target_os = "linux")]
    {
        use uvr_core::sysreqs;

        if let Some(distro) = sysreqs::detect_linux_distro() {
            let sysreqs_packages: Vec<(String, String)> = to_install
                .iter()
                .filter_map(|p| {
                    p.system_requirements
                        .as_ref()
                        .map(|sr| (p.name.clone(), sr.clone()))
                })
                .collect();

            if !sysreqs_packages.is_empty() {
                let missing = sysreqs::check_system_deps(&client, &sysreqs_packages, &distro).await;
                if !missing.is_empty() {
                    let all_pkgs: Vec<&str> = missing
                        .values()
                        .flat_map(|reqs| reqs.iter().map(|r| r.package.as_str()))
                        .collect::<std::collections::BTreeSet<&str>>()
                        .into_iter()
                        .collect();

                    println!(
                        "\n{} Missing system dependencies for {} package(s):",
                        style("⚠").yellow().bold(),
                        missing.len()
                    );
                    for (pkg_name, reqs) in &missing {
                        let names: Vec<&str> = reqs.iter().map(|r| r.package.as_str()).collect();
                        println!(
                            "  {} requires: {}",
                            style(pkg_name).cyan(),
                            names.join(", ")
                        );
                    }
                    println!(
                        "\n  Install with: {}\n",
                        style(format!("sudo apt-get install -y {}", all_pkgs.join(" "))).bold()
                    );
                    println!("  Continuing installation (some packages may fail to compile)...\n");
                }
            }
        }
    }
    let cache_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".uvr")
        .join("cache");

    // Determine R binary and version for P3M lookup.
    let r_constraint = project.manifest.project.r_version.as_deref();
    let r_binary = find_r_binary(r_constraint)
        .context("R not found. Install R or use `uvr r install <version>`")?;

    // For uvr-managed R installs:
    // 1. Ensure etc/Renviron.site has DYLD_LIBRARY_PATH set so that sub-R processes
    //    spawned by R CMD INSTALL can find libR.dylib (macOS SIP strips DYLD_* vars).
    // 2. Compute the path to libR.dylib so binary packages extracted from P3M can be
    //    patched to reference the managed R's libR instead of the CRAN framework path.
    let r_home_opt = r_binary
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf());
    let libr_path: Option<std::path::PathBuf> = if let Some(ref r_home) = r_home_opt {
        if r_home.to_string_lossy().contains(".uvr/r-versions") {
            let _ = patch_renviron_site(r_home);
            // Patch all R dylib install names so BLAS/LAPACK sibling libraries
            // (libRlapack, libRblas, libgfortran) are found at the managed path
            // rather than the original CRAN framework path. Idempotent.
            patch_r_dylibs(r_home);
            Some(r_home.join("lib").join("libR.dylib"))
        } else {
            None
        }
    } else {
        None
    };

    // Retroactively patch already-installed binary packages whose .so files still
    // reference the CRAN framework libR path (installed before patching support was
    // added). Idempotent: no-op when the .so already points to the managed R path.
    if let Some(ref libr) = libr_path {
        if libr.exists() {
            for pkg in &lockfile.packages {
                let pkg_dir = library.join(&pkg.name);
                if pkg_dir.exists() {
                    patch_installed_so_files(&pkg_dir, libr);
                }
            }
        }
    }

    let r_minor = query_r_version(&r_binary)
        .map(|v| {
            let parts: Vec<&str> = v.splitn(3, '.').collect();
            if parts.len() >= 2 {
                format!("{}.{}", parts[0], parts[1])
            } else {
                v
            }
        })
        .unwrap_or_else(|| "4.4".to_string());

    // Fetch P3M binary index (gracefully returns empty if unavailable).
    let p3m = match Platform::detect() {
        Ok(platform @ (Platform::MacOsArm64 | Platform::MacOsX86_64)) => {
            P3MBinaryIndex::fetch(&client, &r_minor, platform).await
        }
        _ => P3MBinaryIndex::empty(),
    };

    let bioc_release = lockfile.r.bioc_version.as_deref();

    // Decide URL and install method for each package.
    // Prefer P3M binary if available for the exact version; fall back to source.
    let pkg_urls: Vec<(&LockedPackage, String, bool)> = to_install
        .iter()
        .map(|p| {
            if let Some(bin_url) = p3m.binary_url(&p.name, &p.version) {
                (*p, bin_url.to_string(), true)
            } else {
                (*p, source_url(p, bioc_release), false)
            }
        })
        .collect();

    let binary_count = pkg_urls.iter().filter(|(_, _, b)| *b).count();
    let source_count = pkg_urls.len() - binary_count;
    if binary_count > 0 {
        println!(
            "  {} binary, {} from source",
            style(binary_count).cyan(),
            source_count
        );
    }

    let pairs: Vec<(&LockedPackage, &str, bool)> = pkg_urls
        .iter()
        .map(|(p, url, is_binary)| (*p, url.as_str(), *is_binary))
        .collect();

    let downloader = Downloader::new(client, cache_dir, jobs);
    let tarballs = downloader
        .download_all(&pairs)
        .await
        .context("Download failed")?;

    let installer = RCmdInstall::new(r_binary.to_string_lossy());

    for ((pkg, _, is_binary), tarball) in pkg_urls.iter().zip(tarballs.iter()) {
        let pb = make_spinner(&format!("Installing {} {}...", pkg.name, pkg.version));

        if *is_binary {
            install_binary_package(tarball, &library, &pkg.name, libr_path.as_deref())
                .with_context(|| format!("Failed to install {}", pkg.name))?;
        } else {
            installer
                .install(tarball, &library, &pkg.name)
                .with_context(|| format!("Failed to install {}", pkg.name))?;
        }

        pb.finish_with_message(format!(
            "{} {} {}{}",
            style("✓").green(),
            pkg.name,
            style(&pkg.version).dim(),
            if *is_binary { "" } else { " (compiled)" },
        ));
    }

    println!(
        "{} {} package(s) installed",
        style("✓").green().bold(),
        to_install.len()
    );

    Ok(())
}

fn is_installed(pkg: &LockedPackage, library: &std::path::Path) -> bool {
    library.join(&pkg.name).join("DESCRIPTION").exists()
}

/// Compare two lockfiles for semantic equivalence, ignoring fields that can
/// legitimately differ between lockfile versions (e.g. `url`, `checksum`).
/// Compares: R major.minor version + set of (name, version, source) triples.
fn lockfiles_equivalent(
    a: &uvr_core::lockfile::Lockfile,
    b: &uvr_core::lockfile::Lockfile,
) -> bool {
    if r_minor(&a.r.version) != r_minor(&b.r.version) {
        return false;
    }
    if a.packages.len() != b.packages.len() {
        return false;
    }
    // Both are sorted alphabetically by the resolver, so zip is safe.
    a.packages
        .iter()
        .zip(b.packages.iter())
        .all(|(ap, bp)| ap.name == bp.name && ap.version == bp.version && ap.source == bp.source)
}

/// Return true only if `s` looks like an actual version number (e.g. `"4.5.3"`),
/// not a semver constraint (`">=4.0.0"`) or wildcard (`"*"`).
/// Used to guard the version-mismatch wipe so that old lockfiles with constraint
/// strings don't trigger a library wipe on every sync run.
fn looks_like_version(s: &str) -> bool {
    !s.is_empty() && s.starts_with(|c: char| c.is_ascii_digit())
}

/// Extract `"major.minor"` from a version string like `"4.4.2"` or `"4.4"`.
fn r_minor(version: &str) -> String {
    let parts: Vec<&str> = version.splitn(3, '.').collect();
    if parts.len() >= 2 {
        format!("{}.{}", parts[0], parts[1])
    } else {
        version.to_string()
    }
}

/// Return the source download URL for a locked package.
/// Prefers the stored `url` field; falls back to reconstructing it.
/// Uses `raw_version` (e.g. `"1.1-3"`) when available so the reconstructed
/// filename matches the actual CRAN tarball (e.g. `scales_1.1-3.tar.gz`).
fn source_url(pkg: &LockedPackage, bioc_release: Option<&str>) -> String {
    if let Some(url) = &pkg.url {
        return url.clone();
    }
    let ver = pkg.raw_version.as_deref().unwrap_or(&pkg.version);
    use uvr_core::lockfile::PackageSource;
    match pkg.source {
        PackageSource::Cran => format!(
            "https://cran.r-project.org/src/contrib/{}_{}.tar.gz",
            pkg.name, ver
        ),
        PackageSource::Bioconductor => {
            let release = bioc_release.unwrap_or("release");
            format!(
                "https://bioconductor.org/packages/{release}/bioc/src/contrib/{}_{}.tar.gz",
                pkg.name, ver
            )
        }
        PackageSource::GitHub | PackageSource::Local => String::new(),
    }
}
