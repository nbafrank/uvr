use std::path::{Path, PathBuf};
use std::process::Command;

use tracing::info;

use crate::error::{Result, UvrError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOsArm64,
    MacOsX86_64,
    LinuxX86_64,
    LinuxArm64,
    WindowsX86_64,
}

impl Platform {
    pub fn detect() -> Result<Self> {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        return Ok(Platform::MacOsArm64);
        #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
        return Ok(Platform::MacOsX86_64);
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        return Ok(Platform::LinuxX86_64);
        #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
        return Ok(Platform::LinuxArm64);
        #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
        return Ok(Platform::WindowsX86_64);
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        Err(UvrError::UnsupportedPlatform(format!(
            "{}/{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        )))
    }

    /// Return the Rust target triple for this platform (e.g. `"aarch64-apple-darwin"`).
    pub fn rust_target_triple(&self) -> &'static str {
        match self {
            Platform::MacOsArm64 => "aarch64-apple-darwin",
            Platform::MacOsX86_64 => "x86_64-apple-darwin",
            Platform::LinuxX86_64 => "x86_64-unknown-linux-gnu",
            Platform::LinuxArm64 => "aarch64-unknown-linux-gnu",
            Platform::WindowsX86_64 => "x86_64-pc-windows-msvc",
        }
    }

    pub fn is_windows(&self) -> bool {
        matches!(self, Platform::WindowsX86_64)
    }

    pub fn is_macos(&self) -> bool {
        matches!(self, Platform::MacOsArm64 | Platform::MacOsX86_64)
    }

    /// Return the download URL for a given R version.
    pub fn download_url(&self, version: &str) -> String {
        match self {
            Platform::MacOsArm64 => format!(
                "https://cran.r-project.org/bin/macosx/{}/base/R-{version}-arm64.pkg",
                macos_arm64_dir()
            ),
            Platform::MacOsX86_64 => format!(
                "https://cran.r-project.org/bin/macosx/{}/base/R-{version}-x86_64.pkg",
                macos_x86_64_dir()
            ),
            Platform::LinuxX86_64 => {
                let distro = detect_posit_distro_slug();
                format!("https://cdn.posit.co/r/{distro}/pkgs/r-{version}_1_amd64.deb")
            }
            Platform::LinuxArm64 => {
                let distro = detect_posit_distro_slug();
                format!("https://cdn.posit.co/r/{distro}/pkgs/r-{version}_1_arm64.deb")
            }
            Platform::WindowsX86_64 => {
                format!("https://cran.r-project.org/bin/windows/base/R-{version}-win.exe")
            }
        }
    }

    /// Fallback URL when the primary 4xx's.
    ///
    /// - Windows: older R releases live at `/base/old/<version>/` (CRAN moves them out of `/base/`).
    /// - macOS Sonoma+: CRAN also publishes a `big-sur-arm64/` (resp. `big-sur-x86_64/`) dir
    ///   that holds older R versions not yet rebuilt for Sonoma. We try that as a fallback.
    /// - macOS pre-Sonoma: no fallback to `sonoma-*` because those binaries require macOS 14+
    ///   and won't run.
    /// - Linux uses the Posit CDN which hosts all versions at the same path.
    pub fn download_url_fallback(&self, version: &str) -> Option<String> {
        match self {
            Platform::WindowsX86_64 => Some(format!(
                "https://cran.r-project.org/bin/windows/base/old/{version}/R-{version}-win.exe"
            )),
            Platform::MacOsArm64 if macos_major_version() >= 14 => Some(format!(
                "https://cran.r-project.org/bin/macosx/big-sur-arm64/base/R-{version}-arm64.pkg"
            )),
            Platform::MacOsX86_64 if macos_major_version() >= 14 => Some(format!(
                "https://cran.r-project.org/bin/macosx/big-sur-x86_64/base/R-{version}-x86_64.pkg"
            )),
            Platform::MacOsArm64
            | Platform::MacOsX86_64
            | Platform::LinuxX86_64
            | Platform::LinuxArm64 => None,
        }
    }

    /// Where to find the directory listing for available R versions.
    /// Used to build a helpful error message when a requested version 404s.
    pub fn directory_listing_url(&self) -> Option<String> {
        match self {
            Platform::MacOsArm64 => Some(format!(
                "https://cran.r-project.org/bin/macosx/{}/base/",
                macos_arm64_dir()
            )),
            Platform::MacOsX86_64 => Some(format!(
                "https://cran.r-project.org/bin/macosx/{}/base/",
                macos_x86_64_dir()
            )),
            Platform::WindowsX86_64 => {
                Some("https://cran.r-project.org/bin/windows/base/".to_string())
            }
            // Posit CDN doesn't expose a directory listing.
            Platform::LinuxX86_64 | Platform::LinuxArm64 => None,
        }
    }
}

/// Detect macOS major version (Sonoma=14, Ventura=13, Big Sur=11). Cached.
/// Returns 0 on non-macOS platforms; falls back to 11 if `sw_vers` fails on macOS.
#[cfg(target_os = "macos")]
fn macos_major_version() -> u32 {
    use std::sync::OnceLock;
    static CACHE: OnceLock<u32> = OnceLock::new();
    *CACHE.get_or_init(|| {
        Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.trim().split('.').next()?.parse::<u32>().ok())
            .unwrap_or(11)
    })
}

#[cfg(not(target_os = "macos"))]
fn macos_major_version() -> u32 {
    0
}

/// CRAN subdir for arm64 binaries on the running macOS.
/// Sonoma (14) and later: `sonoma-arm64`. Earlier: `big-sur-arm64`.
fn macos_arm64_dir() -> &'static str {
    if macos_major_version() >= 14 {
        "sonoma-arm64"
    } else {
        "big-sur-arm64"
    }
}

/// CRAN subdir for x86_64 binaries on the running macOS.
/// Sonoma (14) and later: `sonoma-x86_64`. Earlier: `big-sur-x86_64`.
fn macos_x86_64_dir() -> &'static str {
    if macos_major_version() >= 14 {
        "sonoma-x86_64"
    } else {
        "big-sur-x86_64"
    }
}

/// Process-wide override for the Posit CDN distro slug. Set by
/// `uvr r install --distribution <slug>` before invoking the downloader,
/// for users on Linux distros uvr can't autodetect (e.g. PopOS, Manjaro,
/// other Ubuntu/Arch derivatives — see #54).
static DISTRO_OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Set the Posit CDN distro slug for the rest of this process. **Write-once**:
/// subsequent calls are silently ignored (`OnceLock::set` returns `Err`). This
/// matches the CLI's one-shot model — `uvr r install --distribution X` runs
/// once per process. Library consumers that need per-call overrides must run
/// each in a separate process.
///
/// Slug examples: `"ubuntu-2204"`, `"debian-12"`, `"rhel-9"`.
pub fn set_posit_distro_override(slug: String) {
    let _ = DISTRO_OVERRIDE.set(slug);
}

