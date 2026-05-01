use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;

use chrono::Local;
use flate2::read::GzDecoder;
use tracing::debug;

use crate::error::Result;
use crate::r_version::downloader::Platform;
use crate::resolver::normalize_version;

/// Pre-built binary package index from Posit Package Manager (P3M).
///
/// P3M provides pre-compiled binaries for:
/// - macOS (`.tgz`, extracted directly into the library).
/// - Windows (`.zip`, extracted directly into the library).
/// - Linux distros covered by PPM's `__linux__/<codename>/` URL space
///   (Ubuntu 20.04/22.04/24.04, Debian 11/12, RHEL 8/9, openSUSE 15.4/15.5,
///   …) — served as `.tar.gz` files at the same URL path as source, with
///   a User-Agent-keyed binary build (#55). The tarball still installs
///   via `R CMD INSTALL`, but no compilation is needed.
pub struct P3MBinaryIndex {
    /// package name → (version, binary_url)
    packages: HashMap<String, (String, String)>,
}

impl P3MBinaryIndex {
    pub fn empty() -> Self {
        P3MBinaryIndex {
            packages: HashMap::new(),
        }
    }

    /// Fetch (and cache) the P3M binary PACKAGES index for the given R minor version
    /// and platform. When `bioc_release` is provided, also fetches the matching P3M
    /// Bioconductor binary index and merges it in — so Bioc packages like edgeR get
    /// pre-compiled binaries instead of requiring local compilation (which needs
    /// gfortran etc. on macOS). Returns an empty index on any error so callers fall
    /// back to source.
    pub async fn fetch(
        client: &reqwest::Client,
        r_minor: &str,
        platform: Platform,
        bioc_release: Option<&str>,
        posit_distro_slug: Option<&str>,
    ) -> Self {
        let Some(info) = platform_info(platform, posit_distro_slug, r_minor) else {
            // Unsupported platform combination: macOS x86 / Windows always
            // OK; Linux only when we recognise the distro. Falls through
            // to source — same behavior as before #55.
            return Self::empty();
        };

        let cran_fut = fetch_repo_index(client, r_minor, &info, P3MRepo::Cran);
        let bioc_fut = async {
            match bioc_release {
                // BLOCK fix: Bioconductor doesn't serve Linux binaries — its
                // /packages/<release>/bioc/src/contrib/PACKAGES.gz is the
                // SOURCE index. Without this guard, Linux projects with Bioc
                // deps would register source tarball URLs in the binary index,
                // and `install_binary_package` would extract a source tree
                // (R/, src/, no libs/*.so) into the library — silently broken
                // installs that fail at `library()` time.
                Some(release) if !info.is_linux => {
                    fetch_repo_index(client, r_minor, &info, P3MRepo::Bioc(release))
                        .await
                        .ok()
                }
                _ => None,
            }
        };
        let (cran_result, bioc_result) = tokio::join!(cran_fut, bioc_fut);

        let mut packages: HashMap<String, (String, String)> = HashMap::new();
        match cran_result {
            Ok(cran) => packages.extend(cran.packages),
            Err(e) => {
                tracing::warn!(
                    "P3M CRAN binary index unavailable ({}), falling back to source",
                    e
                );
            }
        }
        if let Some(bioc) = bioc_result {
            // CRAN entries take precedence on name conflicts — extend(...) would
            // overwrite, so insert only if not already present.
            for (name, entry) in bioc.packages {
                packages.entry(name).or_insert(entry);
            }
        }
        P3MBinaryIndex { packages }
    }

    /// Return the binary download URL if P3M has a binary for the exact (name, version).
    pub fn binary_url(&self, name: &str, version: &str) -> Option<&str> {
        self.packages
            .get(name)
            .filter(|(v, _)| v == version)
            .map(|(_, url)| url.as_str())
    }
}

