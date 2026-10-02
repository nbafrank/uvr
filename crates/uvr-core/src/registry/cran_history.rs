//! Older CRAN releases, for lowest-version resolution (#193).
//!
//! CRAN's `PACKAGES` index lists only the current release of each package.
//! Older releases sit under `src/contrib/Archive/<name>/`, and CRAN serves no
//! index of their DESCRIPTION metadata. crandb (METACRAN,
//! <https://crandb.r-pkg.org>) serves the DESCRIPTION of every release as one
//! JSON document per package, so one request per package gives both the
//! candidate versions and the dependencies of each.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use semver::Version;
use serde::Deserialize;
use serde_json::{Map, Value};
use tracing::{debug, warn};

use crate::error::{Result, UvrError};
use crate::registry::cran::{parse_dep_field, CranPackageEntry, DepConstraint};
use crate::resolver::normalize_version;

pub const CRANDB_URL: &str = "https://crandb.r-pkg.org";

/// Where release histories are cached: `<cache>/cran-history/<name>.json`.
pub fn cache_dir() -> PathBuf {
    crate::env_vars::cache_dir_or_temp().join("cran-history")
}

/// Every release of `name` that crandb knows.
///
/// `current` is the release the CRAN index lists. Releases never change after
/// publication, so a cached document that already has `current` also has
/// every older release, and is used without a request. A document without it
/// predates the current release and is fetched again.
pub async fn fetch(
    client: &reqwest::Client,
    base_url: &str,
    cache_dir: &Path,
    name: &str,
    current: &Version,
) -> Result<Vec<CranPackageEntry>> {
    if !crate::package_name::is_valid(name) {
        return Err(UvrError::Other(format!("Invalid package name '{name}'")));
    }
    let cache = cache_dir.join(format!("{name}.json"));
    let cached = std::fs::read_to_string(&cache)
        .ok()
        .and_then(|text| parse(&text).ok());
    if cached
        .as_ref()
        .is_some_and(|entries| entries.iter().any(|e| &e.version == current))
    {
        debug!("{name} release history: cached");
        return Ok(cached.unwrap_or_default());
    }

    let url = format!("{}/{name}/all", base_url.trim_end_matches('/'));
    debug!("Fetching {name} release history from {url}");
    let text = match fetch_text(client, &url).await {
        Ok(Some(text)) => text,
        Ok(None) => {
            warn!(
                "{url} has no release history for '{name}'; only its current version can be selected"
            );
            return Ok(Vec::new());
        }
        Err(e) => {
            return match cached {
                Some(entries) => {
                    warn!("Could not refresh the release history of '{name}' ({e}); using the cached copy");
                    Ok(entries)
                }
                None => Err(UvrError::Other(format!(
                    "Failed to fetch the release history of '{name}' from {url}, \
                     which lowest-version resolution needs: {e}"
                ))),
            };
        }
    };
    let entries = parse(&text)?;

    // Write the cache only after a successful parse.
    let _ = std::fs::create_dir_all(cache_dir);
    if let Err(e) = std::fs::write(&cache, &text) {
        warn!(
            "Failed to cache {name} release history at {}: {e}",
            cache.display()
        );
    }
    Ok(entries)
}

/// GET `url`; `None` on 404 (crandb does not know the package).
async fn fetch_text(client: &reqwest::Client, url: &str) -> Result<Option<String>> {
    let resp = client.get(url).send().await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    Ok(Some(resp.error_for_status()?.text().await?))
}

#[derive(Deserialize)]
struct Document {
    versions: BTreeMap<String, Map<String, Value>>,
}

/// Parse a crandb `/<name>/all` document into index entries.
pub(crate) fn parse(json: &str) -> Result<Vec<CranPackageEntry>> {
    let doc: Document = serde_json::from_str(json)
        .map_err(|e| UvrError::Other(format!("Malformed crandb document: {e}")))?;
    Ok(doc.versions.values().filter_map(entry).collect())
}

fn entry(fields: &Map<String, Value>) -> Option<CranPackageEntry> {
    let text = |key: &str| fields.get(key).and_then(Value::as_str).map(squash);
    let name = text("Package")?;
    let raw_version = text("Version")?;
    let version = match Version::parse(&normalize_version(&raw_version)) {
        Ok(v) => v,
        Err(e) => {
            warn!("package '{name}': unparseable archived version '{raw_version}' ({e}); skipping");
            return None;
        }
    };
    Some(CranPackageEntry {
        name,
        version,
        raw_version,
        depends: deps(fields.get("Depends")),
        imports: deps(fields.get("Imports")),
        linking_to: deps(fields.get("LinkingTo")),
        md5sum: text("MD5sum").unwrap_or_default(),
        system_requirements: text("SystemRequirements"),
        path: None,
        built: None,
    })
}

