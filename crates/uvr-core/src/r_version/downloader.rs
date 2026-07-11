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

    /// Return the download URL for the portable R build of `version`.
    ///
    /// Every platform pulls a relocatable build from the rstudio/r-builds CDN
    /// (`cdn.posit.co/r`). These extract-and-run archives need no post-install
    /// path patching — R locates its own `R_HOME` at runtime. Note the macOS
    /// binaries carry only an ad-hoc code signature (not notarized; verified
    /// with `codesign -dv`), so download integrity rests on TLS to the CDN —
    /// the index publishes no checksums to verify against.
    pub fn download_url(&self, version: &str) -> String {
        match self {
            Platform::MacOsArm64 => {
                format!("{PORTABLE_CDN}/macos/R-{version}-macos-arm64.tar.gz")
            }
            Platform::MacOsX86_64 => {
                format!("{PORTABLE_CDN}/macos/R-{version}-macos.tar.gz")
            }
            Platform::LinuxX86_64 | Platform::LinuxArm64 => {
                let id = linux_portable_id();
                let arch = if matches!(self, Platform::LinuxArm64) {
                    "-arm64"
                } else {
                    ""
                };
                format!("{PORTABLE_CDN}/{id}/R-{version}-{id}{arch}.tar.gz")
            }
            Platform::WindowsX86_64 => {
                format!("{PORTABLE_CDN}/windows/R-{version}-windows.zip")
            }
        }
    }
}

/// Root of the rstudio/r-builds portable R CDN.
const PORTABLE_CDN: &str = "https://cdn.posit.co/r";

/// Unified version index for the portable builds.
const VERSIONS_JSON_URL: &str = "https://cdn.posit.co/r/versions.json";

/// macOS and Windows portable builds start at R 4.1.0 — the CDN returns 403
/// for earlier versions on both platforms (verified: R 4.0.5 → 403 on
/// `macos` and `windows`, 200 on `manylinux_2_34`). Linux builds have no floor.
const MAC_WIN_MIN_R_VERSION: (u32, u32, u32) = (4, 1, 0);

/// The minimum R version published on the portable CDN for `platform`, or
/// `None` when the platform has no floor (Linux).
fn portable_min_r_version(platform: Platform) -> Option<(u32, u32, u32)> {
    (platform.is_macos() || platform.is_windows()).then_some(MAC_WIN_MIN_R_VERSION)
}

/// manylinux_2_34 portable builds require glibc >= 2.34.
const MANYLINUX_GLIBC_MIN: (u32, u32) = (2, 34);

/// Portable build platform identifier for the running Linux libc:
/// `musllinux_1_2` on musl (Alpine), `manylinux_2_34` on glibc.
fn linux_portable_id() -> &'static str {
    if linux_is_musl() {
        "musllinux_1_2"
    } else {
        "manylinux_2_34"
    }
}

/// True when the host uses musl libc — detected from `/etc/os-release`
/// (`ID=alpine`) or the presence of a musl dynamic loader under `/lib`.
fn linux_is_musl() -> bool {
    if let Ok(content) = std::fs::read_to_string("/etc/os-release") {
        for line in content.lines() {
            if let Some(val) = line.strip_prefix("ID=") {
                if val.trim_matches('"').eq_ignore_ascii_case("alpine") {
                    return true;
                }
            }
        }
    }
    std::fs::read_dir("/lib")
        .map(|rd| {
            rd.flatten()
                .any(|e| e.file_name().to_string_lossy().starts_with("ld-musl-"))
        })
        .unwrap_or(false)
}

/// Parse the host glibc version from `getconf GNU_LIBC_VERSION` (e.g. "glibc 2.39").
/// Returns `None` when `getconf` is absent or unparseable (treated as "unknown",
/// so the floor check is skipped rather than failing a possibly-fine host).
fn detect_glibc_version() -> Option<(u32, u32)> {
    let out = Command::new("getconf")
        .arg("GNU_LIBC_VERSION")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let ver = s.split_whitespace().last()?;
    let mut it = ver.split('.');
    let maj = it.next()?.parse().ok()?;
    let min = it.next().unwrap_or("0").parse().ok()?;
    Some((maj, min))
}

