use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use uvr_core::installer::package_cache;

use crate::ui;

pub fn run_clean() -> Result<()> {
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

/// Total size in bytes of all files under `path` (recursive; best effort).
fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(read_dir) = std::fs::read_dir(path) {
        for entry in read_dir.flatten() {
            let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            if is_dir {
                total += dir_size(&entry.path());
            } else {
                total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    total
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