/// Which binary repo to query. Each has a different URL prefix.
///
/// CRAN binaries come from P3M (Posit Package Manager).
/// Bioc binaries come directly from bioconductor.org — P3M does not mirror them.
#[derive(Clone, Copy)]
enum P3MRepo<'a> {
    Cran,
    Bioc(&'a str), // Bioc release (e.g. "3.21")
}

impl<'a> P3MRepo<'a> {
    /// Build the repo prefix. On Linux for the CRAN repo we inject the
    /// `__linux__/<codename>` segment that triggers PPM's binary-aware
    /// routing. macOS/Windows use the plain `cran/latest` prefix and pick
    /// up binaries through `bin/<arch>/contrib/<r_minor>/`.
    fn url_prefix(&self, info: &PlatformInfo) -> String {
        match self {
            P3MRepo::Cran => match &info.linux_codename {
                Some(codename) => {
                    format!("https://packagemanager.posit.co/cran/__linux__/{codename}/latest")
                }
                None => "https://packagemanager.posit.co/cran/latest".to_string(),
            },
            P3MRepo::Bioc(release) => {
                format!("https://bioconductor.org/packages/{release}/bioc")
            }
        }
    }
    fn cache_tag(&self) -> String {
        match self {
            P3MRepo::Cran => "cran".to_string(),
            P3MRepo::Bioc(release) => format!("bioc-{release}"),
        }
    }
    fn label(&self) -> String {
        match self {
            P3MRepo::Cran => "CRAN".to_string(),
            P3MRepo::Bioc(release) => format!("Bioc {release}"),
        }
    }
}

async fn fetch_repo_index(
    client: &reqwest::Client,
    r_minor: &str,
    platform_info: &PlatformInfo,
    repo: P3MRepo<'_>,
) -> Result<P3MBinaryIndex> {
    let cache = cache_path(
        r_minor,
        &format!("{}-{}", platform_info.cache_key, repo.cache_tag()),
    );

    // Use today's cached file if present.
    let (text, from_cache) = if let Ok(cached) = std::fs::read_to_string(&cache) {
        (cached, true)
    } else {
        let url = index_url(&repo, platform_info, r_minor);
        debug!("Fetching P3M {} binary index from {url}", repo.label());
        let mut req = client.get(&url);
        if let Some(ua) = platform_info.user_agent.as_deref() {
            // PPM Linux uses the User-Agent header to gate binary vs source
            // responses. Without an R-shaped UA, PPM serves source even at
            // the `__linux__/<codename>/...` URL.
            req = req.header(reqwest::header::USER_AGENT, ua);
        }
        let bytes = req.send().await?.error_for_status()?.bytes().await?;
        let mut gz = GzDecoder::new(bytes.as_ref());
        let mut text = String::new();
        gz.read_to_string(&mut text)?;
        (text, false)
    };

    let index = parse_index(&text, r_minor, platform_info, &repo);

    // Write cache only AFTER successful parse — avoids poisoning the
    // daily cache with corrupt or truncated network responses.
    if !from_cache {
        if let Some(parent) = cache.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&cache, &text);
    }

    Ok(index)
}

/// Build the URL for a repo's `PACKAGES.gz` index. macOS and Windows live
/// under `bin/<arch>/contrib/<r_minor>/`; Linux lives at the repo root's
/// `src/contrib/` (PPM serves binaries via UA negotiation, not URL path).
fn index_url(repo: &P3MRepo<'_>, info: &PlatformInfo, r_minor: &str) -> String {
    let prefix = repo.url_prefix(info);
    if info.is_linux {
        format!("{prefix}/src/contrib/PACKAGES.gz")
    } else {
        format!(
            "{prefix}/bin/{}/contrib/{r_minor}/PACKAGES.gz",
            info.url_segment
        )
    }
}

/// Build the URL for a single package tarball. Mirrors `index_url`'s
/// platform layout difference.
fn package_url(
    repo: &P3MRepo<'_>,
    info: &PlatformInfo,
    r_minor: &str,
    name: &str,
    version: &str,
) -> String {
    let prefix = repo.url_prefix(info);
    let ext = info.pkg_ext;
    if info.is_linux {
        format!("{prefix}/src/contrib/{name}_{version}.{ext}")
    } else {
        format!(
            "{prefix}/bin/{}/contrib/{r_minor}/{name}_{version}.{ext}",
            info.url_segment
        )
    }
}