/// Ensure the host libc is new enough for the portable Linux builds. No-op on
/// musl and on non-Linux hosts. Returns a clear error on glibc < 2.34, the
/// floor for the manylinux_2_34 builds (excludes Ubuntu 20.04, RHEL 8, Debian 11).
fn ensure_linux_libc_supported() -> Result<()> {
    if !cfg!(target_os = "linux") || linux_is_musl() {
        return Ok(());
    }
    if let Some((maj, min)) = detect_glibc_version() {
        if (maj, min) < MANYLINUX_GLIBC_MIN {
            let (rmaj, rmin) = MANYLINUX_GLIBC_MIN;
            return Err(UvrError::UnsupportedPlatform(format!(
                "glibc {maj}.{min} is too old for portable R builds (need >= {rmaj}.{rmin}). \
                 Distros below this floor — Ubuntu 20.04, RHEL 8, Debian 11 — are not supported \
                 by uvr's R installer. Use your system package manager's R, or build R from source."
            )));
        }
    }
    Ok(())
}

/// Parse an `X.Y.Z` R version into a comparable tuple. Extra components and
/// non-numeric tails are ignored. Returns `None` if the first component isn't numeric.
fn parse_r_version(version: &str) -> Option<(u32, u32, u32)> {
    let mut it = version.split('.');
    let maj = it.next()?.parse().ok()?;
    let min = it.next().unwrap_or("0").parse().ok()?;
    let patch = it.next().unwrap_or("0").parse().ok()?;
    Some((maj, min, patch))
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
    let content = std::fs::read_to_string("/etc/os-release").ok();
    detect_posit_distro_slug_from_os_release(content.as_deref())
}

/// Testable helper: parse os-release content (or fall back) into a Posit
/// CDN distro slug. Module-private; the inline test module calls it
/// directly. Production callers go through [`detect_posit_distro_slug`].
pub(crate) fn detect_posit_distro_slug_from_os_release(content: Option<&str>) -> String {
    let content = match content {
        Some(c) => c,
        None => return "ubuntu-2204".to_string(),
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
            let major = version_id.split('.').next().unwrap_or(&version_id);
            format!("rhel-{major}")
        }
        "opensuse-leap" | "sles" => {
            let ver = version_id.replace('.', "");
            format!("opensuse-{ver}")
        }
        "alpine" => {
            // Truncate `3.23.4` → `3.23` to match the #30 sysreqs normalization
            // and to make `ppm_linux_codename` return None (P3M is then skipped
            // cleanly; sync falls through to source compile).
            let minor = version_id.split('.').take(2).collect::<Vec<_>>().join(".");
            format!("alpine-{minor}")
        }
        _ => "ubuntu-2204".to_string(),
    }
}

/// Parsed host platform triple, modeled after Rust target triples and
/// R's `R.Version()$platform` reporting.
///
/// Used to construct user-agent strings and to match `Built:` fields in
/// CRAN-like binary repositories (cran.rpkgs.com, P3M, etc.).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostTriple {
    /// CPU architecture: `"x86_64"` | `"aarch64"`.
    pub arch: String,
    /// Vendor: `"pc"` on Linux/Windows, `"apple"` on macOS.
    pub vendor: String,
    /// OS: `"linux"` | `"darwin"` | `"windows"`.
    pub os: String,
    /// ABI / libc: `"gnu"` | `"musl"` | `"darwin"` | `"msvc"`.
    pub abi: String,
}

/// Build a `HostTriple` from optional `/etc/os-release` content and the
/// detected `Platform`. Module-private; the inline test module calls it
/// directly. Production callers go through [`host_triple()`].
fn host_triple_from_os_release(content: Option<&str>, platform: Platform) -> HostTriple {
    let mut id = String::new();
    if let Some(c) = content {
        for line in c.lines() {
            if let Some(val) = line.strip_prefix("ID=") {
                id = val.trim_matches('"').to_lowercase();
                break;
            }
        }
    }

    let arch = match platform {
        Platform::LinuxX86_64 | Platform::MacOsX86_64 | Platform::WindowsX86_64 => "x86_64",
        Platform::LinuxArm64 | Platform::MacOsArm64 => "aarch64",
    };

    let (vendor, os, default_abi) = match platform {
        Platform::LinuxX86_64 | Platform::LinuxArm64 => ("pc", "linux", "gnu"),
        Platform::MacOsArm64 | Platform::MacOsX86_64 => ("apple", "darwin", "darwin"),
        Platform::WindowsX86_64 => ("pc", "windows", "msvc"),
    };

    let abi = match (platform, id.as_str()) {
        (Platform::LinuxX86_64 | Platform::LinuxArm64, "alpine") => "musl",
        _ => default_abi,
    };

    HostTriple {
        arch: arch.to_string(),
        vendor: vendor.to_string(),
        os: os.to_string(),
        abi: abi.to_string(),
    }
}

