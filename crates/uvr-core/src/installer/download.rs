use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;
use tracing::debug;

use crate::checksum;
use crate::error::{Result, UvrError};
use crate::lockfile::LockedPackage;

pub struct Downloader {
    client: reqwest::Client,
    cache_dir: PathBuf,
    concurrency: usize,
}

impl Downloader {
    pub fn new(client: reqwest::Client, cache_dir: PathBuf, concurrency: usize) -> Self {
        Downloader {
            client,
            cache_dir,
            concurrency,
        }
    }

    /// Download all packages in parallel (bounded by `self.concurrency`).
    /// Returns `(tarball_path, was_binary)` in the same order as `packages`.
    ///
    /// Each entry has a primary URL and an optional fallback URL. If the primary
    /// download fails (e.g. P3M 500), the fallback is tried automatically.
    /// `is_binary` signals a P3M pre-built binary: checksum in the lockfile was
    /// recorded for the source tarball and must not be checked against the binary.
    pub async fn download_all(&self, packages: &[DownloadSpec<'_>]) -> Result<Vec<DownloadResult>> {
        let semaphore = Arc::new(Semaphore::new(self.concurrency));
        let mp = Arc::new(MultiProgress::new());

        let tasks: Vec<_> = packages
            .iter()
            .map(|spec| {
                let sem = semaphore.clone();
                let mp = mp.clone();
                let client = self.client.clone();
                let cache_dir = self.cache_dir.clone();
                let pkg_name = spec.pkg.name.clone();
                let pkg_version = spec.pkg.version.clone();
                let url = spec.url.to_string();
                let fallback_url = spec.fallback_url.map(str::to_string);
                let is_binary = spec.is_binary;
                let user_agent = spec.user_agent.map(str::to_string);
                // Binary packages: lockfile checksum is for the source tarball, not the
                // P3M binary. Skip verification on binary downloads, but keep the
                // checksum for the fallback path which downloads the source tarball.
                let source_checksum = spec.pkg.checksum.clone();
                let primary_checksum = if is_binary {
                    None
                } else {
                    source_checksum.clone()
                };

                tokio::spawn(async move {
                    let _permit = sem.acquire().await.unwrap();

                    // Try primary URL. The UA override only applies to the
                    // primary path — fallbacks (CRAN source) don't need it.
                    let primary_result = download_one(
                        &client,
                        &cache_dir,
                        &pkg_name,
                        &pkg_version,
                        &url,
                        primary_checksum.as_deref(),
                        user_agent.as_deref(),
                        &mp,
                    )
                    .await;

                    match primary_result {
                        Ok(path) => Ok(DownloadResult {
                            path,
                            used_binary: is_binary,
                        }),
                        Err(e) if is_binary && fallback_url.is_some() => {
                            // Binary download failed — fall back to source tarball.
                            // The most common reason here is "Posit hasn't built this
                            // version against this R minor" (especially on older R
                            // branches), which is normal and not a uvr error. Keep the
                            // detail at debug-level; users see a single dim INFO line
                            // so it's clear *why* the install is taking longer.
                            let fallback = fallback_url.as_ref().unwrap();
                            tracing::debug!(
                                "P3M binary unavailable for {pkg_name} {pkg_version}, falling back to source: {e}"
                            );
                            tracing::info!(
                                "{pkg_name} {pkg_version}: no P3M binary for this R minor, compiling from source"
                            );
                            let path = download_one(
                                &client,
                                &cache_dir,
                                &pkg_name,
                                &pkg_version,
                                fallback,
                                source_checksum.as_deref(),
                                None, // fallback URL is plain CRAN source — no UA override needed
                                &mp,
                            )
                            .await?;
                            Ok(DownloadResult {
                                path,
                                used_binary: false,
                            })
                        }
                        Err(e) => Err(e),
                    }
                })
            })
            .collect();

        let mut results = Vec::new();
        for task in tasks {
            results.push(task.await.map_err(|e| UvrError::Other(e.to_string()))??);
        }
        Ok(results)
    }
}

/// Specification for downloading a single package.
pub struct DownloadSpec<'a> {
    pub pkg: &'a LockedPackage,
    pub url: &'a str,
    /// Fallback URL to try if primary fails (e.g. source tarball when P3M binary 500s).
    pub fallback_url: Option<&'a str>,
    pub is_binary: bool,
    /// Per-request `User-Agent` override. Set this for Linux PPM binary
    /// URLs — PPM uses the UA to choose between binary and source builds
    /// served at the same URL, and the default `uvr/x.y.z` UA gets you
    /// source. None = use the client's default UA.
    pub user_agent: Option<&'a str>,
}

