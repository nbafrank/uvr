use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};

use uvr_core::installer::binary_install::{install_binary_package, patch_installed_so_files};
use uvr_core::installer::download::{DownloadSpec, Downloader};
use uvr_core::installer::package_cache;
use uvr_core::installer::r_cmd_install::RCmdInstall;
use uvr_core::lockfile::{LockedPackage, Lockfile};
use uvr_core::project::Project;
use uvr_core::r_version::detector::{find_r_binary, query_r_version};
use uvr_core::r_version::downloader::{
    patch_r_dylibs, patch_r_executables, patch_renviron_site, Platform,
};
use uvr_core::registry::p3m::P3MBinaryIndex;
use uvr_core::resolver::topological_install_order;

use crate::ui;
use crate::ui::palette;

pub async fn run(
    frozen: bool,
    no_dev: bool,
    jobs: usize,
    library: Option<PathBuf>,
    timeout: Option<Duration>,
) -> Result<()> {
    let project = Project::find_cwd().context("Not inside a uvr project")?;
    // CLI --library takes precedence, then UVR_LIBRARY env var.
    let library = library.or_else(|| std::env::var("UVR_LIBRARY").ok().map(PathBuf::from));
    run_inner(&project, frozen, no_dev, jobs, library.as_deref(), timeout).await
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
    timeout: Option<Duration>,
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

    // Add uvr entries to .Rbuildignore only when DESCRIPTION has `Package:`
    // (real R package source tree). DESCRIPTION may have been created after
    // `uvr init`, so we check on every sync.
    if crate::commands::init::is_r_package_dir(&project.root) {
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
            ui::bullet_dim(format!("Skipping {skipped} dev-only package(s)"));
        }
        filtered
    } else {
        lockfile
    };

    install_from_lockfile(project, &lockfile, jobs, library_override, timeout).await
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
    timeout: Option<Duration>,
) -> Result<()> {
    let library = library_override
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| project.library_path());

    // Resolve R binary + version once (spawning R is ~250ms, so avoid repeating).
    let r_constraint = project.manifest.project.r_version.as_deref();
    let r_info: Option<(PathBuf, String)> = find_r_binary(r_constraint)
        .ok()
        .and_then(|bin| query_r_version(&bin).map(|ver| (bin, ver)));

    // Detect R version mismatch in two places:
    //   (a) lockfile R minor vs current R → user retargeted lockfile but library is stale.
    //   (b) library sentinel R minor vs current R → user upgraded R out from under the library
    //       even though the lockfile already reflects the new R (issue #66).
    // (b) is the load-bearing check on its own; (a) stays for the case where there is no
    // sentinel yet (older libraries, fresh checkouts that ran `uvr lock` before `uvr sync`).
    if let Some((_, ref current_r)) = r_info {
        let current_minor = r_minor(current_r);
        let locked_minor = r_minor(&lockfile.r.version);
        let sentinel_minor = read_library_r_sentinel(&library);

        // #70 guard — must fire BEFORE the wipe check, not inside it. A library
        // already at `current_minor` (sentinel matches) skips the wipe but still
        // installs new packages under the resolved R. From a calling R session
        // on a different minor, those packages are unloadable in the live
        // session. Bail unconditionally when the calling R differs from the
        // R uvr would install for. Terminal-invoked uvr (R_HOME unset) returns
        // None from calling_r_minor() and proceeds normally.
        if let Some(calling_minor) = calling_r_minor() {
            if calling_minor != current_minor {
                anyhow::bail!(
                    "Refusing to install: uvr is running inside R {calling} but the project pin/lockfile \
                     resolves to R {target}. Packages built for R {target} would not load in this {calling} \
                     session. Restart R against {target} (e.g. point your IDE at \
                     ~/.uvr/r-versions/{target_full}/bin/R), or update the pin to match this session, \
                     then re-run `uvr sync`.",
                    calling = calling_minor,
                    target = current_minor,
                    target_full = current_r,
                );
            }
        }

        let lockfile_mismatch =
            looks_like_version(&lockfile.r.version) && current_minor != locked_minor;
        let sentinel_mismatch = sentinel_minor.as_ref().is_some_and(|m| m != &current_minor);

        if lockfile_mismatch || sentinel_mismatch {
            let from = sentinel_minor.unwrap_or(locked_minor);
            ui::warn(format!(
                "R version changed ({} {} {}) — wiping library and reinstalling",
                palette::dim(&from),
                palette::dim(ui::glyph::arrow()),
                palette::info(&current_minor),
            ));
            if library.exists() {
                std::fs::remove_dir_all(&library).context("Failed to wipe project library")?;
            }
            std::fs::create_dir_all(&library).context("Failed to recreate library directory")?;
        }
    }

    let start = ui::now();

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
        if let Some((ref r_bin, ref current_r)) = r_info {
            ensure_companion_package(&library, current_r, r_bin);
        }
    }

    if to_install.is_empty() {
        if let Some((_, ref current_r)) = r_info {
            write_library_r_sentinel(&library, &r_minor(current_r));
        }
        ui::summary(
            "Everything is up to date",
            format!(
                "{} package(s) in {}",
                lockfile.packages.len(),
                palette::format_duration(start.elapsed())
            ),
        );
        return Ok(());
    }

    // Show what's changing: new installs vs upgrades.
    let mut new_count = 0usize;
    let mut upgrade_count = 0usize;
    for pkg in &to_install {
        let old_ver = installed_version(&pkg.name, &library);
        if let Some(old) = &old_ver {
            ui::row_upgrade(&pkg.name, old, &pkg.version);
            upgrade_count += 1;
        } else {
            new_count += 1;
        }
    }

    let client = crate::commands::util::build_client()?;

    // On Linux, check for missing system dependencies before installing.
    #[cfg(target_os = "linux")]
    {
        use uvr_core::sysreqs;

        if let Some(distro) = sysreqs::detect_linux_distro() {
            let queries: Vec<sysreqs::PackageSysReqQuery> = to_install
                .iter()
                .map(|p| sysreqs::PackageSysReqQuery {
                    name: p.name.clone(),
                    system_requirements: p.system_requirements.clone(),
                })
                .collect();

            if !queries.is_empty() {
                let check = sysreqs::check_system_deps(&client, &queries, &distro).await;

                if check.unsupported_distro && check.missing.is_empty() {
                    // PPM doesn't cover this distro AND the local fallback
                    // found nothing (either no SystemRequirements present or
                    // no rule matched). Tell the user we skipped the check
                    // rather than silently claiming everything is fine.
                    eprintln!();
                    ui::warn_block(
                        &format!("System dependency check skipped on {distro}"),
                        vec![
                            "Posit's sysreqs catalog doesn't cover this distribution, and the local fallback had no applicable rules.".to_string(),
                            "Packages with system-library requirements may fail to compile from source.".to_string(),
                        ],
                    );
                    ui::hint(
                        "Install build prerequisites manually (e.g. libxml2-dev, libcurl-dev, libssl-dev) if source builds fail.",
                    );
                    eprintln!();
                } else if !check.missing.is_empty() {
                    let missing = &check.missing;
                    let all_pkgs: Vec<&str> = missing
                        .values()
                        .flat_map(|reqs| reqs.iter().map(|r| r.package.as_str()))
                        .collect::<std::collections::BTreeSet<&str>>()
                        .into_iter()
                        .collect();

                    eprintln!();
                    // Structured warning with a loud `⚠ WARN` header and one
                    // bullet per package → missing deps. The user's fix is
                    // delivered as a proper hint below, not an extra warn line.
                    let body: Vec<String> = missing
                        .iter()
                        .map(|(pkg_name, reqs)| {
                            let names: Vec<&str> =
                                reqs.iter().map(|r| r.package.as_str()).collect();
                            format!("{pkg_name} needs: {}", names.join(", "))
                        })
                        .collect();
                    ui::warn_block(
                        &format!(
                            "Missing system dependencies for {} package(s)",
                            missing.len()
                        ),
                        body,
                    );
                    let install_cmd = if which::which("apk").is_ok() {
                        format!("apk add {}", all_pkgs.join(" "))
                    } else if which::which("dnf").is_ok() {
                        format!("sudo dnf install -y {}", all_pkgs.join(" "))
                    } else {
                        format!("sudo apt-get install -y {}", all_pkgs.join(" "))
                    };
                    ui::hint(format!("Install with: {install_cmd}"));
                    ui::hint("Continuing — some packages may fail to compile without these.");
                    eprintln!();
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
                patch_r_executables(r_home);
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

    // Linux-specific UA. PPM serves source vs. binary at the same URL, gated
    // by the User-Agent. The index fetch in `P3MBinaryIndex::fetch` sets this
    // UA on its own request; we need to set the same UA on the per-package
    // tarball downloads or PPM serves source for those even though the index
    // told us they were binary (would extract a source tree as if it were a
    // binary package — silent breakage). Built once and attached per-spec
    // below.
    let detected_platform = Platform::detect();
    let linux_ppm_user_agent: Option<String> = match detected_platform {
        Ok(p) if matches!(p, Platform::LinuxX86_64 | Platform::LinuxArm64) => {
            let slug = uvr_core::r_version::downloader::detect_posit_distro_slug();
            uvr_core::registry::p3m::ppm_linux_codename(&slug).map(|_| {
                let arch = if matches!(p, Platform::LinuxX86_64) {
                    "x86_64"
                } else {
                    "aarch64"
                };
                // R version is the project's actual minor, not a hardcoded
                // string — future-proofs the UA against PPM tightening its
                // sniffing rules.
                format!("R ({r_minor_str}.0 {arch}-pc-linux-gnu {arch} linux-gnu)")
            })
        }
        _ => None,
    };

    let plans: Vec<PkgPlan> = if !cache_misses.is_empty() {
        let p3m = match detected_platform {
            Ok(platform) => {
                // #55: Linux gets binaries via PPM's `__linux__/<codename>` URL
                // space, gated by a User-Agent the registry sets internally.
                // The slug is the same string we use for R install URLs;
                // platform_info() inside p3m.rs translates it to PPM's
                // codename system. None for non-Linux platforms.
                let slug = if matches!(platform, Platform::LinuxX86_64 | Platform::LinuxArm64) {
                    Some(uvr_core::r_version::downloader::detect_posit_distro_slug())
                } else {
                    None
                };
                P3MBinaryIndex::fetch(
                    &client,
                    &r_minor_str,
                    platform,
                    bioc_release,
                    slug.as_deref(),
                )
                .await
            }
            Err(_) => P3MBinaryIndex::empty(),
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

    // Compact plan line: "3 cached · 4 binary · 1 from source"
    if cache_hit_count > 0 || binary_count > 0 || source_count > 0 {
        let mut parts = Vec::new();
        if cache_hit_count > 0 {
            parts.push(format!("{cache_hit_count} cached"));
        }
        if binary_count > 0 {
            parts.push(format!("{binary_count} binary"));
        }
        if source_count > 0 {
            parts.push(format!("{source_count} from source"));
        }
        let sep = format!(" {} ", ui::glyph::bullet());
        let action = match (new_count, upgrade_count) {
            (n, 0) => format!("Installing {n} package(s)"),
            (0, u) => format!("Upgrading {u} package(s)"),
            (n, u) => format!("Installing {n}, upgrading {u}"),
        };
        // Colon between the action and the breakdown reads cleaner than a bullet,
        // which visually duplicates the separators inside `parts` — especially in
        // ASCII mode where `bullet()` renders as `.` and the line ends up looking
        // like "Installing 116 package(s) . 111 binary . 5 from source".
        ui::info(format!("{}: {}", action, palette::dim(parts.join(&sep))));
    }

    if !plans.is_empty() {
        let specs: Vec<DownloadSpec> = plans
            .iter()
            .map(|p| DownloadSpec {
                pkg: p.pkg,
                url: &p.url,
                fallback_url: p.fallback_url.as_deref(),
                is_binary: p.is_binary,
                // Attach the R-shaped UA only for Linux PPM binary URLs so
                // PPM serves the binary tarball, not source. macOS / Windows
                // / source-fallback paths leave it None (default uvr UA).
                user_agent: if p.is_binary && p.url.contains("/__linux__/") {
                    linux_ppm_user_agent.as_deref()
                } else {
                    None
                },
            })
            .collect();

        let downloader = Downloader::new(client, cache_dir, jobs);
        let results = downloader
            .download_all(&specs)
            .await
            .context("Download failed")?;

        let installer = RCmdInstall::new(r_binary.to_string_lossy());

        // Aggregate progress bar — one line for the whole install phase.
        let total = plans.len() as u64;
        let pb = ui::make_aggregate_bar(total);
        for (plan, result) in plans.iter().zip(results.iter()) {
            let verb = if result.used_binary {
                "installing"
            } else {
                "compiling"
            };
            pb.set_message(format!("{verb} {} {}", plan.pkg.name, plan.pkg.version));

            if result.used_binary {
                install_binary_package(
                    &result.path,
                    &library,
                    &plan.pkg.name,
                    libr_path.as_deref(),
                )
                .with_context(|| format!("Failed to install {}", plan.pkg.name))?;
            } else {
                // For source compilation, surface the last build line as the bar message.
                let name = plan.pkg.name.clone();
                let version = plan.pkg.version.clone();
                let pb_for_closure = pb.clone();
                installer
                    .install_streaming(&result.path, &library, &plan.pkg.name, timeout, |line| {
                        let short: String = line.chars().take(50).collect();
                        pb_for_closure.set_message(format!("compiling {name} {version} ({short})"));
                    })
                    .with_context(|| format!("Failed to install {}", plan.pkg.name))?;
            }

            pb.inc(1);

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
        pb.finish_and_clear();
    }

    // Final summary: "✓ Ready — N packages in 1.8s" + cache hit rate subtitle.
    let total_count = to_install.len();
    let elapsed = palette::format_duration(start.elapsed());
    let headline = match (new_count, upgrade_count) {
        (n, 0) => format!("Installed {n} package(s) in {elapsed}"),
        (0, u) => format!("Upgraded {u} package(s) in {elapsed}"),
        (n, u) => format!("Installed {n}, upgraded {u} in {elapsed}"),
    };
    let mut sub_parts: Vec<String> = Vec::new();
    if total_count > 0 {
        let hit_pct = (cache_hit_count as f64 / total_count as f64 * 100.0).round() as u64;
        sub_parts.push(format!("{hit_pct}% cache hit"));
    }
    if binary_count > 0 {
        sub_parts.push(format!("{binary_count} binary"));
    }
    if source_count > 0 {
        sub_parts.push(format!("{source_count} from source"));
    }
    let sep = format!(" {} ", ui::glyph::bullet());
    ui::summary(headline, sub_parts.join(&sep));

    if let Some((_, ref current_r)) = r_info {
        write_library_r_sentinel(&library, &r_minor(current_r));
    }

    Ok(())
}

/// Pinned commit SHA and expected SHA-256 hash of the companion R package tarball.
///
/// IMPORTANT — `COMPANION_HASH` is the SHA-256 of the GitHub
/// `https://api.github.com/repos/<owner>/<repo>/tarball/<sha>` endpoint output,
/// **not** the `https://github.com/<owner>/<repo>/archive/<sha>.tar.gz` archive.
/// Both are gzipped tarballs of the same tree but use different compression
/// settings → different SHA-256. To compute a new hash:
///   curl -sL "https://api.github.com/repos/nbafrank/uvr-r/tarball/<sha>" | shasum -a 256
/// Mismatch is silently fatal: `ensure_companion_package` swallows install
/// failures and the user just doesn't get the companion R package.
const COMPANION_SHA: &str = "f20019c39d8ab16dd360632c0f44b7e6a947162d";
const COMPANION_HASH: &str = "1bc618215ad80666eea815d88f6bf53ca1c201f7883b970647c96eb18b677ffe";

/// Install the uvr R companion package from GitHub into the project library
/// if it's not already installed. Failures are silently ignored — the companion
/// package is a convenience, not a requirement.
///
/// Security: the download is pinned to an immutable commit SHA and verified
/// against a hardcoded SHA-256 hash, preventing supply-chain attacks via the
/// companion repo.
pub fn ensure_companion_package(
    library: &std::path::Path,
    current_r_version: &str,
    r_binary: &std::path::Path,
) {
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

    // Retry once if the first attempt fails with a bad cached tarball or a
    // transient download/install failure.
    let mut last_err: Option<String> = None;
    for attempt in 0..2 {
        match try_install_companion(library, &tarball, r_binary) {
            Ok(()) => {
                // #60: don't surface the install in the user-facing output —
                // by the time they see uvr::sync()'s output they've already
                // loaded the companion. Available under -v / --verbose.
                tracing::debug!("uvr R companion package installed");
                return;
            }
            Err(e) => {
                last_err = Some(e.to_string());
                // Force re-download on next attempt — wipe the cached tarball
                // regardless of hash, since any failure here means the cached
                // file is suspect (wrong hash, truncated, corrupted).
                let _ = std::fs::remove_file(&tarball);
                if attempt == 0 {
                    tracing::debug!("Companion install attempt 1 failed: {e}; retrying");
                }
            }
        }
    }

    ui::warn(format!(
        "Could not install the uvr R companion package automatically ({}).\n   \
         Install manually from R: remotes::install_github(\"nbafrank/uvr-r\", lib = .libPaths()[1])",
        last_err.as_deref().unwrap_or("unknown error"),
    ));
}

/// Attempt the download + verify + install cycle once. Returns the first
/// failure on any step. Caller decides retry policy.
///
/// Install uses `R CMD INSTALL` on the extracted source tree — required for
/// `library(uvr)` to work, since R's loader expects the compiled install
/// layout (`Meta/package.rds`, lazy-load `.rdb`/`.rdx`, help indices) that
/// only `R CMD INSTALL` produces. An earlier shortcut that directly copied
/// source files was ~400ms faster but produced a directory that looked
/// installed to `.Rprofile`'s package count but that R refused to load.
fn try_install_companion(
    library: &std::path::Path,
    tarball: &std::path::Path,
    r_binary: &std::path::Path,
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Download if cached tarball is missing (pinned SHA = immutable, no TTL needed).
    if !tarball.exists() {
        let url = format!("https://api.github.com/repos/nbafrank/uvr-r/tarball/{COMPANION_SHA}");
        let resp = ureq::get(&url).header("User-Agent", "uvr").call()?;
        let bytes = resp.into_body().read_to_vec()?;
        std::fs::write(tarball, &bytes)?;
    }

    // Verify SHA-256 checksum on every run (cache could be corrupted or tampered with).
    let bytes = std::fs::read(tarball)?;
    {
        use sha2::{Digest, Sha256};
        let hash = hex::encode(Sha256::digest(&bytes));
        if hash != COMPANION_HASH {
            return Err(format!(
                "companion tarball checksum mismatch (expected {}, got {})",
                &COMPANION_HASH[..12],
                &hash[..12]
            )
            .into());
        }
    }

    // R CMD INSTALL can take a tarball directly — it extracts, finds the
    // package dir by DESCRIPTION, and installs to --library. The GitHub
    // tarball has a `nbafrank-uvr-r-<sha>/` top-level dir, but R CMD INSTALL
    // keys on the Package: field from DESCRIPTION, so the installed dir ends
    // up correctly named `uvr/`.
    let installer =
        uvr_core::installer::r_cmd_install::RCmdInstall::new(r_binary.to_string_lossy());
    installer
        .install(tarball, library, "uvr")
        .map_err(|e| Box::<dyn std::error::Error>::from(e.to_string()))?;

    // Postcondition: Meta/package.rds is what `library()` checks first.
    let dest = library.join("uvr");
    if !dest.join("Meta").join("package.rds").exists() {
        return Err(format!(
            "R CMD INSTALL reported success but Meta/package.rds missing at {}",
            dest.display()
        )
        .into());
    }

    Ok(())
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

/// Major.minor of the R session that invoked uvr, if any. Read from
/// `R_HOME` (set by R when it spawns child processes); used by the sync
/// wipe guard to refuse a destructive rebuild that would strand the
/// calling R session with packages built for a different R (#70).
fn calling_r_minor() -> Option<String> {
    let r_home = std::env::var("R_HOME").ok()?;
    let r_name = if cfg!(windows) { "R.exe" } else { "R" };
    let bin = std::path::PathBuf::from(&r_home).join("bin").join(r_name);
    let ver = uvr_core::r_version::detector::query_r_version(&bin)?;
    Some(r_minor(&ver))
}

/// Path to the per-library sentinel that records which R minor the library
/// was last populated against. Used to detect cross-R-minor reuse (#66).
fn library_sentinel_path(library: &std::path::Path) -> std::path::PathBuf {
    library.join(".uvr-r-version")
}

/// Read the library's R-minor sentinel. Returns `None` when the sentinel is
/// absent (legacy library or fresh sync) or the file is malformed.
fn read_library_r_sentinel(library: &std::path::Path) -> Option<String> {
    let raw = std::fs::read_to_string(library_sentinel_path(library)).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Write the library's R-minor sentinel. Best-effort: failures are swallowed
/// (they only cost us the next-run safety net, not correctness today).
fn write_library_r_sentinel(library: &std::path::Path, minor: &str) {
    let _ = std::fs::create_dir_all(library);
    let _ = std::fs::write(library_sentinel_path(library), format!("{minor}\n"));
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
    fn library_sentinel_roundtrip() {
        let tmp = tempfile::TempDir::new().unwrap();
        let lib = tmp.path().join("library");
        assert!(read_library_r_sentinel(&lib).is_none());
        write_library_r_sentinel(&lib, "4.5");
        assert_eq!(read_library_r_sentinel(&lib).as_deref(), Some("4.5"));
        // Overwriting reflects the new value.
        write_library_r_sentinel(&lib, "4.6");
        assert_eq!(read_library_r_sentinel(&lib).as_deref(), Some("4.6"));
    }

    #[test]
    fn library_sentinel_handles_whitespace() {
        let tmp = tempfile::TempDir::new().unwrap();
        let lib = tmp.path().join("library");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(library_sentinel_path(&lib), "  4.5\n\n").unwrap();
        assert_eq!(read_library_r_sentinel(&lib).as_deref(), Some("4.5"));
    }

    #[test]
    fn library_sentinel_empty_returns_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        let lib = tmp.path().join("library");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(library_sentinel_path(&lib), "").unwrap();
        assert!(read_library_r_sentinel(&lib).is_none());
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
