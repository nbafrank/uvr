use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::Read;
use std::path::PathBuf;
use std::sync::Mutex;

use flate2::read::GzDecoder;
use semver::{Version, VersionReq};
use tracing::{debug, warn};

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
    /// `Path:` field — relative location of the tarball within the
    /// repository, when not at the default `<base>/<name>_<version>.tar.gz`.
    pub path: Option<String>,
    /// `Built:` field, parsed. Present iff this entry is a binary build.
    pub built: Option<BuiltInfo>,
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
        let safe_path = self.path.as_deref().filter(|p| {
            !p.is_empty() && !p.starts_with('/') && !p.split('/').any(|seg| seg == "..")
        });
        let dir = match safe_path {
            Some(p) => format!("{}/{}", base, p),
            None => base.to_string(),
        };
        format!("{}/{}_{}.tar.gz", dir, self.name, self.raw_version)
    }
}

#[derive(Debug, Clone)]
pub struct DepConstraint {
    pub name: String,
    pub req: Option<VersionReq>,
}

/// Parsed `Built:` field from a CRAN PACKAGES entry. Present on binary
/// builds; absent on source-only entries.
///
/// Canonical format (semicolon-separated, four fields):
///
/// ```text
/// R 4.5.0; x86_64-pc-linux-musl; 2025-01-15 12:00:00 UTC; unix
/// ```
///
/// Extra fields beyond the fourth are ignored. The build date is stored
/// raw and not validated — kept only for diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltInfo {
    pub r_version: String,
    pub platform: String,
    pub date: String,
    pub os_family: String,
}

impl BuiltInfo {
    /// Returns true iff this binary build matches the given host:
    /// - R `major.minor` equals `r_minor` (patch ignored)
    /// - Platform triple matches `host` vendor-relaxed: arch, OS, and ABI must
    ///   match exactly, but the vendor segment is ignored (Alpine reports
    ///   `unknown`; Ubuntu/Debian report `pc`; uvr always emits `pc`)
    /// - OS family matches host's OS (linux/macos = "unix"; windows = "windows")
    pub fn matches_host(
        &self,
        host: &crate::r_version::downloader::HostTriple,
        r_minor: &str,
    ) -> bool {
        // R version major.minor match (ignore patch).
        let built_minor: String = self
            .r_version
            .split('.')
            .take(2)
            .collect::<Vec<_>>()
            .join(".");
        if built_minor != r_minor {
            return false;
        }

        // Triple match, vendor-relaxed: R reports `unknown` on Alpine
        // but `pc` on Ubuntu/Debian, while uvr's host_triple unconditionally
        // emits `pc` on Linux. Normalize both sides by dropping the
        // vendor segment (the 2nd of 4) before comparing.
        let host_normalized = format!("{}-{}-{}", host.arch, host.os, host.abi);
        if normalize_triple_drop_vendor(&self.platform) != host_normalized {
            return false;
        }

        // OS family.
        let expected_os_family = match host.os.as_str() {
            "windows" => "windows",
            _ => "unix",
        };
        if self.os_family != expected_os_family {
            return false;
        }

        true
    }
}

/// Drop the vendor segment (2nd of typically 4) from a platform triple so
/// vendor variants (Alpine's `unknown` vs Ubuntu's `pc` vs macOS's `apple`)
/// don't cause false mismatches.
///
/// Examples:
/// - `aarch64-unknown-linux-musl` → `aarch64-linux-musl`
/// - `aarch64-pc-linux-musl`      → `aarch64-linux-musl`
/// - `x86_64-w64-mingw32`         → `x86_64-mingw32`
/// - `aarch64-apple-darwin20`     → `aarch64-darwin20`
///
/// If the input doesn't look like a 3+ segment triple, returns it
/// unchanged.
pub(crate) fn normalize_triple_drop_vendor(triple: &str) -> String {
    // Split on the first dash to capture arch, then on the next dash to
    // drop the vendor segment, then rejoin with the remainder.
    let Some((arch, rest)) = triple.split_once('-') else {
        return triple.to_string();
    };
    let Some((_vendor, tail)) = rest.split_once('-') else {
        return triple.to_string();
    };
    format!("{arch}-{tail}")
}

/// Parse a `Built:` field. Lenient: returns `None` on anything that doesn't
/// look like a binary-build marker (missing `R` prefix, fewer than four
/// fields, etc.). Treats unparseable as "not a binary".
pub fn parse_built(s: &str) -> Option<BuiltInfo> {
    let parts: Vec<&str> = s.split(';').map(|p| p.trim()).collect();
    if parts.len() < 4 {
        return None;
    }
    let r_field = parts[0];
    let r_version = r_field.strip_prefix("R ")?.trim().to_string();
    if r_version.is_empty() {
        return None;
    }
    Some(BuiltInfo {
        r_version,
        platform: parts[1].to_string(),
        date: parts[2].to_string(),
        os_family: parts[3].to_string(),
    })
}

/// In-memory CRAN index.
#[derive(Debug, Default)]
pub struct CranIndex {
    /// name → versions sorted newest-first
    packages: HashMap<String, Vec<CranPackageEntry>>,
}

impl CranIndex {
    /// The newest version satisfying `constraint`.
    pub fn get_best(&self, name: &str, constraint: Option<&str>) -> Result<&CranPackageEntry> {
        self.select(name, constraint, false)
    }

    /// The oldest version satisfying `constraint` (#193).
    pub fn get_lowest(&self, name: &str, constraint: Option<&str>) -> Result<&CranPackageEntry> {
        self.select(name, constraint, true)
    }

    fn select(
        &self,
        name: &str,
        constraint: Option<&str>,
        lowest: bool,
    ) -> Result<&CranPackageEntry> {
        let entries = self
            .packages
            .get(name)
            .ok_or_else(|| UvrError::PackageNotFound(name.to_string()))?;

        let req = match constraint {
            Some(c) if c != "*" && !c.is_empty() => Some(parse_version_req(c)?),
            _ => None,
        };
        let satisfies = |e: &&CranPackageEntry| {
            req.as_ref()
                .is_none_or(|r| crate::resolver::version_matches_req(&e.version, r))
        };

        // Entries are sorted newest-first.
        let found = if lowest {
            // Old releases can need packages that have since left CRAN
            // (testthat 0.1 needs mutatr). Skip those releases, as a
            // backtracking resolver would; if every candidate has such a
            // dependency (a Bioconductor import, say), keep the oldest.
            let mut candidates = entries.iter().rev().filter(satisfies);
            let oldest = candidates.clone().next();
            candidates
                .find(|e| {
                    e.requires_as_deps()
                        .iter()
                        .all(|d| self.packages.contains_key(&d.name))
                })
                .or(oldest)
        } else {
            entries.iter().find(satisfies)
        };
        found.ok_or_else(|| UvrError::NoMatchingVersion {
            package: name.to_string(),
            constraint: constraint.unwrap_or("*").to_string(),
        })
    }

