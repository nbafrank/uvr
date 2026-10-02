use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};

use uvr_core::installer::package_cache::{self, dir_size};
use uvr_core::r_version::detector;

use crate::ui;

/// `uvr cache clean [--package <name>] [--r-version <minor>]`.
///
/// With no filters, wipes the tarball download cache and the global
/// extracted-package cache entirely. With filters, removes only the entries
/// that provably match every given filter.
pub fn run_clean(packages: &[String], r_versions: &[String]) -> Result<()> {
    if packages.is_empty() && r_versions.is_empty() {
        run_clean_all()
    } else {
        run_clean_filtered(packages, r_versions)
    }
}

fn run_clean_all() -> Result<()> {
    let cache_dir = uvr_core::env_vars::cache_dir()
        .unwrap_or_else(|| std::path::PathBuf::from(".uvr").join("cache"));

    let mut count = 0u64;
    let mut bytes = 0u64;

    // Clean tarball download cache (may also contain subdirectories such as
    // `with-envs/` created by `uvr run --with`).
    if cache_dir.exists() {
        let (removed, removed_bytes, failed) = remove_cache_entries(&cache_dir)?;
        count += removed;
        bytes += removed_bytes;
        for (path, err) in &failed {
            ui::warn(format!("Failed to remove {}: {err}", path.display()));
        }
    }

    // Clean global package cache
    let packages_dir = package_cache::global_packages_dir();
    if packages_dir.exists() {
        let (pkg_count, pkg_bytes) = package_cache::cache_stats();
        count += pkg_count;
        bytes += pkg_bytes;
        let _ = std::fs::remove_dir_all(&packages_dir);
    }

    if count == 0 {
        ui::success("Cache is already empty");
    } else {
        ui::success(format!(
            "Cleared {count} item(s) ({}) from cache",
            ui::palette::format_bytes(bytes)
        ));
    }
    Ok(())
}

fn run_clean_filtered(packages: &[String], r_versions: &[String]) -> Result<()> {
    // Packages are cached per R *minor* version: normalize "4.5.3" → "4.5".
    let r_minors: Vec<String> = r_versions.iter().map(|v| normalize_r_minor(v)).collect();
    for (given, minor) in r_versions.iter().zip(&r_minors) {
        if given != minor {
            ui::warn(format!(
                "Packages are cached by R minor version; treating {given} as {minor}"
            ));
        }
    }

    let mut count = 0u64;
    let mut bytes = 0u64;

    // Tarball download cache: filenames embed `<name>_<version>`, so only the
    // package filter can apply — a tarball's R version is not recoverable from
    // its name. When --r-version is also given, tarballs are left alone: a
    // filtered clean only removes what provably matches every filter.
    if !packages.is_empty() && r_minors.is_empty() {
        let cache_dir = uvr_core::env_vars::cache_dir()
            .unwrap_or_else(|| std::path::PathBuf::from(".uvr").join("cache"));
        if cache_dir.exists() {
            let (removed, removed_bytes, failed) = remove_matching_tarballs(&cache_dir, packages)?;
            count += removed;
            bytes += removed_bytes;
            for (path, err) in &failed {
                ui::warn(format!("Failed to remove {}: {err}", path.display()));
            }
        }
    }

    // Global extracted-package cache.
    let mut legacy_skipped = 0u64;
    let packages_dir = package_cache::global_packages_dir();
    if packages_dir.exists() {
        let outcome = remove_matching_package_entries(&packages_dir, packages, &r_minors)?;
        count += outcome.removed;
        bytes += outcome.removed_bytes;
        legacy_skipped = outcome.legacy_skipped;
        for (path, err) in &outcome.failed {
            ui::warn(format!("Failed to remove {}: {err}", path.display()));
        }
    }

    let mut filter_desc: Vec<String> = Vec::new();
    if !packages.is_empty() {
        filter_desc.push(format!("packages: {}", packages.join(", ")));
    }
    if !r_minors.is_empty() {
        filter_desc.push(format!("R versions: {}", r_minors.join(", ")));
    }
    let filter_desc = filter_desc.join("; ");

    if count == 0 {
        ui::info(format!("No cache entries matched {filter_desc}"));
    } else {
        let noun = if count == 1 { "entry" } else { "entries" };
        ui::success(format!(
            "Cleared {count} {noun} ({}) matching {filter_desc}",
            ui::palette::format_bytes(bytes)
        ));
    }
    if legacy_skipped > 0 {
        let noun = if legacy_skipped == 1 {
            "entry"
        } else {
            "entries"
        };
        ui::info(format!(
            "Left {legacy_skipped} legacy {noun} without R-version metadata untouched \
             (created by an older uvr; use --package or a full clean to remove them)"
        ));
    }
    Ok(())
}

/// Reduce an R version to its minor series ("4.5.3" → "4.5"). Values without
/// at least three dot-separated components are returned unchanged.
fn normalize_r_minor(version: &str) -> String {
    let mut parts = version.split('.');
    match (parts.next(), parts.next()) {
        (Some(major), Some(minor)) if parts.next().is_some() => format!("{major}.{minor}"),
        _ => version.to_string(),
    }
}