fn parse_index(
    text: &str,
    r_minor: &str,
    info: &PlatformInfo,
    repo: &P3MRepo<'_>,
) -> P3MBinaryIndex {
    let mut packages = HashMap::new();
    for block in text.split("\n\n") {
        let block = block.trim();
        if block.is_empty() {
            continue;
        }
        let mut name = None;
        let mut version = None;
        for line in block.lines() {
            if let Some(v) = line.strip_prefix("Package: ") {
                name = Some(v.trim().to_string());
            } else if let Some(v) = line.strip_prefix("Version: ") {
                version = Some(v.trim().to_string());
            }
        }
        if let (Some(n), Some(v)) = (name, version) {
            let url = package_url(repo, info, r_minor, &n, &v);
            // Normalize the version (e.g. "4.6.0-1" → "4.6.0.1") to match the
            // semver-normalized version stored in LockedPackage.
            packages.insert(n, (normalize_version(&v), url));
        }
    }
    debug!(
        "P3M {} binary index: {} packages",
        repo.label(),
        packages.len()
    );
    P3MBinaryIndex { packages }
}

/// Platform-specific info for P3M URL construction.
struct PlatformInfo {
    /// URL segment after `/bin/` (e.g. `macosx/big-sur-arm64`, `windows`).
    /// Empty for Linux (URLs use `src/contrib/` instead, with a codename
    /// in the prefix).
    url_segment: String,
    /// File extension for binary packages (`tgz`, `zip`, `tar.gz`).
    pkg_ext: &'static str,
    /// Cache key suffix.
    cache_key: String,
    /// True when this platform uses PPM's Linux URL layout (no `bin/`,
    /// no R-minor segment; binaries are routed via User-Agent).
    is_linux: bool,
    /// PPM Linux codename (e.g. `jammy`, `bookworm`, `rhel9`). Used to
    /// build the `__linux__/<codename>/latest` URL prefix. None for
    /// macOS/Windows.
    linux_codename: Option<String>,
    /// Optional User-Agent override. PPM Linux requires an R-shaped UA
    /// to serve binaries instead of source — without it, the `tar.gz` at
    /// the same URL is the source bundle. None for macOS/Windows.
    user_agent: Option<String>,
}

/// Map platform to P3M URL info. Returns `None` for unsupported combinations
/// (e.g. Linux with a distro PPM doesn't recognize).
///
/// `posit_distro_slug` should be the same slug uvr uses for the R install
/// URL (`ubuntu-2204`, `debian-12`, `rhel-9`, …); we translate it to PPM's
/// codename system (`jammy`, `bookworm`, `rhel9`) here.
fn platform_info(
    platform: Platform,
    posit_distro_slug: Option<&str>,
    r_minor: &str,
) -> Option<PlatformInfo> {
    match platform {
        Platform::MacOsArm64 => Some(PlatformInfo {
            url_segment: "macosx/big-sur-arm64".to_string(),
            cache_key: "macos-arm64".to_string(),
            pkg_ext: "tgz",
            is_linux: false,
            linux_codename: None,
            user_agent: None,
        }),
        Platform::MacOsX86_64 => Some(PlatformInfo {
            url_segment: "macosx/big-sur-x86_64".to_string(),
            cache_key: "macos-x86_64".to_string(),
            pkg_ext: "tgz",
            is_linux: false,
            linux_codename: None,
            user_agent: None,
        }),
        Platform::WindowsX86_64 => Some(PlatformInfo {
            url_segment: "windows".to_string(),
            cache_key: "windows".to_string(),
            pkg_ext: "zip",
            is_linux: false,
            linux_codename: None,
            user_agent: None,
        }),
        Platform::LinuxX86_64 | Platform::LinuxArm64 => {
            // PPM Linux only — Bioc binary support comes via Bioconductor's
            // own server, which doesn't serve Linux binaries today; Bioc on
            // Linux falls back to source as before.
            let slug = posit_distro_slug?;
            let codename = ppm_linux_codename(slug)?;
            let arch = if matches!(platform, Platform::LinuxX86_64) {
                "x86_64"
            } else {
                "aarch64"
            };
            // R prints UA as `R (<ver> <triple> <arch> <os>-gnu)`. PPM
            // currently sniffs the "R " prefix and the linux-gnu marker;
            // pass the actual r_minor so the UA stays plausibly current
            // if PPM tightens its sniffing rules.
            let user_agent = format!("R ({r_minor}.0 {arch}-pc-linux-gnu {arch} linux-gnu)");
            Some(PlatformInfo {
                url_segment: String::new(),
                cache_key: format!("linux-{codename}-{arch}"),
                pkg_ext: "tar.gz",
                is_linux: true,
                linux_codename: Some(codename.to_string()),
                user_agent: Some(user_agent),
            })
        }
    }
}

