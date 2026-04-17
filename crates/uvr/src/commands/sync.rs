use std::path::PathBuf;

use anyhow::{Context, Result};
use console::style;

use uvr_core::installer::binary_install::{install_binary_package, patch_installed_so_files};
use uvr_core::installer::download::{DownloadSpec, Downloader};
use uvr_core::installer::package_cache;
use uvr_core::installer::r_cmd_install::RCmdInstall;
use uvr_core::lockfile::{LockedPackage, Lockfile};
use uvr_core::project::Project;
use uvr_core::r_version::detector::{find_r_binary, query_r_version};
use uvr_core::r_version::downloader::{patch_r_dylibs, patch_renviron_site, Platform};
use uvr_core::registry::p3m::P3MBinaryIndex;
use uvr_core::resolver::topological_install_order;

use crate::commands::util::make_spinner;

pub async fn run(frozen: bool, no_dev: bool, jobs: usize, library: Option<PathBuf>) -> Result<()> {
    let project = Project::find_cwd().context("Not inside a uvr project")?;
    // CLI --library takes precedence, then UVR_LIBRARY env var.
    let library = library.or_else(|| std::env::var("UVR_LIBRARY").ok().map(PathBuf::from));
    run_inner(&project, frozen, no_dev, jobs, library.as_deref()).await
}

