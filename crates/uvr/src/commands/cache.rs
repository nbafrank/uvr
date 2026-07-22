use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use uvr_core::installer::package_cache::{self, dir_size};

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
}