/// Detect the Posit CDN distro slug from `/etc/os-release`, or use the
/// override set by [`set_posit_distro_override`] if any.
///
/// Returns strings like `"ubuntu-2204"`, `"ubuntu-2404"`, `"debian-12"`,
/// `"centos-7"`, `"rhel-9"`, `"opensuse-154"`. Falls back to `"ubuntu-2204"`.
pub fn detect_posit_distro_slug() -> String {
    if let Some(override_slug) = DISTRO_OVERRIDE.get() {
        return override_slug.clone();
    }
    // Read os-release; fall back to default if anything fails
    let content = match std::fs::read_to_string("/etc/os-release") {
        Ok(c) => c,
        Err(_) => return "ubuntu-2204".to_string(),
    };

    let mut id = String::new();
    let mut version_id = String::new();
    for line in content.lines() {
        if let Some(val) = line.strip_prefix("ID=") {
            id = val.trim_matches('"').to_lowercase();
        } else if let Some(val) = line.strip_prefix("VERSION_ID=") {
            version_id = val.trim_matches('"').to_string();
        }
    }

    // Posit CDN uses no dots in version for Ubuntu/openSUSE, but keeps them for others
    match id.as_str() {
        "ubuntu" => {
            let ver = version_id.replace('.', "");
            format!("ubuntu-{ver}")
        }
        "debian" => format!("debian-{version_id}"),
        "centos" => format!("centos-{version_id}"),
        "rhel" | "rocky" | "almalinux" => {
            // Major version only
            let major = version_id.split('.').next().unwrap_or(&version_id);
            format!("rhel-{major}")
        }
        "opensuse-leap" | "sles" => {
            let ver = version_id.replace('.', "");
            format!("opensuse-{ver}")
        }
        _ => "ubuntu-2204".to_string(),
    }
}

/// Download and extract R to `~/.uvr/r-versions/<version>/`.
pub async fn download_and_install_r(
    client: &reqwest::Client,
    version: &str,
    platform: Platform,
) -> Result<PathBuf> {
    let install_dir = crate::config::r_versions_dir()
        .ok_or_else(|| UvrError::Other("Cannot determine r-versions directory".into()))?
        .join(version);

    let r_binary_name = if platform.is_windows() { "R.exe" } else { "R" };
    let r_binary = install_dir.join("bin").join(r_binary_name);
    if r_binary.exists() {
        info!("R {version} already installed at {}", install_dir.display());
        return Ok(install_dir);
    }

    let url = platform.download_url(version);
    info!("Downloading R {version} from {url}");

    let response = client.get(&url).send().await?;
    let bytes = if response.status().is_client_error() {
        // Older R versions live at a different URL on some mirrors
        // (e.g. Windows: /base/old/<ver>/).  Try fallback on any 4xx error,
        // not just 404, because some mirrors redirect to an error page.
        if let Some(fallback) = platform.download_url_fallback(version) {
            info!(
                "Primary URL returned {}, trying {fallback}",
                response.status()
            );
            let fallback_resp = client.get(&fallback).send().await?;
            if fallback_resp.status().is_success() {
                fallback_resp.bytes().await?
            } else {
                return Err(version_not_found_error(
                    client,
                    version,
                    platform,
                    fallback_resp.status(),
                )
                .await);
            }
        } else {
            return Err(
                version_not_found_error(client, version, platform, response.status()).await,
            );
        }
    } else {
        response.error_for_status()?.bytes().await?
    };

    std::fs::create_dir_all(&install_dir)?;

    match platform {
        Platform::MacOsArm64 | Platform::MacOsX86_64 => {
            install_r_macos(&bytes, version, &install_dir)?;
        }
        Platform::LinuxX86_64 | Platform::LinuxArm64 => {
            install_r_linux(&bytes, version, &install_dir)?;
        }
        Platform::WindowsX86_64 => {
            install_r_windows(&bytes, version, &install_dir)?;
        }
    }

    if !r_binary.exists() {
        return Err(UvrError::Other(format!(
            "R binary not found after installation at {}",
            r_binary.display()
        )));
    }

    info!("R {version} installed to {}", install_dir.display());
    Ok(install_dir)
}

/// Build a helpful error when CRAN/Posit returns 4xx for a requested R version.
/// Tries to list available versions from the platform's directory listing so
/// users see "latest available is 4.5.3" instead of just "404 Not Found".
async fn version_not_found_error(
    client: &reqwest::Client,
    version: &str,
    platform: Platform,
    status: reqwest::StatusCode,
) -> UvrError {
    // Best-effort version enumeration. If the listing fetch fails or the
    // platform has no listing endpoint, fall back to the generic message.
    let mut available_hint = String::new();
    if let Some(listing_url) = platform.directory_listing_url() {
        if let Ok(resp) = client.get(&listing_url).send().await {
            if let Ok(body) = resp.text().await {
                let mut versions: Vec<String> = scan_versions_from_listing(&body);
                versions.sort_by(|a, b| version_compare(a, b));
                versions.dedup();
                if let Some(latest) = versions.last() {
                    available_hint = format!("\nLatest available for your platform: {latest} (from {listing_url}).\nTry `uvr r install {latest}`, or `uvr r list --all` to see every published version.");
                }
            }
        }
    }
    if available_hint.is_empty() {
        available_hint = "\nCheck available versions with `uvr r list --all`. If R was just released, the upstream mirror may not have published the build for your platform yet — try again in a day.".to_string();
    }
    UvrError::Other(format!(
        "R {version} is not published for your platform (HTTP {status}).{available_hint}"
    ))
}

