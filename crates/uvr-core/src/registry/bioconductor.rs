use std::collections::HashMap;
use std::io::Read;

use flate2::read::GzDecoder;
use tracing::{debug, info};

use crate::error::{Result, UvrError};
use crate::lockfile::PackageSource;
use crate::registry::cran::{parse_dcf_block, CranPackageEntry};
use crate::registry::PackageInfo;
use crate::resolver::PackageRegistry;

fn bioc_release_for_r(r_major: u64, r_minor: u64) -> &'static str {
    match (r_major, r_minor) {
        (4, 5) => "3.21",
        (4, 4) => "3.20",
        (4, 3) => "3.18",
        (4, 2) => "3.16",
        (4, 1) => "3.14",
        (4, 0) => "3.12",
        _ => "3.21",
    }
}

pub struct BiocRegistry {
    packages: HashMap<String, CranPackageEntry>,
    bioc_release: String,
}

impl BiocRegistry {
    /// Fetch the Bioconductor package index for the release matching `r_version`.
    pub async fn fetch(client: &reqwest::Client, r_version: &str) -> Result<Self> {
        let parts: Vec<&str> = r_version.split('.').collect();
        let major: u64 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(4);
        let minor: u64 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(4);
        let bioc_release = bioc_release_for_r(major, minor);
        Self::fetch_release(client, bioc_release).await
    }

    /// Fetch the Bioconductor package index for a specific release (e.g. `"3.18"`).
    pub async fn fetch_release(client: &reqwest::Client, bioc_release: &str) -> Result<Self> {
        let cache_key = format!("bioc-{bioc_release}");
        let cache_path = crate::registry::cran::cache_path_for(&cache_key);
        let has_cache = cache_path.exists();

        let url = format!(
            "https://bioconductor.org/packages/{bioc_release}/bioc/src/contrib/PACKAGES.gz"
        );

        // Try HTTP conditional request if we have a cached index
        if has_cache {
            if let Some((etag, last_modified)) =
                crate::registry::cran::read_cache_meta(&cache_key)
            {
                let mut req = client.get(&url);
                if let Some(ref e) = etag {
                    req = req.header("If-None-Match", e.as_str());
                }
                if let Some(ref lm) = last_modified {
                    req = req.header("If-Modified-Since", lm.as_str());
                }
                if let Ok(resp) = req.send().await {
                    if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
                        debug!("Bioconductor {bioc_release}: HTTP 304, using cache");
                        let raw = std::fs::read_to_string(&cache_path)?;
                        let packages = parse_bioc_text(&raw);
                        info!(
                            "Bioconductor {bioc_release}: {} packages (cached)",
                            packages.len()
                        );
                        return Ok(BiocRegistry {
                            packages,
                            bioc_release: bioc_release.to_string(),
                        });
                    } else if resp.status().is_success() {
                        let new_etag = resp
                            .headers()
                            .get("etag")
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_string);
                        let new_lm = resp
                            .headers()
                            .get("last-modified")
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_string);
                        let bytes = resp.bytes().await?;
                        let mut gz = GzDecoder::new(bytes.as_ref());
                        let mut text = String::new();
                        gz.read_to_string(&mut text)?;
                        let packages = parse_bioc_text(&text);
                        if let Some(parent) = cache_path.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        let _ = std::fs::write(&cache_path, &text);
                        crate::registry::cran::write_cache_meta(
                            &cache_key,
                            new_etag.as_deref(),
                            new_lm.as_deref(),
                        );
                        info!(
                            "Bioconductor {bioc_release}: {} packages (updated)",
                            packages.len()
                        );
                        return Ok(BiocRegistry {
                            packages,
                            bioc_release: bioc_release.to_string(),
                        });
                    }
                }
                // Conditional request failed — use stale cache
                debug!("Bioconductor {bioc_release}: conditional request failed, using cache");
                let raw = std::fs::read_to_string(&cache_path)?;
                let packages = parse_bioc_text(&raw);
                info!(
                    "Bioconductor {bioc_release}: {} packages (cached, stale)",
                    packages.len()
                );
                return Ok(BiocRegistry {
                    packages,
                    bioc_release: bioc_release.to_string(),
                });
            }
            // No meta but cache exists — use it
            let raw = std::fs::read_to_string(&cache_path)?;
            let packages = parse_bioc_text(&raw);
            info!(
                "Bioconductor {bioc_release}: {} packages (cached)",
                packages.len()
            );
            return Ok(BiocRegistry {
                packages,
                bioc_release: bioc_release.to_string(),
            });
        }

        // No cache — full download
        info!("Downloading Bioconductor {bioc_release} PACKAGES.gz...");
        let resp = client.get(&url).send().await?;
        if !resp.status().is_success() {
            return Err(UvrError::Other(format!(
                "Failed to fetch Bioconductor index (HTTP {})",
                resp.status()
            )));
        }
        let new_etag = resp
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let new_lm = resp
            .headers()
            .get("last-modified")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let bytes = resp.bytes().await?;
        let mut gz = GzDecoder::new(bytes.as_ref());
        let mut text = String::new();
        gz.read_to_string(&mut text)?;
        let packages = parse_bioc_text(&text);

        if let Some(parent) = cache_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&cache_path, &text);
        crate::registry::cran::write_cache_meta(
            &cache_key,
            new_etag.as_deref(),
            new_lm.as_deref(),
        );

        info!("Bioconductor {bioc_release}: {} packages", packages.len());
        Ok(BiocRegistry {
            packages,
            bioc_release: bioc_release.to_string(),
        })
    }

    /// The Bioconductor release version this registry was fetched for (e.g. `"3.18"`).
    pub fn release(&self) -> &str {
        &self.bioc_release
    }
}

