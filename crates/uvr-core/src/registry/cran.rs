use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;

use flate2::read::GzDecoder;
use semver::{Version, VersionReq};
use tracing::debug;

use crate::error::{Result, UvrError};
use crate::lockfile::PackageSource;
use crate::registry::{Dep, PackageInfo};
use crate::resolver::{is_base_package, normalize_version, parse_version_req, PackageRegistry};

const CRAN_PACKAGES_URL: &str = "https://cran.r-project.org/src/contrib/PACKAGES.gz";
const CRAN_SRC_BASE: &str = "https://cran.r-project.org/src/contrib";

/// A parsed entry from CRAN's PACKAGES.gz.
#[derive(Debug, Clone)]
pub struct CranPackageEntry {
    pub name: String,
    /// Normalized semver for comparison.
    pub version: Version,
    /// Original version string from DESCRIPTION (e.g. `"1.1-3"`), used in tarball URLs.
    pub raw_version: String,
    pub depends: Vec<DepConstraint>,
    pub imports: Vec<DepConstraint>,
    /// Header-only packages needed at compile time (e.g. `cpp11`, `Rcpp`).
    pub linking_to: Vec<DepConstraint>,
    pub md5sum: String,
    /// Raw `SystemRequirements` field from DESCRIPTION, if present.
    pub system_requirements: Option<String>,
}

impl CranPackageEntry {
    /// All non-base dependencies as `Dep` values carrying their version constraints.
    /// Includes `LinkingTo` entries so header packages are installed before dependents.
    pub fn requires_as_deps(&self) -> Vec<Dep> {
        self.depends
            .iter()
            .chain(self.imports.iter())
            .chain(self.linking_to.iter())
            .filter(|d| !is_base_package(&d.name))
            .map(|d| Dep {
                name: d.name.clone(),
                constraint: d.req.as_ref().map(|r| r.to_string()),
            })
            .collect()
    }

    /// All non-base dependency names (for the lockfile `requires` field).
    pub fn all_requires(&self) -> Vec<String> {
        self.requires_as_deps()
            .into_iter()
            .map(|d| d.name)
            .collect()
    }

    pub fn tarball_url(&self) -> String {
        self.tarball_url_with_base(CRAN_SRC_BASE)
    }

    pub fn tarball_url_with_base(&self, base: &str) -> String {
        format!("{}/{}_{}.tar.gz", base, self.name, self.raw_version)
    }
}

#[derive(Debug, Clone)]
pub struct DepConstraint {
    pub name: String,
    pub req: Option<VersionReq>,
}

/// In-memory CRAN index.
#[derive(Debug, Default)]
pub struct CranIndex {
    /// name → versions sorted newest-first
    packages: HashMap<String, Vec<CranPackageEntry>>,
}

impl CranIndex {
    pub fn get_best(&self, name: &str, constraint: Option<&str>) -> Result<&CranPackageEntry> {
        let entries = self
            .packages
            .get(name)
            .ok_or_else(|| UvrError::PackageNotFound(name.to_string()))?;

        let req = match constraint {
            Some(c) if c != "*" && !c.is_empty() => Some(parse_version_req(c)?),
            _ => None,
        };

        entries
            .iter()
            .find(|e| {
                req.as_ref()
                    .is_none_or(|r| crate::resolver::version_matches_req(&e.version, r))
            })
            .ok_or_else(|| UvrError::NoMatchingVersion {
                package: name.to_string(),
                constraint: constraint.unwrap_or("*").to_string(),
            })
    }

    fn insert(&mut self, entry: CranPackageEntry) {
        let vec = self.packages.entry(entry.name.clone()).or_default();
        vec.push(entry);
        vec.sort_by(|a, b| b.version.cmp(&a.version));
    }

    pub fn len(&self) -> usize {
        self.packages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.packages.is_empty()
    }
}

/// The CRAN registry — downloads and caches the package index.
/// Also used for CRAN-like repositories (r-multiverse, r-universe, PPM)
/// via `fetch_from()`.
pub struct CranRegistry {
    index: CranIndex,
    /// Base URL for tarballs (e.g. `https://cran.r-project.org/src/contrib`).
    src_base: String,
    /// Package source to record in the lockfile.
    source: PackageSource,
}

impl CranRegistry {
    /// Fetch (or load from cache) the CRAN index.
    pub async fn fetch(client: &reqwest::Client, force_refresh: bool) -> Result<Self> {
        Self::fetch_from(
            client,
            "cran",
            CRAN_PACKAGES_URL,
            CRAN_SRC_BASE,
            PackageSource::Cran,
            force_refresh,
        )
        .await
    }