/// Entries that could not be removed, with the error for each path.
type RemovalFailures = Vec<(PathBuf, std::io::Error)>;

/// Remove every entry in `cache_dir`, using `remove_dir_all` for directories
/// and `remove_file` for everything else (symlinks are unlinked, not followed).
///
/// Returns `(removed_count, removed_bytes, failures)`; only entries that were
/// actually removed are counted.
fn remove_cache_entries(cache_dir: &Path) -> Result<(u64, u64, RemovalFailures)> {
    let mut count = 0u64;
    let mut bytes = 0u64;
    let mut failed = Vec::new();

    for entry in std::fs::read_dir(cache_dir)
        .with_context(|| format!("Cannot read cache dir {}", cache_dir.display()))?
        .flatten()
    {
        let path = entry.path();
        // DirEntry::file_type does not follow symlinks, so a symlink to a
        // directory is treated as a file and unlinked rather than traversed.
        let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
        let entry_bytes = if is_dir {
            dir_size(&path)
        } else {
            entry.metadata().map(|m| m.len()).unwrap_or(0)
        };
        let result = if is_dir {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        match result {
            Ok(()) => {
                count += 1;
                bytes += entry_bytes;
            }
            Err(err) => failed.push((path, err)),
        }
    }

    Ok((count, bytes, failed))
}

/// Remove flat files in the tarball download cache whose name matches one of
/// `packages`, including their `.sha256` sidecars. Directories (e.g.
/// `with-envs/` from `uvr run --with`) are never touched by a filtered clean.
///
/// Returns `(removed_count, removed_bytes, failures)`.
fn remove_matching_tarballs(
    cache_dir: &Path,
    packages: &[String],
) -> Result<(u64, u64, RemovalFailures)> {
    let mut count = 0u64;
    let mut bytes = 0u64;
    let mut failed = Vec::new();

    for entry in std::fs::read_dir(cache_dir)
        .with_context(|| format!("Cannot read cache dir {}", cache_dir.display()))?
        .flatten()
    {
        // DirEntry::file_type does not follow symlinks, so a symlink to a
        // directory counts as a file — but it can only be removed if its
        // *name* matches a requested package tarball.
        if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
            continue;
        }
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if !tarball_matches_package(name, packages) {
            continue;
        }
        let path = entry.path();
        let entry_bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
        match std::fs::remove_file(&path) {
            Ok(()) => {
                count += 1;
                bytes += entry_bytes;
            }
            Err(err) => failed.push((path, err)),
        }
    }

    Ok((count, bytes, failed))
}

/// Whether a cached tarball (or its `.sha256` sidecar) belongs to one of
/// `packages`. Cache filenames are `<8-hex-tag>-<basename>` (see
/// `cache_filename` in uvr-core's `installer::download`), where the basename
/// follows R's `<name>_<version>.<ext>` convention; entries from older uvr
/// versions may be the bare basename. Package names cannot contain `_`, so a
/// `<name>_` prefix match is exact. Sidecars keep the `<name>_` prefix
/// (`with_extension` only swaps the trailing extension), so they match by the
/// same rule. R runtime tarballs (`R-4.4.1.tar.gz`) never match.
fn tarball_matches_package(filename: &str, packages: &[String]) -> bool {
    let basename = match filename.split_once('-') {
        Some((tag, rest)) if tag.len() == 8 && tag.chars().all(|c| c.is_ascii_hexdigit()) => rest,
        _ => filename,
    };
    packages.iter().any(|pkg| {
        basename
            .strip_prefix(pkg.as_str())
            .is_some_and(|rest| rest.starts_with('_'))
    })
}

/// Result of a filtered pass over the global extracted-package cache.
#[derive(Default)]
struct PackageCleanOutcome {
    removed: u64,
    removed_bytes: u64,
    /// Entries that matched the package filter but could not be attributed to
    /// an R version (no metadata — created by an older uvr) while an
    /// --r-version filter was active. Left untouched.
    legacy_skipped: u64,
    failed: RemovalFailures,
}

/// Remove entries in the global extracted-package cache that match every
/// active filter. Entry directory names are `<name>-<version>-<hash>` cache
/// keys; the R minor comes from the metadata file `package_cache::store`
/// writes inside each entry. Anything that is not a parseable cache entry
/// (stray files, temp staging dirs) is left alone in filtered mode.
fn remove_matching_package_entries(
    packages_dir: &Path,
    packages: &[String],
    r_minors: &[String],
) -> Result<PackageCleanOutcome> {
    let mut outcome = PackageCleanOutcome::default();

    for entry in std::fs::read_dir(packages_dir)
        .with_context(|| format!("Cannot read package cache dir {}", packages_dir.display()))?
        .flatten()
    {
        if !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
            continue;
        }
        let file_name = entry.file_name();
        let Some(key) = file_name.to_str() else {
            continue;
        };
        let Some(name) = package_cache::package_name_from_key(key) else {
            continue;
        };
        if !packages.is_empty() && !packages.iter().any(|p| p == name) {
            continue;
        }
        let path = entry.path();
        if !r_minors.is_empty() {
            match package_cache::read_entry_meta(&path) {
                Some(meta) if r_minors.contains(&meta.r_minor) => {}
                Some(_) => continue,
                None => {
                    outcome.legacy_skipped += 1;
                    continue;
                }
            }
        }
        let entry_bytes = dir_size(&path);
        match std::fs::remove_dir_all(&path) {
            Ok(()) => {
                outcome.removed += 1;
                outcome.removed_bytes += entry_bytes;
            }
            Err(err) => outcome.failed.push((path, err)),
        }
    }

    Ok(outcome)
}