/// Detect the host triple by reading `/etc/os-release` and combining with
/// `Platform::detect()`. Used at sync time to construct the UA and match
/// `Built:` fields.
pub fn host_triple() -> HostTriple {
    let content = std::fs::read_to_string("/etc/os-release").ok();
    let platform = Platform::detect().unwrap_or(Platform::LinuxX86_64);
    host_triple_from_os_release(content.as_deref(), platform)
}

/// Host platform info plus pretty distro label and R version, suitable for
/// constructing user-agent strings.
#[derive(Debug, Clone)]
pub struct HostInfo {
    pub triple: HostTriple,
    /// Pretty distro label as it appears in the UA, e.g. `"Alpine Linux 3.23.4"`.
    /// Defaults to `"unknown"` when `/etc/os-release` is missing or sparse.
    pub distro_label: String,
    /// R minor or patch version, e.g. `"4.5.0"`. Caller-supplied; not detected.
    pub r_version: String,
}

/// Build a `HostInfo` from optional `/etc/os-release` content. Module-private;
/// the inline test module calls it directly. Production callers go through
/// [`host_info()`].
fn host_info_from_os_release(
    content: Option<&str>,
    platform: Platform,
    r_version: &str,
) -> HostInfo {
    let triple = host_triple_from_os_release(content, platform);

    let mut name = String::new();
    let mut version_id = String::new();
    if let Some(c) = content {
        for line in c.lines() {
            if let Some(val) = line.strip_prefix("NAME=") {
                name = val.trim_matches('"').to_string();
            } else if let Some(val) = line.strip_prefix("VERSION_ID=") {
                version_id = val.trim_matches('"').to_string();
            }
        }
    }

    let distro_label = if name.is_empty() {
        "unknown".to_string()
    } else if version_id.is_empty() {
        name
    } else {
        format!("{name} {version_id}")
    };

    HostInfo {
        triple,
        distro_label,
        r_version: r_version.to_string(),
    }
}

/// Detect the host info. `r_version` should be the R version in use for the
/// project (caller-supplied because uvr knows the project R version).
pub fn host_info(r_version: &str) -> HostInfo {
    let content = std::fs::read_to_string("/etc/os-release").ok();
    let platform = Platform::detect().unwrap_or(Platform::LinuxX86_64);
    host_info_from_os_release(content.as_deref(), platform, r_version)
}

/// Normalize an R version for use in a User-Agent string (#124).
///
/// Real R always reports a full three-part version (`4.5.1`); uvr callers
/// often hold only the minor series (`4.5`). Pad a bare `X.Y` to `X.Y.0` so
/// every UA uvr sends uses one canonical form — the PPM index fetch
/// (`registry/p3m.rs`) and the tarball download (`host_info` →
/// [`user_agent`]) must agree, because the download cache key folds the UA
/// in (#122): divergent forms would mean spurious re-downloads if the two
/// paths ever fed the same URL.
pub fn normalize_ua_r_version(v: &str) -> String {
    if v.split('.').count() == 2 {
        format!("{v}.0")
    } else {
        v.to_string()
    }
}

/// Construct a User-Agent string matching what real R sends via
/// `getOption("HTTPUserAgent")`:
///
/// ```text
/// R (<ver> <triple> <arch> <os>-<abi>)
/// ```
///
/// Examples:
/// - Alpine: `R (4.5.0 x86_64-pc-linux-musl x86_64 linux-musl)`
/// - Ubuntu: `R (4.5.0 x86_64-pc-linux-gnu x86_64 linux-gnu)`
///
/// PPM's UA gating requires this exact `R (` prefix; see the test in
/// `registry/p3m.rs`. cran.rpkgs.com uses the platform triple substring
/// (`linux-musl` vs `linux-gnu`) to route requests to the right binary.
pub fn user_agent(info: &HostInfo) -> String {
    let HostTriple {
        arch,
        vendor,
        os,
        abi,
    } = &info.triple;
    format!(
        "R ({} {}-{}-{}-{} {} {}-{})",
        normalize_ua_r_version(&info.r_version),
        arch,
        vendor,
        os,
        abi,
        arch,
        os,
        abi
    )
}

