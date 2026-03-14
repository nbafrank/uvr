use std::path::{Path, PathBuf};
use std::sync::Arc;

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
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
        Downloader { client, cache_dir, concurrency }
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
                let checksum = if *is_binary { None } else { pkg.checksum.clone() };

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
            let cached = std::fs::read(&dest)?;
            if checksum::verify(expected, &cached, name).is_ok() {
                debug!("Cache hit (verified): {filename}");
                return Ok(dest);
            }
            debug!("Cache corrupt for {name}, re-downloading");
            let _ = std::fs::remove_file(&dest); // best-effort removal
            // fall through to re-download
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

    let resp = client.get(url).send().await?.error_for_status()?;
    let bytes = resp.bytes().await?;

    if let Some(expected) = expected_checksum {
        if expected.starts_with("md5:") || expected.starts_with("sha256:") {
            checksum::verify(expected, &bytes, name)?;
        }
    }

    std::fs::write(&dest, &bytes)?;
    pb.finish_with_message(format!("Downloaded {name} {version}"));
    Ok(dest)
}