/// A leftover younger than this can belong to a `uvr sync` that is still
/// running: a live download or cache store looks the same as a crashed one.
const ORPHAN_MIN_AGE: Duration = Duration::from_secs(60 * 60);

/// Why `uvr cache prune` removes an entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PruneReason {
    /// Left behind by an interrupted download or cache store.
    Orphan,
    /// Extracted package built for an R minor version that is not installed.
    StaleR,
    /// Raw download cache file (only with `--ci`).
    Download,
}

impl PruneReason {
    fn label(self) -> &'static str {
        match self {
            PruneReason::Orphan => "Leftovers of interrupted installs",
            PruneReason::StaleR => "Extracted packages for R versions that are not installed",
            PruneReason::Download => "Raw downloads (--ci)",
        }
    }
}

/// One cache entry that `uvr cache prune` removes.
#[derive(Debug)]
struct PruneEntry {
    reason: PruneReason,
    path: PathBuf,
    bytes: u64,
}

/// What a scan of the two caches found.
#[derive(Debug, Default)]
struct PrunePlan {
    entries: Vec<PruneEntry>,
    /// Extracted-package entries without R-version metadata (created by an
    /// older uvr). Their R version is unknown, so they are kept.
    legacy_skipped: u64,
    /// Temp files and staging dirs younger than the age limit. A running
    /// sync can own them, so they are kept.
    young_skipped: u64,
}

/// `uvr cache prune [--dry-run] [--ci]`.
///
/// Removes only entries that no sync can use: leftovers of interrupted
/// downloads and cache stores, and extracted packages built for an R minor
/// version that uvr cannot find on this machine. `--ci` also removes the raw
/// download cache files. Reports the count and bytes for each category.
pub fn run_prune(dry_run: bool, ci: bool) -> Result<()> {
    let mut installed: Vec<String> = detector::find_all()
        .iter()
        .map(|r| normalize_r_minor(&r.version))
        .collect();
    installed.sort();
    installed.dedup();
    if installed.is_empty() {
        // With no R found, every extract would look stale.
        ui::warn(
            "No R installation found; keeping all extracted packages. \
             Install R (or load its module), then prune again to remove extracts for old R versions",
        );
    } else {
        ui::info(format!(
            "Installed R: {} (extracted packages for other R versions are unused)",
            installed.join(", ")
        ));
    }

    let plan = plan_prune(
        &uvr_core::env_vars::cache_dir_or_temp(),
        &package_cache::global_packages_dir(),
        &installed,
        ci,
        ORPHAN_MIN_AGE,
    )?;

    let mut reasons = vec![PruneReason::Orphan];
    if !installed.is_empty() {
        reasons.push(PruneReason::StaleR);
    }
    if ci {
        reasons.push(PruneReason::Download);
    }

    let (entries, failed) = if dry_run {
        (plan.entries, RemovalFailures::new())
    } else {
        remove_planned(plan.entries)
    };
    for (path, err) in &failed {
        ui::warn(format!("Failed to remove {}: {err}", path.display()));
    }
    if entries.is_empty() && !failed.is_empty() {
        let noun = if failed.len() == 1 {
            "entry"
        } else {
            "entries"
        };
        anyhow::bail!(
            "Could not remove {} cache {noun}; see the warnings above",
            failed.len()
        );
    }

    let bytes: u64 = entries.iter().map(|e| e.bytes).sum();
    let summary = format!(
        "{} cache {} ({})",
        entries.len(),
        if entries.len() == 1 {
            "entry"
        } else {
            "entries"
        },
        ui::palette::format_bytes(bytes)
    );
    if dry_run {
        ui::info(format!("Dry run: would prune {summary}"));
    } else {
        ui::success(format!("Pruned {summary}"));
    }
    for reason in reasons {
        let (count, bytes) = tally(&entries, reason);
        ui::bullet(format!(
            "{}: {count} ({})",
            reason.label(),
            ui::palette::format_bytes(bytes)
        ));
    }
    if dry_run && !entries.is_empty() {
        ui::info("Would remove:");
        for entry in &entries {
            ui::bullet_dim(format!(
                "{} ({})",
                entry.path.display(),
                ui::palette::format_bytes(entry.bytes)
            ));
        }
    }
    if plan.legacy_skipped > 0 {
        let noun = if plan.legacy_skipped == 1 {
            "entry"
        } else {
            "entries"
        };
        ui::info(format!(
            "Kept {} legacy {noun} without R-version metadata \
             (created by an older uvr; use `uvr cache clean --package` or a full clean to remove them)",
            plan.legacy_skipped
        ));
    }
    if plan.young_skipped > 0 {
        ui::info(format!(
            "Kept {} leftover(s) changed less than an hour ago (a running sync can still use them)",
            plan.young_skipped
        ));
    }
    Ok(())
}