/// True when `s` looks like a real R version string (`X.Y.Z` with
/// all-digit components, optionally with a fourth `.W` for build
/// numbers). Rejects directory-listing artefacts like `..` (parent-dir
/// link) and `.` that pass a naive digits-and-dots check. Used by both
/// the directory-listing scraper in `fetch_available_versions` (uvr-r
/// #9) and `scan_versions_from_listing` to keep the version surface
/// clean.
///
/// Three-component minimum: CRAN's R 4+ has never been published as
/// `X.Y` only — every release is `X.Y.Z`. Tightening the lower bound
/// catches false-accepts that the prior 2-component allowance let
/// through.
fn is_real_r_version(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() < 3 || parts.len() > 4 {
        return false;
    }
    parts
        .iter()
        .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// Pull `R-X.Y.Z` version strings out of an HTML directory listing.
fn scan_versions_from_listing(body: &str) -> Vec<String> {
    use regex::Regex;
    let re = Regex::new(r"R-(\d+\.\d+\.\d+)(?:[-_])").unwrap();
    re.captures_iter(body)
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
        .collect()
}

/// Numeric comparison of "X.Y.Z" version strings. Non-numeric components
/// sort lexicographically as a fallback.
fn version_compare(a: &str, b: &str) -> std::cmp::Ordering {
    let parse =
        |s: &str| -> Vec<u32> { s.split('.').filter_map(|p| p.parse::<u32>().ok()).collect() };
    parse(a).cmp(&parse(b))
}

/// macOS: `.pkg` → xar → Payload (gzip+cpio) → extract
fn install_r_macos(pkg_bytes: &[u8], version: &str, dest: &Path) -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let pkg_path = tmp.path().join(format!("R-{version}.pkg"));
    std::fs::write(&pkg_path, pkg_bytes)?;

    // Step 1: pkgutil --expand R.pkg <expanded_dir>
    let expanded_dir = tmp.path().join("expanded");
    let out = Command::new("pkgutil")
        .args([
            "--expand",
            &pkg_path.to_string_lossy(),
            &expanded_dir.to_string_lossy(),
        ])
        .output()?;
    if !out.status.success() {
        return Err(UvrError::Other(format!(
            "pkgutil --expand failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }

    // Step 2: find ALL Payload files (the pkg is a product archive with multiple
    // component packages: R framework, texinfo, tcltk, GUI, …).
    // We try each Payload until we find one that contains a usable R binary.
    let payloads = find_all_payload_files(&expanded_dir)?;
    if payloads.is_empty() {
        return Err(UvrError::Other(
            "No Payload files found in expanded pkg".into(),
        ));
    }

    // Step 3 + 4: extract each Payload and look for bin/R
    let resources = payloads
        .iter()
        .find_map(|payload| {
            let stage_dir = tmp
                .path()
                .join(format!("stage-{}", payload.display().to_string().len()));
            std::fs::create_dir_all(&stage_dir).ok()?;
            let ok = Command::new("tar")
                .args([
                    "xf",
                    &payload.to_string_lossy(),
                    "-C",
                    &stage_dir.to_string_lossy(),
                ])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if !ok {
                return None;
            }
            find_dir_with_r_binary(&stage_dir, 0)
        })
        .ok_or_else(|| {
            UvrError::Other(format!(
                "Could not find bin/R in any of {} Payload(s) in the pkg",
                payloads.len()
            ))
        })?;
    info!("Found R Resources at {}", resources.display());

    // Step 5: copy Resources contents → dest.
    // -P: preserve symlinks as-is (don't follow/dereference them).
    //     fontconfig/fonts/conf.d contains symlinks pointing to system files that
    //     aren't in the extracted pkg — -P copies the symlink itself, not the target.
    std::fs::create_dir_all(dest)?;
    let src = format!("{}/.", resources.to_string_lossy());
    let out = Command::new("cp")
        .args(["-rP", &src, &dest.to_string_lossy()])
        .output()?;
    if !out.status.success() {
        return Err(UvrError::Other(format!(
            "Failed to copy R Resources to {}: {}",
            dest.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }

    // Step 6: patch all text files that reference the original R_HOME.
    // bin/R, etc/Makeconf, etc/ldpaths, etc/R, and others contain absolute
    // paths hardcoded at build time. R 4.5 builds use the Versions-prefixed
    // path everywhere (`/Library/Frameworks/R.framework/Versions/4.5-arm64/
    // Resources`); R 4.6 uses TWO prefixes — Versions-prefixed for
    // `R_HOME_DIR` and the bare `/Library/Frameworks/R.framework/Resources`
    // (the framework's `Current` symlink target) for `R_SHARE_DIR`,
    // `R_INCLUDE_DIR`, `R_DOC_DIR`. We patch both so that `R.home("share")`
    // resolves to our managed install at runtime — without it, source-package
    // installs on R 4.6 fail in `tools::makeLazyLoading` because
    // `nspackloader.R` is looked up under the framework path that doesn't
    // exist in our copy.
    let original_r_home = extract_r_home_dir(dest)?;
    let dest_str = dest.to_string_lossy();
    info!("Patching R_HOME: {} → {}", original_r_home, dest_str);

    // Iterate the set of known framework prefixes that CRAN bakes into bin/R
    // and friends. R 4.5 and earlier use only `R_HOME_DIR`'s
    // Versions-prefixed form everywhere; R 4.6 added the bare-Resources
    // form for the SHARE/INCLUDE/DOC env vars (the framework's
    // `Current`-symlink target). If a future CRAN build introduces a
    // third prefix variant, append it to this slice — guard against a
    // self-rewrite via the `dest_str` comparison.
    let prefixes: &[&str] = &[
        original_r_home.as_str(),
        "/Library/Frameworks/R.framework/Resources",
    ];
    let mut seen = std::collections::HashSet::new();
    for prefix in prefixes {
        if !seen.insert(*prefix) || *prefix == dest_str.as_ref() {
            continue;
        }
        patch_text_files(dest, prefix, &dest_str)?;
    }

    // Step 7: fix the LIBR line in etc/Makeconf.
    // After the text substitution above, LIBR still contains the framework path
    // (e.g. `-F/Library/Frameworks/R.framework/.. -framework R`) because it uses
    // the framework ROOT, not the Resources subdirectory.
    // Replace it with a plain `-L<dest>/lib -lR` that works from any location.
    patch_makeconf_libr(dest)?;

    // Step 8: fix libR.dylib's embedded install-name so packages compiled against
    // it can find the library at its new location without needing DYLD_LIBRARY_PATH.
    fix_libr_install_name(dest);

    // Step 9(a): patch ALL sibling dylibs (libRlapack, libRblas, libgfortran, …)
    // so they reference each other via managed-R paths rather than the original
    // CRAN framework paths.  Without this, symbols like `_dgebal_` (LAPACK) that
    // live in libRlapack/libRblas are invisible to R and packages like Matrix fail
    // to load with "Symbol not found: _dgebal_".
    patch_r_dylibs(dest);

    // Step 9(a.5): patch bin/exec/R (and siblings). R 4.6+ ships these with
    // hardened-runtime signatures pointing at the framework path; without
    // rewriting those load commands and re-signing ad-hoc, dyld can't find
    // libR.dylib and the process is SIGKILLed at startup. (Pre-4.6 used
    // ad-hoc signing so DYLD_LIBRARY_PATH was enough — 4.6 changed that.)
    patch_r_executables(dest);

    // Step 9(b): write etc/Renviron.site so that DYLD_LIBRARY_PATH is set for every
    // R process this installation spawns — including the fresh `R --slave` sessions
    // used by R CMD INSTALL for byte-compilation.  On macOS 15+ (SIP), DYLD_*
    // variables are stripped when inherited through /bin/sh, so setting the env
    // var on the parent process alone is not sufficient.
    // R expands ${R_HOME} in Renviron.site before any package code runs.
    write_renviron_site(dest)?;

    Ok(())
}

/// Read `<dest>/bin/R` and extract the value of `R_HOME_DIR=...`.
fn extract_r_home_dir(dest: &Path) -> Result<String> {
    let r_script = dest.join("bin").join("R");
    let content = std::fs::read_to_string(&r_script)?;
    for line in content.lines() {
        if let Some(val) = line.strip_prefix("R_HOME_DIR=") {
            return Ok(val.trim().to_string());
        }
    }
    // Fallback: standard macOS R framework path.
    Ok("/Library/Frameworks/R.framework/Resources".to_string())
}

/// Recursively replace `old` with `new` in every text file under `dir`.
/// Symlinks are skipped (they can't be patched and may be dangling).
///
/// Binary files are detected by the presence of a null byte and skipped entirely.
/// This prevents accidentally corrupting (and invalidating the code signature of)
/// Mach-O binaries or dylibs that may embed the path string as a constant.
fn patch_text_files(dir: &Path, old: &str, new: &str) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            patch_text_files(&path, old, new)?;
        } else if ft.is_file() {
            if let Ok(bytes) = std::fs::read(&path) {
                // Skip binary files — null bytes are a reliable indicator.
                if bytes.contains(&0) {
                    continue;
                }
                if let Ok(content) = String::from_utf8(bytes) {
                    if content.contains(old) {
                        std::fs::write(&path, content.replace(old, new))?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Rewrite the `LIBR = …` line in `<dest>/etc/Makeconf` to use `-lR` instead
/// of the macOS `-framework R` flag, which only works from the original install path.
fn patch_makeconf_libr(dest: &Path) -> Result<()> {
    let makeconf = dest.join("etc").join("Makeconf");
    if !makeconf.exists() {
        return Ok(());
    }
    let content = std::fs::read_to_string(&makeconf)?;
    let dest_str = dest.to_string_lossy();
    let patched = content
        .lines()
        .map(|line| {
            let t = line.trim_start();
            if t.starts_with("LIBR =") || t.starts_with("LIBR=") {
                format!("LIBR = -L{dest_str}/lib -lR")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let patched = if content.ends_with('\n') {
        patched + "\n"
    } else {
        patched
    };
    std::fs::write(&makeconf, patched)?;
    Ok(())
}

/// Update the embedded install-name of `libR.dylib` to its actual path so that
/// packages compiled against it can find it at runtime without DYLD_LIBRARY_PATH.
///
/// IMPORTANT: `install_name_tool` invalidates the Mach-O code signature.
/// On Apple Silicon every dylib must be signed; we always re-apply an ad-hoc
/// signature afterwards. The ad-hoc re-sign also strips the original hardened
/// runtime flag (set by CRAN's Developer ID signing on R 4.6+) — without that
/// strip, macOS would refuse to load the dylib through DYLD_LIBRARY_PATH.
fn fix_libr_install_name(dest: &Path) {
    let libr = dest.join("lib").join("libR.dylib");
    if !libr.exists() {
        return;
    }
    let new_id = libr.to_string_lossy().to_string();
    let _ = Command::new("install_name_tool")
        .args(["-id", &new_id, &new_id])
        .status();
    // Always re-sign — even if install_name_tool was a no-op, the original
    // CRAN signature has the hardened runtime flag set on R 4.6+, which makes
    // DYLD_LIBRARY_PATH ineffective for the executables that load this dylib.
    resign_adhoc(&libr);
}

/// Patch executable Mach-O binaries under `<r_home>/bin/exec/` so they load
/// our managed `lib/libR.dylib` instead of the framework path baked in by CRAN.
///
/// Without this, R 4.6+ (which CRAN signs with hardened runtime) silently
/// SIGKILLs at startup because:
///   1. `bin/exec/R` has a load command for `/Library/Frameworks/R.framework/Versions/4.6/Resources/lib/libR.dylib`.
///   2. The framework path doesn't exist in our extracted install.
///   3. Hardened runtime causes macOS to strip `DYLD_LIBRARY_PATH`, so the
///      `Renviron.site` hint we set has no effect on the dyld lookup.
///
/// Fix: rewrite the load command to point at our `lib/libR.dylib`, then re-sign
/// ad-hoc (which also clears the hardened-runtime flag).
pub fn patch_r_executables(r_home: &Path) {
    let exec_dir = r_home.join("bin").join("exec");
    if !exec_dir.exists() {
        return;
    }
    let lib_dir = r_home.join("lib");
    let Ok(entries) = std::fs::read_dir(&exec_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        rewrite_framework_loads(&path, &lib_dir);
    }
}

/// Rewrite every `/Library/Frameworks/R.framework/...` load command in `binary`
/// to point at the corresponding file in `lib_dir` (matched by basename), then
/// re-sign ad-hoc.
fn rewrite_framework_loads(binary: &Path, lib_dir: &Path) {
    let path_str = binary.to_string_lossy().to_string();
    let deps_out = match Command::new("otool").args(["-L", &path_str]).output() {
        Ok(o) => o,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::warn!(
                "otool not found — skipping load-command patch for {}. \
                 Install Xcode Command Line Tools (`xcode-select --install`) so R 4.6+ installs work.",
                binary.display()
            );
            return;
        }
        Err(_) => return,
    };
    if !deps_out.status.success() {
        return;
    }
    let Ok(deps) = String::from_utf8(deps_out.stdout) else {
        return;
    };
    for line in deps.lines().skip(1) {
        let dep = line.split_whitespace().next().unwrap_or("");
        if !dep.contains("/Library/Frameworks/R.framework/") {
            continue;
        }
        let Some(filename) = std::path::Path::new(dep).file_name() else {
            continue;
        };
        let new_dep = lib_dir.join(filename);
        if !new_dep.exists() {
            continue;
        }
        let new_dep_str = new_dep.to_string_lossy().to_string();
        let _ = Command::new("install_name_tool")
            .args(["-change", dep, &new_dep_str, &path_str])
            .status();
    }
    // Always re-sign — even when no load commands changed, R 4.6+ ships with
    // hardened runtime that suppresses DYLD_LIBRARY_PATH and blocks library
    // validation against ad-hoc dylibs. An ad-hoc re-sign drops the runtime
    // flag so the existing `Renviron.site` workaround actually takes effect.
    resign_adhoc(binary);
}

/// Re-sign a Mach-O ad-hoc, clearing any prior hardened-runtime flag.
fn resign_adhoc(path: &Path) {
    let path_str = path.to_string_lossy().to_string();
    match Command::new("codesign")
        .args(["--force", "--sign", "-", &path_str])
        .status()
    {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::warn!(
                "codesign not found — skipping ad-hoc re-sign of {}. \
                 R 4.6+ binaries will SIGKILL at startup until codesign is on PATH.",
                path.display()
            );
        }
        Err(_) => {}
    }
}

/// Patch all `.dylib` install names in `<r_home>/lib/` to use the managed-R
/// path instead of the original CRAN framework path.
///
/// The CRAN macOS `.pkg` compiles every dylib with an install name pointing
/// to `/Library/Frameworks/R.framework/…`. After extraction to
/// `~/.uvr/r-versions/<ver>/`, those absolute paths don't exist, so
/// `libRblas`/`libRlapack`/`libgfortran` are never found by the dynamic linker.
/// Packages compiled against system R (e.g. `Matrix`) expect LAPACK symbols
/// from `libR.dylib`'s load chain — if `libRlapack.dylib` is never loaded,
/// those symbols are absent and `dlopen` fails.
///
/// This function is idempotent: if all paths already point to the managed-R
/// lib dir the `install_name_tool` calls succeed silently.
pub fn patch_r_dylibs(r_home: &Path) {
    let lib_dir = r_home.join("lib");
    if !lib_dir.exists() {
        return;
    }
    let lib_str = lib_dir.to_string_lossy().to_string();

    let Ok(entries) = std::fs::read_dir(&lib_dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("dylib") {
            continue;
        }
        let path_str = path.to_string_lossy().to_string();

        // Fix the dylib's own install name if it still points to the framework.
        let old_id = Command::new("otool")
            .args(["-D", &path_str])
            .output()
            .ok()
            .and_then(|o| {
                String::from_utf8(o.stdout)
                    .ok()
                    .and_then(|t| t.lines().nth(1).map(|l| l.trim().to_string()))
            })
            .unwrap_or_default();

        let mut needs_resign = false;
        if old_id.contains("/Library/Frameworks/R.framework/") {
            let _ = Command::new("install_name_tool")
                .args(["-id", &path_str, &path_str])
                .status();
            needs_resign = true;
        }

        // Fix all dependency paths pointing into the R framework.
        let deps = Command::new("otool")
            .args(["-L", &path_str])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .unwrap_or_default();

        for line in deps.lines().skip(1) {
            let dep = line.split_whitespace().next().unwrap_or("");
            if !dep.contains("/Library/Frameworks/R.framework/") {
                continue;
            }
            let filename = std::path::Path::new(dep)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if filename.is_empty() {
                continue;
            }
            let new_dep = format!("{lib_str}/{filename}");
            if Command::new("install_name_tool")
                .args(["-change", dep, &new_dep, &path_str])
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
            {
                needs_resign = true;
            }
        }

        if needs_resign {
            let _ = Command::new("codesign")
                .args(["--force", "--sign", "-", &path_str])
                .status();
        }
    }
}

/// Write `etc/Renviron.site` so every R process from this installation
pub fn patch_renviron_site(r_home: &Path) -> Result<()> {
    write_renviron_site(r_home)
}

/// Write `etc/Renviron.site` so every R process from this installation
/// automatically has `DYLD_LIBRARY_PATH` pointing at its own lib directory.
///
/// R reads this file at startup (before user code) and expands `${R_HOME}`.
/// This survives the `DYLD_*` stripping that macOS applies to sub-processes
/// spawned through SIP-protected shells like `/bin/sh`.
fn write_renviron_site(dest: &Path) -> Result<()> {
    let renviron = dest.join("etc").join("Renviron.site");
    // Append only if our line isn't already present (idempotent for re-installs).
    let existing = std::fs::read_to_string(&renviron).unwrap_or_default();
    if existing.contains("DYLD_LIBRARY_PATH=${R_HOME}/lib") {
        return Ok(());
    }
    let content = format!(
        "{existing}# Added by uvr: ensure libR.dylib is always findable by sub-processes.\n\
         DYLD_LIBRARY_PATH=${{R_HOME}}/lib\n\
         LD_LIBRARY_PATH=${{R_HOME}}/lib\n"
    );
    std::fs::write(&renviron, content)?;
    Ok(())
}

fn find_dir_with_r_binary(dir: &Path, depth: usize) -> Option<PathBuf> {
    if depth > 12 {
        return None; // guard against symlink loops
    }
    if dir.join("bin").join("R").exists() {
        return Some(dir.to_path_buf());
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return None;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // Use metadata() so we follow symlinks (file_type() does not).
        if std::fs::metadata(&path)
            .map(|m| m.is_dir())
            .unwrap_or(false)
        {
            if let Some(found) = find_dir_with_r_binary(&path, depth + 1) {
                return Some(found);
            }
        }
    }
    None
}

fn find_all_payload_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    collect_payloads(dir, &mut found)?;
    Ok(found)
}

fn collect_payloads(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        if e.file_type()?.is_dir() {
            collect_payloads(&e.path(), out)?;
        } else if e.file_name().to_string_lossy() == "Payload" {
            out.push(e.path());
        }
    }
    Ok(())
}

/// Windows: Inno Setup `.exe` → silent install to `dest` without admin rights.
///
/// The `/CURRENTUSER` flag tells Inno Setup to install for the current user only,
/// avoiding the need for admin/UAC elevation — a key differentiator over rig.
fn install_r_windows(exe_bytes: &[u8], version: &str, dest: &Path) -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let exe_path = tmp.path().join(format!("R-{version}-win.exe"));
    std::fs::write(&exe_path, exe_bytes)?;

    info!("Installing R {version} silently to {}", dest.display());

    // Inno Setup log for diagnosing install failures
    let log_path = tmp.path().join("r-install.log");

    // Do NOT embed extra quotes around the /DIR= path — Rust's Command API
    // already quotes arguments when building the Windows command line.
    // Embedding quotes causes double-escaping that Inno Setup rejects (exit 1).
    let dir_arg = format!("/DIR={}", dest.to_string_lossy());
    let log_arg = format!("/LOG={}", log_path.to_string_lossy());
    // /MERGETASKS="!recordversion": the R Inno Setup installer exposes a
    // `recordversion` task which writes `HKCU\Software\R-core\R\<ver>\InstallPath`
    // to the Windows registry. That key is what RStudio (and other registry-aware
    // R GUIs) reads to pick its "default" R version — so writing it every time
    // `uvr r install` runs silently clobbers the user's RStudio choice on every
    // install. Since uvr manages its own R resolution, we never want this side
    // effect. The `!` prefix disables the task while leaving other defaults intact.
    let output = Command::new(&exe_path)
        .args([
            "/VERYSILENT",
            "/SUPPRESSMSGBOXES",
            &dir_arg,
            "/CURRENTUSER",
            "/NOICONS",
            "/NORESTART",
            "/MERGETASKS=!recordversion",
            &log_arg,
        ])
        .output()?;

    if !output.status.success() {
        let mut detail = format!(
            "R {version} silent installer failed (exit {})",
            output.status.code().unwrap_or(-1)
        );

        // Include Inno Setup log if available
        if let Ok(log) = std::fs::read_to_string(&log_path) {
            let lines: Vec<&str> = log.lines().collect();
            let start = lines.len().saturating_sub(20);
            let last_lines = lines[start..].join("\n");
            detail.push_str(&format!("\n\nInstaller log (last 20 lines):\n{last_lines}"));
        }

        // Include stdout/stderr if any
        if !output.stdout.is_empty() {
            if let Ok(stdout) = String::from_utf8(output.stdout) {
                detail.push_str(&format!("\n\nstdout: {stdout}"));
            }
        }
        if !output.stderr.is_empty() {
            if let Ok(stderr) = String::from_utf8(output.stderr) {
                detail.push_str(&format!("\n\nstderr: {stderr}"));
            }
        }

        return Err(UvrError::Other(detail));
    }

    Ok(())
}

/// Linux: `.deb` → `ar x` → `data.tar.gz` → extract
fn install_r_linux(deb_bytes: &[u8], version: &str, dest: &Path) -> Result<()> {
    // Pre-flight: `ar` (binutils) and `tar` are required. Some minimal Ubuntu
    // / Debian images and stripped-down container bases ship without binutils;
    // raw ENOENT from Command::status() reads as "I/O error: No such file or
    // directory" with no hint at the cause. Surface a clear message before
    // we even try to download.
    require_tool(
        "ar",
        &[
            "Debian/Ubuntu: sudo apt install binutils",
            "RHEL/Fedora:  sudo dnf install binutils",
            "Alpine:       sudo apk add binutils",
        ],
    )?;
    require_tool(
        "tar",
        &[
            "Debian/Ubuntu: sudo apt install tar",
            "Alpine:       sudo apk add tar",
        ],
    )?;

    let tmp = tempfile::tempdir()?;
    let deb_path = tmp.path().join(format!("r-{version}.deb"));
    std::fs::write(&deb_path, deb_bytes)?;

    // ar x <deb>
    let status = Command::new("ar")
        .args(["x", &deb_path.to_string_lossy()])
        .current_dir(tmp.path())
        .status()
        .map_err(|e| {
            UvrError::Other(format!(
                "Failed to run `ar x` on the downloaded .deb ({e}). Install binutils."
            ))
        })?;
    if !status.success() {
        return Err(UvrError::Other("ar x failed on .deb".into()));
    }

    // Find data.tar.*
    let data_tar = find_data_tar(tmp.path())?;

    let status = Command::new("tar")
        .args([
            "xf",
            &data_tar.to_string_lossy(),
            "-C",
            &dest.to_string_lossy(),
            "--strip-components=4", // strip ./opt/R/<version>/
        ])
        .status()
        .map_err(|e| {
            UvrError::Other(format!(
                "Failed to run `tar` to extract the .deb payload ({e}). Install tar."
            ))
        })?;
    if !status.success() {
        return Err(UvrError::Other(
            "tar extraction of Linux R .deb failed".into(),
        ));
    }

    // Patch hardcoded /opt/R/<version> paths to the actual install dir.
    // The Posit .deb is built with /opt/R/<version> baked into bin/R,
    // etc/Makeconf, etc/ldpaths, etc. We replace them all so R_HOME resolves
    // correctly from ~/.uvr/r-versions/<version>/.
    let original_prefix = format!("/opt/R/{version}");
    patch_text_files(dest, &original_prefix, &dest.to_string_lossy())?;

    // Write Renviron.site so LD_LIBRARY_PATH is set for every R process
    // spawned from this installation. The .so files in lib/ have their RPATH
    // pointing to /opt/R/<version>/lib; setting LD_LIBRARY_PATH at the R
    // level is the simplest way to ensure they resolve without patchelf.
    write_renviron_site(dest)?;

    Ok(())
}

/// Verify a CLI tool is present on `PATH`. Returns a clear error including
/// platform-specific install hints if it isn't, so users see actionable
/// diagnostics instead of "I/O error: No such file or directory".
fn require_tool(tool: &str, install_hints: &[&str]) -> Result<()> {
    if which::which(tool).is_ok() {
        return Ok(());
    }
    let hints = install_hints.join("\n  ");
    Err(UvrError::Other(format!(
        "`{tool}` not found on PATH — required to extract the R .deb. Install with:\n  {hints}"
    )))
}

fn find_data_tar(dir: &Path) -> Result<PathBuf> {
    for ext in &["data.tar.gz", "data.tar.xz", "data.tar.zst", "data.tar.bz2"] {
        let p = dir.join(ext);
        if p.exists() {
            return Ok(p);
        }
    }
    Err(UvrError::Other("data.tar.* not found in .deb".into()))
}

/// Fetch the list of available R versions for `platform` from the CRAN CDN.
///
/// Returns versions sorted oldest-first (e.g. `["4.3.0", "4.3.1", ...]`).
pub async fn fetch_available_versions(
    client: &reqwest::Client,
    platform: Platform,
) -> Result<Vec<String>> {
    // Use the platform-specific binary index when possible; fall back to the
    // CRAN source index (which lists every released version) for Linux.
    let macos_arm64_listing = format!(
        "https://cran.r-project.org/bin/macosx/{}/base/",
        macos_arm64_dir()
    );
    let macos_x86_64_listing = format!(
        "https://cran.r-project.org/bin/macosx/{}/base/",
        macos_x86_64_dir()
    );
    let (url, prefix, suffix): (&str, &str, &str) = match platform {
        Platform::MacOsArm64 => (macos_arm64_listing.as_str(), "R-", "-arm64.pkg"),
        Platform::MacOsX86_64 => (macos_x86_64_listing.as_str(), "R-", "-x86_64.pkg"),
        Platform::LinuxX86_64 | Platform::LinuxArm64 => {
            // CRAN's `/src/base/` lists subdirs (R-1, R-2, R-3, R-4) — not
            // tarballs. Point at `R-4/` directly to enumerate current-major
            // releases. Pre-R-4 versions are out of scope for uvr (R 3.x
            // pre-dates the supported R 4.0+ ABI).
            ("https://cran.r-project.org/src/base/R-4/", "R-", ".tar.gz")
        }
        Platform::WindowsX86_64 => (
            "https://cran.r-project.org/bin/windows/base/",
            "R-",
            "-win.exe",
        ),
    };

    let html = client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;

    // Parse href="R-<version><suffix>" fragments from the directory listing HTML.
    let needle = format!("href=\"{prefix}");
    let mut versions: Vec<String> = html
        .split(needle.as_str())
        .skip(1)
        .filter_map(|chunk| {
            let end = chunk.find(suffix)?;
            let ver = &chunk[..end];
            if is_real_r_version(ver) {
                Some(ver.to_string())
            } else {
                None
            }
        })
        .collect();

    // On Windows, also scrape the "old" directory for archived versions.
    if matches!(platform, Platform::WindowsX86_64) {
        let old_url = "https://cran.r-project.org/bin/windows/base/old/";
        if let Ok(old_html) = client
            .get(old_url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
        {
            if let Ok(text) = old_html.text().await {
                // Old directory lists subdirectories like href="4.4.2/".
                // It also lists `href="../"` (parent dir) — that one
                // satisfies the prior digits-and-dots sanity check (`..`
                // is two dots, no digits) and was getting picked up as a
                // bogus "version", surfacing as a `..` row in `uvr r
                // list --all` and `uvr::r_list(all = TRUE)` (uvr-r #9).
                // `is_real_r_version` requires at least one digit per
                // component.
                for chunk in text.split("href=\"").skip(1) {
                    if let Some(end) = chunk.find('/') {
                        let ver = &chunk[..end];
                        if is_real_r_version(ver) {
                            versions.push(ver.to_string());
                        }
                    }
                }
            }
        }
    }

    // Sort numerically by component (not lexicographically).
    versions.sort_by(|a, b| {
        let parse = |s: &str| -> Vec<u64> { s.split('.').filter_map(|p| p.parse().ok()).collect() };
        parse(a).cmp(&parse(b))
    });
    versions.dedup();
    Ok(versions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn platform_detect_succeeds() {
        // Should always succeed on supported platforms (macOS/Linux/Windows)
        let platform = Platform::detect().unwrap();
        // Just verify it returns something valid
        assert!(matches!(
            platform,
            Platform::MacOsArm64
                | Platform::MacOsX86_64
                | Platform::LinuxX86_64
                | Platform::LinuxArm64
                | Platform::WindowsX86_64
        ));
    }

    #[test]
    fn platform_is_macos() {
        assert!(Platform::MacOsArm64.is_macos());
        assert!(Platform::MacOsX86_64.is_macos());
        assert!(!Platform::LinuxX86_64.is_macos());
        assert!(!Platform::WindowsX86_64.is_macos());
    }

    #[test]
    fn platform_is_windows() {
        assert!(Platform::WindowsX86_64.is_windows());
        assert!(!Platform::MacOsArm64.is_windows());
        assert!(!Platform::LinuxX86_64.is_windows());
    }

    #[test]
    fn download_url_macos_arm64() {
        let url = Platform::MacOsArm64.download_url("4.4.2");
        assert!(url.contains("arm64"));
        assert!(url.contains("4.4.2"));
        assert!(url.ends_with(".pkg"));
        // Must be one of the two known CRAN dirs.
        assert!(
            url.contains("/sonoma-arm64/") || url.contains("/big-sur-arm64/"),
            "unexpected dir in {url}"
        );
    }

    #[test]
    fn is_real_r_version_accepts_versions() {
        assert!(is_real_r_version("4.5.3"));
        assert!(is_real_r_version("3.6.3"));
        assert!(is_real_r_version("4.5.3.0")); // 4 components, rare but valid
    }

    #[test]
    fn is_real_r_version_rejects_directory_listing_noise() {
        // Reproduces uvr-r #9 — the Windows `/base/old/` directory listing
        // includes `href="../"` (parent dir). Without this guard the `..`
        // string passed the all-digits-and-dots check and ended up in the
        // version list, surfacing as a `..` row in `uvr r list --all` and
        // `uvr::r_list(all = TRUE)`.
        assert!(!is_real_r_version(".."));
        assert!(!is_real_r_version("."));
        assert!(!is_real_r_version(""));
        assert!(!is_real_r_version("4."));
        assert!(!is_real_r_version(".4.5"));
        assert!(!is_real_r_version("4..5"));
        assert!(!is_real_r_version("v4.5.3"));
        assert!(!is_real_r_version("4.5.3-rc"));
        // CRAN's R 4+ doesn't ship 2-component releases. Reject so any
        // future scrape changes that produce 2-part strings flag
        // visibly rather than slipping into the list.
        assert!(!is_real_r_version("4.6"));
    }

    #[test]
    fn macos_arm64_dir_is_expected() {
        let d = macos_arm64_dir();
        assert!(
            d == "sonoma-arm64" || d == "big-sur-arm64",
            "unexpected macos_arm64_dir: {d}"
        );
    }

    #[test]
    fn macos_fallback_only_on_sonoma() {
        let fb = Platform::MacOsArm64.download_url_fallback("4.5.3");
        if macos_major_version() >= 14 {
            let fb = fb.expect("Sonoma+ should provide big-sur fallback");
            assert!(fb.contains("/big-sur-arm64/"), "{fb}");
        } else {
            assert!(
                fb.is_none(),
                "non-Sonoma must not return sonoma fallback (binary won't run): {fb:?}"
            );
        }
    }

    #[test]
    fn directory_listing_matches_primary() {
        let listing = Platform::MacOsArm64.directory_listing_url().unwrap();
        let url = Platform::MacOsArm64.download_url("4.4.2");
        // Listing dir should be the same arch dir as the download URL uses.
        let dir = macos_arm64_dir();
        assert!(listing.contains(dir));
        assert!(url.contains(dir));
    }

    #[test]
    fn download_url_macos_x86() {
        let url = Platform::MacOsX86_64.download_url("4.3.1");
        assert!(url.contains("x86_64"));
        assert!(url.contains("4.3.1"));
        assert!(url.ends_with(".pkg"));
    }

    #[test]
    fn download_url_linux_x86() {
        let url = Platform::LinuxX86_64.download_url("4.4.2");
        assert!(url.contains("amd64"));
        assert!(url.contains("4.4.2"));
        assert!(url.ends_with(".deb"));
    }

    #[test]
    fn download_url_linux_arm64() {
        let url = Platform::LinuxArm64.download_url("4.4.2");
        assert!(url.contains("arm64"));
        assert!(url.ends_with(".deb"));
    }

    #[test]
    fn download_url_windows() {
        let url = Platform::WindowsX86_64.download_url("4.4.2");
        assert!(url.contains("windows"));
        assert!(url.contains("4.4.2"));
        assert!(url.ends_with(".exe"));
    }

    #[test]
    fn extract_r_home_from_script() {
        let dir = TempDir::new().unwrap();
        let bin_dir = dir.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::write(
            bin_dir.join("R"),
            "#!/bin/sh\nR_HOME_DIR=/custom/path/to/R\nexport R_HOME\n",
        )
        .unwrap();
        let result = extract_r_home_dir(dir.path()).unwrap();
        assert_eq!(result, "/custom/path/to/R");
    }

    #[test]
    fn extract_r_home_fallback() {
        let dir = TempDir::new().unwrap();
        let bin_dir = dir.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::write(bin_dir.join("R"), "#!/bin/sh\necho hello\n").unwrap();
        let result = extract_r_home_dir(dir.path()).unwrap();
        assert_eq!(result, "/Library/Frameworks/R.framework/Resources");
    }

    #[test]
    fn find_dir_with_r_binary_direct() {
        let dir = TempDir::new().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("R"), "").unwrap();
        let found = find_dir_with_r_binary(dir.path(), 0);
        assert_eq!(found, Some(dir.path().to_path_buf()));
    }

    #[test]
    fn find_dir_with_r_binary_nested() {
        let dir = TempDir::new().unwrap();
        let nested = dir.path().join("Resources");
        let bin = nested.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("R"), "").unwrap();
        let found = find_dir_with_r_binary(dir.path(), 0);
        assert_eq!(found, Some(nested));
    }

    #[test]
    fn find_dir_with_r_binary_not_found() {
        let dir = TempDir::new().unwrap();
        let found = find_dir_with_r_binary(dir.path(), 0);
        assert!(found.is_none());
    }

    #[test]
    fn find_dir_depth_limit() {
        let dir = TempDir::new().unwrap();
        // Even if R binary exists deeply nested, depth=13 guard should kick in
        let found = find_dir_with_r_binary(dir.path(), 13);
        assert!(found.is_none());
    }

    #[test]
    fn find_data_tar_gz() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("data.tar.gz"), "").unwrap();
        let result = find_data_tar(dir.path()).unwrap();
        assert!(result.to_string_lossy().contains("data.tar.gz"));
    }

    #[test]
    fn find_data_tar_xz() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("data.tar.xz"), "").unwrap();
        let result = find_data_tar(dir.path()).unwrap();
        assert!(result.to_string_lossy().contains("data.tar.xz"));
    }

    #[test]
    fn find_data_tar_missing() {
        let dir = TempDir::new().unwrap();
        let result = find_data_tar(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn collect_payloads_finds_files() {
        let dir = TempDir::new().unwrap();
        let sub = dir.path().join("R-fw.pkg");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("Payload"), "data").unwrap();
        let payloads = find_all_payload_files(dir.path()).unwrap();
        assert_eq!(payloads.len(), 1);
    }

    #[test]
    fn collect_payloads_empty_dir() {
        let dir = TempDir::new().unwrap();
        let payloads = find_all_payload_files(dir.path()).unwrap();
        assert!(payloads.is_empty());
    }

    #[test]
    fn patch_makeconf_libr_rewrites() {
        let dir = TempDir::new().unwrap();
        let etc = dir.path().join("etc");
        std::fs::create_dir_all(&etc).unwrap();
        std::fs::write(
            etc.join("Makeconf"),
            "CC = gcc\nLIBR = -F/Library/Frameworks/R.framework/.. -framework R\nCFLAGS = -O2\n",
        )
        .unwrap();
        patch_makeconf_libr(dir.path()).unwrap();
        let content = std::fs::read_to_string(etc.join("Makeconf")).unwrap();
        assert!(content.contains(&format!("LIBR = -L{}/lib -lR", dir.path().display())));
        assert!(content.contains("CC = gcc"));
        assert!(content.contains("CFLAGS = -O2"));
    }

    #[test]
    fn patch_makeconf_missing_file() {
        let dir = TempDir::new().unwrap();
        // Should be a no-op, not an error
        patch_makeconf_libr(dir.path()).unwrap();
    }

    #[test]
    fn write_renviron_site_creates_file() {
        let dir = TempDir::new().unwrap();
        let etc = dir.path().join("etc");
        std::fs::create_dir_all(&etc).unwrap();
        write_renviron_site(dir.path()).unwrap();
        let content = std::fs::read_to_string(etc.join("Renviron.site")).unwrap();
        assert!(content.contains("DYLD_LIBRARY_PATH=${R_HOME}/lib"));
        assert!(content.contains("LD_LIBRARY_PATH=${R_HOME}/lib"));
    }

    #[test]
    fn write_renviron_site_idempotent() {
        let dir = TempDir::new().unwrap();
        let etc = dir.path().join("etc");
        std::fs::create_dir_all(&etc).unwrap();
        write_renviron_site(dir.path()).unwrap();
        let first = std::fs::read_to_string(etc.join("Renviron.site")).unwrap();
        write_renviron_site(dir.path()).unwrap();
        let second = std::fs::read_to_string(etc.join("Renviron.site")).unwrap();
        assert_eq!(first, second, "write_renviron_site should be idempotent");
    }

    #[test]
    fn patch_text_files_replaces_in_text() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("test.txt"),
            "path=/old/location\nother=/old/location/sub\n",
        )
        .unwrap();
        patch_text_files(dir.path(), "/old/location", "/new/location").unwrap();
        let content = std::fs::read_to_string(dir.path().join("test.txt")).unwrap();
        assert!(content.contains("/new/location"));
        assert!(!content.contains("/old/location\n"));
    }

    #[test]
    fn patch_text_files_skips_binary() {
        let dir = TempDir::new().unwrap();
        let mut data = b"/old/location".to_vec();
        data.push(0); // null byte → binary file
        data.extend_from_slice(b"more data");
        std::fs::write(dir.path().join("binary.so"), &data).unwrap();
        patch_text_files(dir.path(), "/old/location", "/new/location").unwrap();
        let content = std::fs::read(dir.path().join("binary.so")).unwrap();
        // Should be unchanged — binary files are skipped
        assert_eq!(content, data);
    }
}