/// crandb stores a dependency field as `{name: constraint}`, with `"*"` for
/// no constraint. Rebuild the DESCRIPTION form and parse it as CRAN's is.
fn deps(value: Option<&Value>) -> Vec<DepConstraint> {
    let field = match value {
        Some(Value::Object(map)) => map
            .iter()
            .map(|(dep, c)| match c.as_str().map(squash) {
                Some(c) if c != "*" => format!("{dep} ({c})"),
                _ => dep.clone(),
            })
            .collect::<Vec<_>>()
            .join(", "),
        Some(Value::String(s)) => squash(s),
        _ => return Vec::new(),
    };
    parse_dep_field(&field)
}

/// Collapse the line breaks that DESCRIPTION values keep (`">=\n1.2.0"`).
/// Older crandb documents spell them as the text `<U+000a>`.
fn squash(s: &str) -> String {
    s.replace("<U+000a>", " ")
        .replace("<U+000A>", " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from `https://crandb.r-pkg.org/glue/all` and
    /// `.../ggplot2/all`, keeping the shapes the parser must handle.
    const GLUE_DOC: &str = r#"{
      "_id": "glue", "name": "glue", "latest": "1.8.0", "archived": false,
      "versions": {
        "1.0.0": {"Package": "glue", "Version": "1.0.0",
                  "Depends": {"R": ">= 3.0.0"},
                  "Suggests": {"testthat": "*"}},
        "1.6.0": {"Package": "glue", "Version": "1.6.0",
                  "Depends": {"R": ">= 3.4"},
                  "Imports": {"methods": "*", "scales": ">=\n1.2.0", "rlang": ">=<U+000a>0.4"},
                  "MD5sum": "0123456789abcdef0123456789abcdef"},
        "1.8.0": {"Package": "glue", "Version": "1.8.0",
                  "Depends": "R (>= 3.6)",
                  "LinkingTo": {"cpp11": ">= 0.4.0"},
                  "SystemRequirements": "a\n  b",
                  "MD5sum": "283bba07f61d89e32f9895021bf5099c"},
        "bogus": {"Package": "glue", "Version": "not a version"}
      }
    }"#;

    fn by_version<'a>(entries: &'a [CranPackageEntry], v: &str) -> &'a CranPackageEntry {
        entries
            .iter()
            .find(|e| e.raw_version == v)
            .unwrap_or_else(|| panic!("no {v}"))
    }

    #[test]
    fn parse_reads_every_release_with_its_dependencies() {
        let entries = parse(GLUE_DOC).unwrap();
        // The unparseable version is skipped, not fatal.
        assert_eq!(entries.len(), 3);

        let old = by_version(&entries, "1.0.0");
        assert_eq!(old.name, "glue");
        assert!(old.md5sum.is_empty(), "no MD5sum → no checksum");
        assert!(old.requires_as_deps().is_empty(), "R is a base package");

        let mid = by_version(&entries, "1.6.0");
        let mut deps: Vec<(String, Option<String>)> = mid
            .requires_as_deps()
            .into_iter()
            .map(|d| (d.name, d.constraint))
            .collect();
        deps.sort();
        // methods is a base package; Suggests is not a dependency. A
        // constraint with an embedded line break, in either spelling, still
        // binds.
        assert_eq!(
            deps,
            [
                ("rlang".to_string(), Some(">=0.4.0".to_string())),
                ("scales".to_string(), Some(">=1.2.0".to_string()))
            ]
        );
        assert_eq!(mid.md5sum, "0123456789abcdef0123456789abcdef");

        let new = by_version(&entries, "1.8.0");
        // DESCRIPTION-text form of a dependency field is accepted too.
        assert_eq!(new.depends.len(), 1);
        assert_eq!(new.depends[0].name, "R");
        assert_eq!(new.linking_to[0].name, "cpp11");
        assert_eq!(new.system_requirements.as_deref(), Some("a b"));
    }

    #[test]
    fn parse_rejects_a_document_without_versions() {
        assert!(parse(r#"{"error":"not_found","reason":"document not found"}"#).is_err());
    }

    /// A port nothing listens on, so a fetch fails fast without the network.
    fn dead_url() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    }

    fn fetch_blocking(cache: &Path, current: &str) -> Result<Vec<CranPackageEntry>> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let client = reqwest::Client::new();
        let current = Version::parse(current).unwrap();
        rt.block_on(fetch(&client, &dead_url(), cache, "glue", &current))
    }

    #[test]
    fn fetch_uses_a_cache_that_has_the_current_release() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("glue.json"), GLUE_DOC).unwrap();
        // The server is unreachable, so success proves no request was needed.
        let entries = fetch_blocking(dir.path(), "1.8.0").unwrap();
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn fetch_falls_back_to_a_stale_cache_when_offline() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("glue.json"), GLUE_DOC).unwrap();
        // 1.8.1 is newer than the cached document, so a refresh is tried,
        // fails, and the stale copy is used.
        let entries = fetch_blocking(dir.path(), "1.8.1").unwrap();
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn fetch_without_cache_or_network_names_the_package() {
        let dir = tempfile::tempdir().unwrap();
        let err = fetch_blocking(dir.path(), "1.8.0").unwrap_err().to_string();
        assert!(err.contains("release history of 'glue'"), "{err}");
    }
}