/// Count and total bytes of the `entries` with `reason`.
fn tally(entries: &[PruneEntry], reason: PruneReason) -> (u64, u64) {
    entries
        .iter()
        .filter(|e| e.reason == reason)
        .fold((0, 0), |(count, bytes), e| (count + 1, bytes + e.bytes))
}

/// Find what `uvr cache prune` removes, without removing anything.
///
/// Download cache (top-level regular files only; directories such as
/// `with-envs/` and symlinks are never candidates):
/// - `.uvr-dl-*` files: partial downloads (`download_one`'s temp files);
/// - `*.sha256` sidecars whose tarball is gone;
/// - with `ci`, every other file too.
///
/// Extracted-package cache (real directories only):
/// - `.tmp*` directories: staging dirs of an interrupted
///   `package_cache::store` (tempfile's default prefix);
/// - cache entries whose recorded R minor is not in `installed_r_minors`.
///   An empty `installed_r_minors` means no R was found, and then no entry
///   counts as stale (otherwise every entry would).
///
/// Temp files and staging dirs younger than `min_age` are kept: they can
/// belong to a sync that is still running.
fn plan_prune(
    cache_dir: &Path,
    packages_dir: &Path,
    installed_r_minors: &[String],
    ci: bool,
    min_age: Duration,
) -> Result<PrunePlan> {
    let mut plan = PrunePlan::default();

    if cache_dir.exists() {
        let files: Vec<(PathBuf, std::fs::Metadata)> = std::fs::read_dir(cache_dir)
            .with_context(|| format!("Cannot read cache dir {}", cache_dir.display()))?
            .flatten()
            // DirEntry::file_type does not follow symlinks.
            .filter(|e| e.file_type().is_ok_and(|ft| ft.is_file()))
            .filter_map(|e| Some((e.path(), e.metadata().ok()?)))
            .collect();
        // `download_one` names a sidecar `<tarball>.with_extension("sha256")`.
        let is_sidecar = |p: &Path| p.extension().is_some_and(|ext| ext == "sha256");
        let live_sidecars: HashSet<PathBuf> = files
            .iter()
            .filter(|(p, _)| !is_sidecar(p))
            .map(|(p, _)| p.with_extension("sha256"))
            .collect();
        for (path, meta) in files {
            let is_partial = path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(".uvr-dl-"));
            let reason = if is_partial {
                if !is_older_than(&meta, min_age) {
                    plan.young_skipped += 1;
                    continue;
                }
                PruneReason::Orphan
            } else if is_sidecar(&path) && !live_sidecars.contains(&path) {
                PruneReason::Orphan
            } else if ci {
                PruneReason::Download
            } else {
                continue;
            };
            plan.entries.push(PruneEntry {
                reason,
                bytes: meta.len(),
                path,
            });
        }
    }

    if packages_dir.exists() {
        for entry in std::fs::read_dir(packages_dir)
            .with_context(|| format!("Cannot read package cache dir {}", packages_dir.display()))?
            .flatten()
        {
            if !entry.file_type().is_ok_and(|ft| ft.is_dir()) {
                continue;
            }
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            let path = entry.path();
            let reason = if name.starts_with(".tmp") {
                if !entry.metadata().is_ok_and(|m| is_older_than(&m, min_age)) {
                    plan.young_skipped += 1;
                    continue;
                }
                PruneReason::Orphan
            } else if !installed_r_minors.is_empty()
                && package_cache::package_name_from_key(name).is_some()
            {
                match package_cache::read_entry_meta(&path) {
                    Some(meta) if !installed_r_minors.contains(&meta.r_minor) => {
                        PruneReason::StaleR
                    }
                    Some(_) => continue,
                    None => {
                        plan.legacy_skipped += 1;
                        continue;
                    }
                }
            } else {
                continue;
            };
            plan.entries.push(PruneEntry {
                reason,
                bytes: dir_size(&path),
                path,
            });
        }
    }

    Ok(plan)
}

/// Whether `meta` was last modified at least `age` ago. A modification time
/// in the future counts as new.
fn is_older_than(meta: &std::fs::Metadata, age: Duration) -> bool {
    meta.modified()
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|elapsed| elapsed >= age)
}