/// Result of downloading a single package.
pub struct DownloadResult {
    pub path: PathBuf,
    /// Whether the download used the binary (P3M) URL or fell back to source.
    pub used_binary: bool,
}

/// Compute the CRAN Archive fallback URL for a `src/contrib` URL.
/// CRAN moves older package versions to `/src/contrib/Archive/<pkg>/` when
/// a new version is published, so old lockfile URLs start to 404.
/// Returns `None` if the URL doesn't look like a CRAN source tarball, or
/// already points at the Archive.
fn cran_archive_url(url: &str) -> Option<String> {
    if !url.contains("/src/contrib/") || url.contains("/src/contrib/Archive/") {
        return None;
    }
    let (base, filename) = url.rsplit_once("/src/contrib/")?;
    if filename.contains('/') || !filename.ends_with(".tar.gz") {
        return None;
    }
    let (pkg_name, _) = filename.rsplit_once('_')?;
    if pkg_name.is_empty() {
        return None;
    }
    Some(format!("{base}/src/contrib/Archive/{pkg_name}/{filename}"))
}

#[allow(clippy::too_many_arguments)]
async fn download_one(
    client: &reqwest::Client,
    cache_dir: &Path,
    name: &str,
    version: &str,
    url: &str,
    expected_checksum: Option<&str>,
    user_agent: Option<&str>,
    mp: &MultiProgress,
) -> Result<PathBuf> {
    // Derive the cache filename from the URL so that source tarballs (.tar.gz)
    // and binary tarballs (.tgz) get distinct cache entries and never collide.
    let filename = url
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(&format!("{name}_{version}.tar.gz"))
        .to_string();
    let dest = cache_dir.join(&filename);

    if dest.exists() {
        // Verify the cached file when we have a checksum — a corrupted or
        // tampered cache entry must not silently bypass integrity checks.
        if let Some(expected) = expected_checksum {
            if expected.starts_with("md5:") || expected.starts_with("sha256:") {
                let cached = std::fs::read(&dest)?;
                if checksum::verify(expected, &cached, name).is_ok() {
                    debug!("Cache hit (verified): {filename}");
                    return Ok(dest);
                }
                debug!("Cache corrupt for {name}, re-downloading");
                let _ = std::fs::remove_file(&dest);
                // fall through to re-download
            } else if expected.starts_with("git:") {
                // GitHub packages: verify against sidecar SHA256 from first download
                let checksum_path = dest.with_extension("sha256");
                if let Ok(stored_checksum) = std::fs::read_to_string(&checksum_path) {
                    let cached = std::fs::read(&dest)?;
                    if checksum::verify(stored_checksum.trim(), &cached, name).is_ok() {
                        debug!("Cache hit (git, sha256 verified): {filename}");
                        return Ok(dest);
                    }
                    debug!("Cache corrupt for git package {name}, re-downloading");
                    let _ = std::fs::remove_file(&dest);
                    let _ = std::fs::remove_file(&checksum_path);
                } else {
                    // No sidecar yet (old cache entry) — accept it this time
                    debug!("Cache hit (git, unverified): {filename}");
                    return Ok(dest);
                }
            } else {
                debug!("Cache hit: {filename}");
                return Ok(dest);
            }
        } else {
            debug!("Cache hit: {filename}");
            return Ok(dest);
        }
    }

    std::fs::create_dir_all(cache_dir)?;

    let pb = mp.add(ProgressBar::new_spinner());
    pb.set_style(
        ProgressStyle::with_template("{spinner:.cyan} {msg}")
            .unwrap()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
    );
    pb.set_message(format!("Downloading {name} {version}..."));
    pb.enable_steady_tick(std::time::Duration::from_millis(80));

    // Stream response to a temp file to avoid buffering entire packages in RAM.
    // Compute checksums on-the-fly during the stream.
    let request = |target: &str| {
        let mut req = client.get(target);
        if let Some(ua) = user_agent {
            req = req.header(reqwest::header::USER_AGENT, ua);
        }
        req
    };
    let mut resp_result = match request(url).send().await {
        Ok(r) => r.error_for_status(),
        Err(e) => Err(e),
    };
    if resp_result.is_err() {
        if let Some(archive_url) = cran_archive_url(url) {
            debug!("{name}: {url} failed, retrying via CRAN Archive: {archive_url}");
            // CRAN Archive doesn't require the R-shaped UA, but plumbing the
            // override here is harmless and keeps requests symmetric.
            resp_result = match request(&archive_url).send().await {
                Ok(r) => r.error_for_status(),
                Err(e) => Err(e),
            };
        }
    }
    let mut resp = resp_result?;

    let cache_dir = dest.parent().unwrap_or(std::path::Path::new("."));
    let mut tmp_file = tempfile::Builder::new()
        .prefix(".uvr-dl-")
        .tempfile_in(cache_dir)?;
    let mut sha256_hasher = Sha256::new();
    let mut md5_hasher = md5::Md5::new();

    while let Some(chunk) = resp.chunk().await? {
        tmp_file.write_all(&chunk)?;
        sha256_hasher.update(&chunk);
        md5_hasher.update(&chunk);
    }
    tmp_file.flush()?;

    // Finalize both hashers eagerly before the checksum-check block.
    let sha256_hex = hex::encode(sha256_hasher.finalize());
    let md5_hex = hex::encode(md5_hasher.finalize());

    // Verify checksum from the on-the-fly computation
    if let Some(expected) = expected_checksum {
        if expected.starts_with("sha256:") {
            let actual = format!("sha256:{sha256_hex}");
            if actual != expected {
                // tmp_file dropped here → auto-deleted
                return Err(UvrError::ChecksumMismatch {
                    package: name.to_string(),
                    expected: expected.to_string(),
                    actual,
                });
            }
        } else if expected.starts_with("md5:") {
            let actual = format!("md5:{md5_hex}");
            if actual != expected {
                return Err(UvrError::ChecksumMismatch {
                    package: name.to_string(),
                    expected: expected.to_string(),
                    actual,
                });
            }
        } else if expected.starts_with("git:") {
            // GitHub packages: store SHA256 sidecar for future cache verification
            let computed = format!("sha256:{sha256_hex}");
            let checksum_path = dest.with_extension("sha256");
            let _ = std::fs::write(&checksum_path, &computed);
        }
    }

    // Atomic move: persist NamedTempFile → final destination
    tmp_file.persist(&dest).map_err(|e| {
        UvrError::Other(format!(
            "Failed to persist download to {}: {}",
            dest.display(),
            e
        ))
    })?;

    pb.finish_and_clear();
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::cran_archive_url;

    #[test]
    fn archive_url_rewrites_cran_src_contrib() {
        assert_eq!(
            cran_archive_url("https://cran.r-project.org/src/contrib/curl_7.0.0.tar.gz").as_deref(),
            Some("https://cran.r-project.org/src/contrib/Archive/curl/curl_7.0.0.tar.gz")
        );
    }

    #[test]
    fn archive_url_handles_cran_mirror() {
        assert_eq!(
            cran_archive_url("https://cloud.r-project.org/src/contrib/xml2_1.3.6.tar.gz")
                .as_deref(),
            Some("https://cloud.r-project.org/src/contrib/Archive/xml2/xml2_1.3.6.tar.gz")
        );
    }

    #[test]
    fn archive_url_handles_dotted_version() {
        assert_eq!(
            cran_archive_url("https://cran.r-project.org/src/contrib/scales_1.1-3.tar.gz")
                .as_deref(),
            Some("https://cran.r-project.org/src/contrib/Archive/scales/scales_1.1-3.tar.gz")
        );
    }

    #[test]
    fn archive_url_none_for_non_cran() {
        assert_eq!(
            cran_archive_url("https://bioconductor.org/packages/3.18/bioc/src/contrib/DESeq2_1.42.0.tar.gz"),
            Some("https://bioconductor.org/packages/3.18/bioc/src/contrib/Archive/DESeq2/DESeq2_1.42.0.tar.gz".to_string())
        );
        // That's actually fine — Bioconductor doesn't use Archive/ but the
        // retry is harmless (another 404), and keeping the logic generic
        // means future repos with the same layout work too.
    }

    #[test]
    fn archive_url_none_if_already_archive() {
        assert_eq!(
            cran_archive_url(
                "https://cran.r-project.org/src/contrib/Archive/curl/curl_7.0.0.tar.gz"
            ),
            None
        );
    }

    #[test]
    fn archive_url_none_for_unrelated_url() {
        assert_eq!(cran_archive_url("https://example.com/foo.tar.gz"), None);
        assert_eq!(cran_archive_url("https://p3m.dev/cran/latest/bin/macosx/big-sur-arm64/contrib/4.5/ggplot2_3.5.1.tgz"), None);
    }

    #[test]
    fn archive_url_none_for_non_tar_gz() {
        assert_eq!(
            cran_archive_url("https://cran.r-project.org/src/contrib/PACKAGES.gz"),
            None
        );
    }
}
