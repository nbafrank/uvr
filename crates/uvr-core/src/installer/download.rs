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
    /// Returns paths to the downloaded tarballs in the same order as `packages`.
    ///
    /// `is_binary` signals a P3M pre-built binary: checksum in the lockfile was
    /// recorded for the source tarball and must not be checked against the binary.
    pub async fn download_all(
        &self,
        packages: &[(&LockedPackage, &str, bool)], // (package, url, is_binary)
    ) -> Result<Vec<PathBuf>> {
        let semaphore = Arc::new(Semaphore::new(self.concurrency));
        let mp = Arc::new(MultiProgress::new());

        let tasks: Vec<_> = packages
            .iter()
            .map(|(pkg, url, is_binary)| {
                let sem = semaphore.clone();
                let mp = mp.clone();
                let client = self.client.clone();
                let cache_dir = self.cache_dir.clone();
                let pkg_name = pkg.name.clone();
                let pkg_version = pkg.version.clone();
                let url = url.to_string();
                // Binary packages: checksum in lockfile is for the source tarball;
                // skip verification so we don't reject a valid P3M binary.
                let checksum = if *is_binary {
                    None
                } else {
                    pkg.checksum.clone()
                };

                tokio::spawn(async move {
                    let _permit = sem.acquire().await.unwrap();
                    download_one(
                        &client,
                        &cache_dir,
                        &pkg_name,
                        &pkg_version,
                        &url,
                        checksum.as_deref(),
                        &mp,
                    )
                    .await
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

async fn download_one(
    client: &reqwest::Client,
    cache_dir: &Path,
    name: &str,
    version: &str,
    url: &str,
    expected_checksum: Option<&str>,
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
    let mut resp = client.get(url).send().await?.error_for_status()?;

    let tmp_path = dest.with_extension("tmp");
    let mut file = std::fs::File::create(&tmp_path)?;
    let mut sha256_hasher = Sha256::new();
    let mut md5_hasher = md5::Md5::new();

    while let Some(chunk) = resp.chunk().await? {
        file.write_all(&chunk)?;
        sha256_hasher.update(&chunk);
        md5_hasher.update(&chunk);
    }
    file.flush()?;
    drop(file);

    // Verify checksum from the on-the-fly computation
    if let Some(expected) = expected_checksum {
        if expected.starts_with("sha256:") {
            let actual = format!("sha256:{}", hex::encode(sha256_hasher.finalize()));
            if actual != expected {
                let _ = std::fs::remove_file(&tmp_path);
                return Err(UvrError::ChecksumMismatch {
                    package: name.to_string(),
                    expected: expected.to_string(),
                    actual,
                });
            }
        } else if expected.starts_with("md5:") {
            let actual = format!("md5:{}", hex::encode(md5_hasher.finalize()));
            if actual != expected {
                let _ = std::fs::remove_file(&tmp_path);
                return Err(UvrError::ChecksumMismatch {
                    package: name.to_string(),
                    expected: expected.to_string(),
                    actual,
                });
            }
        } else if expected.starts_with("git:") {
            // GitHub packages: store SHA256 sidecar for future cache verification
            let computed = format!("sha256:{}", hex::encode(sha256_hasher.finalize()));
            let checksum_path = dest.with_extension("sha256");
            let _ = std::fs::write(&checksum_path, &computed);
        }
    }

    // Atomic move: temp → final destination
    std::fs::rename(&tmp_path, &dest)?;

    pb.finish_with_message(format!("Downloaded {name} {version}"));
    Ok(dest)
}