impl PackageRegistry for BiocRegistry {
    fn resolve_package(&self, name: &str, constraint: Option<&str>) -> Result<PackageInfo> {
        let entry = self
            .packages
            .get(name)
            .ok_or_else(|| UvrError::PackageNotFound(name.to_string()))?;

        // Validate constraint if provided
        if let Some(c) = constraint {
            if c != "*" && !c.is_empty() {
                let req = crate::resolver::parse_version_req(c)?;
                if !crate::resolver::version_matches_req(&entry.version, &req) {
                    return Err(UvrError::NoMatchingVersion {
                        package: name.to_string(),
                        constraint: c.to_string(),
                    });
                }
            }
        }

        let url = format!(
            "https://bioconductor.org/packages/{}/bioc/src/contrib/{}_{}.tar.gz",
            self.bioc_release, entry.name, entry.raw_version
        );

        Ok(PackageInfo {
            name: entry.name.clone(),
            version: entry.version.clone(),
            source: PackageSource::Bioconductor,
            checksum: if entry.md5sum.is_empty() {
                None
            } else {
                Some(format!("md5:{}", entry.md5sum))
            },
            requires: entry.requires_as_deps(),
            url,
            raw_version: Some(entry.raw_version.clone()),
            system_requirements: entry.system_requirements.clone(),
        })
    }
}

fn parse_bioc_text(text: &str) -> HashMap<String, CranPackageEntry> {
    let mut packages = HashMap::new();
    for block in text.split("\n\n") {
        let block = block.trim();
        if block.is_empty() {
            continue;
        }
        if let Some(entry) = parse_dcf_block(block) {
            packages.insert(entry.name.clone(), entry);
        }
    }
    packages
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bioc_release_mapping() {
        assert_eq!(bioc_release_for_r(4, 5), "3.21");
        assert_eq!(bioc_release_for_r(4, 4), "3.20");
        assert_eq!(bioc_release_for_r(4, 3), "3.18");
        assert_eq!(bioc_release_for_r(4, 2), "3.16");
        assert_eq!(bioc_release_for_r(4, 1), "3.14");
        assert_eq!(bioc_release_for_r(4, 0), "3.12");
    }

    #[test]
    fn bioc_release_fallback() {
        // Unknown R versions fall back to latest
        assert_eq!(bioc_release_for_r(5, 0), "3.21");
        assert_eq!(bioc_release_for_r(3, 6), "3.21");
    }

    #[test]
    fn resolve_missing_package() {
        let registry = BiocRegistry {
            packages: HashMap::new(),
            bioc_release: "3.20".to_string(),
        };
        let result = registry.resolve_package("NonExistentPkg", None);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_package_basic() {
        let mut packages = HashMap::new();
        packages.insert(
            "DESeq2".to_string(),
            CranPackageEntry {
                name: "DESeq2".to_string(),
                version: semver::Version::new(1, 42, 0),
                raw_version: "1.42.0".to_string(),
                depends: vec![],
                imports: vec![],
                linking_to: vec![],
                md5sum: "abc123".to_string(),
                system_requirements: None,
            },
        );
        let registry = BiocRegistry {
            packages,
            bioc_release: "3.20".to_string(),
        };
        let info = registry.resolve_package("DESeq2", None).unwrap();
        assert_eq!(info.name, "DESeq2");
        assert_eq!(info.source, PackageSource::Bioconductor);
        assert!(info.url.contains("bioconductor.org"));
        assert!(info.url.contains("3.20"));
        assert_eq!(info.checksum, Some("md5:abc123".to_string()));
    }

    #[test]
    fn resolve_package_with_wildcard_constraint() {
        let mut packages = HashMap::new();
        packages.insert(
            "GenomicRanges".to_string(),
            CranPackageEntry {
                name: "GenomicRanges".to_string(),
                version: semver::Version::new(1, 54, 0),
                raw_version: "1.54.0".to_string(),
                depends: vec![],
                imports: vec![],
                linking_to: vec![],
                md5sum: String::new(),
                system_requirements: None,
            },
        );
        let registry = BiocRegistry {
            packages,
            bioc_release: "3.18".to_string(),
        };
        // "*" constraint should match anything
        let info = registry
            .resolve_package("GenomicRanges", Some("*"))
            .unwrap();
        assert_eq!(info.name, "GenomicRanges");
        // Empty md5 → no checksum
        assert_eq!(info.checksum, None);
    }

    #[test]
    fn resolve_package_constraint_mismatch() {
        let mut packages = HashMap::new();
        packages.insert(
            "SummarizedExperiment".to_string(),
            CranPackageEntry {
                name: "SummarizedExperiment".to_string(),
                version: semver::Version::new(1, 30, 0),
                raw_version: "1.30.0".to_string(),
                depends: vec![],
                imports: vec![],
                linking_to: vec![],
                md5sum: String::new(),
                system_requirements: None,
            },
        );
        let registry = BiocRegistry {
            packages,
            bioc_release: "3.18".to_string(),
        };
        // Version 1.30.0 should not satisfy >=2.0.0
        let result = registry.resolve_package("SummarizedExperiment", Some(">=2.0.0"));
        assert!(result.is_err());
    }

    #[test]
    fn release_returns_bioc_version() {
        let registry = BiocRegistry {
            packages: HashMap::new(),
            bioc_release: "3.20".to_string(),
        };
        assert_eq!(registry.release(), "3.20");
    }
}