/// Install all packages from the existing lockfile.
///
/// Does NOT re-resolve — the lockfile is the source of truth.
/// Use `uvr lock` or `uvr add` to update the lockfile.
///
/// With `frozen = true` (CI mode): first verify that the lockfile is consistent
/// with the current manifest. If the manifest has diverged, exit with an error
/// rather than silently installing a stale environment.
pub async fn run_inner(
    project: &Project,
    frozen: bool,
    no_dev: bool,
    jobs: usize,
    library_override: Option<&std::path::Path>,
) -> Result<()> {
    if let Some(lib) = library_override {
        std::fs::create_dir_all(lib)
            .with_context(|| format!("Failed to create library dir: {}", lib.display()))?;
    } else {
        project
            .ensure_library_dir()
            .context("Failed to create .uvr/library/")?;
    }

    // Ensure .Rprofile exists so RStudio sees the uvr library
    crate::commands::init::ensure_rprofile(&project.root).context("Failed to write .Rprofile")?;

    // Write .vscode/settings.json for Positron R interpreter
    crate::commands::init::ensure_positron_settings(&project.root)
        .context("Failed to write Positron settings")?;

    // Add uvr entries to .Rbuildignore for R package projects (DESCRIPTION may have
    // been created after `uvr init`, so we check on every sync).
    if project.root.join("DESCRIPTION").exists() {
        let _ = crate::commands::init::write_rbuildignore(&project.root);
    }

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

    // When --no-dev is set, filter out dev-only packages before installing.
    let lockfile = if no_dev {
        let mut filtered = lockfile.clone();
        let before = filtered.packages.len();
        filtered.packages.retain(|p| !p.dev);
        let skipped = before - filtered.packages.len();
        if skipped > 0 {
            println!(
                "{} Skipping {skipped} dev-only package(s)",
                console::style("→").blue().bold()
            );
        }
        filtered
    } else {
        lockfile
    };

    install_from_lockfile(project, &lockfile, jobs, library_override).await
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
    library_override: Option<&std::path::Path>,
) -> Result<()> {
    let library = library_override
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| project.library_path());

    // Resolve R binary + version once (spawning R is ~250ms, so avoid repeating).
    let r_constraint = project.manifest.project.r_version.as_deref();
    let r_info: Option<(PathBuf, String)> = find_r_binary(r_constraint)
        .ok()
        .and_then(|bin| query_r_version(&bin).map(|ver| (bin, ver)));

    // Detect R version mismatch: compare lockfile major.minor against current R.
    if let Some((_, ref current_r)) = r_info {
        let locked_r = &lockfile.r.version;
        let current_minor = r_minor(current_r);
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
            std::fs::create_dir_all(&library).context("Failed to recreate library directory")?;
        }
    }

    let all_ordered = topological_install_order(&lockfile.packages)
        .context("Failed to determine install order")?;
    let to_install: Vec<&LockedPackage> = all_ordered
        .into_iter()
        .filter(|p| !is_installed(p, &library))
        .collect();

    // Install the uvr R companion package if not already present.
    // Skip the (expensive) R version check when all packages are up to date
    // and the companion is already installed.
    let companion_installed = library.join("uvr").join("DESCRIPTION").exists();
    if !companion_installed || !to_install.is_empty() {
        if let Some((_, ref current_r)) = r_info {
            ensure_companion_package(&library, current_r);
        }
    }

    if to_install.is_empty() {
        println!("{} All packages up to date", style("✓").green().bold());
        return Ok(());
    }

    // Show what's changing: new installs vs upgrades.
    let mut new_count = 0usize;
    let mut upgrade_count = 0usize;
    for pkg in &to_install {
        let old_ver = installed_version(&pkg.name, &library);
        if let Some(old) = &old_ver {
            println!(
                "  {} {} {} → {}",
                style("↑").cyan(),
                style(&pkg.name).cyan(),
                style(old).dim(),
                style(&pkg.version).green()
            );
            upgrade_count += 1;
        } else {
            new_count += 1;
        }
    }

    let summary = match (new_count, upgrade_count) {
        (n, 0) => format!("Installing {n} package(s)..."),
        (0, u) => format!("Upgrading {u} package(s)..."),
        (n, u) => format!("Installing {n} new, upgrading {u} package(s)..."),
    };
    println!("{} {}", style("→").blue().bold(), summary);

    let client = crate::commands::util::build_client()?;

    // On Linux, check for missing system dependencies before installing.
    #[cfg(target_os = "linux")]
    {
        use uvr_core::sysreqs;

        if let Some(distro) = sysreqs::detect_linux_distro() {
            let pkg_names: Vec<String> = to_install.iter().map(|p| p.name.clone()).collect();

            if !pkg_names.is_empty() {
                let missing = sysreqs::check_system_deps(&client, &pkg_names, &distro).await;
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

    // Use the R binary resolved at the top of install_from_lockfile.
    let (r_binary, r_version_str) = r_info
        .as_ref()
        .map(|(b, v)| (b.clone(), v.clone()))
        .ok_or_else(|| {
            anyhow::anyhow!("R not found. Install R or use `uvr r install <version>`")
        })?;

    // For uvr-managed R installs:
    // 1. Ensure etc/Renviron.site has DYLD_LIBRARY_PATH set so that sub-R processes
    //    spawned by R CMD INSTALL can find libR.dylib (macOS SIP strips DYLD_* vars).
    // 2. Compute the path to libR.dylib so binary packages extracted from P3M can be
    //    patched to reference the managed R's libR instead of the CRAN framework path.
    let r_home_opt = r_binary
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf());
    let managed_versions_dir = dirs::home_dir().map(|h| h.join(".uvr").join("r-versions"));
    let libr_path: Option<std::path::PathBuf> = if let Some(ref r_home) = r_home_opt {
        if managed_versions_dir
            .as_ref()
            .map(|d| r_home.starts_with(d))
            .unwrap_or(false)
        {
            // macOS-specific: patch dylib install names and Renviron.site
            if cfg!(target_os = "macos") {
                let _ = patch_renviron_site(r_home);
                patch_r_dylibs(r_home);
            }
            let libr_name = if cfg!(target_os = "macos") {
                "libR.dylib"
            } else if cfg!(target_os = "windows") {
                "R.dll"
            } else {
                "libR.so"
            };
            Some(r_home.join("lib").join(libr_name))
        } else {
            None
        }
    } else {
        None
    };

    // Retroactively patch already-installed binary packages whose .so files still
    // reference the CRAN framework libR path (installed before patching support was
    // added). macOS only — Windows DLLs use PATH, not install names.
    if cfg!(target_os = "macos") {
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
    }

    let r_minor_str = r_minor(&r_version_str);
    let bioc_release = lockfile.r.bioc_version.as_deref();

    // ── Phase 1: check global package cache ──────────────────────────────
    // Packages found in the cache are cloned into the library instantly
    // (CoW on APFS, recursive copy elsewhere) — no download or extraction.
    // This runs BEFORE the P3M index fetch so a fully-cached sync skips
    // the ~1s network round-trip entirely.
    let mut cache_misses: Vec<&LockedPackage> = Vec::new();
    let mut cache_hit_count = 0usize;

    for pkg in &to_install {
        if let Some(cached_dir) = package_cache::lookup_any(
            &pkg.name,
            &pkg.version,
            pkg.checksum.as_deref(),
            &r_minor_str,
            true, // probe binary key first
            libr_path.as_deref(),
        ) {
            match package_cache::clone_to_library(&cached_dir, &library, &pkg.name) {
                Ok(()) => {
                    cache_hit_count += 1;
                    tracing::debug!("Cache hit: {} {}", pkg.name, pkg.version);
                }
                Err(e) => {
                    tracing::debug!(
                        "Package cache clone failed for {}: {e}, will download",
                        pkg.name
                    );
                    cache_misses.push(pkg);
                }
            }
        } else {
            cache_misses.push(pkg);
        }
    }

    // ── Phase 2: download + install remaining packages ────────────────
    // Only fetch P3M binary index if there are packages to download.
    struct PkgPlan<'a> {
        pkg: &'a LockedPackage,
        url: String,
        fallback_url: Option<String>,
        is_binary: bool,
    }

    let plans: Vec<PkgPlan> = if !cache_misses.is_empty() {
        let p3m = match Platform::detect() {
            Ok(
                platform @ (Platform::MacOsArm64 | Platform::MacOsX86_64 | Platform::WindowsX86_64),
            ) => P3MBinaryIndex::fetch(&client, &r_minor_str, platform).await,
            _ => P3MBinaryIndex::empty(),
        };

        cache_misses
            .iter()
            .map(|p| {
                if let Some(bin_url) = p3m.binary_url(&p.name, &p.version) {
                    PkgPlan {
                        pkg: p,
                        url: bin_url.to_string(),
                        fallback_url: Some(source_url(p, bioc_release)),
                        is_binary: true,
                    }
                } else {
                    PkgPlan {
                        pkg: p,
                        url: source_url(p, bioc_release),
                        fallback_url: None,
                        is_binary: false,
                    }
                }
            })
            .collect()
    } else {
        Vec::new()
    };

    // Guard against packages with no download URL.
    for plan in &plans {
        if plan.url.is_empty() {
            anyhow::bail!(
                "Package '{}' has no download URL. Re-run `uvr lock` to regenerate the lockfile.",
                plan.pkg.name
            );
        }
    }

    let binary_count = plans.iter().filter(|p| p.is_binary).count();
    let source_count = plans.len() - binary_count;
    if cache_hit_count > 0 || binary_count > 0 || source_count > 0 {
        let mut parts = Vec::new();
        if cache_hit_count > 0 {
            parts.push(format!("{} cached", cache_hit_count));
        }
        if binary_count > 0 {
            parts.push(format!("{} binary", binary_count));
        }
        if source_count > 0 {
            parts.push(format!("{} from source", source_count));
        }
        println!("  {}", parts.join(", "));
    }

    if !plans.is_empty() {
        let specs: Vec<DownloadSpec> = plans
            .iter()
            .map(|p| DownloadSpec {
                pkg: p.pkg,
                url: &p.url,
                fallback_url: p.fallback_url.as_deref(),
                is_binary: p.is_binary,
            })
            .collect();

        let downloader = Downloader::new(client, cache_dir, jobs);
        let results = downloader
            .download_all(&specs)
            .await
            .context("Download failed")?;

        let installer = RCmdInstall::new(r_binary.to_string_lossy());

        let total = plans.len();
        for (idx, (plan, result)) in plans.iter().zip(results.iter()).enumerate() {
            let progress = format!("[{}/{}]", idx + 1, total);
            let action = if result.used_binary {
                "Installing"
            } else {
                "Compiling"
            };
            let pb = make_spinner(&format!(
                "{} {} {} {}...",
                style(&progress).dim(),
                action,
                plan.pkg.name,
                plan.pkg.version
            ));

            if result.used_binary {
                install_binary_package(
                    &result.path,
                    &library,
                    &plan.pkg.name,
                    libr_path.as_deref(),
                )
                .with_context(|| format!("Failed to install {}", plan.pkg.name))?;
            } else {
                let prefix = format!(
                    "{} Compiling {} {}",
                    style(&progress).dim(),
                    plan.pkg.name,
                    plan.pkg.version
                );
                installer
                    .install_streaming(&result.path, &library, &plan.pkg.name, |line| {
                        let short = if line.len() > 60 { &line[..60] } else { line };
                        pb.set_message(format!("{prefix} ({short})"));
                    })
                    .with_context(|| format!("Failed to install {}", plan.pkg.name))?;
            }

            pb.finish_and_clear();

            // Store the newly installed package in the global cache for future reuse.
            let key = package_cache::cache_key(
                &plan.pkg.name,
                &plan.pkg.version,
                plan.pkg.checksum.as_deref(),
                &r_minor_str,
                result.used_binary,
                libr_path.as_deref(),
            );
            let pkg_dir = library.join(&plan.pkg.name);
            if let Err(e) = package_cache::store(&pkg_dir, &key, &plan.pkg.name) {
                tracing::debug!("Failed to cache {}: {e}", plan.pkg.name);
            }
        }
    }

    println!(
        "{} {} package(s) installed",
        style("✓").green().bold(),
        to_install.len()
    );

    Ok(())
}