/// Download and extract R to `~/.uvr/r-versions/<version>/`.
pub async fn download_and_install_r(
    client: &reqwest::Client,
    version: &str,
    platform: Platform,
) -> Result<PathBuf> {
    let install_dir = crate::env_vars::r_install_dir()
        .ok_or_else(|| UvrError::Other("Cannot determine r-versions directory".into()))?
        .join(version);

    let r_binary_name = if platform.is_windows() { "R.exe" } else { "R" };
    let r_binary = install_dir.join("bin").join(r_binary_name);
    if r_binary.exists() {
        // Validate the existing install actually works before short-
        // circuiting. The previous existence-only check let half-patched
        // installs (e.g. mvuorre's #99 on macOS 26.x) sit forever
        // because `uvr r install` skipped the reinstall, and downstream
        // checks treated `R --version`-returns-nothing as "not
        // installed" and looped the user back here. Now: if `R
        // --version` succeeds we trust it; if it fails we nuke the dir
        // and reinstall fresh.
        if crate::r_version::detector::query_r_version(&r_binary).is_some() {
            info!("R {version} already installed at {}", install_dir.display());
            return Ok(install_dir);
        }
        info!(
            "R {version} install at {} is broken (no version response); reinstalling",
            install_dir.display()
        );
        std::fs::remove_dir_all(&install_dir).map_err(|e| {
            UvrError::Other(format!(
                "Failed to remove broken install at {}: {e}",
                install_dir.display()
            ))
        })?;
    }

    // Preflight: portable manylinux builds need glibc >= 2.34.
    ensure_linux_libc_supported()?;

    // Preflight: macOS and Windows portable builds start at R 4.1.0 — fail
    // with a clear message rather than a bare 403 from the CDN.
    if let Some(floor) = portable_min_r_version(platform) {
        if let Some(v) = parse_r_version(version) {
            if v < floor {
                let (mj, mn, p) = floor;
                return Err(UvrError::Other(format!(
                    "Portable R builds for your platform start at {mj}.{mn}.{p}; R {version} is \
                     not available. Install {mj}.{mn}.{p} or newer."
                )));
            }
        }
    }

    let url = platform.download_url(version);
    info!("Downloading R {version} from {url}");

    let response = client.get(&url).send().await?;
    let bytes = if response.status().is_client_error() {
        return Err(version_not_found_error(client, version, platform, response.status()).await);
    } else {
        response.error_for_status()?.bytes().await?
    };

    // install_r_portable stages next to install_dir and moves the extracted
    // tree into place with one atomic rename — install_dir must not pre-exist.
    install_r_portable(&bytes, &install_dir, platform)?;

    if !r_binary.exists() {
        // Don't leave a tree without bin/R behind: it would trip the
        // exists-but-broken reinstall path on every subsequent run.
        let _ = std::fs::remove_dir_all(&install_dir);
        return Err(UvrError::Other(format!(
            "R binary not found after installation at {}",
            r_binary.display()
        )));
    }

    info!("R {version} installed to {}", install_dir.display());
    Ok(install_dir)
}