/// Remove the planned entries. Symlinks are unlinked, never followed:
/// `symlink_metadata` does not follow one, and `remove_dir_all` does not
/// follow the ones inside a tree. Returns the entries that were removed and
/// the failures.
fn remove_planned(entries: Vec<PruneEntry>) -> (Vec<PruneEntry>, RemovalFailures) {
    let mut removed = Vec::new();
    let mut failed = Vec::new();
    for entry in entries {
        let is_dir = std::fs::symlink_metadata(&entry.path).is_ok_and(|m| m.is_dir());
        let result = if is_dir {
            std::fs::remove_dir_all(&entry.path)
        } else {
            std::fs::remove_file(&entry.path)
        };
        match result {
            Ok(()) => removed.push(entry),
            Err(err) => failed.push((entry.path, err)),
        }
    }
    (removed, failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_files_and_directories_and_counts_only_removed() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path();

        // Plain file (like a cached tarball).
        std::fs::write(cache.join("R-4.4.1.tar.gz"), b"tarball").unwrap();

        // Subdirectory with nested content (like with-envs/ from `uvr run --with`).
        let with_envs = cache.join("with-envs");
        std::fs::create_dir_all(with_envs.join("abc123").join("library")).unwrap();
        std::fs::write(with_envs.join("abc123").join("lockfile"), b"nested file").unwrap();

        let (count, bytes, failed) = remove_cache_entries(cache).unwrap();

        assert_eq!(count, 2, "one file + one directory entry");
        // "tarball" (7) + "nested file" (11)
        assert_eq!(bytes, 18);
        assert!(failed.is_empty(), "unexpected failures: {failed:?}");
        assert!(
            std::fs::read_dir(cache).unwrap().next().is_none(),
            "cache dir should be empty"
        );
        assert!(!with_envs.exists(), "with-envs directory should be gone");
    }

    #[test]
    fn empty_cache_dir_reports_zero() {
        let dir = tempfile::tempdir().unwrap();
        let (count, bytes, failed) = remove_cache_entries(dir.path()).unwrap();
        assert_eq!(count, 0);
        assert_eq!(bytes, 0);
        assert!(failed.is_empty());
    }

    #[test]
    fn normalize_r_minor_truncates_patch_versions() {
        assert_eq!(normalize_r_minor("4.5.3"), "4.5");
        assert_eq!(normalize_r_minor("4.5"), "4.5");
        assert_eq!(normalize_r_minor("4"), "4");
        // Odd inputs pass through unchanged (they simply won't match anything).
        assert_eq!(normalize_r_minor("devel"), "devel");
    }

    #[test]
    fn tarball_matching_by_package_name() {
        let pkgs = vec!["rlang".to_string(), "data.table".to_string()];
        // Hash-tagged filenames (current format).
        assert!(tarball_matches_package(
            "abcd1234-rlang_1.1.4.tar.gz",
            &pkgs
        ));
        assert!(tarball_matches_package(
            "abcd1234-rlang_1.1.4.tar.sha256",
            &pkgs
        ));
        assert!(tarball_matches_package(
            "00ff00ff-data.table_1.15.4.tgz",
            &pkgs
        ));
        // Bare basenames (legacy format).
        assert!(tarball_matches_package("rlang_1.1.4.tar.gz", &pkgs));
        // Prefix must be exact up to the underscore: rlang2 is a different package.
        assert!(!tarball_matches_package(
            "abcd1234-rlang2_1.0.0.tar.gz",
            &pkgs
        ));
        // Other packages and R runtime tarballs never match.
        assert!(!tarball_matches_package(
            "abcd1234-jsonlite_1.8.8.tgz",
            &pkgs
        ));
        assert!(!tarball_matches_package("R-4.4.1.tar.gz", &pkgs));
    }

    #[test]
    fn filtered_tarball_clean_removes_only_matching_files() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path();

        std::fs::write(cache.join("abcd1234-rlang_1.1.4.tar.gz"), b"rlang").unwrap();
        std::fs::write(cache.join("abcd1234-rlang_1.1.4.tar.sha256"), b"cksum").unwrap();
        std::fs::write(cache.join("ffff0000-jsonlite_1.8.8.tgz"), b"jsonlite").unwrap();
        std::fs::write(cache.join("R-4.4.1.tar.gz"), b"runtime").unwrap();
        // Subdirectory (like with-envs/) must survive a filtered clean.
        let with_envs = cache.join("with-envs");
        std::fs::create_dir_all(with_envs.join("abc123")).unwrap();
        std::fs::write(with_envs.join("abc123").join("lockfile"), b"nested").unwrap();

        let (count, bytes, failed) =
            remove_matching_tarballs(cache, &["rlang".to_string()]).unwrap();

        assert_eq!(count, 2, "tarball + sha256 sidecar");
        assert_eq!(bytes, 10); // "rlang" (5) + "cksum" (5)
        assert!(failed.is_empty(), "unexpected failures: {failed:?}");
        assert!(!cache.join("abcd1234-rlang_1.1.4.tar.gz").exists());
        assert!(!cache.join("abcd1234-rlang_1.1.4.tar.sha256").exists());
        assert!(cache.join("ffff0000-jsonlite_1.8.8.tgz").exists());
        assert!(cache.join("R-4.4.1.tar.gz").exists());
        assert!(with_envs.join("abc123").join("lockfile").exists());
    }

    /// Create a fake extracted-package cache entry `<name>-<version>-<hash32>`
    /// in `packages_dir`, optionally with an R-version metadata file.
    fn make_cache_entry(
        packages_dir: &Path,
        name: &str,
        version: &str,
        hash: &str,
        r_minor: Option<&str>,
    ) -> PathBuf {
        let entry = packages_dir.join(format!("{name}-{version}-{hash}"));
        std::fs::create_dir_all(entry.join(name)).unwrap();
        std::fs::write(
            entry.join(name).join("DESCRIPTION"),
            format!("Package: {name}\n"),
        )
        .unwrap();
        if let Some(minor) = r_minor {
            std::fs::write(
                entry.join(package_cache::ENTRY_META_FILENAME),
                format!("r_minor={minor}\nkind=binary\n"),
            )
            .unwrap();
        }
        entry
    }

    const HEX32: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn filtered_package_cache_clean_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let packages_dir = dir.path();

        let rlang = make_cache_entry(packages_dir, "rlang", "1.1.4", HEX32, Some("4.5"));
        let jsonlite = make_cache_entry(packages_dir, "jsonlite", "1.8.8", HEX32, None);
        // Non-entry clutter must survive: stray file + unparseable dir name.
        std::fs::write(packages_dir.join("junk.txt"), b"junk").unwrap();
        std::fs::create_dir_all(packages_dir.join(".tmpStaging")).unwrap();

        let outcome =
            remove_matching_package_entries(packages_dir, &["rlang".to_string()], &[]).unwrap();

        assert_eq!(outcome.removed, 1);
        assert_eq!(outcome.legacy_skipped, 0);
        assert!(outcome.failed.is_empty());
        assert!(!rlang.exists());
        assert!(jsonlite.exists());
        assert!(packages_dir.join("junk.txt").exists());
        assert!(packages_dir.join(".tmpStaging").exists());
    }

    #[test]
    fn filtered_package_cache_clean_by_r_version_skips_legacy_entries() {
        let dir = tempfile::tempdir().unwrap();
        let packages_dir = dir.path();

        let on_45 = make_cache_entry(packages_dir, "pkga", "1.0", HEX32, Some("4.5"));
        let on_44 = make_cache_entry(packages_dir, "pkgb", "1.0", HEX32, Some("4.4"));
        let legacy = make_cache_entry(packages_dir, "pkgc", "1.0", HEX32, None);

        let outcome =
            remove_matching_package_entries(packages_dir, &[], &["4.5".to_string()]).unwrap();

        assert_eq!(outcome.removed, 1);
        assert_eq!(outcome.legacy_skipped, 1, "no-metadata entry is left alone");
        assert!(outcome.failed.is_empty());
        assert!(!on_45.exists());
        assert!(on_44.exists(), "other R minor must survive");
        assert!(legacy.exists(), "legacy entry must survive");
    }

    #[test]
    fn filtered_package_cache_clean_combines_name_and_r_version() {
        let dir = tempfile::tempdir().unwrap();
        let packages_dir = dir.path();

        let rlang_45 = make_cache_entry(packages_dir, "rlang", "1.1.4", HEX32, Some("4.5"));
        let rlang_44 = make_cache_entry(packages_dir, "rlang", "1.1.3", HEX32, Some("4.4"));
        let jsonlite_45 = make_cache_entry(packages_dir, "jsonlite", "1.8.8", HEX32, Some("4.5"));
        // Legacy entry for a *different* package: filtered out by name, so it
        // must not count toward legacy_skipped.
        let other_legacy = make_cache_entry(packages_dir, "cli", "3.6.2", HEX32, None);

        let outcome = remove_matching_package_entries(
            packages_dir,
            &["rlang".to_string()],
            &["4.5".to_string()],
        )
        .unwrap();

        assert_eq!(outcome.removed, 1);
        assert_eq!(outcome.legacy_skipped, 0);
        assert!(!rlang_45.exists());
        assert!(rlang_44.exists());
        assert!(jsonlite_45.exists());
        assert!(other_legacy.exists());
    }

    #[test]
    fn filtered_package_cache_clean_handles_hyphenated_versions() {
        let dir = tempfile::tempdir().unwrap();
        let packages_dir = dir.path();

        // Matrix 1.6-5: version contains a hyphen; the name must still parse.
        let matrix = make_cache_entry(packages_dir, "Matrix", "1.6-5", HEX32, Some("4.5"));

        let outcome =
            remove_matching_package_entries(packages_dir, &["Matrix".to_string()], &[]).unwrap();

        assert_eq!(outcome.removed, 1);
        assert!(!matrix.exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_directory_is_unlinked_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path();
        let target = tempfile::tempdir().unwrap();
        std::fs::write(target.path().join("keep.txt"), b"keep").unwrap();

        std::os::unix::fs::symlink(target.path(), cache.join("link-to-dir")).unwrap();

        let (count, _bytes, failed) = remove_cache_entries(cache).unwrap();

        assert_eq!(count, 1);
        assert!(failed.is_empty());
        assert!(
            target.path().join("keep.txt").exists(),
            "symlink target contents must survive"
        );
    }

    /// A download cache and an extracted-package cache with one of each kind
    /// of entry `uvr cache prune` has to tell apart. Returns the temp dir
    /// guard, the download cache dir, and the package cache dir.
    fn prune_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache");
        let packages = dir.path().join("packages");
        std::fs::create_dir_all(cache.join("with-envs").join("abc123")).unwrap();
        std::fs::write(cache.join("with-envs/abc123/lockfile"), "env").unwrap();
        for (name, body) in [
            // Tarballs and their sidecars, in each shape `with_extension` gives.
            ("aaaaaaaa-rlang_1.1.4.tar.gz", "rlang"),
            ("aaaaaaaa-rlang_1.1.4.tar.sha256", "sha-rlang"),
            ("bbbbbbbb-cli_3.6.6.tgz", "cli"),
            ("bbbbbbbb-cli_3.6.6.sha256", "sha-cli"),
            // A GitHub tarball: its URL basename has no extension.
            ("cccccccc-0123abcd", "gh"),
            ("cccccccc-0123abcd.sha256", "sha-gh"),
            // A valid download that was never extracted is not an orphan.
            ("dddddddd-never_1.0.tar.gz", "never"),
            ("cran-packages.txt", "index"),
            // Orphans: a sidecar whose tarball is gone, a partial download.
            ("eeeeeeee-gone_1.0.tar.sha256", "orphan"),
            (".uvr-dl-AbC123", "partial"),
        ] {
            std::fs::write(cache.join(name), body).unwrap();
        }

        make_cache_entry(&packages, "rlang", "1.1.4", HEX32, Some("4.5"));
        make_cache_entry(&packages, "old", "1.0", HEX32, Some("3.6"));
        make_cache_entry(&packages, "legacy", "1.0", HEX32, None);
        // Staging dir of an interrupted `package_cache::store`.
        std::fs::create_dir_all(packages.join(".tmpStaging/pkg")).unwrap();
        std::fs::write(
            packages.join(".tmpStaging/pkg/DESCRIPTION"),
            "Package: pkg\n",
        )
        .unwrap();
        // Clutter that is not uvr's to judge.
        std::fs::create_dir_all(packages.join("not-a-key")).unwrap();
        std::fs::write(packages.join("junk.txt"), "junk").unwrap();
        (dir, cache, packages)
    }

    /// The plan as `(reason, file name)` pairs, sorted by name.
    fn planned(plan: &PrunePlan) -> Vec<(PruneReason, String)> {
        let mut out: Vec<(PruneReason, String)> = plan
            .entries
            .iter()
            .map(|e| {
                let name = e.path.file_name().unwrap().to_string_lossy().to_string();
                (e.reason, name)
            })
            .collect();
        out.sort_by(|a, b| a.1.cmp(&b.1));
        out
    }

    fn minors(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    #[test]
    fn prune_plan_finds_only_orphans_and_extracts_for_missing_r() {
        let (_dir, cache, packages) = prune_fixture();

        let plan = plan_prune(&cache, &packages, &minors(&["4.5"]), false, Duration::ZERO).unwrap();

        assert_eq!(
            planned(&plan),
            vec![
                (PruneReason::Orphan, ".tmpStaging".to_string()),
                (PruneReason::Orphan, ".uvr-dl-AbC123".to_string()),
                (
                    PruneReason::Orphan,
                    "eeeeeeee-gone_1.0.tar.sha256".to_string()
                ),
                (PruneReason::StaleR, format!("old-1.0-{HEX32}")),
            ]
        );
        assert_eq!(plan.legacy_skipped, 1, "entry without metadata is kept");
        assert_eq!(plan.young_skipped, 0);
        // Planning removes nothing: this is what --dry-run reports.
        assert!(plan.entries.iter().all(|e| e.path.exists()));
    }

    #[test]
    fn prune_removes_exactly_the_plan_and_tallies_bytes_per_category() {
        let (_dir, cache, packages) = prune_fixture();

        let plan = plan_prune(&cache, &packages, &minors(&["4.5"]), false, Duration::ZERO).unwrap();
        let (removed, failed) = remove_planned(plan.entries);

        assert!(failed.is_empty(), "unexpected failures: {failed:?}");
        // "Package: pkg\n" (13) + "partial" (7) + "orphan" (6)
        assert_eq!(tally(&removed, PruneReason::Orphan), (3, 26));
        // "Package: old\n" (13) + "r_minor=3.6\nkind=binary\n" (24)
        assert_eq!(tally(&removed, PruneReason::StaleR), (1, 37));
        assert_eq!(tally(&removed, PruneReason::Download), (0, 0));
        assert!(removed.iter().all(|e| !e.path.exists()));

        for kept in [
            cache.join("aaaaaaaa-rlang_1.1.4.tar.gz"),
            cache.join("aaaaaaaa-rlang_1.1.4.tar.sha256"),
            cache.join("bbbbbbbb-cli_3.6.6.tgz"),
            cache.join("bbbbbbbb-cli_3.6.6.sha256"),
            cache.join("cccccccc-0123abcd"),
            cache.join("cccccccc-0123abcd.sha256"),
            cache.join("dddddddd-never_1.0.tar.gz"),
            cache.join("cran-packages.txt"),
            cache.join("with-envs/abc123/lockfile"),
            packages.join(format!("rlang-1.1.4-{HEX32}/rlang/DESCRIPTION")),
            packages.join(format!("rlang-1.1.4-{HEX32}/.uvr-meta")),
            packages.join(format!("legacy-1.0-{HEX32}/legacy/DESCRIPTION")),
            packages.join("not-a-key"),
            packages.join("junk.txt"),
        ] {
            assert!(kept.exists(), "{} must survive a prune", kept.display());
        }
    }

    #[test]
    fn prune_keeps_leftovers_that_a_running_sync_may_own() {
        let (_dir, cache, packages) = prune_fixture();

        // The fixture's temp file and staging dir are brand new.
        let plan = plan_prune(&cache, &packages, &minors(&["4.5"]), false, ORPHAN_MIN_AGE).unwrap();

        assert_eq!(
            planned(&plan),
            vec![
                (
                    PruneReason::Orphan,
                    "eeeeeeee-gone_1.0.tar.sha256".to_string()
                ),
                (PruneReason::StaleR, format!("old-1.0-{HEX32}")),
            ]
        );
        assert_eq!(plan.young_skipped, 2, "kept leftovers are reported");
    }

    #[test]
    fn prune_with_no_r_found_keeps_every_extract() {
        let (_dir, cache, packages) = prune_fixture();

        // No R found: every entry would look stale, so none may count as stale.
        let plan = plan_prune(&cache, &packages, &[], false, Duration::ZERO).unwrap();

        assert_eq!(tally(&plan.entries, PruneReason::StaleR), (0, 0));
        assert_eq!(tally(&plan.entries, PruneReason::Orphan).0, 3);
        assert_eq!(plan.legacy_skipped, 0);
    }

    #[test]
    fn prune_ci_drops_downloads_and_keeps_extracts() {
        let (_dir, cache, packages) = prune_fixture();

        let plan = plan_prune(&cache, &packages, &minors(&["4.5"]), true, Duration::ZERO).unwrap();
        let (removed, failed) = remove_planned(plan.entries);

        assert!(failed.is_empty(), "unexpected failures: {failed:?}");
        // Every top-level file except the two orphans:
        // 5 + 9 + 3 + 7 + 2 + 6 + 5 + 5 bytes.
        assert_eq!(tally(&removed, PruneReason::Download), (8, 42));
        assert_eq!(tally(&removed, PruneReason::Orphan).0, 3);
        assert_eq!(tally(&removed, PruneReason::StaleR).0, 1);
        let left: Vec<_> = std::fs::read_dir(&cache)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, vec!["with-envs"], "only with-envs/ stays");
        assert!(cache.join("with-envs/abc123/lockfile").exists());
        assert!(packages
            .join(format!("rlang-1.1.4-{HEX32}/rlang/DESCRIPTION"))
            .exists());
    }

    #[cfg(unix)]
    #[test]
    fn prune_never_follows_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache");
        let packages = dir.path().join("packages");
        // Looks like a stale entry, but lives outside the cache.
        let outside = make_cache_entry(dir.path(), "ext", "1.0", HEX32, Some("3.6"));
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(dir.path().join("file.tar.gz"), "outside").unwrap();

        let stale = make_cache_entry(&packages, "old", "1.0", HEX32, Some("3.6"));
        std::os::unix::fs::symlink(&outside, packages.join(format!("lnk-1.0-{HEX32}"))).unwrap();
        std::os::unix::fs::symlink(&outside, stale.join("escape")).unwrap();
        std::os::unix::fs::symlink(
            dir.path().join("file.tar.gz"),
            cache.join("aaaaaaaa-link_1.0.tar.gz"),
        )
        .unwrap();

        let plan = plan_prune(&cache, &packages, &minors(&["4.5"]), true, Duration::ZERO).unwrap();
        assert_eq!(
            planned(&plan),
            vec![(PruneReason::StaleR, format!("old-1.0-{HEX32}"))]
        );

        let (removed, failed) = remove_planned(plan.entries);
        assert!(failed.is_empty(), "unexpected failures: {failed:?}");
        assert_eq!(removed.len(), 1);
        assert!(!stale.exists());
        assert!(outside.join("ext/DESCRIPTION").exists());
        assert!(dir.path().join("file.tar.gz").exists());
        assert!(packages
            .join(format!("lnk-1.0-{HEX32}"))
            .symlink_metadata()
            .is_ok());
    }
}
