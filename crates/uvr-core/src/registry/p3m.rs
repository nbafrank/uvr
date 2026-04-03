use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;

use chrono::Local;
use flate2::read::GzDecoder;
use tracing::info;

use crate::error::Result;
use crate::r_version::downloader::Platform;
use crate::resolver::normalize_version;

/// Pre-built binary package index from Posit Package Manager (P3M).
///
/// P3M provides pre-compiled `.tgz` binaries for macOS that can be extracted
/// directly into the project library — no `R CMD INSTALL` or system libraries needed.
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
    /// and platform. Returns an empty index on any error so callers fall back to source.
    pub async fn fetch(client: &reqwest::Client, r_minor: &str, platform: Platform) -> Self {
        let Some(info) = platform_info(platform) else {
            return Self::empty(); // unsupported platform (e.g. Linux — no P3M binaries)
        };
        match fetch_inner(client, r_minor, info).await {
            Ok(idx) => idx,
            Err(e) => {
                tracing::warn!(
                    "P3M binary index unavailable ({}), falling back to source",
                    e
                );
                Self::empty()
            }
        }
    }

    /// Return the binary download URL if P3M has a binary for the exact (name, version).
    pub fn binary_url(&self, name: &str, version: &str) -> Option<&str> {
        self.packages
            .get(name)
            .filter(|(v, _)| v == version)
            .map(|(_, url)| url.as_str())
    }
}

async fn fetch_inner(
    client: &reqwest::Client,
    r_minor: &str,
    platform_info: PlatformInfo,
) -> Result<P3MBinaryIndex> {
    let cache = cache_path(r_minor, platform_info.cache_key);

    // Use today's cached file if present.
    let (text, from_cache) = if let Ok(cached) = std::fs::read_to_string(&cache) {
        (cached, true)
    } else {
        let url = format!(
            "https://packagemanager.posit.co/cran/latest/bin/{}/contrib/{r_minor}/PACKAGES.gz",
            platform_info.url_segment
        );
        info!("Fetching P3M binary index from {url}");
        let bytes = client
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        let mut gz = GzDecoder::new(bytes.as_ref());
        let mut text = String::new();
        gz.read_to_string(&mut text)?;
        (text, false)
    };

    let index = parse_index(&text, r_minor, &platform_info);

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

fn parse_index(text: &str, r_minor: &str, info: &PlatformInfo) -> P3MBinaryIndex {
    let base = format!(
        "https://packagemanager.posit.co/cran/latest/bin/{}/contrib/{r_minor}",
        info.url_segment
    );
    let ext = info.pkg_ext;
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
            let url = format!("{base}/{n}_{v}.{ext}");
            // Normalize the version (e.g. "4.6.0-1" → "4.6.0.1") to match the
            // semver-normalized version stored in LockedPackage.
            packages.insert(n, (normalize_version(&v), url));
        }
    }
    info!("P3M binary index: {} packages", packages.len());
    P3MBinaryIndex { packages }
}

/// Platform-specific info for P3M URL construction.
struct PlatformInfo {
    /// URL segment after `/bin/` (e.g. `macosx/big-sur-arm64` or `windows`).
    url_segment: &'static str,
    /// File extension for binary packages (`tgz` or `zip`).
    pkg_ext: &'static str,
    /// Cache key suffix.
    cache_key: &'static str,
}

/// Map platform to P3M URL info. Returns `None` for platforms without binary support.
fn platform_info(platform: Platform) -> Option<PlatformInfo> {
    match platform {
        Platform::MacOsArm64 => Some(PlatformInfo {
            url_segment: "macosx/big-sur-arm64",
            cache_key: "macos-arm64",
            pkg_ext: "tgz",
        }),
        Platform::MacOsX86_64 => Some(PlatformInfo {
            url_segment: "macosx/big-sur-x86_64",
            cache_key: "macos-x86_64",
            pkg_ext: "tgz",
        }),
        Platform::WindowsX86_64 => Some(PlatformInfo {
            url_segment: "windows",
            cache_key: "windows",
            pkg_ext: "zip",
        }),
        _ => None, // Linux — no P3M binaries yet
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

    #[test]
    fn parse_index_basic() {
        let text = "\
Package: ggplot2
Version: 3.5.1

Package: dplyr
Version: 1.1.4

";
        let info = PlatformInfo {
            url_segment: "macosx/big-sur-arm64",
            cache_key: "macos-arm64",
            pkg_ext: "tgz",
        };
        let index = parse_index(text, "4.4", &info);
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
        let info = PlatformInfo {
            url_segment: "windows",
            cache_key: "windows",
            pkg_ext: "zip",
        };
        let index = parse_index(text, "4.4", &info);
        let url = index.binary_url("jsonlite", "1.8.8").unwrap();
        assert!(url.contains("jsonlite_1.8.8.zip"));
        assert!(url.contains("/windows/"));
    }

    #[test]
    fn parse_index_empty() {
        let info = PlatformInfo {
            url_segment: "macosx/big-sur-arm64",
            cache_key: "macos-arm64",
            pkg_ext: "tgz",
        };
        let index = parse_index("", "4.4", &info);
        assert_eq!(index.packages.len(), 0);
    }

    #[test]
    fn binary_url_version_mismatch() {
        let text = "Package: ggplot2\nVersion: 3.5.1\n\n";
        let info = PlatformInfo {
            url_segment: "macosx/big-sur-arm64",
            cache_key: "macos-arm64",
            pkg_ext: "tgz",
        };
        let index = parse_index(text, "4.4", &info);
        // Wrong version → None
        assert!(index.binary_url("ggplot2", "3.4.0").is_none());
        // Wrong name → None
        assert!(index.binary_url("dplyr", "3.5.1").is_none());
    }

    #[test]
    fn binary_url_version_normalization() {
        // P3M may have versions like "4.6.0-1" which normalize to "4.6.0.1"
        let text = "Package: RcppArmadillo\nVersion: 14.2.2-1\n\n";
        let info = PlatformInfo {
            url_segment: "macosx/big-sur-arm64",
            cache_key: "macos-arm64",
            pkg_ext: "tgz",
        };
        let index = parse_index(text, "4.4", &info);
        // The lockfile stores the normalized version
        let normalized = crate::resolver::normalize_version("14.2.2-1");
        assert!(index.binary_url("RcppArmadillo", &normalized).is_some());
    }

    #[test]
    fn platform_info_macos_arm64() {
        let info = platform_info(Platform::MacOsArm64).unwrap();
        assert_eq!(info.url_segment, "macosx/big-sur-arm64");
        assert_eq!(info.pkg_ext, "tgz");
    }

    #[test]
    fn platform_info_windows() {
        let info = platform_info(Platform::WindowsX86_64).unwrap();
        assert_eq!(info.url_segment, "windows");
        assert_eq!(info.pkg_ext, "zip");
    }

    #[test]
    fn platform_info_linux_none() {
        assert!(platform_info(Platform::LinuxX86_64).is_none());
        assert!(platform_info(Platform::LinuxArm64).is_none());
    }

    #[test]
    fn empty_index() {
        let idx = P3MBinaryIndex::empty();
        assert!(idx.binary_url("anything", "1.0.0").is_none());
    }
}
