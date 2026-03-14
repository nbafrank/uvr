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
        P3MBinaryIndex { packages: HashMap::new() }
    }

    /// Fetch (and cache) the P3M binary PACKAGES index for the given R minor version
    /// and platform. Returns an empty index on any error so callers fall back to source.
    pub async fn fetch(client: &reqwest::Client, r_minor: &str, platform: Platform) -> Self {
        let Some(arch) = platform_arch(platform) else {
            return Self::empty(); // unsupported platform (e.g. Linux — handle later)
        };
        match fetch_inner(client, r_minor, arch).await {
            Ok(idx) => idx,
            Err(e) => {
                tracing::warn!("P3M binary index unavailable ({}), falling back to source", e);
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

async fn fetch_inner(client: &reqwest::Client, r_minor: &str, arch: &str) -> Result<P3MBinaryIndex> {
    let cache = cache_path(r_minor, arch);

    // Use today's cached file if present.
    let text = if let Ok(cached) = std::fs::read_to_string(&cache) {
        cached
    } else {
        let url = format!(
            "https://packagemanager.posit.co/cran/latest/bin/macosx/{arch}/contrib/{r_minor}/PACKAGES.gz"
        );
        info!("Fetching P3M binary index from {url}");
        let bytes = client.get(&url).send().await?.error_for_status()?.bytes().await?;
        let mut gz = GzDecoder::new(bytes.as_ref());
        let mut text = String::new();
        gz.read_to_string(&mut text)?;
        if let Some(parent) = cache.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&cache, &text);
        text
    };

    Ok(parse_index(&text, r_minor, arch))
}

fn parse_index(text: &str, r_minor: &str, arch: &str) -> P3MBinaryIndex {
    let base = format!(
        "https://packagemanager.posit.co/cran/latest/bin/macosx/{arch}/contrib/{r_minor}"
    );
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
            let url = format!("{base}/{n}_{v}.tgz");
            // Normalize the version (e.g. "4.6.0-1" → "4.6.0.1") to match the
            // semver-normalized version stored in LockedPackage.
            packages.insert(n, (normalize_version(&v), url));
        }
    }
    info!("P3M binary index: {} packages", packages.len());
    P3MBinaryIndex { packages }
}

/// Map platform to the arch string used in P3M URLs (`big-sur-arm64` etc.).
/// Returns `None` for platforms without macOS binary support.
fn platform_arch(platform: Platform) -> Option<&'static str> {
    match platform {
        Platform::MacOsArm64 => Some("big-sur-arm64"),
        Platform::MacOsX86_64 => Some("big-sur-x86_64"),
        _ => None,
    }
}

fn cache_path(r_minor: &str, arch: &str) -> PathBuf {
    let date = Local::now().format("%Y-%m-%d").to_string();
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".uvr")
        .join("cache")
        .join(format!("p3m-{r_minor}-{arch}-{date}.txt"))
}
