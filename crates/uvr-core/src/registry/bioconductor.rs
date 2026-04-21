use std::collections::HashMap;
use std::io::Read;

use flate2::read::GzDecoder;
use tracing::{debug, warn};

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

/// Bioconductor ships packages in four parallel indexes. A software package
/// like DESeq2 may depend on a data/annotation package like GenomeInfoDbData,
/// so we fetch and merge all four.
struct BiocEntry {
    entry: CranPackageEntry,
    /// Sub-repo path fragment (e.g. `"bioc"`, `"data/annotation"`).
    subrepo: &'static str,
}

pub struct BiocRegistry {
    packages: HashMap<String, BiocEntry>,
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
    ///
    /// Fetches software, data/annotation, data/experiment, and workflows sub-repos
    /// in parallel and merges them. Software wins on name conflicts.
    pub async fn fetch_release(client: &reqwest::Client, bioc_release: &str) -> Result<Self> {
        let (software, annotation, experiment, workflows) = tokio::join!(
            fetch_subrepo(client, bioc_release, "bioc", "bioc"),
            fetch_subrepo(client, bioc_release, "data-annotation", "data/annotation"),
            fetch_subrepo(client, bioc_release, "data-experiment", "data/experiment"),
            fetch_subrepo(client, bioc_release, "workflows", "workflows"),
        );

        // Software (bioc) is mandatory — fail if it can't be fetched at all.
        let software = software.map_err(|e| {
            UvrError::Other(format!(
                "Failed to fetch Bioconductor {bioc_release} software index: {e}"
            ))
        })?;

        let mut packages: HashMap<String, BiocEntry> = HashMap::new();

        for (entry_map, subrepo) in [
            (Some(software), "bioc"),
            (annotation.ok(), "data/annotation"),
            (experiment.ok(), "data/experiment"),
            (workflows.ok(), "workflows"),
        ] {
            let Some(map) = entry_map else {
                warn!("Bioconductor {bioc_release} {subrepo}: fetch failed, skipping");
                continue;
            };
            for (name, entry) in map {
                packages.entry(name).or_insert(BiocEntry { entry, subrepo });
            }
        }

        debug!(
            "Bioconductor {bioc_release}: {} packages (software + data + workflows)",
            packages.len()
        );

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

/// Fetch a single Bioconductor sub-repo index. Honors HTTP conditional GET
/// via cached ETag / Last-Modified headers when a local cache exists.
async fn fetch_subrepo(
    client: &reqwest::Client,
    bioc_release: &str,
    cache_key_suffix: &str,
    subrepo_path: &str,
) -> Result<HashMap<String, CranPackageEntry>> {
    let cache_key = format!("bioc-{bioc_release}-{cache_key_suffix}");
    let cache_path = crate::registry::cran::cache_path_for(&cache_key);
    let has_cache = cache_path.exists();

    let url = format!(
        "https://bioconductor.org/packages/{bioc_release}/{subrepo_path}/src/contrib/PACKAGES.gz"
    );

    if has_cache {
        if let Some((etag, last_modified)) = crate::registry::cran::read_cache_meta(&cache_key) {
            let mut req = client.get(&url);
            if let Some(ref e) = etag {
                req = req.header("If-None-Match", e.as_str());
            }
            if let Some(ref lm) = last_modified {
                req = req.header("If-Modified-Since", lm.as_str());
            }
            if let Ok(resp) = req.send().await {
                if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
                    debug!("Bioc {bioc_release}/{subrepo_path}: HTTP 304, using cache");
                    let raw = std::fs::read_to_string(&cache_path)?;
                    return Ok(parse_bioc_text(&raw));
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
                    if let Some(parent) = cache_path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let _ = std::fs::write(&cache_path, &text);
                    crate::registry::cran::write_cache_meta(
                        &cache_key,
                        new_etag.as_deref(),
                        new_lm.as_deref(),
                    );
                    return Ok(parse_bioc_text(&text));
                }
            }
            debug!("Bioc {bioc_release}/{subrepo_path}: conditional request failed, using cache");
            let raw = std::fs::read_to_string(&cache_path)?;
            return Ok(parse_bioc_text(&raw));
        }
        let raw = std::fs::read_to_string(&cache_path)?;
        return Ok(parse_bioc_text(&raw));
    }

    debug!("Downloading Bioconductor {bioc_release}/{subrepo_path} PACKAGES.gz...");
    let resp = client.get(&url).send().await?;
    if !resp.status().is_success() {
        return Err(UvrError::Other(format!(
            "Failed to fetch Bioconductor {subrepo_path} index (HTTP {})",
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

    if let Some(parent) = cache_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&cache_path, &text);
    crate::registry::cran::write_cache_meta(&cache_key, new_etag.as_deref(), new_lm.as_deref());

    Ok(parse_bioc_text(&text))
}

impl PackageRegistry for BiocRegistry {
    fn resolve_package(&self, name: &str, constraint: Option<&str>) -> Result<PackageInfo> {
        let bioc = self
            .packages
            .get(name)
            .ok_or_else(|| UvrError::PackageNotFound(name.to_string()))?;
        let entry = &bioc.entry;

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
            "https://bioconductor.org/packages/{}/{}/src/contrib/{}_{}.tar.gz",
            self.bioc_release, bioc.subrepo, entry.name, entry.raw_version
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

    fn make_entry(name: &str, raw_version: &str, md5: &str) -> CranPackageEntry {
        let parts: Vec<u64> = raw_version
            .split(['.', '-'])
            .filter_map(|s| s.parse().ok())
            .collect();
        let version = semver::Version::new(
            parts.first().copied().unwrap_or(0),
            parts.get(1).copied().unwrap_or(0),
            parts.get(2).copied().unwrap_or(0),
        );
        CranPackageEntry {
            name: name.to_string(),
            version,
            raw_version: raw_version.to_string(),
            depends: vec![],
            imports: vec![],
            linking_to: vec![],
            md5sum: md5.to_string(),
            system_requirements: None,
        }
    }

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
    fn resolve_software_package_uses_bioc_subrepo() {
        let mut packages = HashMap::new();
        packages.insert(
            "DESeq2".to_string(),
            BiocEntry {
                entry: make_entry("DESeq2", "1.42.0", "abc123"),
                subrepo: "bioc",
            },
        );
        let registry = BiocRegistry {
            packages,
            bioc_release: "3.20".to_string(),
        };
        let info = registry.resolve_package("DESeq2", None).unwrap();
        assert_eq!(info.source, PackageSource::Bioconductor);
        assert!(info
            .url
            .contains("/3.20/bioc/src/contrib/DESeq2_1.42.0.tar.gz"));
        assert_eq!(info.checksum, Some("md5:abc123".to_string()));
    }

    #[test]
    fn resolve_annotation_package_uses_data_annotation_subrepo() {
        let mut packages = HashMap::new();
        packages.insert(
            "GenomeInfoDbData".to_string(),
            BiocEntry {
                entry: make_entry("GenomeInfoDbData", "1.2.13", ""),
                subrepo: "data/annotation",
            },
        );
        let registry = BiocRegistry {
            packages,
            bioc_release: "3.21".to_string(),
        };
        let info = registry.resolve_package("GenomeInfoDbData", None).unwrap();
        assert!(info
            .url
            .contains("/3.21/data/annotation/src/contrib/GenomeInfoDbData_1.2.13.tar.gz"));
        assert_eq!(info.checksum, None);
    }

    #[test]
    fn resolve_package_constraint_mismatch() {
        let mut packages = HashMap::new();
        packages.insert(
            "SummarizedExperiment".to_string(),
            BiocEntry {
                entry: make_entry("SummarizedExperiment", "1.30.0", ""),
                subrepo: "bioc",
            },
        );
        let registry = BiocRegistry {
            packages,
            bioc_release: "3.18".to_string(),
        };
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