/// Pinned commit SHA and expected SHA-256 hash of the companion R package tarball.
/// Update these together when releasing a new companion version.
const COMPANION_SHA: &str = "4e89ec7806df9d5e19870e515d24feed50b3bbb4";
const COMPANION_HASH: &str = "5942b23cfcafe6b83b3c500f4820434944051ab30b21e91e49fdd4be405002b8";

/// Install the uvr R companion package from GitHub into the project library
/// if it's not already installed. Failures are silently ignored — the companion
/// package is a convenience, not a requirement.
///
/// Security: the download is pinned to an immutable commit SHA and verified
/// against a hardcoded SHA-256 hash, preventing supply-chain attacks via the
/// companion repo.
pub fn ensure_companion_package(library: &std::path::Path, current_r_version: &str) {
    let desc_path = library.join("uvr").join("DESCRIPTION");
    if desc_path.exists() {
        // Check if the companion was built with a different R major.minor.
        // If so, reinstall to avoid "built under R x.y.z" warnings.
        if !companion_needs_rebuild(&desc_path, current_r_version) {
            return;
        }
        // Remove stale companion before reinstalling
        let _ = std::fs::remove_dir_all(library.join("uvr"));
    }

    let cache_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".uvr")
        .join("cache");
    let _ = std::fs::create_dir_all(&cache_dir);
    let tarball = cache_dir.join(format!("uvr-r-{}.tar.gz", &COMPANION_SHA[..8]));

    // Download if cached tarball is missing (pinned SHA = immutable, no TTL needed).
    // If the hash changes (new companion release), the filename changes too.
    if !tarball.exists() {
        let url = format!("https://api.github.com/repos/nbafrank/uvr-r/tarball/{COMPANION_SHA}");
        let download_ok = (|| -> std::result::Result<(), Box<dyn std::error::Error>> {
            let resp = ureq::get(&url).header("User-Agent", "uvr").call()?;
            let bytes = resp.into_body().read_to_vec()?;
            std::fs::write(&tarball, &bytes)?;
            Ok(())
        })();

        if download_ok.is_err() {
            let _ = std::fs::remove_file(&tarball);
            return;
        }
    }

    // Verify SHA-256 checksum on every run (cache could be corrupted or tampered with)
    if let Ok(bytes) = std::fs::read(&tarball) {
        use sha2::{Digest, Sha256};
        let hash = hex::encode(Sha256::digest(&bytes));
        if hash != COMPANION_HASH {
            tracing::warn!(
                "Companion package checksum mismatch (expected {}, got {}), skipping install",
                &COMPANION_HASH[..12],
                &hash[..12]
            );
            let _ = std::fs::remove_file(&tarball);
            return;
        }
    } else {
        return;
    }

    // Extract directly instead of spawning R CMD INSTALL (~400ms savings).
    // The GitHub tarball contains `owner-repo-sha/` at the top level with the
    // R package source inside. We extract it, find the package dir, and copy
    // the R/, DESCRIPTION, NAMESPACE files into library/uvr/.
    let install_ok = (|| -> std::result::Result<(), Box<dyn std::error::Error>> {
        let file = std::fs::File::open(&tarball)?;
        let gz = flate2::read::GzDecoder::new(file);
        let mut archive = tar::Archive::new(gz);

        let tmp_dir = tempfile::tempdir()?;
        archive.unpack(tmp_dir.path())?;

        // Find the package dir: top-level-dir/ contains R/, DESCRIPTION, NAMESPACE
        let pkg_src = std::fs::read_dir(tmp_dir.path())?
            .flatten()
            .find(|e| e.path().join("DESCRIPTION").exists())
            .map(|e| e.path())
            .ok_or("companion package dir not found in tarball")?;

        let dest = library.join("uvr");
        if dest.exists() {
            std::fs::remove_dir_all(&dest)?;
        }
        package_cache::copy_dir_recursive(&pkg_src, &dest)?;

        Ok(())
    })();

    if install_ok.is_ok() {
        println!("  {} uvr R companion package installed", style("✓").green(),);
    }
}

