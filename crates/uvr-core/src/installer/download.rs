use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;
use tracing::debug;

use crate::auth::{self, GitHost, Repository};
use crate::checksum;
use crate::error::{Result, UvrError};
use crate::lockfile::LockedPackage;

pub struct Downloader {
    client: reqwest::Client,
    cache_dir: PathBuf,
    concurrency: usize,
    repositories: Arc<[Repository]>,
}

impl Downloader {
    pub fn new(client: reqwest::Client, cache_dir: PathBuf, concurrency: usize) -> Self {
        Downloader {
            client,
            cache_dir,
            concurrency,
            repositories: Arc::new([]),
        }
    }

    /// Authenticated repositories (#185). Every request to a URL one of
    /// them serves carries that repository's credential — primary,
    /// fallback, and CRAN-Archive retry alike — and no other request does.
    pub fn with_repositories(mut self, repositories: Vec<Repository>) -> Self {
        self.repositories = repositories.into();
        self
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
                let repos = self.repositories.clone();
                let pkg_name = spec.pkg.name.clone();
                let pkg_version = spec.pkg.version.clone();
                let url = spec.url.to_string();
                let fallback_url = spec.fallback_url.map(str::to_string);
                let is_binary = spec.is_binary;
                let user_agent = spec.user_agent.map(str::to_string);
                // A GitHub/GitLab/Forgejo package: its host's token goes to
                // the URLs on that host (#187), whichever of them this is. A
                // `git::` package is fetched with git instead (#190).
                let source = spec.pkg.source.clone();
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
                    let git = GitHost::for_source(&source);

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
                        git,
                        &repos,
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
                                git,
                                &repos,
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

/// Download the tarball of resolved git package `info` as `uvr sync` does,
/// and return its bytes.
#[cfg(all(test, not(target_os = "windows")))]
pub(crate) fn test_download(
    rt: &tokio::runtime::Runtime,
    info: &crate::registry::PackageInfo,
) -> Result<Vec<u8>> {
    let pkg = LockedPackage {
        name: info.name.clone(),
        version: info.version.to_string(),
        raw_version: None,
        source: info.source.clone(),
        url: Some(info.url.clone()),
        checksum: info.checksum.clone(),
        subdirectory: None,
        requires: vec![],
        system_requirements: None,
        dev: false,
    };
    let cache = tempfile::tempdir()?;
    let results = rt.block_on(
        Downloader::new(reqwest::Client::new(), cache.path().to_path_buf(), 1).download_all(&[
            DownloadSpec {
                pkg: &pkg,
                url: &info.url,
                fallback_url: None,
                is_binary: false,
                user_agent: None,
            },
        ]),
    )?;
    Ok(std::fs::read(&results[0].path)?)
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

/// Build the download-cache filename for a package fetch (#122).
///
/// = short hash of (URL + User-Agent) prefixed onto the URL basename. The
/// basename alone is NOT a unique key for the bytes the server returns: on
/// Linux, PPM serves a *different* R-version binary at the *same URL*, selecting
/// it by the R version in the User-Agent — so two R minors share a basename and
/// would collide, installing a wrong-ABI binary that fails cryptically at
/// `library()` time. Folding the UA into the key disambiguates the Linux case;
/// on Windows/macOS the UA is `None` and the R-minor lives in the URL path, so
/// the URL alone already distinguishes them. The basename suffix preserves the
/// `.tar.gz`/`.tgz` extension (source vs binary) and keeps entries recognizable.
///
/// The 32-bit (8 hex) tag is ample: a collision needs both a hash collision
/// AND an identical basename, and the per-basename population is O(active R
/// minor / distro count) — a handful, so birthday-paradox risk is negligible.
/// The `|` separator between URL and UA can't cause aliasing in practice: URLs
/// always start with `https://` and UAs with `R (`, so no input bridges across
/// it. On the CRAN-Archive retry the entry is keyed on the *primary* URL even
/// though the bytes came from the Archive URL — harmless, because Archive is a
/// bitwise copy of the original and the checksum re-read would catch any drift.
fn cache_filename(url: &str, user_agent: Option<&str>, fallback_name: &str) -> String {
    let basename = url
        .rsplit('/')
        .next()
        // Strip any query string/fragment (e.g. GitLab's
        // `archive.tar.gz?sha=...`) — `?` is an illegal filename character on
        // Windows, and left in it breaks persisting the downloaded temp file
        // (os error 123).
        .map(|s| s.split(['?', '#']).next().unwrap_or(s))
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback_name);
    let mut hasher = Sha256::new();
    hasher.update(url.as_bytes());
    if let Some(ua) = user_agent {
        hasher.update(b"|");
        hasher.update(ua.as_bytes());
    }
    // hex::encode of a SHA-256 digest is always 64 chars, so [..8] never panics.
    let url_tag = hex::encode(hasher.finalize());
    format!("{}-{basename}", &url_tag[..8])
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
    git: Option<GitHost<'_>>,
    repos: &[Repository],
    mp: &MultiProgress,
) -> Result<PathBuf> {
    // Cache filename = short hash of (URL + User-Agent) prefixed onto the URL
    // basename. The basename alone is NOT a unique key for the bytes P3M
    // returns (#122): on Linux, PPM serves a *different* R-version binary at the
    // *same URL*, selecting it by the R version in the User-Agent — so two R
    // minors share a basename and collide, installing a wrong-ABI binary that
    // fails cryptically at library() time. Folding the UA into the key
    // disambiguates the Linux case; on Windows/macOS the UA is None and the
    // R-minor lives in the URL path, so the URL alone already distinguishes
    // them. Keeping the basename suffix preserves the .tar.gz/.tgz extension
    // (source vs binary) and keeps cache entries human-recognizable.
    // A `git::` package (#190): `url` is the clone URL, the same for every
    // commit, so git_generic keys its own cache entry by URL and commit.
    if let Some(GitHost::Git(clone_url)) = git {
        let commit = expected_checksum
            .and_then(|c| c.strip_prefix("git:"))
            .ok_or_else(|| {
                UvrError::Other(format!(
                    "Locked git package '{name}' has no `git:<commit>` checksum. Re-run \
                     `uvr lock`."
                ))
            })?;
        return crate::registry::git_generic::cached_tarball(cache_dir, clone_url, commit).await;
    }

    let fallback_name = format!("{name}_{version}.tar.gz");
    let filename = cache_filename(url, user_agent, &fallback_name);
    let dest = cache_dir.join(&filename);

    if dest.exists() {
        // Verify the cached file — a corrupted or tampered cache entry must
        // not silently bypass integrity checks.
        match expected_checksum {
            Some(expected) if expected.starts_with("md5:") || expected.starts_with("sha256:") => {
                let cached = std::fs::read(&dest)?;
                if checksum::verify(expected, &cached, name).is_ok() {
                    debug!("Cache hit (verified): {filename}");
                    return Ok(dest);
                }
                debug!("Cache corrupt for {name}, re-downloading");
                let _ = std::fs::remove_file(&dest);
                // fall through to re-download
            }
            // No upstream checksum covers these bytes: `git:` entries (the
            // lockfile pins a commit, not a tarball hash) and P3M binaries
            // (the lockfile checksum is for the *source* tarball, checksum
            // here is None).
            _ => {
                if sidecar_cache_hit(&dest, name)? {
                    return Ok(dest);
                }
            }
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
    // A repository credential goes only to URLs that repository serves,
    // and a git host's token only to URLs on that host, which it replaces
    // the repository credential for.
    let send = |target: &str| {
        let mut req = client.get(target);
        if let Some(ua) = user_agent {
            req = req.header(reqwest::header::USER_AGENT, ua);
        }
        let credential = auth::repository_for(repos, target).and_then(|r| r.credential.as_ref());
        if let Some(credential) = credential {
            req = credential.apply(req);
        }
        async move {
            match git {
                Some(git) => git.send(req).await,
                None => req.send().await,
            }
        }
    };
    let mut resp_result = match send(url).await {
        Ok(r) => r.error_for_status(),
        Err(e) => Err(e),
    };
    // A 401/403 from a git host or a known repository is the error to
    // report, even if the Archive retry below fails for some other reason.
    let denied = resp_result
        .as_ref()
        .err()
        .and_then(reqwest::Error::status)
        .and_then(|status| match git.filter(|git| git.serves(url)) {
            Some(git) => git.denied_error(status, url),
            None => auth::repository_for(repos, url)?.denied_error(status),
        });
    if resp_result.is_err() {
        if let Some(archive_url) = cran_archive_url(url) {
            debug!(
                "{name}: {} failed, retrying via CRAN Archive: {}",
                auth::redact_url(url),
                auth::redact_url(&archive_url)
            );
            // CRAN Archive doesn't require the R-shaped UA, but plumbing the
            // override here is harmless and keeps requests symmetric. A git
            // host's token reaches the Archive URL only if it is on that
            // host (#105, #187).
            resp_result = match send(&archive_url).await {
                Ok(r) => r.error_for_status(),
                Err(e) => Err(e),
            };
        }
    }
    let mut resp = match resp_result {
        Ok(resp) => resp,
        Err(e) => return Err(denied.unwrap_or_else(|| e.into())),
    };

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

    // Pin bytes that no upstream checksum covers — `git:` tarballs and P3M
    // binaries (checksum None; P3M publishes no checksums, so first download
    // is trust-on-first-use). The sha256 sidecar lets every future cache hit
    // verify against the first-seen bytes (#129, #140). Written after persist
    // so a sidecar never exists without its tarball.
    let lockfile_verifiable = matches!(
        expected_checksum,
        Some(e) if e.starts_with("md5:") || e.starts_with("sha256:")
    );
    if !lockfile_verifiable {
        write_sidecar(
            &dest.with_extension("sha256"),
            &format!("sha256:{sha256_hex}"),
            name,
        );
    }

    pb.finish_and_clear();
    Ok(dest)
}

/// Whether `dest` is a usable cache entry for bytes that no lockfile
/// checksum covers: it matches the sha256 sidecar pinned on first download
/// (#129, #140). A mismatched entry is removed, so the caller downloads
/// again. An entry with no sidecar yet (from an older uvr, or a crash
/// between download and sidecar write) is accepted once, and the sidecar is
/// backfilled so that the entry is pinned from now on.
pub(crate) fn sidecar_cache_hit(dest: &Path, name: &str) -> Result<bool> {
    if !dest.exists() {
        return Ok(false);
    }
    let checksum_path = dest.with_extension("sha256");
    let cached = std::fs::read(dest)?;
    let Ok(stored_checksum) = std::fs::read_to_string(&checksum_path) else {
        write_sidecar(&checksum_path, &checksum::sha256_hex(&cached), name);
        debug!("Cache hit (sidecar backfilled): {}", dest.display());
        return Ok(true);
    };
    if checksum::verify(stored_checksum.trim(), &cached, name).is_ok() {
        debug!("Cache hit (sidecar sha256 verified): {}", dest.display());
        return Ok(true);
    }
    debug!("Cache corrupt for {name}, re-downloading");
    let _ = std::fs::remove_file(dest);
    let _ = std::fs::remove_file(&checksum_path);
    Ok(false)
}

/// Write a `.sha256` sidecar next to a cached tarball. Failure is non-fatal —
/// the cache entry still works and the sidecar is backfilled on the next hit —
/// but it must not be silent (#140): without the sidecar the entry cannot be
/// integrity-verified.
pub(crate) fn write_sidecar(checksum_path: &Path, checksum: &str, name: &str) {
    if let Err(e) = std::fs::write(checksum_path, checksum) {
        tracing::warn!(
            "{name}: failed to write checksum sidecar {}: {e} — cached file stays unverified until the sidecar can be written",
            checksum_path.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{cache_filename, cran_archive_url, download_one};
    use crate::checksum;
    use std::path::PathBuf;

    /// Seed the cache with a fake tarball for `url` and return its dest path.
    /// The port-9 (discard) URL refuses connections instantly, so any code
    /// path that falls through to a real download fails fast with an error —
    /// which is exactly how the "treated as a miss" tests detect re-download.
    const UNREACHABLE_URL: &str = "http://127.0.0.1:9/pkg_1.0.0.tar.gz";

    fn seed_cache(cache_dir: &std::path::Path, url: &str, bytes: &[u8]) -> PathBuf {
        let filename = cache_filename(url, None, "pkg_1.0.0.tar.gz");
        let dest = cache_dir.join(filename);
        std::fs::write(&dest, bytes).unwrap();
        dest
    }

    async fn run_download_one(
        cache_dir: &std::path::Path,
        url: &str,
        expected_checksum: Option<&str>,
    ) -> crate::error::Result<PathBuf> {
        download_one(
            &reqwest::Client::new(),
            cache_dir,
            "pkg",
            "1.0.0",
            url,
            expected_checksum,
            None,
            None,
            &[],
            &indicatif::MultiProgress::new(),
        )
        .await
    }

    // #129: a binary cache entry (checksum None — the lockfile checksum is for
    // the source tarball) must be verified against its sha256 sidecar on hit.
    #[tokio::test]
    async fn binary_cache_hit_with_matching_sidecar_passes() {
        let tmp = tempfile::tempdir().unwrap();
        let bytes = b"binary tarball bytes";
        let dest = seed_cache(tmp.path(), UNREACHABLE_URL, bytes);
        std::fs::write(dest.with_extension("sha256"), checksum::sha256_hex(bytes)).unwrap();

        let got = run_download_one(tmp.path(), UNREACHABLE_URL, None)
            .await
            .expect("verified cache hit must succeed without touching the network");
        assert_eq!(got, dest);
        assert!(dest.exists());
    }

    // #129: a binary cache entry whose bytes don't match the sidecar is
    // corrupt/tampered — treat as a miss (same policy as the git: path):
    // remove both files and re-download. The unreachable URL makes the
    // re-download fail, proving the poisoned entry was NOT served.
    #[tokio::test]
    async fn binary_cache_hit_with_mismatching_sidecar_is_a_miss() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = seed_cache(tmp.path(), UNREACHABLE_URL, b"tampered bytes");
        let sidecar = dest.with_extension("sha256");
        std::fs::write(&sidecar, checksum::sha256_hex(b"original bytes")).unwrap();

        let result = run_download_one(tmp.path(), UNREACHABLE_URL, None).await;
        assert!(
            result.is_err(),
            "mismatch must force a re-download, not serve the cache"
        );
        assert!(!dest.exists(), "corrupt cache entry must be removed");
        assert!(!sidecar.exists(), "stale sidecar must be removed");
    }

    // #140/#129: a cache entry with no sidecar (old uvr, or crash between
    // download and sidecar write) is accepted once (trust-on-first-use), but
    // the sidecar must be backfilled so the entry is verified from then on —
    // it must not stay permanently exempt from checking.
    #[tokio::test]
    async fn cache_hit_without_sidecar_backfills_sidecar() {
        let tmp = tempfile::tempdir().unwrap();
        let bytes = b"cached without sidecar";
        let dest = seed_cache(tmp.path(), UNREACHABLE_URL, bytes);

        let got = run_download_one(tmp.path(), UNREACHABLE_URL, None)
            .await
            .expect("no-sidecar hit is accepted (TOFU)");
        assert_eq!(got, dest);

        let sidecar = dest.with_extension("sha256");
        let stored = std::fs::read_to_string(&sidecar).expect("sidecar must be backfilled");
        assert_eq!(stored.trim(), checksum::sha256_hex(bytes));

        // And the backfilled pin is enforced: tamper with the bytes and the
        // next hit must reject the entry instead of serving it.
        std::fs::write(&dest, b"tampered after backfill").unwrap();
        let result = run_download_one(tmp.path(), UNREACHABLE_URL, None).await;
        assert!(
            result.is_err(),
            "tampered entry must not be served after backfill"
        );
        assert!(!dest.exists());
        assert!(!sidecar.exists());
    }

    // #189: served bytes that differ from the lockfile's sha256 (a URL
    // dependency whose file changed) are a hard error, and nothing is cached.
    #[cfg(not(target_os = "windows"))]
    #[tokio::test]
    async fn sha256_mismatch_on_download_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let base = crate::registry::url::serve_for_test("200 OK", b"changed bytes".to_vec());
        let url = format!("{base}/pkg_1.0.0.tar.gz");
        let pinned = checksum::sha256_hex(b"original bytes");

        let err = run_download_one(tmp.path(), &url, Some(&pinned))
            .await
            .unwrap_err();
        match err {
            crate::error::UvrError::ChecksumMismatch {
                expected, actual, ..
            } => {
                assert_eq!(expected, pinned);
                assert_eq!(actual, checksum::sha256_hex(b"changed bytes"));
            }
            other => panic!("expected ChecksumMismatch, got {other}"),
        }
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
    }

    // #140: same backfill applies to git: entries — the branch that used to
    // say "accept it this time" must not accept it every time.
    #[tokio::test]
    async fn git_cache_hit_without_sidecar_backfills_sidecar() {
        let tmp = tempfile::tempdir().unwrap();
        let bytes = b"github tarball bytes";
        let dest = seed_cache(tmp.path(), UNREACHABLE_URL, bytes);

        let got = run_download_one(tmp.path(), UNREACHABLE_URL, Some("git:abc123def"))
            .await
            .expect("no-sidecar git hit is accepted (TOFU)");
        assert_eq!(got, dest);

        let stored = std::fs::read_to_string(dest.with_extension("sha256"))
            .expect("git sidecar must be backfilled");
        assert_eq!(stored.trim(), checksum::sha256_hex(bytes));
    }

    // #190: a `git::` package downloads as the archive of its locked
    // commit, keyed by commit (the clone URL is the same for all of them),
    // and a cached archive needs no fetch.
    #[tokio::test]
    async fn git_package_downloads_its_commit_once() {
        use crate::lockfile::{LockedPackage, PackageSource};
        use crate::registry::git_generic::TestRepo;

        let Some(repo) = TestRepo::new() else { return };
        let locked = |commit: Option<&str>| LockedPackage {
            name: "gitpkg".into(),
            version: "0.1.0".into(),
            source: PackageSource::Git {
                url: repo.url.clone(),
            },
            raw_version: None,
            url: None,
            checksum: commit.map(|c| format!("git:{c}")),
            subdirectory: None,
            requires: vec![],
            system_requirements: None,
            dev: false,
        };
        let cache = tempfile::tempdir().unwrap();
        let download = |pkg: LockedPackage| {
            let cache = cache.path().to_path_buf();
            let url = repo.url.clone();
            async move {
                super::Downloader::new(reqwest::Client::new(), cache, 1)
                    .download_all(&[super::DownloadSpec {
                        pkg: &pkg,
                        url: &url,
                        fallback_url: None,
                        is_binary: false,
                        user_agent: None,
                    }])
                    .await
                    .map(|mut results| results.remove(0))
            }
        };

        let first = download(locked(Some(&repo.first))).await.unwrap();
        assert!(!first.used_binary);
        let meta = crate::installer::binary_install::inspect_tarball(&first.path, "gitpkg");
        assert!(meta.is_some_and(|m| m.pure_r), "{}", first.path.display());
        let second = download(locked(Some(&repo.second))).await.unwrap();
        assert_ne!(first.path, second.path);

        std::fs::remove_dir_all(repo.dir.path()).unwrap();
        let again = download(locked(Some(&repo.first))).await.unwrap();
        assert_eq!(again.path, first.path);

        let err = download(locked(None)).await.err().unwrap().to_string();
        assert!(err.contains("no `git:<commit>` checksum"), "{err}");
    }

    // #122: the Linux collision. PPM serves a different-R-ABI binary at the
    // SAME url, selected by the R version in the User-Agent. Two R minors must
    // therefore get distinct cache entries even though url + basename match.
    #[test]
    fn cache_filename_distinguishes_user_agents_on_same_url() {
        let url = "https://packagemanager.posit.co/cran/__linux__/jammy/latest/src/contrib/data.table_1.18.4.tar.gz";
        let fb = "data.table_1.18.4.tar.gz";
        let k45 = cache_filename(url, Some("R (4.5.3 x86_64-pc-linux-gnu)"), fb);
        let k46 = cache_filename(url, Some("R (4.6.0 x86_64-pc-linux-gnu)"), fb);
        assert_ne!(
            k45, k46,
            "different UAs must not collide on one cache entry"
        );
        // The human-readable basename is preserved as a suffix.
        assert!(k45.ends_with("-data.table_1.18.4.tar.gz"));
    }

    #[test]
    fn cache_filename_is_stable_for_same_url_and_ua() {
        let url = "https://cran.r-project.org/src/contrib/curl_7.0.0.tar.gz";
        assert_eq!(
            cache_filename(url, None, "curl_7.0.0.tar.gz"),
            cache_filename(url, None, "curl_7.0.0.tar.gz"),
        );
    }

    #[test]
    fn cache_filename_strips_query_string() {
        // GitLab archive URLs carry `?sha=...` in the final path segment;
        // `?` is illegal in Windows filenames (os error 123) if left in.
        let url =
            "https://gitlab.example.com/api/v4/projects/42/repository/archive.tar.gz?sha=abc123";
        let name = cache_filename(url, None, "mypkg_0.1.0.tar.gz");
        assert!(
            !name.contains('?'),
            "cache filename must not contain '?': {name}"
        );
        assert!(name.ends_with("-archive.tar.gz"));
    }

    #[test]
    fn cache_filename_archive_url_keys_distinctly_from_primary() {
        // The CRAN-Archive retry fetches from a different URL than the primary;
        // that URL gets its own key. (download_one stores Archive bytes under
        // the *primary* key on retry — see cache_filename docs — but a direct
        // fetch of the Archive URL is correctly a distinct entry.)
        let primary = cache_filename(
            "https://cran.r-project.org/src/contrib/curl_7.0.0.tar.gz",
            None,
            "curl_7.0.0.tar.gz",
        );
        let archive = cache_filename(
            "https://cran.r-project.org/src/contrib/Archive/curl/curl_7.0.0.tar.gz",
            None,
            "curl_7.0.0.tar.gz",
        );
        assert_ne!(primary, archive);
    }

    #[test]
    fn cache_filename_distinguishes_source_from_binary() {
        // Source .tar.gz and binary .tgz of the same package (distinct URLs)
        // get distinct entries — preserves the pre-#122 collision guarantee.
        let src = cache_filename(
            "https://cran.r-project.org/src/contrib/ggplot2_3.5.1.tar.gz",
            None,
            "ggplot2_3.5.1.tar.gz",
        );
        let bin = cache_filename(
            "https://p3m.dev/cran/latest/bin/macosx/big-sur-arm64/contrib/4.5/ggplot2_3.5.1.tgz",
            None,
            "ggplot2_3.5.1.tar.gz",
        );
        assert_ne!(src, bin);
        assert!(src.ends_with("-ggplot2_3.5.1.tar.gz"));
        assert!(bin.ends_with("-ggplot2_3.5.1.tgz"));
    }

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

    // ── authenticated repositories (#185) ──────────────────────────────

    #[cfg(not(target_os = "windows"))]
    mod auth {
        use super::super::{download_one, DownloadSpec, Downloader};
        use crate::auth::{test_authorization, test_response, test_server, Credential, Repository};
        use crate::lockfile::{LockedPackage, PackageSource};

        fn repo(url: &str, credential: Option<Credential>) -> Repository {
            Repository {
                name: "private".into(),
                url: url.into(),
                credential,
            }
        }

        fn bearer() -> Option<Credential> {
            Some(Credential::Bearer("tok123".into()))
        }

        /// A repository that serves `bytes` only to `Bearer tok123`.
        fn private_server() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
            test_server(|head| {
                if test_authorization(head) == Some("Bearer tok123") {
                    test_response("200 OK", "", b"tarball bytes")
                } else {
                    test_response("401 Unauthorized", "WWW-Authenticate: Bearer\r\n", b"")
                }
            })
        }

        async fn fetch(url: &str, repos: &[Repository]) -> crate::error::Result<()> {
            let tmp = tempfile::tempdir().unwrap();
            download_one(
                &reqwest::Client::new(),
                tmp.path(),
                "a",
                "1.0",
                url,
                None,
                None,
                None,
                repos,
                &indicatif::MultiProgress::new(),
            )
            .await
            .map(drop)
        }

        #[tokio::test]
        async fn credential_goes_to_the_repository_and_nowhere_else() {
            let (private, private_seen) = private_server();
            let (public, public_seen) = test_server(|_| test_response("404 Not Found", "", b""));
            let pkg = LockedPackage {
                name: "a".into(),
                version: "1.0".into(),
                source: PackageSource::Custom {
                    name: "private".into(),
                },
                raw_version: None,
                url: None,
                checksum: None,
                subdirectory: None,
                requires: vec![],
                system_requirements: None,
                dev: false,
            };
            // A binary from another host that 404s, then the source
            // fallback from the private repository.
            let binary = format!("{public}/bin/a_1.0.tgz");
            let source = format!("{private}/cran/src/contrib/a_1.0.tar.gz");
            let tmp = tempfile::tempdir().unwrap();
            let results = Downloader::new(reqwest::Client::new(), tmp.path().to_path_buf(), 1)
                .with_repositories(vec![repo(&format!("{private}/cran"), bearer())])
                .download_all(&[DownloadSpec {
                    pkg: &pkg,
                    url: &binary,
                    fallback_url: Some(&source),
                    is_binary: true,
                    user_agent: None,
                }])
                .await
                .expect("the fallback download carries the repository credential");
            assert!(!results[0].used_binary);
            assert_eq!(std::fs::read(&results[0].path).unwrap(), b"tarball bytes");

            let public_seen = public_seen.lock().unwrap();
            assert_eq!(public_seen.len(), 1);
            assert_eq!(test_authorization(&public_seen[0]), None, "{public_seen:?}");
            let private_seen = private_seen.lock().unwrap();
            assert_eq!(test_authorization(&private_seen[0]), Some("Bearer tok123"));
        }

        // #187: sync used to send a Forgejo/GitLab token to the plan's
        // primary URL, whatever its host: a P3M or custom-source binary for
        // a package of the same name got the token. Now a git host's token
        // goes only to URLs on that host, the source fallback included.
        #[test]
        fn git_host_token_stays_on_its_host() {
            use crate::auth::{test_git_origin, test_private_git_host, GitEnv};

            let _env = GitEnv::new(&["UVR_FORGEJO_TOKEN_FORGEJO_TEST"]);
            std::env::set_var("UVR_FORGEJO_TOKEN_FORGEJO_TEST", "fj-tok");
            let (forgejo, forgejo_seen) = test_private_git_host("token fj-tok", |_| {
                test_response("200 OK", "", b"forgejo tarball")
            });
            let (binary, binary_seen) = test_server(|_| test_response("404 Not Found", "", b""));
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            test_git_origin("forgejo.test", &forgejo);

            let pkg = LockedPackage {
                name: "a".into(),
                version: "1.0".into(),
                source: PackageSource::Forgejo {
                    host: "forgejo.test".into(),
                },
                raw_version: None,
                url: None,
                checksum: Some("git:abc".into()),
                subdirectory: None,
                requires: vec![],
                system_requirements: None,
                dev: false,
            };
            let binary_url = format!("{binary}/bin/a_1.0.tgz");
            let archive_url = format!("{forgejo}/api/v1/repos/o/a/archive/abc.tar.gz");
            let tmp = tempfile::tempdir().unwrap();
            let download = |url: &str, fallback_url: Option<&str>, is_binary: bool| {
                rt.block_on(
                    Downloader::new(reqwest::Client::new(), tmp.path().to_path_buf(), 1)
                        .download_all(&[DownloadSpec {
                            pkg: &pkg,
                            url,
                            fallback_url,
                            is_binary,
                            user_agent: None,
                        }]),
                )
                .map(|mut results| results.remove(0))
            };

            // A binary on another host, then the source on the Forgejo host.
            let result = download(&binary_url, Some(&archive_url), true)
                .expect("the fallback download carries the Forgejo token");
            assert!(!result.used_binary);
            assert_eq!(std::fs::read(&result.path).unwrap(), b"forgejo tarball");
            // A locked URL on another host gets no token either.
            let err = download(
                &format!("{binary}/api/v1/repos/o/a/archive/x.tar.gz"),
                None,
                false,
            )
            .err()
            .expect("another host does not serve the package")
            .to_string();
            assert!(!err.contains("fj-tok"), "{err}");

            let binary_seen = binary_seen.lock().unwrap();
            assert_eq!(binary_seen.len(), 2);
            for head in binary_seen.iter() {
                assert_eq!(test_authorization(head), None, "{head}");
            }
            let forgejo_seen = forgejo_seen.lock().unwrap();
            assert_eq!(forgejo_seen.len(), 1);
            assert_eq!(test_authorization(&forgejo_seen[0]), Some("token fj-tok"));
            drop(forgejo_seen);

            // A refused token names the variable, not a repository one.
            std::env::set_var("UVR_FORGEJO_TOKEN_FORGEJO_TEST", "wrong-tok");
            let other = format!("{forgejo}/api/v1/repos/o/a/archive/def.tar.gz");
            let err = download(&other, None, false)
                .err()
                .expect("a wrong token is refused")
                .to_string();
            assert!(
                err.contains("Forgejo host forgejo.test returned HTTP 401 Unauthorized")
                    && err.contains("refused the token in UVR_FORGEJO_TOKEN_FORGEJO_TEST"),
                "{err}"
            );
            assert!(
                !err.contains("wrong-tok") && !err.contains("UVR_REPO_"),
                "{err}"
            );
        }

        #[tokio::test]
        async fn basic_auth_reaches_the_repository() {
            let (url, seen) = test_server(|head| match test_authorization(head) {
                // base64("alice:s3cret")
                Some("Basic YWxpY2U6czNjcmV0") => test_response("200 OK", "", b"ok"),
                _ => test_response("401 Unauthorized", "", b""),
            });
            let basic = Credential::Basic {
                username: "alice".into(),
                password: "s3cret".into(),
            };
            fetch(
                &format!("{url}/src/contrib/a_1.0.tar.gz"),
                &[repo(&url, Some(basic))],
            )
            .await
            .expect("basic auth accepted");
            assert_eq!(seen.lock().unwrap().len(), 1);
        }

        // reqwest 0.12 drops `Authorization` when a redirect changes host or
        // port; this pins that behaviour for repository credentials.
        #[tokio::test]
        async fn redirect_to_another_host_drops_the_credential() {
            let (cdn, cdn_seen) = test_server(|_| test_response("200 OK", "", b"from cdn"));
            let location = format!("Location: {cdn}/blob/a_1.0.tar.gz\r\n");
            let (private, private_seen) =
                test_server(move |_| test_response("302 Found", &location, b""));

            fetch(
                &format!("{private}/a_1.0.tar.gz"),
                &[repo(&private, bearer())],
            )
            .await
            .expect("redirect followed");
            let private_seen = private_seen.lock().unwrap();
            assert_eq!(test_authorization(&private_seen[0]), Some("Bearer tok123"));
            let cdn_seen = cdn_seen.lock().unwrap();
            assert_eq!(cdn_seen.len(), 1);
            assert_eq!(test_authorization(&cdn_seen[0]), None, "{cdn_seen:?}");
        }

        #[tokio::test]
        async fn refusal_names_the_repository_and_the_variable() {
            let (url, seen) = private_server();
            let tarball = format!("{url}/src/contrib/a_1.0.tar.gz");

            // No credential: say which variable to set, even though the
            // CRAN-Archive retry is what failed last.
            let err = fetch(&tarball, &[repo(&url, None)])
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("repository 'private'"), "{err}");
            assert!(err.contains("401 Unauthorized"), "{err}");
            assert!(err.contains("UVR_REPO_TOKEN_PRIVATE"), "{err}");
            assert_eq!(seen.lock().unwrap().len(), 2, "primary + Archive retry");

            // A wrong token: say it was refused, never print it.
            let wrong = Some(Credential::Bearer("wrong-token".into()));
            let err = fetch(&tarball, &[repo(&url, wrong)])
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("refused the token"), "{err}");
            assert!(!err.contains("wrong-token"), "{err}");
            // The Archive retry is on the same repository, so it had the token too.
            let seen = seen.lock().unwrap();
            assert_eq!(test_authorization(&seen[3]), Some("Bearer wrong-token"));
        }
    }
}