    /// The newest entry for `name`: the current release in a CRAN index.
    fn newest(&self, name: &str) -> Option<&CranPackageEntry> {
        self.packages.get(name).and_then(|v| v.first())
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

    /// Iterator over all entries (all versions of all packages).
    pub fn all_entries(&self) -> impl Iterator<Item = &CranPackageEntry> {
        self.packages.values().flat_map(|v| v.iter())
    }

    /// Find the entry for `(name, exact version)`. Compares the normalized
    /// version (e.g. `1.1-3` → `1.1.3`).
    pub fn find_exact(&self, name: &str, version: &str) -> Option<&CranPackageEntry> {
        let want = normalize_version(version);
        let parsed = Version::parse(&want).ok()?;
        self.packages
            .get(name)?
            .iter()
            .find(|e| e.version == parsed)
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
    /// Packages whose older CRAN releases are merged into `index` (#193).
    history_loaded: HashSet<String>,
    /// Packages a lowest-version lookup asked for before their history was
    /// loaded. The caller loads them and resolves again.
    history_wanted: Mutex<BTreeSet<String>>,
}

impl CranRegistry {
    fn new(index: CranIndex, src_base: &str, source: PackageSource) -> Self {
        CranRegistry {
            index,
            src_base: src_base.to_string(),
            source,
            history_loaded: HashSet::new(),
            history_wanted: Mutex::new(BTreeSet::new()),
        }
    }

    /// Fetch (or load from cache) the CRAN index.
    pub async fn fetch(client: &reqwest::Client, force_refresh: bool) -> Result<Self> {
        Self::fetch_from(
            client,
            "cran",
            CRAN_PACKAGES_URL,
            CRAN_SRC_BASE,
            PackageSource::Cran,
            force_refresh,
            None,
            None,
        )
        .await
    }

    /// Fetch a CRAN-like index from any repository that serves PACKAGES.gz.
    /// Used for custom repositories like r-multiverse, r-universe, etc.
    ///
    /// `user_agent` is forwarded on every HTTP request so hosts like
    /// cran.rpkgs.com can route to the correct binary flavour (musl vs gnu)
    /// based on the R-shaped UA. Pass `None` to use the default client UA.
    ///
    /// The repository's credential, if the environment has one, goes on the
    /// index request (see [`crate::auth::resolve`]).
    pub async fn fetch_custom(
        client: &reqwest::Client,
        repo_name: &str,
        base_url: &str,
        force_refresh: bool,
        user_agent: Option<&str>,
    ) -> Result<Self> {
        let base_url = base_url.trim_end_matches('/');
        let packages_url = format!("{base_url}/src/contrib/PACKAGES.gz");
        let src_base = format!("{base_url}/src/contrib");
        // Use repo_name + URL hash as cache key to avoid collisions
        // between repos on the same hostname with different paths.
        use md5::{Digest, Md5};
        let hash = hex::encode(Md5::digest(base_url.as_bytes()));
        let cache_key = format!("{repo_name}-{}", &hash[..8]);
        let repo = crate::auth::Repository::new(repo_name, base_url);
        Self::fetch_from(
            client,
            &cache_key,
            &packages_url,
            &src_base,
            PackageSource::Custom {
                name: repo_name.to_string(),
            },
            force_refresh,
            user_agent,
            Some(&repo),
        )
        .await
    }

    /// Returns true iff at least one entry in this registry has a `Built:`
    /// line matching the host. Used at sync time to decide whether this
    /// custom source contributes binaries.
    /// Display identity: the source's name and its tarball base URL, for
    /// telling the user *which* source served their packages (#205).
    pub fn name_and_base(&self) -> (&str, &str) {
        let name = match &self.source {
            PackageSource::Custom { name } => name.as_str(),
            PackageSource::Cran => "CRAN",
            PackageSource::Bioconductor => "Bioconductor",
            _ => "source",
        };
        (name, &self.src_base)
    }

    pub fn is_binary_capable(
        &self,
        host: &crate::r_version::downloader::HostTriple,
        r_minor: &str,
    ) -> bool {
        self.index.all_entries().any(|e| {
            e.built
                .as_ref()
                .is_some_and(|b| b.matches_host(host, r_minor))
        })
    }

    /// Returns the binary tarball URL for `(name, version)` only if the
    /// matching entry exists, its version matches, and its `Built:` line
    /// matches the host. Path traversal hardening is applied.
    pub fn binary_url_for(
        &self,
        name: &str,
        version: &str,
        host: &crate::r_version::downloader::HostTriple,
        r_minor: &str,
    ) -> Option<String> {
        let entry = self.index.find_exact(name, version)?;
        let built = entry.built.as_ref()?;
        if !built.matches_host(host, r_minor) {
            return None;
        }
        Some(entry.tarball_url_with_base(&self.src_base))
    }

    /// Test-only constructor.
    #[doc(hidden)]
    pub fn for_test(index: CranIndex, src_base: String) -> Self {
        Self::new(
            index,
            &src_base,
            PackageSource::Custom {
                name: "test".to_string(),
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    async fn fetch_from(
        client: &reqwest::Client,
        cache_key: &str,
        packages_url: &str,
        src_base: &str,
        source: PackageSource,
        force_refresh: bool,
        user_agent: Option<&str>,
        repo: Option<&crate::auth::Repository>,
    ) -> Result<Self> {
        let cache_path = cache_path_for(cache_key);
        let has_cache = cache_path.exists();
        let get = |url: &str| {
            let mut req = client.get(url);
            if let Some(ua) = user_agent {
                req = req.header(reqwest::header::USER_AGENT, ua);
            }
            match repo.and_then(|r| r.credential.as_ref()) {
                Some(credential) => credential.apply(req),
                None => req,
            }
        };
        let denied = |status| repo.and_then(|r| r.denied_error(status));

        // Try HTTP conditional request if we have a cached index and aren't forcing refresh.
        if !force_refresh && has_cache {
            if let Some((etag, last_modified)) = read_cache_meta(cache_key) {
                let mut req = get(packages_url);
                if let Some(ref e) = etag {
                    req = req.header("If-None-Match", e.as_str());
                }
                if let Some(ref lm) = last_modified {
                    req = req.header("If-Modified-Since", lm.as_str());
                }
                let resp = req.send().await;
                // A refused credential must not hide behind the stale cache.
                if let Some(e) = resp.as_ref().ok().and_then(|r| denied(r.status())) {
                    return Err(e);
                }
                match resp {
                    Ok(resp) if resp.status() == reqwest::StatusCode::NOT_MODIFIED => {
                        debug!("{} index: HTTP 304 Not Modified, using cache", cache_key);
                        let raw = std::fs::read(&cache_path)?;
                        let text = String::from_utf8_lossy(&raw);
                        let index = parse_packages_gz(&text)?;
                        debug!("{} index: {} packages (cached)", cache_key, index.len());
                        return Ok(Self::new(index, src_base, source));
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
                        // Write cache + meta after successful parse. Only
                        // record the new ETag/Last-Modified once the data
                        // write succeeds — otherwise a later conditional GET
                        // could 304 against stale/absent cache content.
                        if let Some(parent) = cache_path.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        if let Err(e) = std::fs::write(&cache_path, &decompressed) {
                            warn!(
                                "{cache_key} index: failed to write cache data to {}: {e}; not updating cache meta",
                                cache_path.display()
                            );
                        } else {
                            write_cache_meta(cache_key, new_etag.as_deref(), new_lm.as_deref());
                        }
                        debug!("{} index: {} packages (updated)", cache_key, index.len());
                        return Ok(Self::new(index, src_base, source));
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
                        return Ok(Self::new(index, src_base, source));
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
            return Ok(Self::new(index, src_base, source));
        }

        // No cache or force refresh — full download
        debug!("Downloading {} PACKAGES.gz...", cache_key);
        let resp = get(packages_url).send().await?;
        if !resp.status().is_success() {
            if let Some(e) = denied(resp.status()) {
                return Err(e);
            }
            return Err(UvrError::Other(format!(
                "Failed to fetch package index from {} (HTTP {})",
                crate::auth::redact_url(packages_url),
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
        // Only record the new ETag/Last-Modified once the data write succeeds.
        // If the data write fails but the meta is written anyway, a later
        // conditional GET could 304 against stale/absent cache content.
        if let Err(e) = std::fs::write(&cache_path, &decompressed) {
            warn!(
                "{cache_key} index: failed to write cache data to {}: {e}; not updating cache meta",
                cache_path.display()
            );
        } else {
            write_cache_meta(cache_key, new_etag.as_deref(), new_lm.as_deref());
        }

        debug!("{} index: {} packages", cache_key, index.len());
        Ok(Self::new(index, src_base, source))
    }

    /// Build a tarball URL for a package entry using this registry's base URL.
    /// CRAN keeps every release but the current one under `Archive/<name>/`.
    fn tarball_url(&self, entry: &CranPackageEntry) -> String {
        let archived = self.source == PackageSource::Cran
            && self
                .index
                .newest(&entry.name)
                .is_some_and(|current| current.version != entry.version);
        if archived {
            format!(
                "{}/Archive/{}/{}_{}.tar.gz",
                self.src_base, entry.name, entry.name, entry.raw_version
            )
        } else {
            format!(
                "{}/{}_{}.tar.gz",
                self.src_base, entry.name, entry.raw_version
            )
        }
    }

    fn package_info(&self, entry: &CranPackageEntry) -> PackageInfo {
        PackageInfo {
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
            subdirectory: None,
        }
    }

    /// Take the packages that lowest-version lookups asked for before their
    /// release history was loaded. Empty once every package the resolver
    /// looked at has its history.
    pub fn take_wanted_history(&self) -> BTreeSet<String> {
        std::mem::take(
            &mut *self
                .history_wanted
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        )
    }

    /// Fetch the older CRAN releases of `names` and merge them into the index,
    /// so lowest-version lookups can pick them (#193).
    pub async fn load_history(
        &mut self,
        client: &reqwest::Client,
        names: BTreeSet<String>,
    ) -> Result<()> {
        let jobs: Vec<(String, Version)> = names
            .into_iter()
            .filter_map(|name| {
                let current = self.index.newest(&name)?.version.clone();
                Some((name, current))
            })
            .collect();
        let cache_dir = super::cran_history::cache_dir();
        // ponytail: fixed batches of 8 bound the concurrent requests to crandb.
        for batch in jobs.chunks(8) {
            let mut set = tokio::task::JoinSet::new();
            for (name, current) in batch.iter().cloned() {
                let client = client.clone();
                let cache_dir = cache_dir.clone();
                set.spawn(async move {
                    let fetched = super::cran_history::fetch(
                        &client,
                        super::cran_history::CRANDB_URL,
                        &cache_dir,
                        &name,
                        &current,
                    )
                    .await;
                    (name, fetched)
                });
            }
            while let Some(joined) = set.join_next().await {
                let (name, fetched) = joined.map_err(|e| UvrError::Other(e.to_string()))?;
                self.merge_history(&name, fetched?);
            }
        }
        Ok(())
    }

    /// Merge the releases of `name` that are older than its current index
    /// entry. Newer ones are skipped: the index is authoritative for what is
    /// current, and a newer crandb release would get a wrong Archive URL.
    fn merge_history(&mut self, name: &str, entries: Vec<CranPackageEntry>) {
        if let Some(current) = self.index.newest(name).map(|e| e.version.clone()) {
            for entry in entries {
                if entry.name == name && entry.version < current {
                    self.index.insert(entry);
                }
            }
        }
        self.history_loaded.insert(name.to_string());
    }

    /// The PACKAGES index lists current releases only. Record the gap; the
    /// caller loads the history and resolves again.
    fn want_history(&self, name: &str) {
        if self.source == PackageSource::Cran
            && self.index.packages.contains_key(name)
            && !self.history_loaded.contains(name)
        {
            self.history_wanted
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(name.to_string());
        }
    }
}

impl PackageRegistry for CranRegistry {
    fn resolve_package(&self, name: &str, constraint: Option<&str>) -> Result<PackageInfo> {
        let entry = self.index.get_best(name, constraint);
        // A bound that excludes the current release (`< 1.1`, or `== 1.0.0`
        // from an override, #195) may fit an older one. A lower bound alone
        // cannot, so it fails without a history request.
        if matches!(entry, Err(UvrError::NoMatchingVersion { .. }))
            && constraint
                .and_then(|c| parse_version_req(c).ok())
                .is_some_and(|req| {
                    req.comparators
                        .iter()
                        .any(|c| !matches!(c.op, semver::Op::Greater | semver::Op::GreaterEq))
                })
        {
            self.want_history(name);
        }
        Ok(self.package_info(entry?))
    }

    fn resolve_package_lowest(&self, name: &str, constraint: Option<&str>) -> Result<PackageInfo> {
        // Answer from what is loaded until the history is in.
        self.want_history(name);
        let entry = self.index.get_lowest(name, constraint)?;
        Ok(self.package_info(entry))
    }
}

fn cache_dir() -> PathBuf {
    crate::env_vars::cache_dir_or_temp()
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
    let version = match Version::parse(&version_str) {
        Ok(v) => v,
        Err(e) => {
            warn!(
                "package '{name}': unparseable version '{raw_version}' (normalized '{version_str}': {e}); dropping from index"
            );
            return None;
        }
    };

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
    let path = fields.get("Path").cloned().filter(|p| !p.is_empty());
    let built = fields.get("Built").and_then(|s| parse_built(s));

    Some(CranPackageEntry {
        name,
        version,
        raw_version,
        depends,
        imports,
        linking_to,
        md5sum,
        system_requirements,
        path,
        built,
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
                let req = match parse_version_req(constraint_str) {
                    Ok(r) => Some(r),
                    Err(e) => {
                        warn!(
                            "dependency '{name}': unparseable version constraint '{constraint_str}' ({e}); treating as unconstrained"
                        );
                        None
                    }
                };
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
    fn parse_dep_field_malformed_constraint_falls_back_to_unconstrained() {
        // Issue #149: a typo'd constraint must not be silently dropped as a
        // constrained dep — it falls back to unconstrained (a warn is logged).
        let deps = parse_dep_field("rlang (=> 1.0.0), dplyr (>= 1.0.0)");
        assert_eq!(deps.len(), 2);
        assert_eq!(deps[0].name, "rlang");
        assert!(
            deps[0].req.is_none(),
            "malformed constraint should fall back to unconstrained"
        );
        assert_eq!(deps[1].name, "dplyr");
        assert!(deps[1].req.is_some(), "valid constraint still parsed");
    }

    #[test]
    fn parse_dcf_block_unparseable_version_dropped() {
        // Issue #150: an entry whose version doesn't parse as semver (even after
        // normalization) is dropped from the index (a warn is logged on drop).
        let block = "Package: weird\nVersion: not-a-version\nMD5sum: abc\n";
        assert!(
            parse_dcf_block(block).is_none(),
            "unparseable version should drop the entry"
        );

        // A valid version on the same shape still parses.
        let ok = "Package: fine\nVersion: 1.2.3\nMD5sum: abc\n";
        assert!(parse_dcf_block(ok).is_some());
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

    #[test]
    fn parse_built_canonical() {
        let b =
            parse_built("R 4.5.0; x86_64-pc-linux-musl; 2025-01-15 12:00:00 UTC; unix").unwrap();
        assert_eq!(b.r_version, "4.5.0");
        assert_eq!(b.platform, "x86_64-pc-linux-musl");
        assert_eq!(b.date, "2025-01-15 12:00:00 UTC");
        assert_eq!(b.os_family, "unix");
    }

    #[test]
    fn parse_built_extra_fields_ignored() {
        let b = parse_built("R 4.5.0; x86_64-pc-linux-gnu; 2025-01-15; unix; extra; junk").unwrap();
        assert_eq!(b.os_family, "unix");
    }

    #[test]
    fn parse_built_too_few_fields_returns_none() {
        assert!(parse_built("R 4.5.0; x86_64-pc-linux-musl").is_none());
        assert!(parse_built("R 4.5.0; x86_64; 2025-01-15").is_none());
    }

    #[test]
    fn parse_built_no_r_prefix_returns_none() {
        assert!(parse_built("4.5.0; x86_64-pc-linux-musl; 2025-01-15; unix").is_none());
    }

    #[test]
    fn parse_built_unparseable_returns_none() {
        assert!(parse_built("garbage").is_none());
        assert!(parse_built("").is_none());
    }

    #[test]
    fn parse_built_with_whitespace_around_separators() {
        let b = parse_built("R 4.5.0 ; x86_64-pc-linux-musl ; 2025-01-15 ; unix").unwrap();
        assert_eq!(b.r_version, "4.5.0");
        assert_eq!(b.platform, "x86_64-pc-linux-musl");
    }

    #[test]
    fn parse_dcf_extracts_path_and_built() {
        let block = "Package: rlang
Version: 1.1.6
Path: linux/musl-3.23
Built: R 4.5.0; x86_64-pc-linux-musl; 2025-01-15 12:00:00 UTC; unix
MD5sum: abc123";
        let entry = parse_dcf_block(block).unwrap();
        assert_eq!(entry.name, "rlang");
        assert_eq!(entry.path.as_deref(), Some("linux/musl-3.23"));
        let b = entry.built.as_ref().unwrap();
        assert_eq!(b.platform, "x86_64-pc-linux-musl");
    }

    #[test]
    fn parse_dcf_no_path_no_built_regression() {
        let block = "Package: jsonlite
Version: 1.8.8
MD5sum: deadbeef";
        let entry = parse_dcf_block(block).unwrap();
        assert!(entry.path.is_none());
        assert!(entry.built.is_none());
    }

    #[test]
    fn parse_dcf_built_unparseable_yields_none() {
        let block = "Package: foo
Version: 1.0
Built: garbage-not-actually-built-format";
        let entry = parse_dcf_block(block).unwrap();
        assert!(entry.built.is_none(), "garbage Built: should be ignored");
    }

    fn entry_with_path(path: Option<&str>) -> CranPackageEntry {
        CranPackageEntry {
            name: "rlang".into(),
            version: Version::parse("1.1.6").unwrap(),
            raw_version: "1.1.6".into(),
            depends: vec![],
            imports: vec![],
            linking_to: vec![],
            md5sum: String::new(),
            system_requirements: None,
            path: path.map(|p| p.to_string()),
            built: None,
        }
    }

    #[test]
    fn tarball_url_no_path_unchanged() {
        let entry = entry_with_path(None);
        assert_eq!(
            entry.tarball_url_with_base("https://example.com/src/contrib"),
            "https://example.com/src/contrib/rlang_1.1.6.tar.gz"
        );
    }

    #[test]
    fn tarball_url_with_path_appends_subdir() {
        let entry = entry_with_path(Some("linux/musl-3.23"));
        assert_eq!(
            entry.tarball_url_with_base("https://example.com/src/contrib"),
            "https://example.com/src/contrib/linux/musl-3.23/rlang_1.1.6.tar.gz"
        );
    }

    #[test]
    fn tarball_url_rejects_path_traversal() {
        let entry = entry_with_path(Some("../../../etc"));
        // Falls back to default URL (path is dropped) — no traversal.
        assert_eq!(
            entry.tarball_url_with_base("https://example.com/src/contrib"),
            "https://example.com/src/contrib/rlang_1.1.6.tar.gz"
        );
    }

    #[test]
    fn tarball_url_rejects_absolute_path() {
        let entry = entry_with_path(Some("/usr/bin"));
        assert_eq!(
            entry.tarball_url_with_base("https://example.com/src/contrib"),
            "https://example.com/src/contrib/rlang_1.1.6.tar.gz"
        );
    }

    #[test]
    fn tarball_url_empty_path_treated_as_none() {
        let entry = entry_with_path(Some(""));
        assert_eq!(
            entry.tarball_url_with_base("https://example.com/src/contrib"),
            "https://example.com/src/contrib/rlang_1.1.6.tar.gz"
        );
    }

    fn registry_from_packages(text: &str, src_base: &str) -> CranRegistry {
        let index = parse_packages_gz(text).unwrap();
        CranRegistry::for_test(index, src_base.to_string())
    }

    fn musl_alpine_host() -> crate::r_version::downloader::HostTriple {
        crate::r_version::downloader::HostTriple {
            arch: "x86_64".into(),
            vendor: "pc".into(),
            os: "linux".into(),
            abi: "musl".into(),
        }
    }

    fn gnu_ubuntu_host() -> crate::r_version::downloader::HostTriple {
        crate::r_version::downloader::HostTriple {
            arch: "x86_64".into(),
            vendor: "pc".into(),
            os: "linux".into(),
            abi: "gnu".into(),
        }
    }

    #[test]
    fn built_matches_host_exact_alpine() {
        let b = parse_built("R 4.5.0; x86_64-pc-linux-musl; 2025-01-15; unix").unwrap();
        assert!(b.matches_host(&musl_alpine_host(), "4.5"));
    }

    #[test]
    fn built_matches_host_r_minor_patch_ignored() {
        let b = parse_built("R 4.5.0; x86_64-pc-linux-musl; 2025-01-15; unix").unwrap();
        assert!(b.matches_host(&musl_alpine_host(), "4.5"));
        let b2 = parse_built("R 4.5.3; x86_64-pc-linux-musl; 2025-01-15; unix").unwrap();
        assert!(b2.matches_host(&musl_alpine_host(), "4.5"));
    }

    #[test]
    fn built_mismatches_r_minor() {
        let b = parse_built("R 4.4.0; x86_64-pc-linux-musl; 2025-01-15; unix").unwrap();
        assert!(!b.matches_host(&musl_alpine_host(), "4.5"));
    }

    #[test]
    fn built_mismatches_libc() {
        let b = parse_built("R 4.5.0; x86_64-pc-linux-gnu; 2025-01-15; unix").unwrap();
        assert!(!b.matches_host(&musl_alpine_host(), "4.5"));
    }

    #[test]
    fn built_mismatches_arch() {
        let b = parse_built("R 4.5.0; aarch64-pc-linux-musl; 2025-01-15; unix").unwrap();
        assert!(!b.matches_host(&musl_alpine_host(), "4.5"));
    }

    #[test]
    fn built_mismatches_os_family_windows_on_linux() {
        let b = parse_built("R 4.5.0; x86_64-w64-mingw32; 2025-01-15; windows").unwrap();
        assert!(!b.matches_host(&gnu_ubuntu_host(), "4.5"));
    }

    #[test]
    fn is_binary_capable_yes_with_musl_built() {
        let pkgs = "Package: rlang
Version: 1.1.6
Built: R 4.5.0; x86_64-pc-linux-musl; 2025-01-15; unix

";
        let reg = registry_from_packages(pkgs, "https://example.com/src/contrib");
        assert!(reg.is_binary_capable(&musl_alpine_host(), "4.5"));
    }

    #[test]
    fn is_binary_capable_no_for_source_only() {
        let pkgs = "Package: rlang
Version: 1.1.6

";
        let reg = registry_from_packages(pkgs, "https://example.com/src/contrib");
        assert!(!reg.is_binary_capable(&musl_alpine_host(), "4.5"));
    }

    #[test]
    fn is_binary_capable_no_for_wrong_libc() {
        let pkgs = "Package: rlang
Version: 1.1.6
Built: R 4.5.0; x86_64-pc-linux-gnu; 2025-01-15; unix

";
        let reg = registry_from_packages(pkgs, "https://example.com/src/contrib");
        assert!(!reg.is_binary_capable(&musl_alpine_host(), "4.5"));
    }

    #[test]
    fn binary_url_for_returns_url_when_match() {
        let pkgs = "Package: rlang
Version: 1.1.6
Built: R 4.5.0; x86_64-pc-linux-musl; 2025-01-15; unix

";
        let reg = registry_from_packages(pkgs, "https://example.com/src/contrib");
        assert_eq!(
            reg.binary_url_for("rlang", "1.1.6", &musl_alpine_host(), "4.5"),
            Some("https://example.com/src/contrib/rlang_1.1.6.tar.gz".to_string())
        );
    }

    #[test]
    fn binary_url_for_honors_path() {
        let pkgs = "Package: rlang
Version: 1.1.6
Path: x86_64/alpine-3.23
Built: R 4.5.0; x86_64-pc-linux-musl; 2025-01-15; unix

";
        let reg = registry_from_packages(pkgs, "https://example.com/src/contrib");
        assert_eq!(
            reg.binary_url_for("rlang", "1.1.6", &musl_alpine_host(), "4.5"),
            Some(
                "https://example.com/src/contrib/x86_64/alpine-3.23/rlang_1.1.6.tar.gz".to_string()
            )
        );
    }

    #[test]
    fn binary_url_for_returns_none_when_no_match() {
        let pkgs = "Package: rlang
Version: 1.1.6

";
        let reg = registry_from_packages(pkgs, "https://example.com/src/contrib");
        assert!(reg
            .binary_url_for("rlang", "1.1.6", &musl_alpine_host(), "4.5")
            .is_none());
    }

    #[test]
    fn binary_url_for_returns_none_for_unknown_package() {
        let pkgs = "Package: rlang
Version: 1.1.6
Built: R 4.5.0; x86_64-pc-linux-musl; 2025-01-15; unix

";
        let reg = registry_from_packages(pkgs, "https://example.com/src/contrib");
        assert!(reg
            .binary_url_for("nonexistent", "0.0.0", &musl_alpine_host(), "4.5")
            .is_none());
    }

    #[test]
    fn matches_host_ignores_vendor_alpine_r_reports_unknown() {
        // R built on Alpine reports `aarch64-unknown-linux-musl`,
        // but uvr's host_triple emits `aarch64-pc-linux-musl`.
        // These must match anyway — vendor is decorative.
        let b = parse_built("R 4.5.2; aarch64-unknown-linux-musl; 2025-01-15; unix").unwrap();
        let host = crate::r_version::downloader::HostTriple {
            arch: "aarch64".into(),
            vendor: "pc".into(),
            os: "linux".into(),
            abi: "musl".into(),
        };
        assert!(b.matches_host(&host, "4.5"));
    }

    #[test]
    fn matches_host_ignores_vendor_inverse_direction() {
        // Symmetry: Built: pc, host: unknown should also match.
        let b = parse_built("R 4.5.0; x86_64-pc-linux-gnu; 2025-01-15; unix").unwrap();
        let host = crate::r_version::downloader::HostTriple {
            arch: "x86_64".into(),
            vendor: "unknown".into(),
            os: "linux".into(),
            abi: "gnu".into(),
        };
        assert!(b.matches_host(&host, "4.5"));
    }

    #[test]
    fn matches_host_still_rejects_arch_mismatch() {
        // Vendor-lenient does NOT mean wildly lenient. Arch must still match.
        let b = parse_built("R 4.5.0; aarch64-pc-linux-musl; 2025-01-15; unix").unwrap();
        let host = crate::r_version::downloader::HostTriple {
            arch: "x86_64".into(),
            vendor: "pc".into(),
            os: "linux".into(),
            abi: "musl".into(),
        };
        assert!(!b.matches_host(&host, "4.5"));
    }

    #[test]
    fn matches_host_still_rejects_abi_mismatch_with_relaxed_vendor() {
        // gnu Built: vs musl host — still mismatch.
        let b = parse_built("R 4.5.0; x86_64-unknown-linux-gnu; 2025-01-15; unix").unwrap();
        let host = crate::r_version::downloader::HostTriple {
            arch: "x86_64".into(),
            vendor: "pc".into(),
            os: "linux".into(),
            abi: "musl".into(),
        };
        assert!(!b.matches_host(&host, "4.5"));
    }

    #[test]
    fn normalize_triple_drop_vendor_examples() {
        assert_eq!(
            normalize_triple_drop_vendor("aarch64-unknown-linux-musl"),
            "aarch64-linux-musl"
        );
        assert_eq!(
            normalize_triple_drop_vendor("aarch64-pc-linux-musl"),
            "aarch64-linux-musl"
        );
        assert_eq!(
            normalize_triple_drop_vendor("x86_64-w64-mingw32"),
            "x86_64-mingw32"
        );
        assert_eq!(
            normalize_triple_drop_vendor("aarch64-apple-darwin20"),
            "aarch64-darwin20"
        );
        // Degenerate inputs: returned as-is.
        assert_eq!(normalize_triple_drop_vendor("aarch64"), "aarch64");
        assert_eq!(normalize_triple_drop_vendor("aarch64-musl"), "aarch64-musl");
        assert_eq!(normalize_triple_drop_vendor(""), "");
    }

    // #185: the repository's env credential reaches the index request, a
    // refusal is actionable (also when a cached index exists), and a public
    // repository gets no `Authorization` header at all. #186: with no env
    // credential, the netrc entry for the host is used instead.
    #[cfg(not(target_os = "windows"))]
    #[test]
    fn fetch_custom_authenticates_the_index_request() {
        use crate::auth::{test_authorization, test_response, test_server};
        use std::io::Write;

        let _env = crate::env_vars::env_lock();
        let cache = tempfile::tempdir().unwrap();
        let vars = ["UVR_CACHE_DIR", "UVR_REPO_TOKEN_AUTHREPO", "NETRC"];
        let saved: Vec<_> = vars.iter().map(std::env::var_os).collect();
        std::env::set_var("UVR_CACHE_DIR", cache.path());
        std::env::set_var("UVR_REPO_TOKEN_AUTHREPO", "tok123");
        // No netrc file yet: the user's own ~/.netrc must not interfere.
        std::env::set_var("NETRC", cache.path().join("no-netrc"));

        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(b"Package: privpkg\nVersion: 1.0\n").unwrap();
        let gz = gz.finish().unwrap();
        let index = gz.clone();
        let (private, private_seen) = test_server(move |head| {
            // base64("alice:n3trc-pw")
            let auth = test_authorization(head);
            if auth == Some("Bearer tok123") || auth == Some("Basic YWxpY2U6bjN0cmMtcHc=") {
                test_response("200 OK", "ETag: \"v1\"\r\n", &index)
            } else {
                test_response("401 Unauthorized", "", b"")
            }
        });
        let (public, public_seen) = test_server(move |_| test_response("200 OK", "", &gz));

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let client = reqwest::Client::new();
        let fetch = |name: &'static str, url: String, force: bool| {
            rt.block_on(CranRegistry::fetch_custom(&client, name, &url, force, None))
        };

        let reg = fetch("authrepo", private.clone(), true).expect("token accepted");
        assert!(reg.resolve_package("privpkg", None).is_ok());

        // Without the token: an actionable error, from the fresh fetch and
        // from the conditional one that would otherwise fall back to cache.
        std::env::remove_var("UVR_REPO_TOKEN_AUTHREPO");
        for force in [false, true] {
            let err = fetch("authrepo", private.clone(), force)
                .err()
                .expect("401 must fail")
                .to_string();
            assert!(err.contains("repository 'authrepo'"), "{err}");
            assert!(err.contains("UVR_REPO_TOKEN_AUTHREPO"), "{err}");
        }

        // Regression: a public repository is fetched exactly as before.
        let reg = fetch("public", public, true).expect("public repo");
        assert!(reg.resolve_package("privpkg", None).is_ok());
        let seen = public_seen.lock().unwrap();
        assert_eq!(test_authorization(&seen[0]), None, "{seen:?}");
        drop(seen);

        // #186: no env credential, but a netrc entry for the host.
        let write_netrc = |name: &str, password: &str| {
            use std::os::unix::fs::PermissionsExt;
            // A new file name each time: netrc is cached per path.
            let path = cache.path().join(name);
            std::fs::write(
                &path,
                format!("machine 127.0.0.1 login alice password {password}\n"),
            )
            .unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            std::env::set_var("NETRC", &path);
        };
        write_netrc("netrc-good", "n3trc-pw");
        let reg = fetch("authrepo", private.clone(), true).expect("netrc accepted");
        assert!(reg.resolve_package("privpkg", None).is_ok());
        let last = private_seen.lock().unwrap().last().cloned().unwrap();
        assert_eq!(
            test_authorization(&last),
            Some("Basic YWxpY2U6bjN0cmMtcHc=")
        );

        write_netrc("netrc-bad", "wrong-pw");
        let err = fetch("authrepo", private.clone(), true)
            .err()
            .expect("a wrong netrc password must fail")
            .to_string();
        assert!(err.contains("`machine 127.0.0.1` entry in"), "{err}");
        assert!(!err.contains("wrong-pw"), "{err}");

        // An env token takes precedence over netrc.
        std::env::set_var("UVR_REPO_TOKEN_AUTHREPO", "tok123");
        fetch("authrepo", private, true).expect("token accepted");
        let last = private_seen.lock().unwrap().last().cloned().unwrap();
        assert_eq!(test_authorization(&last), Some("Bearer tok123"));

        for (var, value) in vars.iter().zip(saved) {
            match value {
                Some(v) => std::env::set_var(var, v),
                None => std::env::remove_var(var),
            }
        }
    }

    // ── lowest-version resolution (#193) ────────────────────────────

    /// A repository listing several releases of `rlang`.
    const MULTI_VERSION: &str = "Package: rlang
Version: 1.1.4

Package: rlang
Version: 0.4.0

Package: rlang
Version: 1.0-2

";

    #[test]
    fn get_lowest_picks_the_oldest_satisfying_version() {
        let index = parse_packages_gz(MULTI_VERSION).unwrap();
        let v = |e: &CranPackageEntry| e.raw_version.clone();
        assert_eq!(v(index.get_lowest("rlang", None).unwrap()), "0.4.0");
        assert_eq!(v(index.get_lowest("rlang", Some("*")).unwrap()), "0.4.0");
        assert_eq!(
            v(index.get_lowest("rlang", Some(">=1.0")).unwrap()),
            "1.0-2"
        );
        assert_eq!(
            v(index.get_lowest("rlang", Some(">1.0.2")).unwrap()),
            "1.1.4"
        );
        // Regression: highest selection is unchanged.
        assert_eq!(v(index.get_best("rlang", None).unwrap()), "1.1.4");
        assert_eq!(v(index.get_best("rlang", Some("<1.1")).unwrap()), "1.0-2");
    }

    #[test]
    fn get_lowest_without_a_satisfying_version_names_package_and_bound() {
        let index = parse_packages_gz(MULTI_VERSION).unwrap();
        let err = index.get_lowest("rlang", Some(">=2.0")).unwrap_err();
        assert!(matches!(
            &err,
            UvrError::NoMatchingVersion { package, constraint }
                if package == "rlang" && constraint == ">=2.0"
        ));
        assert_eq!(
            err.to_string(),
            "No version of 'rlang' satisfies constraint '>=2.0'"
        );
        assert!(matches!(
            index.get_lowest("absent", None),
            Err(UvrError::PackageNotFound(_))
        ));
    }

    #[test]
    fn get_lowest_skips_releases_that_need_packages_gone_from_cran() {
        let index = parse_packages_gz(
            "Package: testthat
Version: 0.1
Depends: R (>= 2.8), mutatr, digest

Package: testthat
Version: 0.3
Imports: digest

Package: digest
Version: 0.6.0

Package: bioclike
Version: 1.0
Imports: Biobase

Package: bioclike
Version: 2.0
Imports: Biobase

",
        )
        .unwrap();
        // mutatr is not in the index, so 0.1 cannot be installed.
        assert_eq!(
            index.get_lowest("testthat", None).unwrap().raw_version,
            "0.3"
        );
        // An exact request for the skipped release still gets it.
        assert_eq!(
            index
                .get_lowest("testthat", Some("==0.1"))
                .unwrap()
                .raw_version,
            "0.1"
        );
        // When every release imports a package from outside the index
        // (a Bioconductor dependency), the oldest is still chosen.
        assert_eq!(
            index.get_lowest("bioclike", None).unwrap().raw_version,
            "1.0"
        );
    }

    fn cran_registry(packages: &str) -> CranRegistry {
        CranRegistry::new(
            parse_packages_gz(packages).unwrap(),
            "https://cran.example/src/contrib",
            PackageSource::Cran,
        )
    }

    fn history(dcf: &str) -> Vec<CranPackageEntry> {
        parse_packages_gz(dcf)
            .unwrap()
            .all_entries()
            .cloned()
            .collect()
    }

    #[test]
    fn lowest_lookup_asks_for_missing_history_once() {
        let mut reg = cran_registry("Package: glue\nVersion: 1.8.0\nMD5sum: new\n\n");

        // Without history, the current release is the only candidate, and
        // the gap is reported. A package CRAN does not list is not.
        let info = reg.resolve_package_lowest("glue", None).unwrap();
        assert_eq!(info.version.to_string(), "1.8.0");
        assert!(reg.resolve_package_lowest("absent", None).is_err());
        assert_eq!(
            reg.take_wanted_history(),
            BTreeSet::from(["glue".to_string()])
        );
        assert!(reg.take_wanted_history().is_empty(), "take drains");

        // Highest lookups never ask.
        reg.resolve_package("glue", None).unwrap();
        assert!(reg.take_wanted_history().is_empty());

        reg.merge_history(
            "glue",
            history(
                "Package: glue\nVersion: 1.0.0\n\n\
                 Package: glue\nVersion: 1.6.0\nMD5sum: old\n\n\
                 Package: glue\nVersion: 1.8.0\nMD5sum: dup\n\n\
                 Package: glue\nVersion: 1.9.0\n\n\
                 Package: other\nVersion: 0.1\n\n",
            ),
        );
        // Only releases older than the current one are merged: the current
        // one keeps its index entry, a newer one is ignored, and so is a
        // document entry for another package.
        let versions: Vec<String> = reg
            .index
            .packages
            .get("glue")
            .unwrap()
            .iter()
            .map(|e| e.raw_version.clone())
            .collect();
        assert_eq!(versions, ["1.8.0", "1.6.0", "1.0.0"]);
        assert!(!reg.index.packages.contains_key("other"));

        let info = reg.resolve_package_lowest("glue", Some(">=1.5")).unwrap();
        assert_eq!(info.version.to_string(), "1.6.0");
        assert_eq!(
            info.url,
            "https://cran.example/src/contrib/Archive/glue/glue_1.6.0.tar.gz"
        );
        assert_eq!(info.checksum.as_deref(), Some("md5:old"));
        let info = reg.resolve_package_lowest("glue", None).unwrap();
        assert_eq!(info.checksum, None, "no MD5sum in the history → none");
        assert!(reg.take_wanted_history().is_empty(), "history is loaded");

        // The current release keeps its plain URL.
        let info = reg.resolve_package("glue", None).unwrap();
        assert_eq!(
            info.url,
            "https://cran.example/src/contrib/glue_1.8.0.tar.gz"
        );
        assert_eq!(info.checksum.as_deref(), Some("md5:new"));
    }

    #[test]
    fn highest_lookup_asks_for_history_when_a_bound_excludes_the_current_release() {
        // #195: an override (or an upper bound) can name an older release.
        let mut reg = cran_registry("Package: glue\nVersion: 1.8.0\n\n");
        // A floor above the current release: older ones cannot help.
        assert!(reg.resolve_package("glue", Some(">=2.0")).is_err());
        assert!(reg.take_wanted_history().is_empty());

        let err = reg.resolve_package("glue", Some("==1.6.0")).unwrap_err();
        assert!(matches!(err, UvrError::NoMatchingVersion { .. }));
        assert_eq!(
            reg.take_wanted_history(),
            BTreeSet::from(["glue".to_string()])
        );

        reg.merge_history(
            "glue",
            history("Package: glue\nVersion: 1.6.0\n\nPackage: glue\nVersion: 1.7.0\n\n"),
        );
        let info = reg.resolve_package("glue", Some("==1.6.0")).unwrap();
        assert_eq!(
            info.url,
            "https://cran.example/src/contrib/Archive/glue/glue_1.6.0.tar.gz"
        );
        let info = reg.resolve_package("glue", Some(">=1.0, <1.8")).unwrap();
        assert_eq!(info.version.to_string(), "1.7.0");
        // With the history in, a release that never existed fails for good.
        assert!(matches!(
            reg.resolve_package("glue", Some("==1.6.5")),
            Err(UvrError::NoMatchingVersion { .. })
        ));
        assert!(reg.take_wanted_history().is_empty());
    }

    #[test]
    fn override_to_an_archived_release_resolves_under_highest() {
        // The `uvr lock` loop with an override below the current release:
        // pass 1 fails and asks for the history, pass 2 locks the Archive
        // release even though `app` requires a newer one (#195).
        use crate::manifest::{DependencySpec, Manifest};
        use crate::resolver::Resolver;

        let lock_with = |forced: &str| {
            let mut reg = cran_registry(
                "Package: app\nVersion: 2.0\nImports: glue (>= 1.7.0)\n\n\
                 Package: glue\nVersion: 1.8.0\n\n",
            );
            let mut manifest = Manifest::new("t", None);
            manifest.add_dep("app".into(), DependencySpec::Version("*".into()), false);
            manifest
                .override_dependencies
                .insert("glue".into(), forced.into());
            let mut passes = 0;
            loop {
                passes += 1;
                let result = Resolver::new(&reg).resolve(&manifest, None, None, HashMap::new());
                let wanted = reg.take_wanted_history();
                if wanted.is_empty() {
                    break (result, passes);
                }
                for name in wanted {
                    reg.merge_history(&name, history("Package: glue\nVersion: 1.6.0\n\n"));
                }
            }
        };

        let (lock, passes) = lock_with("1.6.0");
        let lock = lock.unwrap();
        assert_eq!(passes, 2);
        let glue = lock.get_package("glue").unwrap();
        assert_eq!(glue.version, "1.6.0");
        assert_eq!(
            glue.url.as_deref(),
            Some("https://cran.example/src/contrib/Archive/glue/glue_1.6.0.tar.gz")
        );

        // A version CRAN never had is an error that names the override.
        let (lock, passes) = lock_with("1.6.5");
        assert_eq!(passes, 2);
        assert_eq!(
            lock.unwrap_err().to_string(),
            "No repository has glue 1.6.5, which [override-dependencies] requires"
        );
    }

    #[test]
    fn custom_repositories_do_not_load_history_or_use_archive_urls() {
        let reg = registry_from_packages(MULTI_VERSION, "https://repo.example/src/contrib");
        let info = reg.resolve_package_lowest("rlang", None).unwrap();
        assert_eq!(info.version.to_string(), "0.4.0");
        assert_eq!(
            info.url,
            "https://repo.example/src/contrib/rlang_0.4.0.tar.gz"
        );
        assert!(reg.take_wanted_history().is_empty());
    }

    #[test]
    fn lowest_resolution_converges_as_history_loads() {
        // The loop `uvr lock` runs: resolve, load the history the pass asked
        // for, and resolve again until a pass asks for nothing.
        use crate::manifest::{DependencySpec, Manifest, ResolutionStrategy};
        use crate::resolver::Resolver;

        let mut reg = cran_registry(
            "Package: app\nVersion: 2.0\nImports: helper (>= 1.0)\n\n\
             Package: helper\nVersion: 1.5\n\n\
             Package: extra\nVersion: 3.0\n\n",
        );
        let histories = [
            (
                "app",
                "Package: app\nVersion: 0.5\nImports: legacy\n\n\
                 Package: app\nVersion: 1.0\nImports: extra\n\n",
            ),
            ("helper", "Package: helper\nVersion: 0.9\n\n"),
        ];
        let mut manifest = Manifest::new("t", None);
        manifest.add_dep("app".into(), DependencySpec::Version("*".into()), false);

        let mut passes = 0;
        let lock = loop {
            passes += 1;
            let result = Resolver::new(&reg)
                .with_strategy(ResolutionStrategy::Lowest)
                .resolve(&manifest, None, None, HashMap::new());
            let wanted = reg.take_wanted_history();
            if wanted.is_empty() {
                break result.unwrap();
            }
            for name in wanted {
                let dcf = histories.iter().find(|(n, _)| *n == name).map(|(_, d)| *d);
                reg.merge_history(&name, dcf.map(history).unwrap_or_default());
            }
        };
        // Pass 1 sees only current releases and asks for app and helper.
        // Pass 2 picks app 1.0 (0.5 needs `legacy`, which left CRAN), which
        // brings in extra, so it asks for extra. Pass 3 asks for nothing.
        assert_eq!(passes, 3);
        let version = |n: &str| lock.get_package(n).map(|p| p.version.clone());
        assert_eq!(version("app").as_deref(), Some("1.0.0"));
        assert_eq!(version("extra").as_deref(), Some("3.0.0"));
        assert_eq!(lock.packages.len(), 2, "helper is no longer needed");
    }
}