/// Check if the installed companion package was built under a different R major.minor.
fn companion_needs_rebuild(desc_path: &std::path::Path, current_r_version: &str) -> bool {
    let desc = match std::fs::read_to_string(desc_path) {
        Ok(d) => d,
        Err(_) => return true,
    };

    // Extract "Built: R x.y.z; ..." line from DESCRIPTION
    let built_version = desc.lines().find_map(|line| {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Built:") {
            // Format: "R 4.5.3; ; 2026-04-03 ..."
            let rest = rest.trim();
            rest.strip_prefix("R ")
                .and_then(|v| v.split(';').next())
                .map(|v| v.trim().to_string())
        } else {
            None
        }
    });

    let built_minor = match built_version {
        Some(v) => r_minor(&v),
        None => return true, // No Built field — can't verify, rebuild to be safe
    };

    let current_minor = r_minor(current_r_version);

    built_minor != current_minor
}

fn is_installed(pkg: &LockedPackage, library: &std::path::Path) -> bool {
    let desc_path = library.join(&pkg.name).join("DESCRIPTION");
    let Ok(content) = std::fs::read_to_string(&desc_path) else {
        return false;
    };
    let fields = uvr_core::dcf::parse_dcf_fields(&content);
    match fields.get("Version") {
        Some(v) => {
            let installed = v.trim();
            installed == pkg.version
                || uvr_core::resolver::normalize_version(installed) == pkg.version
                || pkg.raw_version.as_deref() == Some(installed)
        }
        None => false,
    }
}