    /// Fetch a CRAN-like index from any repository that serves PACKAGES.gz.
    /// Used for custom repositories like r-multiverse, r-universe, etc.
    pub async fn fetch_custom(
        client: &reqwest::Client,
        repo_name: &str,
        base_url: &str,
        force_refresh: bool,
    ) -> Result<Self> {
        let base_url = base_url.trim_end_matches('/');
        let packages_url = format!("{base_url}/src/contrib/PACKAGES.gz");
        let src_base = format!("{base_url}/src/contrib");
        // Use repo_name + URL hash as cache key to avoid collisions
        // between repos on the same hostname with different paths.
        use md5::{Digest, Md5};
        let hash = hex::encode(Md5::digest(base_url.as_bytes()));
        let cache_key = format!("{repo_name}-{}", &hash[..8]);
        Self::fetch_from(
            client,
            &cache_key,
            &packages_url,
            &src_base,
            PackageSource::Custom {
                name: repo_name.to_string(),
            },
            force_refresh,
        )
        .await
    }

    async fn fetch_from(
        client: &reqwest::Client,
        cache_key: &str,
        packages_url: &str,
        src_base: &str,
        source: PackageSource,
        force_refresh: bool,
    ) -> Result<Self> {
        let cache_path = cache_path_for(cache_key);
        let has_cache = cache_path.exists();

        // Try HTTP conditional request if we have a cached index and aren't forcing refresh.
        if !force_refresh && has_cache {
            if let Some((etag, last_modified)) = read_cache_meta(cache_key) {
                let mut req = client.get(packages_url);
                if let Some(ref e) = etag {
                    req = req.header("If-None-Match", e.as_str());
                }
                if let Some(ref lm) = last_modified {
                    req = req.header("If-Modified-Since", lm.as_str());
                }
                match req.send().await {
                    Ok(resp) if resp.status() == reqwest::StatusCode::NOT_MODIFIED => {
                        debug!("{} index: HTTP 304 Not Modified, using cache", cache_key);
                        let raw = std::fs::read(&cache_path)?;
                        let text = String::from_utf8_lossy(&raw);
                        let index = parse_packages_gz(&text)?;
                        debug!("{} index: {} packages (cached)", cache_key, index.len());
                        return Ok(CranRegistry {
                            index,
                            src_base: src_base.to_string(),
                            source,
                        });
                    }
                    Ok(resp) if resp.status().is_success() => {
                        // Index changed — save new ETag/Last-Modified and body
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
                        let mut decompressed = Vec::new();
                        gz.read_to_end(&mut decompressed)?;
                        let text = String::from_utf8_lossy(&decompressed);
                        let index = parse_packages_gz(&text)?;
                        // Write cache + meta after successful parse
                        if let Some(parent) = cache_path.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        let _ = std::fs::write(&cache_path, &decompressed);
                        write_cache_meta(cache_key, new_etag.as_deref(), new_lm.as_deref());
                        debug!("{} index: {} packages (updated)", cache_key, index.len());
                        return Ok(CranRegistry {
                            index,
                            src_base: src_base.to_string(),
                            source,
                        });
                    }
                    Ok(_) | Err(_) => {
                        // Conditional request failed — fall back to cached data
                        debug!(
                            "{} index: conditional request failed, using cache",
                            cache_key
                        );
                        let raw = std::fs::read(&cache_path)?;
                        let text = String::from_utf8_lossy(&raw);
                        let index = parse_packages_gz(&text)?;
                        debug!(
                            "{} index: {} packages (cached, stale)",
                            cache_key,
                            index.len()
                        );
                        return Ok(CranRegistry {
                            index,
                            src_base: src_base.to_string(),
                            source,
                        });
                    }
                }
            }
            // No meta file but cache exists — use cache and do a full fetch to get headers
            debug!(
                "Loading {} index from cache (no meta): {}",
                cache_key,
                cache_path.display()
            );
            let raw = std::fs::read(&cache_path)?;
            let text = String::from_utf8_lossy(&raw);
            let index = parse_packages_gz(&text)?;
            debug!("{} index: {} packages (cached)", cache_key, index.len());
            return Ok(CranRegistry {
                index,
                src_base: src_base.to_string(),
                source,
            });
        }

        // No cache or force refresh — full download
        debug!("Downloading {} PACKAGES.gz...", cache_key);
        let resp = client.get(packages_url).send().await?;
        if !resp.status().is_success() {
            return Err(UvrError::Other(format!(
                "Failed to fetch package index from {packages_url} (HTTP {})",
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
        let mut decompressed = Vec::new();
        gz.read_to_end(&mut decompressed)?;

        let text = String::from_utf8_lossy(&decompressed);
        let index = parse_packages_gz(&text)?;

        // Write cache + meta after successful parse
        if let Some(parent) = cache_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&cache_path, &decompressed);
        write_cache_meta(cache_key, new_etag.as_deref(), new_lm.as_deref());

        debug!("{} index: {} packages", cache_key, index.len());
        Ok(CranRegistry {
            index,
            src_base: src_base.to_string(),
            source,
        })
    }

    /// Build a tarball URL for a package entry using this registry's base URL.
    fn tarball_url(&self, entry: &CranPackageEntry) -> String {
        format!(
            "{}/{}_{}.tar.gz",
            self.src_base, entry.name, entry.raw_version
        )
    }
}

impl PackageRegistry for CranRegistry {
    fn resolve_package(&self, name: &str, constraint: Option<&str>) -> Result<PackageInfo> {
        let entry = self.index.get_best(name, constraint)?;
        Ok(PackageInfo {
            name: entry.name.clone(),
            version: entry.version.clone(),
            source: self.source.clone(),
            checksum: if entry.md5sum.is_empty() {
                None
            } else {
                Some(format!("md5:{}", entry.md5sum))
            },
            requires: entry.requires_as_deps(),
            url: self.tarball_url(entry),
            raw_version: Some(entry.raw_version.clone()),
            system_requirements: entry.system_requirements.clone(),
        })
    }
}

fn cache_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".uvr")
        .join("cache")
}

pub(crate) fn cache_path_for(key: &str) -> PathBuf {
    cache_dir().join(format!("{key}-packages.txt"))
}

fn cache_meta_path_for(key: &str) -> PathBuf {
    cache_dir().join(format!("{key}-packages.meta"))
}

/// Read cached ETag and Last-Modified from the sidecar `.meta` file.
pub(crate) fn read_cache_meta(key: &str) -> Option<(Option<String>, Option<String>)> {
    let meta_path = cache_meta_path_for(key);
    let content = std::fs::read_to_string(&meta_path).ok()?;
    let mut etag = None;
    let mut last_modified = None;
    for line in content.lines() {
        if let Some(v) = line.strip_prefix("etag: ") {
            etag = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("last-modified: ") {
            last_modified = Some(v.to_string());
        }
    }
    Some((etag, last_modified))
}

/// Write ETag and Last-Modified to the sidecar `.meta` file.
pub(crate) fn write_cache_meta(key: &str, etag: Option<&str>, last_modified: Option<&str>) {
    let meta_path = cache_meta_path_for(key);
    let mut content = String::new();
    if let Some(e) = etag {
        content.push_str(&format!("etag: {e}\n"));
    }
    if let Some(lm) = last_modified {
        content.push_str(&format!("last-modified: {lm}\n"));
    }
    let _ = std::fs::write(&meta_path, content);
}

/// Parse DCF-format PACKAGES text into a `CranIndex`.
pub fn parse_packages_gz(text: &str) -> Result<CranIndex> {
    let mut index = CranIndex::default();
    for block in text.split("\n\n") {
        let block = block.trim();
        if block.is_empty() {
            continue;
        }
        if let Some(entry) = parse_dcf_block(block) {
            index.insert(entry);
        }
    }
    Ok(index)
}

/// Parse a single DCF record. `pub(crate)` so Bioconductor can reuse it.
pub(crate) fn parse_dcf_block(block: &str) -> Option<CranPackageEntry> {
    let fields = crate::dcf::parse_dcf_fields(block);

    let name = fields.get("Package")?.clone();
    let raw_version = fields.get("Version")?.clone();
    let version_str = normalize_version(&raw_version);
    let version = Version::parse(&version_str).ok()?;

    let depends = fields
        .get("Depends")
        .map(|s| parse_dep_field(s))
        .unwrap_or_default();
    let imports = fields
        .get("Imports")
        .map(|s| parse_dep_field(s))
        .unwrap_or_default();
    // LinkingTo packages provide headers needed at compile time (e.g. cpp11, Rcpp).
    // They must be installed before the dependent package, so treat them as deps.
    let linking_to = fields
        .get("LinkingTo")
        .map(|s| parse_dep_field(s))
        .unwrap_or_default();
    let md5sum = fields.get("MD5sum").cloned().unwrap_or_default();
    let system_requirements = fields.get("SystemRequirements").cloned();

    Some(CranPackageEntry {
        name,
        version,
        raw_version,
        depends,
        imports,
        linking_to,
        md5sum,
        system_requirements,
    })
}

/// Parse `"dplyr (>= 1.0.0), rlang, R (>= 4.1.0)"` → `Vec<DepConstraint>`.
pub fn parse_dep_field(s: &str) -> Vec<DepConstraint> {
    s.split(',')
        .filter_map(|part| {
            let part = part.trim();
            if part.is_empty() {
                return None;
            }
            if let Some(paren_pos) = part.find('(') {
                let name = part[..paren_pos].trim().to_string();
                let constraint_str = part[paren_pos + 1..].trim_end_matches(')').trim();
                let req = parse_version_req(constraint_str).ok();
                Some(DepConstraint { name, req })
            } else {
                Some(DepConstraint {
                    name: part.to_string(),
                    req: None,
                })
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_PACKAGES: &str = r#"Package: ggplot2
Version: 3.4.4
Depends: R (>= 3.3.0)
Imports: cli, dplyr, glue, gtable (>= 0.1.1), isoband, MASS, mgcv, rlang (>= 1.0.0), scales (>= 1.2.0), stats, tibble, withr
MD5sum: abc123def456

Package: dplyr
Version: 1.1.4
Imports: cli (>= 3.4.0), generics, glue (>= 1.3.2), lifecycle (>= 1.0.3), magrittr (>= 1.5.0), methods, pillar (>= 1.9.0), R6, rlang (>= 1.1.0), tibble (>= 3.2.0), tidyselect (>= 1.2.0), utils, vctrs (>= 0.6.4)
MD5sum: 123abc

Package: scales
Version: 1.1-3
MD5sum: def789

"#;

    #[test]
    fn parse_packages() {
        let index = parse_packages_gz(SAMPLE_PACKAGES).unwrap();
        assert_eq!(index.len(), 3);

        let gg = index.get_best("ggplot2", None).unwrap();
        assert_eq!(gg.version, Version::parse("3.4.4").unwrap());
        assert_eq!(gg.raw_version, "3.4.4");
        assert!(!gg.md5sum.is_empty());
        assert!(gg.all_requires().contains(&"dplyr".to_string()));
        assert!(!gg.all_requires().contains(&"R".to_string()));

        // Constraints are preserved
        let deps = gg.requires_as_deps();
        let rlang_dep = deps.iter().find(|d| d.name == "rlang").unwrap();
        assert_eq!(rlang_dep.constraint.as_deref(), Some(">=1.0.0"));
    }

    #[test]
    fn raw_version_preserved_for_url() {
        let index = parse_packages_gz(SAMPLE_PACKAGES).unwrap();
        let scales = index.get_best("scales", None).unwrap();
        // Semver-normalized version
        assert_eq!(scales.version, Version::parse("1.1.3").unwrap());
        // Raw version preserved
        assert_eq!(scales.raw_version, "1.1-3");
        // URL uses raw_version, not semver
        assert!(scales.tarball_url().contains("scales_1.1-3.tar.gz"));
    }

    #[test]
    fn parse_dep_field_test() {
        let deps = parse_dep_field("dplyr (>= 1.0.0), rlang, R (>= 4.1.0)");
        assert_eq!(deps.len(), 3);
        assert_eq!(deps[0].name, "dplyr");
        assert!(deps[0].req.is_some());
        assert_eq!(deps[1].name, "rlang");
        assert!(deps[1].req.is_none());
    }

    #[test]
    fn constraint_version_req_roundtrip() {
        // VersionReq::to_string() produces parseable output
        let req = parse_version_req(">=1.0.0").unwrap();
        let s = req.to_string();
        let req2 = parse_version_req(&s).unwrap();
        assert!(req2.matches(&Version::parse("1.1.0").unwrap()));
    }

    #[test]
    fn system_requirements_parsed() {
        let dcf =
            "Package: xml2\nVersion: 1.3.6\nSystemRequirements: libxml2 (>= 2.9.0)\nMD5sum: abc\n";
        let entry = parse_dcf_block(dcf).unwrap();
        assert_eq!(
            entry.system_requirements.as_deref(),
            Some("libxml2 (>= 2.9.0)")
        );

        // Packages without SystemRequirements get None
        let dcf2 = "Package: dplyr\nVersion: 1.1.4\nMD5sum: def\n";
        let entry2 = parse_dcf_block(dcf2).unwrap();
        assert!(entry2.system_requirements.is_none());
    }
}