/// Build a helpful error when the portable CDN returns 4xx for a requested R
/// version. Best-effort: queries `versions.json` so users see "latest available
/// is 4.5.3" instead of a bare "404 Not Found".
async fn version_not_found_error(
    client: &reqwest::Client,
    version: &str,
    platform: Platform,
    status: reqwest::StatusCode,
) -> UvrError {
    let available_hint = match fetch_available_versions(client, platform).await {
        Ok(versions) if !versions.is_empty() => {
            let latest = versions.last().unwrap();
            format!(
                "\nLatest available for your platform: {latest}.\n\
                 Try `uvr r install {latest}`, or `uvr r list --all` to see every published version."
            )
        }
        _ => "\nCheck available versions with `uvr r list --all`. If R was just released, the \
              portable build for your platform may not be published yet — try again later."
            .to_string(),
    };
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

/// Locate the directory containing `bin/R` (or `bin/R.exe` on Windows) within an
/// extracted portable archive. Portable tarballs may nest R under a top-level
/// `R-<version>/` directory, so we recurse to find the real `R_HOME` root.
fn find_dir_with_r_binary(dir: &Path, depth: usize) -> Option<PathBuf> {
    if depth > 12 {
        return None; // guard against symlink loops
    }
    let bin = dir.join("bin");
    if bin.join("R").exists() || bin.join("R.exe").exists() {
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

/// Fetch the list of available R versions from the portable build index
/// (`cdn.posit.co/r/versions.json`).
///
/// Returns versions sorted oldest-first (e.g. `["4.3.0", "4.3.1", ...]`),
/// dropping the rolling `next`/`devel` channels. On macOS and Windows the
/// list is clamped to R >= 4.1.0 (the CDN's floor for both platforms).
pub async fn fetch_available_versions(
    client: &reqwest::Client,
    platform: Platform,
) -> Result<Vec<String>> {
    let body = client
        .get(VERSIONS_JSON_URL)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let json: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| UvrError::Other(format!("Failed to parse versions.json: {e}")))?;
    let arr = json
        .get("r_versions")
        .and_then(|v| v.as_array())
        .ok_or_else(|| UvrError::Other("versions.json missing `r_versions` array".into()))?;

    let floor = portable_min_r_version(platform);
    let mut versions: Vec<String> = arr
        .iter()
        .filter_map(|v| v.as_str())
        // Drop rolling channels ("next", "devel") and any non-X.Y.Z label.
        .filter(|s| is_real_r_version(s))
        .filter(|s| match floor {
            Some(f) => parse_r_version(s).map(|v| v >= f).unwrap_or(false),
            None => true,
        })
        .map(|s| s.to_string())
        .collect();

    versions.sort_by_key(|a| parse_r_version(a));
    versions.dedup();
    Ok(versions)
}

/// Install a portable R build by extracting `bytes` into `dest`.
///
/// The rstudio/r-builds portable archives are relocatable: R resolves its own
/// `R_HOME` at runtime and bundles its dependency libraries. So there is no
/// path patching and no install-name rewriting — we extract the archive and
/// move the `R_HOME` directory into `dest` with a single rename.
///
/// Extraction stages in a dot-prefixed sibling of `dest`, not the OS temp
/// dir: `/tmp` is commonly a different filesystem than `~/.uvr`, where a
/// cross-device rename fails (EXDEV) and a per-directory copy fallback could
/// be interrupted, leaving `dest` half-populated yet passing the `bin/R`
/// existence checks. Same-directory staging makes the final rename atomic:
/// `dest` either doesn't exist or is complete. The staging dir is removed on
/// every error path (dot-prefixed so version listing skips it if the process
/// dies uncleanly).
fn install_r_portable(bytes: &[u8], dest: &Path, platform: Platform) -> Result<()> {
    let parent = dest.parent().ok_or_else(|| {
        UvrError::Other(format!(
            "Install path {} has no parent directory",
            dest.display()
        ))
    })?;
    std::fs::create_dir_all(parent)
        .map_err(|e| UvrError::Other(format!("Failed to create {}: {e}", parent.display())))?;
    let tmp = tempfile::Builder::new()
        .prefix(".uvr-stage-")
        .tempdir_in(parent)
        .map_err(|e| UvrError::Other(format!("Failed to create staging dir for R: {e}")))?;
    let stage = tmp.path();

    if platform.is_windows() {
        extract_zip_to(bytes, stage)?;
    } else {
        extract_tar_gz_to(bytes, stage)?;
    }

    // Portable archives may nest R under a top-level `R-<version>/` dir.
    let r_home = find_dir_with_r_binary(stage, 0).ok_or_else(|| {
        UvrError::Other(
            "Extracted R archive did not contain a bin/R — the download may be corrupt".into(),
        )
    })?;

    std::fs::rename(&r_home, dest).map_err(|e| {
        UvrError::Other(format!(
            "Failed to move extracted R into place ({} -> {}): {e}",
            r_home.display(),
            dest.display()
        ))
    })?;
    Ok(())
}

/// Extract a `.tar.gz` into `dest`, preserving symlinks and unix permissions.
fn extract_tar_gz_to(bytes: &[u8], dest: &Path) -> Result<()> {
    let dec = flate2::read::GzDecoder::new(bytes);
    let mut ar = tar::Archive::new(dec);
    ar.set_preserve_permissions(true);
    ar.set_overwrite(true);
    ar.unpack(dest)
        .map_err(|e| UvrError::Other(format!("Failed to extract R tarball: {e}")))?;
    Ok(())
}

/// Extract a `.zip` into `dest` (Windows portable builds).
fn extract_zip_to(bytes: &[u8], dest: &Path) -> Result<()> {
    let reader = std::io::Cursor::new(bytes);
    let mut zip = zip::ZipArchive::new(reader)
        .map_err(|e| UvrError::Other(format!("Failed to open R zip: {e}")))?;
    zip.extract(dest)
        .map_err(|e| UvrError::Other(format!("Failed to extract R zip: {e}")))?;
    Ok(())
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
        assert_eq!(
            url,
            "https://cdn.posit.co/r/macos/R-4.4.2-macos-arm64.tar.gz"
        );
    }

    #[test]
    fn download_url_macos_x86() {
        let url = Platform::MacOsX86_64.download_url("4.3.1");
        assert_eq!(url, "https://cdn.posit.co/r/macos/R-4.3.1-macos.tar.gz");
    }

    #[test]
    fn download_url_linux_x86_is_portable() {
        let url = Platform::LinuxX86_64.download_url("4.4.2");
        // The libc infix (manylinux_2_34 vs musllinux_1_2) is host-dependent,
        // so assert the portable shape rather than the exact infix.
        assert!(url.starts_with("https://cdn.posit.co/r/"));
        assert!(url.contains("/R-4.4.2-"));
        assert!(url.ends_with(".tar.gz"));
        assert!(!url.contains("-arm64"));
    }

    #[test]
    fn download_url_linux_arm64_is_portable() {
        let url = Platform::LinuxArm64.download_url("4.4.2");
        assert!(url.contains("/R-4.4.2-"));
        assert!(url.ends_with("-arm64.tar.gz"));
    }

    #[test]
    fn download_url_windows() {
        let url = Platform::WindowsX86_64.download_url("4.4.2");
        assert_eq!(url, "https://cdn.posit.co/r/windows/R-4.4.2-windows.zip");
    }

    #[test]
    fn is_real_r_version_accepts_versions() {
        assert!(is_real_r_version("4.5.3"));
        assert!(is_real_r_version("3.6.3"));
        assert!(is_real_r_version("4.5.3.0")); // 4 components, rare but valid
    }

    #[test]
    fn is_real_r_version_rejects_noise() {
        assert!(!is_real_r_version(".."));
        assert!(!is_real_r_version("."));
        assert!(!is_real_r_version(""));
        assert!(!is_real_r_version("4."));
        assert!(!is_real_r_version(".4.5"));
        assert!(!is_real_r_version("4..5"));
        assert!(!is_real_r_version("v4.5.3"));
        assert!(!is_real_r_version("4.5.3-rc"));
        assert!(!is_real_r_version("4.6"));
        // Rolling channels in versions.json must be filtered out.
        assert!(!is_real_r_version("next"));
        assert!(!is_real_r_version("devel"));
    }

    #[test]
    fn parse_r_version_orders_correctly() {
        assert_eq!(parse_r_version("4.4.2"), Some((4, 4, 2)));
        assert_eq!(parse_r_version("4.4"), Some((4, 4, 0)));
        assert!(parse_r_version("4.1.0") >= Some(MAC_WIN_MIN_R_VERSION));
        assert!(parse_r_version("4.0.5") < Some(MAC_WIN_MIN_R_VERSION));
        assert!(parse_r_version("next").is_none());
    }

    #[test]
    fn portable_floor_applies_to_macos_and_windows() {
        // The CDN 403s pre-4.1.0 builds on macOS AND Windows (verified live);
        // Linux publishes older versions. Regression guard for the floor
        // check only gating on is_macos().
        assert!(portable_min_r_version(Platform::MacOsArm64).is_some());
        assert!(portable_min_r_version(Platform::MacOsX86_64).is_some());
        assert!(portable_min_r_version(Platform::WindowsX86_64).is_some());
        assert!(portable_min_r_version(Platform::LinuxX86_64).is_none());
        assert!(portable_min_r_version(Platform::LinuxArm64).is_none());
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
    fn install_r_portable_flattens_nested_tarball() {
        // Build a .tar.gz whose R_HOME is nested under `R-4.4.2/` (as the
        // portable archives are), then verify install_r_portable flattens it
        // so `<dest>/bin/R` exists.
        let mut tar_buf = Vec::new();
        {
            let enc = flate2::write::GzEncoder::new(&mut tar_buf, flate2::Compression::fast());
            let mut builder = tar::Builder::new(enc);
            let script = b"#!/bin/sh\necho R\n";
            let mut header = tar::Header::new_gnu();
            header.set_size(script.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, "R-4.4.2/bin/R", &script[..])
                .unwrap();
            let lib = b"libR";
            let mut h2 = tar::Header::new_gnu();
            h2.set_size(lib.len() as u64);
            h2.set_mode(0o644);
            h2.set_cksum();
            builder
                .append_data(&mut h2, "R-4.4.2/lib/libR.so", &lib[..])
                .unwrap();
            builder.into_inner().unwrap().finish().unwrap();
        }

        let dest_dir = TempDir::new().unwrap();
        let dest = dest_dir.path().join("4.4.2");
        install_r_portable(&tar_buf, &dest, Platform::LinuxX86_64).unwrap();
        assert!(dest.join("bin").join("R").exists());
        assert!(dest.join("lib").join("libR.so").exists());
        // The sibling staging dir must be gone after a successful install.
        let leftovers: Vec<_> = std::fs::read_dir(dest_dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".uvr-stage-"))
            .collect();
        assert!(leftovers.is_empty(), "staging dir leaked: {leftovers:?}");
    }

    #[test]
    fn install_r_portable_cleans_stage_on_bad_archive() {
        // A tarball with no bin/R must error AND leave neither dest nor a
        // staging dir behind.
        let mut tar_buf = Vec::new();
        {
            let enc = flate2::write::GzEncoder::new(&mut tar_buf, flate2::Compression::fast());
            let mut builder = tar::Builder::new(enc);
            let junk = b"not R";
            let mut header = tar::Header::new_gnu();
            header.set_size(junk.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "R-4.4.2/README", &junk[..])
                .unwrap();
            builder.into_inner().unwrap().finish().unwrap();
        }

        let dest_dir = TempDir::new().unwrap();
        let dest = dest_dir.path().join("4.4.2");
        assert!(install_r_portable(&tar_buf, &dest, Platform::LinuxX86_64).is_err());
        assert!(!dest.exists(), "dest must not exist after a failed install");
        let leftovers: Vec<_> = std::fs::read_dir(dest_dir.path())
            .unwrap()
            .flatten()
            .collect();
        assert!(leftovers.is_empty(), "staging dir leaked: {leftovers:?}");
    }

    #[test]
    fn host_triple_alpine_x86_64() {
        let os_release = r#"NAME="Alpine Linux"
ID=alpine
VERSION_ID=3.23.4
"#;
        let triple = host_triple_from_os_release(Some(os_release), Platform::LinuxX86_64);
        assert_eq!(triple.arch, "x86_64");
        assert_eq!(triple.vendor, "pc");
        assert_eq!(triple.os, "linux");
        assert_eq!(triple.abi, "musl");
    }

    #[test]
    fn host_triple_ubuntu_x86_64() {
        let os_release = r#"NAME="Ubuntu"
ID=ubuntu
VERSION_ID="22.04"
"#;
        let triple = host_triple_from_os_release(Some(os_release), Platform::LinuxX86_64);
        assert_eq!(triple.abi, "gnu");
    }

    #[test]
    fn host_triple_alpine_aarch64() {
        let os_release = r#"ID=alpine
VERSION_ID=3.23
"#;
        let triple = host_triple_from_os_release(Some(os_release), Platform::LinuxArm64);
        assert_eq!(triple.arch, "aarch64");
        assert_eq!(triple.abi, "musl");
    }

    #[test]
    fn host_triple_no_os_release_falls_back_to_gnu() {
        let triple = host_triple_from_os_release(None, Platform::LinuxX86_64);
        assert_eq!(triple.abi, "gnu");
    }

    #[test]
    fn host_triple_macos() {
        let triple = host_triple_from_os_release(None, Platform::MacOsArm64);
        assert_eq!(triple.vendor, "apple");
        assert_eq!(triple.os, "darwin");
        assert_eq!(triple.abi, "darwin");
    }

    #[test]
    fn host_triple_windows() {
        let triple = host_triple_from_os_release(None, Platform::WindowsX86_64);
        assert_eq!(triple.arch, "x86_64");
        assert_eq!(triple.vendor, "pc");
        assert_eq!(triple.os, "windows");
        assert_eq!(triple.abi, "msvc");
    }

    #[test]
    fn user_agent_alpine_matches_real_r() {
        let info = HostInfo {
            triple: HostTriple {
                arch: "x86_64".into(),
                vendor: "pc".into(),
                os: "linux".into(),
                abi: "musl".into(),
            },
            distro_label: "Alpine Linux 3.23.4".into(),
            r_version: "4.5.0".into(),
        };
        assert_eq!(
            user_agent(&info),
            "R (4.5.0 x86_64-pc-linux-musl x86_64 linux-musl)"
        );
    }

    #[test]
    fn user_agent_ubuntu_matches_real_r() {
        let info = HostInfo {
            triple: HostTriple {
                arch: "x86_64".into(),
                vendor: "pc".into(),
                os: "linux".into(),
                abi: "gnu".into(),
            },
            distro_label: "Ubuntu 22.04".into(),
            r_version: "4.5.0".into(),
        };
        assert_eq!(
            user_agent(&info),
            "R (4.5.0 x86_64-pc-linux-gnu x86_64 linux-gnu)"
        );
    }

    #[test]
    fn user_agent_normalizes_minor_only_r_version() {
        // #124: sync.rs feeds host_info() the minor series ("4.5") while
        // p3m.rs builds its index UA as "{r_minor}.0". Both must emit the
        // same canonical three-part form, or the download cache key (which
        // folds the UA in, #122) diverges between paths.
        assert_eq!(normalize_ua_r_version("4.5"), "4.5.0");
        assert_eq!(normalize_ua_r_version("4.5.1"), "4.5.1");

        let info = HostInfo {
            triple: HostTriple {
                arch: "x86_64".into(),
                vendor: "pc".into(),
                os: "linux".into(),
                abi: "gnu".into(),
            },
            distro_label: "Ubuntu 22.04".into(),
            r_version: "4.5".into(), // minor-only, as passed by sync.rs
        };
        // Must exactly match the p3m.rs index-fetch UA for the same R.
        assert_eq!(
            user_agent(&info),
            "R (4.5.0 x86_64-pc-linux-gnu x86_64 linux-gnu)"
        );
    }

    #[test]
    fn user_agent_satisfies_ppm_gating() {
        // PPM's UA gating in registry/p3m.rs sniffs for the literal "R (" prefix.
        // Regression guard: any future change to user_agent() that drops this
        // prefix will silently break P3M binary downloads on Ubuntu/Debian.
        let info = HostInfo {
            triple: HostTriple {
                arch: "x86_64".into(),
                vendor: "pc".into(),
                os: "linux".into(),
                abi: "gnu".into(),
            },
            distro_label: "Ubuntu 22.04".into(),
            r_version: "4.5.0".into(),
        };
        let ua = user_agent(&info);
        assert!(
            ua.starts_with("R ("),
            "PPM gating requires 'R (' prefix; got: {ua}"
        );
        assert!(ua.contains("linux-gnu"));
    }

    #[test]
    fn host_info_uses_pretty_distro_label() {
        let os_release = r#"NAME="Alpine Linux"
ID=alpine
VERSION_ID=3.23.4
"#;
        let info = host_info_from_os_release(Some(os_release), Platform::LinuxX86_64, "4.5.0");
        assert_eq!(info.distro_label, "Alpine Linux 3.23.4");
        assert_eq!(info.r_version, "4.5.0");
    }

    #[test]
    fn host_info_unknown_distro_label() {
        let info = host_info_from_os_release(None, Platform::LinuxX86_64, "4.5.0");
        assert_eq!(info.distro_label, "unknown");
    }

    #[test]
    fn detect_posit_distro_slug_alpine_full_version() {
        // Alpine 3.23.4 reports VERSION_ID="3.23.4"; we truncate to 3.23
        // (matching the existing #30 sysreqs normalization).
        let slug = detect_posit_distro_slug_from_os_release(Some("ID=alpine\nVERSION_ID=3.23.4\n"));
        assert_eq!(slug, "alpine-3.23");
    }

    #[test]
    fn detect_posit_distro_slug_alpine_minor_only() {
        let slug = detect_posit_distro_slug_from_os_release(Some("ID=alpine\nVERSION_ID=3.21\n"));
        assert_eq!(slug, "alpine-3.21");
    }

    #[test]
    fn detect_posit_distro_slug_unknown_distro_still_falls_back() {
        // Regression: Arch / NixOS / Gentoo / etc. keep the ubuntu-2204
        // fallback (out of scope for this PR).
        let slug = detect_posit_distro_slug_from_os_release(Some("ID=arch\n"));
        assert_eq!(slug, "ubuntu-2204");
    }

    #[test]
    fn detect_posit_distro_slug_alpine_skips_p3m() {
        // Integration check: alpine slug must not resolve to a PPM codename,
        // so P3MBinaryIndex returns empty for alpine and sync falls through
        // to source compile.
        assert!(crate::registry::p3m::ppm_linux_codename("alpine-3.23").is_none());
        assert!(crate::registry::p3m::ppm_linux_codename("alpine-3.21").is_none());
    }
}