/// Read the installed version of a package from its DESCRIPTION, or None if not installed.
fn installed_version(name: &str, library: &std::path::Path) -> Option<String> {
    let desc_path = library.join(name).join("DESCRIPTION");
    let content = std::fs::read_to_string(&desc_path).ok()?;
    let fields = uvr_core::dcf::parse_dcf_fields(&content);
    fields.get("Version").map(|v| v.trim().to_string())
}

/// Compare two lockfiles for semantic equivalence, ignoring fields that can
/// legitimately differ between lockfile versions (e.g. `url`, `checksum`).
/// Compares: R major.minor version + set of (name, version, source, requires) tuples.
fn lockfiles_equivalent(
    a: &uvr_core::lockfile::Lockfile,
    b: &uvr_core::lockfile::Lockfile,
) -> bool {
    if r_minor(&a.r.version) != r_minor(&b.r.version) {
        return false;
    }
    if a.r.bioc_version != b.r.bioc_version {
        return false;
    }
    if a.packages.len() != b.packages.len() {
        return false;
    }
    let mut a_pkgs: Vec<_> = a.packages.iter().collect();
    let mut b_pkgs: Vec<_> = b.packages.iter().collect();
    a_pkgs.sort_by(|x, y| x.name.cmp(&y.name));
    b_pkgs.sort_by(|x, y| x.name.cmp(&y.name));
    a_pkgs.iter().zip(b_pkgs.iter()).all(|(ap, bp)| {
        let mut a_reqs = ap.requires.clone();
        let mut b_reqs = bp.requires.clone();
        a_reqs.sort();
        b_reqs.sort();
        ap.name == bp.name && ap.version == bp.version && ap.source == bp.source && a_reqs == b_reqs
    })
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
        PackageSource::Custom { .. } => {
            // Custom repo packages should always have a stored URL from resolution.
            // Fall back to empty if somehow missing.
            String::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uvr_core::lockfile::{LockedPackage, Lockfile, PackageSource, RVersionPin};

    #[test]
    fn r_minor_three_component() {
        assert_eq!(r_minor("4.4.2"), "4.4");
    }

    #[test]
    fn r_minor_two_component() {
        assert_eq!(r_minor("4.4"), "4.4");
    }

    #[test]
    fn r_minor_single_component() {
        assert_eq!(r_minor("4"), "4");
    }

    #[test]
    fn looks_like_version_valid() {
        assert!(looks_like_version("4.5.3"));
        assert!(looks_like_version("4.4"));
        assert!(looks_like_version("3.6.3"));
    }

    #[test]
    fn looks_like_version_invalid() {
        assert!(!looks_like_version(""));
        assert!(!looks_like_version(">=4.0.0"));
        assert!(!looks_like_version("*"));
    }

    #[test]
    fn lockfiles_equivalent_identical() {
        let lf = Lockfile {
            r: RVersionPin {
                version: "4.4.2".into(),
                bioc_version: None,
            },
            packages: vec![LockedPackage {
                name: "jsonlite".into(),
                version: "1.8.8".into(),
                raw_version: None,
                source: PackageSource::Cran,
                checksum: Some("md5:abc".into()),
                requires: vec!["methods".into()],
                url: Some("https://cran.r-project.org/test".into()),
                system_requirements: None,
                dev: false,
            }],
        };
        assert!(lockfiles_equivalent(&lf, &lf));
    }

    #[test]
    fn lockfiles_equivalent_ignores_url_and_checksum() {
        let lf1 = Lockfile {
            r: RVersionPin {
                version: "4.4.2".into(),
                bioc_version: None,
            },
            packages: vec![LockedPackage {
                name: "jsonlite".into(),
                version: "1.8.8".into(),
                raw_version: None,
                source: PackageSource::Cran,
                checksum: Some("md5:abc".into()),
                requires: vec![],
                url: Some("https://example.com/old".into()),
                system_requirements: None,
                dev: false,
            }],
        };
        let lf2 = Lockfile {
            r: RVersionPin {
                version: "4.4.2".into(),
                bioc_version: None,
            },
            packages: vec![LockedPackage {
                name: "jsonlite".into(),
                version: "1.8.8".into(),
                raw_version: None,
                source: PackageSource::Cran,
                checksum: Some("md5:xyz".into()),
                requires: vec![],
                url: Some("https://example.com/new".into()),
                system_requirements: None,
                dev: false,
            }],
        };
        assert!(lockfiles_equivalent(&lf1, &lf2));
    }

    #[test]
    fn lockfiles_not_equivalent_different_version() {
        let make = |ver: &str| Lockfile {
            r: RVersionPin {
                version: "4.4.2".into(),
                bioc_version: None,
            },
            packages: vec![LockedPackage {
                name: "jsonlite".into(),
                version: ver.into(),
                raw_version: None,
                source: PackageSource::Cran,
                checksum: None,
                requires: vec![],
                url: None,
                system_requirements: None,
                dev: false,
            }],
        };
        assert!(!lockfiles_equivalent(&make("1.8.7"), &make("1.8.8")));
    }

    #[test]
    fn lockfiles_not_equivalent_different_r_minor() {
        let make = |r_ver: &str| Lockfile {
            r: RVersionPin {
                version: r_ver.into(),
                bioc_version: None,
            },
            packages: vec![],
        };
        assert!(!lockfiles_equivalent(&make("4.3.2"), &make("4.4.2")));
    }

    #[test]
    fn lockfiles_equivalent_same_r_minor() {
        let make = |r_ver: &str| Lockfile {
            r: RVersionPin {
                version: r_ver.into(),
                bioc_version: None,
            },
            packages: vec![],
        };
        // Same minor → equivalent
        assert!(lockfiles_equivalent(&make("4.4.1"), &make("4.4.2")));
    }

    #[test]
    fn lockfiles_not_equivalent_different_requires() {
        let make = |requires: Vec<String>| Lockfile {
            r: RVersionPin {
                version: "4.4.2".into(),
                bioc_version: None,
            },
            packages: vec![LockedPackage {
                name: "ggplot2".into(),
                version: "3.5.1".into(),
                raw_version: None,
                source: PackageSource::Cran,
                checksum: None,
                requires,
                url: None,
                system_requirements: None,
                dev: false,
            }],
        };
        assert!(!lockfiles_equivalent(
            &make(vec!["rlang".into()]),
            &make(vec!["rlang".into(), "scales".into()])
        ));
    }

    #[test]
    fn source_url_cran() {
        let pkg = LockedPackage {
            name: "jsonlite".into(),
            version: "1.8.8".into(),
            raw_version: None,
            source: PackageSource::Cran,
            checksum: None,
            requires: vec![],
            url: None,
            system_requirements: None,
            dev: false,
        };
        let url = source_url(&pkg, None);
        assert_eq!(
            url,
            "https://cran.r-project.org/src/contrib/jsonlite_1.8.8.tar.gz"
        );
    }

    #[test]
    fn source_url_uses_raw_version() {
        let pkg = LockedPackage {
            name: "scales".into(),
            version: "1.1.3".into(),
            raw_version: Some("1.1-3".into()),
            source: PackageSource::Cran,
            checksum: None,
            requires: vec![],
            url: None,
            system_requirements: None,
            dev: false,
        };
        let url = source_url(&pkg, None);
        assert!(url.contains("scales_1.1-3.tar.gz"));
    }

    #[test]
    fn source_url_prefers_stored_url() {
        let pkg = LockedPackage {
            name: "jsonlite".into(),
            version: "1.8.8".into(),
            raw_version: None,
            source: PackageSource::Cran,
            checksum: None,
            requires: vec![],
            url: Some("https://custom-mirror.org/jsonlite.tar.gz".into()),
            system_requirements: None,
            dev: false,
        };
        let url = source_url(&pkg, None);
        assert_eq!(url, "https://custom-mirror.org/jsonlite.tar.gz");
    }

    #[test]
    fn source_url_bioconductor() {
        let pkg = LockedPackage {
            name: "DESeq2".into(),
            version: "1.42.0".into(),
            raw_version: None,
            source: PackageSource::Bioconductor,
            checksum: None,
            requires: vec![],
            url: None,
            system_requirements: None,
            dev: false,
        };
        let url = source_url(&pkg, Some("3.20"));
        assert!(url.contains("bioconductor.org"));
        assert!(url.contains("3.20"));
        assert!(url.contains("DESeq2_1.42.0.tar.gz"));
    }

    #[test]
    fn source_url_github_empty() {
        let pkg = LockedPackage {
            name: "mypkg".into(),
            version: "0.1.0".into(),
            raw_version: None,
            source: PackageSource::GitHub,
            checksum: None,
            requires: vec![],
            url: None,
            system_requirements: None,
            dev: false,
        };
        assert!(source_url(&pkg, None).is_empty());
    }

    #[test]
    fn is_installed_check() {
        let dir = tempfile::TempDir::new().unwrap();
        let pkg = LockedPackage {
            name: "jsonlite".into(),
            version: "1.8.8".into(),
            raw_version: None,
            source: PackageSource::Cran,
            checksum: None,
            requires: vec![],
            url: None,
            system_requirements: None,
            dev: false,
        };

        // Not installed
        assert!(!is_installed(&pkg, dir.path()));

        // Create dir without DESCRIPTION → not installed
        std::fs::create_dir_all(dir.path().join("jsonlite")).unwrap();
        assert!(!is_installed(&pkg, dir.path()));

        // Create DESCRIPTION with matching version → installed
        std::fs::write(
            dir.path().join("jsonlite").join("DESCRIPTION"),
            "Package: jsonlite\nVersion: 1.8.8\n",
        )
        .unwrap();
        assert!(is_installed(&pkg, dir.path()));

        // Wrong version → not installed
        std::fs::write(
            dir.path().join("jsonlite").join("DESCRIPTION"),
            "Package: jsonlite\nVersion: 1.7.0\n",
        )
        .unwrap();
        assert!(!is_installed(&pkg, dir.path()));

        // Dash version in DESCRIPTION matches normalized lockfile version
        let dash_pkg = LockedPackage {
            name: "scales".into(),
            version: "1.1.3".into(), // normalized
            raw_version: Some("1.1-3".into()),
            source: PackageSource::Cran,
            checksum: None,
            requires: vec![],
            url: None,
            system_requirements: None,
            dev: false,
        };
        std::fs::create_dir_all(dir.path().join("scales")).unwrap();
        std::fs::write(
            dir.path().join("scales").join("DESCRIPTION"),
            "Package: scales\nVersion: 1.1-3\n",
        )
        .unwrap();
        assert!(is_installed(&dash_pkg, dir.path()));

        // Dash version without raw_version still matches via normalization
        let dash_pkg_no_raw = LockedPackage {
            name: "scales".into(),
            version: "1.1.3".into(),
            raw_version: None,
            source: PackageSource::Cran,
            checksum: None,
            requires: vec![],
            url: None,
            system_requirements: None,
            dev: false,
        };
        assert!(is_installed(&dash_pkg_no_raw, dir.path()));
    }
}