/// Translate a uvr/Posit-CDN distro slug (`ubuntu-2204`, `debian-12`,
/// `rhel-9`, `opensuse-155`, …) to PPM's Linux codename (`jammy`,
/// `bookworm`, `rhel9`, `opensuse155`). Returns `None` for distros PPM
/// doesn't carry binaries for. Slug list pulled from
/// https://packagemanager.posit.co/client/#/repos/cran/setup.
pub fn ppm_linux_codename(posit_slug: &str) -> Option<&'static str> {
    match posit_slug {
        "ubuntu-2004" => Some("focal"),
        "ubuntu-2204" => Some("jammy"),
        "ubuntu-2404" => Some("noble"),
        "debian-11" => Some("bullseye"),
        "debian-12" => Some("bookworm"),
        "rhel-7" | "centos-7" => Some("centos7"),
        "rhel-8" | "centos-8" => Some("centos8"),
        "rhel-9" | "centos-9" => Some("rhel9"),
        "opensuse-154" => Some("opensuse154"),
        "opensuse-155" => Some("opensuse155"),
        _ => None,
    }
}

fn cache_path(r_minor: &str, key: &str) -> PathBuf {
    let date = Local::now().format("%Y-%m-%d").to_string();
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".uvr")
        .join("cache")
        .join(format!("p3m-{r_minor}-{key}-{date}.txt"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn macos_arm64_info() -> PlatformInfo {
        PlatformInfo {
            url_segment: "macosx/big-sur-arm64".to_string(),
            cache_key: "macos-arm64".to_string(),
            pkg_ext: "tgz",
            is_linux: false,
            linux_codename: None,
            user_agent: None,
        }
    }

    fn windows_info() -> PlatformInfo {
        PlatformInfo {
            url_segment: "windows".to_string(),
            cache_key: "windows".to_string(),
            pkg_ext: "zip",
            is_linux: false,
            linux_codename: None,
            user_agent: None,
        }
    }

    fn linux_info(codename: &str) -> PlatformInfo {
        PlatformInfo {
            url_segment: String::new(),
            cache_key: format!("linux-{codename}-x86_64"),
            pkg_ext: "tar.gz",
            is_linux: true,
            linux_codename: Some(codename.to_string()),
            user_agent: Some("R (4.5.3 x86_64-pc-linux-gnu x86_64 linux-gnu)".to_string()),
        }
    }

    #[test]
    fn parse_index_basic() {
        let text = "\
Package: ggplot2
Version: 3.5.1

Package: dplyr
Version: 1.1.4

";
        let info = macos_arm64_info();
        let index = parse_index(text, "4.4", &info, &P3MRepo::Cran);
        assert_eq!(index.packages.len(), 2);

        let url = index.binary_url("ggplot2", "3.5.1").unwrap();
        assert!(url.contains("ggplot2_3.5.1.tgz"));
        assert!(url.contains("big-sur-arm64"));
        assert!(url.contains("4.4"));

        let url = index.binary_url("dplyr", "1.1.4").unwrap();
        assert!(url.contains("dplyr_1.1.4.tgz"));
    }

    #[test]
    fn parse_index_windows() {
        let text = "Package: jsonlite\nVersion: 1.8.8\n\n";
        let info = windows_info();
        let index = parse_index(text, "4.4", &info, &P3MRepo::Cran);
        let url = index.binary_url("jsonlite", "1.8.8").unwrap();
        assert!(url.contains("jsonlite_1.8.8.zip"));
        assert!(url.contains("/windows/"));
    }

    #[test]
    fn parse_index_empty() {
        let info = macos_arm64_info();
        let index = parse_index("", "4.4", &info, &P3MRepo::Cran);
        assert_eq!(index.packages.len(), 0);
    }

    #[test]
    fn parse_index_bioc_uses_bioconductor_url() {
        let text = "Package: edgeR\nVersion: 4.6.3\n\n";
        let info = macos_arm64_info();
        let index = parse_index(text, "4.5", &info, &P3MRepo::Bioc("3.21"));
        let url = index.binary_url("edgeR", "4.6.3").unwrap();
        assert!(url.contains("bioconductor.org/packages/3.21/bioc/bin"));
        assert!(url.contains("edgeR_4.6.3.tgz"));
    }

    #[test]
    fn binary_url_version_mismatch() {
        let text = "Package: ggplot2\nVersion: 3.5.1\n\n";
        let info = macos_arm64_info();
        let index = parse_index(text, "4.4", &info, &P3MRepo::Cran);
        // Wrong version → None
        assert!(index.binary_url("ggplot2", "3.4.0").is_none());
        // Wrong name → None
        assert!(index.binary_url("dplyr", "3.5.1").is_none());
    }

    #[test]
    fn binary_url_version_normalization() {
        // P3M may have versions like "4.6.0-1" which normalize to "4.6.0.1"
        let text = "Package: RcppArmadillo\nVersion: 14.2.2-1\n\n";
        let info = macos_arm64_info();
        let index = parse_index(text, "4.4", &info, &P3MRepo::Cran);
        let normalized = crate::resolver::normalize_version("14.2.2-1");
        assert!(index.binary_url("RcppArmadillo", &normalized).is_some());
    }

    #[test]
    fn platform_info_macos_arm64() {
        let info = platform_info(Platform::MacOsArm64, None, "4.5").unwrap();
        assert_eq!(info.url_segment, "macosx/big-sur-arm64");
        assert_eq!(info.pkg_ext, "tgz");
        assert!(!info.is_linux);
    }

    #[test]
    fn platform_info_windows() {
        let info = platform_info(Platform::WindowsX86_64, None, "4.5").unwrap();
        assert_eq!(info.url_segment, "windows");
        assert_eq!(info.pkg_ext, "zip");
        assert!(!info.is_linux);
    }

    #[test]
    fn platform_info_linux_supported_distro() {
        // #55: Linux gets a binary index when the distro maps to a PPM codename.
        let info = platform_info(Platform::LinuxX86_64, Some("ubuntu-2204"), "4.5")
            .expect("ubuntu-2204 covered");
        assert!(info.is_linux);
        assert_eq!(info.linux_codename.as_deref(), Some("jammy"));
        assert_eq!(info.pkg_ext, "tar.gz");
        assert!(info
            .user_agent
            .as_deref()
            .is_some_and(|ua| ua.contains("R (") && ua.contains("linux-gnu")));
    }

    #[test]
    fn platform_info_linux_unknown_distro_none() {
        // Slackware isn't on PPM — falls back to None (source-only).
        assert!(platform_info(Platform::LinuxX86_64, Some("slackware-15"), "4.5").is_none());
        assert!(platform_info(Platform::LinuxArm64, Some("nixos-2411"), "4.5").is_none());
        // Without a slug, we can't translate.
        assert!(platform_info(Platform::LinuxX86_64, None, "4.5").is_none());
    }

    #[test]
    fn linux_index_url_uses_codename_and_src_contrib() {
        // #55: PPM Linux URLs put the codename in the prefix and serve
        // PACKAGES from `src/contrib/`, not `bin/<arch>/contrib/<minor>/`.
        let info = linux_info("jammy");
        let url = index_url(&P3MRepo::Cran, &info, "4.5");
        assert_eq!(
            url,
            "https://packagemanager.posit.co/cran/__linux__/jammy/latest/src/contrib/PACKAGES.gz"
        );
    }

    #[test]
    fn linux_package_url_drops_r_minor_segment() {
        let info = linux_info("bookworm");
        let url = package_url(&P3MRepo::Cran, &info, "4.5", "ggplot2", "3.5.1");
        assert_eq!(
            url,
            "https://packagemanager.posit.co/cran/__linux__/bookworm/latest/src/contrib/ggplot2_3.5.1.tar.gz"
        );
    }

    #[test]
    fn parse_index_linux_yields_linux_url() {
        let text = "Package: jsonlite\nVersion: 1.8.8\n\n";
        let info = linux_info("jammy");
        let index = parse_index(text, "4.5", &info, &P3MRepo::Cran);
        let url = index.binary_url("jsonlite", "1.8.8").unwrap();
        assert!(url.contains("/__linux__/jammy/"));
        assert!(url.ends_with("jsonlite_1.8.8.tar.gz"));
    }

    #[test]
    fn ppm_codename_mapping_known_distros() {
        assert_eq!(ppm_linux_codename("ubuntu-2204"), Some("jammy"));
        assert_eq!(ppm_linux_codename("ubuntu-2404"), Some("noble"));
        assert_eq!(ppm_linux_codename("debian-12"), Some("bookworm"));
        assert_eq!(ppm_linux_codename("rhel-9"), Some("rhel9"));
        assert_eq!(ppm_linux_codename("opensuse-155"), Some("opensuse155"));
        assert!(ppm_linux_codename("alpine-3.21").is_none());
    }

    #[test]
    fn ppm_codename_rhel_centos_naming_discontinuity() {
        // RHEL 9 broke the centosN naming convention because CentOS Stream
        // replaced CentOS Linux. Pin both the symmetric and the asymmetric
        // cases since they're the highest-value regression anchors when
        // someone edits the match table.
        assert_eq!(ppm_linux_codename("rhel-7"), Some("centos7"));
        assert_eq!(ppm_linux_codename("centos-7"), Some("centos7"));
        assert_eq!(ppm_linux_codename("rhel-8"), Some("centos8"));
        assert_eq!(ppm_linux_codename("centos-8"), Some("centos8"));
        assert_eq!(ppm_linux_codename("rhel-9"), Some("rhel9"));
        assert_eq!(ppm_linux_codename("centos-9"), Some("rhel9"));
    }

    #[test]
    fn linux_user_agent_uses_r_minor_not_hardcoded() {
        // Reviewer flagged the prior hardcoded "4.5.3" UA as a future
        // staleness risk if PPM tightens its UA matching. r_minor flows
        // through and shows up in the UA — verify both arch variants.
        let info_x86 =
            platform_info(Platform::LinuxX86_64, Some("ubuntu-2204"), "4.6").expect("supported");
        let ua = info_x86.user_agent.expect("Linux gets a UA");
        assert!(ua.starts_with("R (4.6.0 x86_64-pc-linux-gnu"), "got {ua}");
        assert!(ua.contains("linux-gnu"));

        let info_arm =
            platform_info(Platform::LinuxArm64, Some("debian-12"), "4.5").expect("supported");
        let ua = info_arm.user_agent.expect("Linux gets a UA");
        assert!(ua.starts_with("R (4.5.0 aarch64-pc-linux-gnu"), "got {ua}");
    }

    #[test]
    fn empty_index() {
        let idx = P3MBinaryIndex::empty();
        assert!(idx.binary_url("anything", "1.0.0").is_none());
    }
}
