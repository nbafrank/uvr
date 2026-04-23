//! Global extracted-package cache.
//!
//! Sits between the tarball download cache (`~/.uvr/cache/`) and per-project
//! libraries (`.uvr/library/`). When a package has been extracted before with
//! the same version, checksum, R version, and platform, the cached directory
//! tree is attached to the project library instead of re-extracting the tarball.
//!
//! Per-platform attach strategy:
//! - **macOS (APFS)**: `clonefile()` — an instant copy-on-write operation. The
//!   project library sees a normal directory; actual data is shared with the
//!   cache until one side diverges.
//! - **Linux**: a whole-directory symlink from the project library to the cached
//!   tree. This dedupes disk usage across projects (issue #24 follow-up) and
//!   matches renv's behavior. R resolves library paths through symlinks
//!   transparently.
//! - **Windows**: recursive file copy. Symlinks on Windows need admin rights
//!   and are fragile across users/drives; copy stays predictable.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tracing::debug;

/// Return the global package cache directory (`~/.uvr/packages/`).
pub fn global_packages_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".uvr")
        .join("packages")
}

/// Compute the cache key for a package.
///
/// The key encodes everything that affects the on-disk artifact: source
/// identity (checksum), R ABI (minor version), install method (binary vs
/// source), platform, and the concrete libR path (since macOS `.so` files
/// are patched with absolute paths to the managed R installation).
pub fn cache_key(
    name: &str,
    version: &str,
    checksum: Option<&str>,
    r_minor: &str,
    is_binary: bool,
    libr_path: Option<&Path>,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(checksum.unwrap_or("none").as_bytes());
    hasher.update(b"|");
    hasher.update(r_minor.as_bytes());
    hasher.update(b"|");
    hasher.update(if is_binary {
        b"binary" as &[u8]
    } else {
        b"source"
    });
    hasher.update(b"|");
    hasher.update(std::env::consts::ARCH.as_bytes());
    hasher.update(b"-");
    hasher.update(std::env::consts::OS.as_bytes());
    if let Some(p) = libr_path {
        hasher.update(b"|");
        hasher.update(p.to_string_lossy().as_bytes());
    }
    let hash = hex::encode(hasher.finalize());
    format!("{}-{}-{}", name, version, &hash[..32])
}

/// Look up a package in the global cache, trying both binary and source keys.
///
/// Returns `Some((path, is_binary))` if found. Tries the `is_binary` variant
/// first, then the opposite — this handles the case where P3M reported a
/// binary URL but the download fell back to source (or vice versa).
pub fn lookup_any(
    name: &str,
    version: &str,
    checksum: Option<&str>,
    r_minor: &str,
    is_binary_hint: bool,
    libr_path: Option<&Path>,
) -> Option<PathBuf> {
    // Try the hinted variant first, then the opposite.
    for &try_binary in &[is_binary_hint, !is_binary_hint] {
        let key = cache_key(name, version, checksum, r_minor, try_binary, libr_path);
        let pkg_dir = global_packages_dir().join(&key).join(name);
        if pkg_dir.join("DESCRIPTION").exists() {
            return Some(pkg_dir);
        }
    }
    None
}

/// Check if a package exists in the global cache under a specific key.
///
/// Returns the path to the package subdirectory (e.g.
/// `~/.uvr/packages/<key>/<name>/`) if the cached entry looks valid
/// (contains a `DESCRIPTION` file).
pub fn lookup(name: &str, key: &str) -> Option<PathBuf> {
    let pkg_dir = global_packages_dir().join(key).join(name);
    if pkg_dir.join("DESCRIPTION").exists() {
        Some(pkg_dir)
    } else {
        None
    }
}

/// Attach a cached package directory to the project library.
///
/// See module docs for the per-platform strategy. On any attach-time failure
/// (clonefile rejects a non-APFS volume, symlink creation hits a weird FS)
/// we silently fall back to a recursive copy so sync always makes progress.
pub fn clone_to_library(
    cached_pkg_dir: &Path,
    library: &Path,
    package_name: &str,
) -> std::io::Result<()> {
    let dest = library.join(package_name);
    // Remove whatever's there — dir, file, or (possibly broken) symlink from
    // a prior sync. `dest.exists()` follows symlinks and would miss broken
    // ones, which is exactly the state we'd land in if the cache was cleaned
    // between syncs.
    remove_entry(&dest)?;

    #[cfg(target_os = "macos")]
    {
        match clone_dir_macos(cached_pkg_dir, &dest) {
            Ok(()) => {
                debug!(
                    "clonefile: {} → {}",
                    cached_pkg_dir.display(),
                    dest.display()
                );
                return Ok(());
            }
            Err(e) => {
                debug!("clonefile failed ({}), falling back to copy", e);
                // Fall through
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        // Symlink instead of copying: cache is the source of truth, each
        // project library holds one cheap link per package. If `uvr cache
        // clean` later removes the target, the next `uvr sync` reseeds the
        // cache and rewrites the link.
        match std::os::unix::fs::symlink(cached_pkg_dir, &dest) {
            Ok(()) => {
                debug!(
                    "symlinked {} → {}",
                    dest.display(),
                    cached_pkg_dir.display()
                );
                return Ok(());
            }
            Err(e) => {
                debug!("symlink failed ({}), falling back to copy", e);
                // Fall through
            }
        }
    }

    copy_dir_recursive(cached_pkg_dir, &dest)
}

/// Remove a filesystem entry whatever its kind — directory, regular file,
/// or symlink (including broken symlinks). `Ok(())` when the path doesn't
/// exist. Used by `clone_to_library` so re-syncing over any prior state
/// (old copy, fresh symlink, stale symlink) works uniformly.
fn remove_entry(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(md) if md.file_type().is_symlink() => std::fs::remove_file(path),
        Ok(md) if md.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Atomically store a package directory into the global cache.
///
/// Uses a temporary directory + rename so concurrent processes never see
/// a half-written cache entry. If the entry already exists (another process
/// won the race), the temporary copy is discarded.
pub fn store(source_pkg_dir: &Path, key: &str, package_name: &str) -> std::io::Result<()> {
    let packages_dir = global_packages_dir();
    std::fs::create_dir_all(&packages_dir)?;

    let final_dir = packages_dir.join(key);
    if final_dir.exists() {
        if final_dir.join(package_name).join("DESCRIPTION").exists() {
            // Already cached (another process or a previous run).
            return Ok(());
        }
        // Corrupted/partial entry from a prior crash — remove and replace.
        let _ = std::fs::remove_dir_all(&final_dir);
    }

    // Stage into a temporary directory next to the final location.
    let staging = tempfile::TempDir::new_in(&packages_dir)?;
    let staged_pkg = staging.path().join(package_name);

    #[cfg(target_os = "macos")]
    {
        match clone_dir_macos(source_pkg_dir, &staged_pkg) {
            Ok(()) => {}
            Err(_) => {
                copy_dir_recursive(source_pkg_dir, &staged_pkg)?;
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        copy_dir_recursive(source_pkg_dir, &staged_pkg)?;
    }

    // Atomic rename. If it fails because the target already exists, that's fine.
    match std::fs::rename(staging.path(), &final_dir) {
        Ok(()) => {
            // Prevent TempDir destructor from removing the renamed directory
            let _ = staging.keep();
            debug!("Cached {} in {}", package_name, final_dir.display());
            Ok(())
        }
        Err(e)
            if e.kind() == std::io::ErrorKind::AlreadyExists
                || e.raw_os_error() == Some(39 /* ENOTEMPTY */)
                || e.raw_os_error() == Some(17 /* EEXIST */) =>
        {
            // Another process cached it first — our staging dir will be
            // cleaned up by the TempDir drop.
            debug!("Cache race for {}, using existing entry", package_name);
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// macOS: clone an entire directory tree using the `clonefile()` syscall.
///
/// This is an instant copy-on-write operation on APFS volumes — no data
/// is physically copied until one side is modified. Returns `ENOTSUP` on
/// non-APFS filesystems (e.g. HFS+, NFS, SMB).
///
/// `CLONE_NOFOLLOW` (flag 1) prevents following a symlink at the source
/// root path only. Symlinks *inside* the tree are reproduced as-is (not
/// traversed). This matches R package semantics — internal symlinks are
/// preserved faithfully.
#[cfg(target_os = "macos")]
fn clone_dir_macos(src: &Path, dst: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::raw::{c_char, c_int};

    extern "C" {
        fn clonefile(src: *const c_char, dst: *const c_char, flags: u32) -> c_int;
    }

    let src_c =
        CString::new(src.to_str().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid path")
        })?)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let dst_c =
        CString::new(dst.to_str().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid path")
        })?)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;

    // CLONE_NOFOLLOW = 1
    let ret = unsafe { clonefile(src_c.as_ptr(), dst_c.as_ptr(), 1u32) };
    if ret != 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Recursively copy a directory tree. Symlinks are reproduced as symlinks
/// (not traversed) to match `clonefile()` behavior and prevent traversal
/// outside the source tree.
pub fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            // Reproduce symlinks as-is (same target, not followed).
            #[cfg(unix)]
            {
                let target = std::fs::read_link(&src_path)?;
                std::os::unix::fs::symlink(&target, &dst_path)?;
            }
            #[cfg(not(unix))]
            {
                // On Windows, fall back to copying the symlink target.
                if src_path.is_dir() {
                    copy_dir_recursive(&src_path, &dst_path)?;
                } else {
                    std::fs::copy(&src_path, &dst_path)?;
                }
            }
        } else if ft.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// Total size of the global package cache in bytes, plus entry count.
pub fn cache_stats() -> (u64, u64) {
    let dir = global_packages_dir();
    if !dir.exists() {
        return (0, 0);
    }
    let mut bytes = 0u64;
    let mut count = 0u64;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            count += 1;
            bytes += dir_size(&entry.path());
        }
    }
    (count, bytes)
}

fn dir_size(path: &Path) -> u64 {
    if path.is_file() {
        return path.metadata().map(|m| m.len()).unwrap_or(0);
    }
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            total += dir_size(&entry.path());
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn cache_key_deterministic() {
        let k1 = cache_key("ggplot2", "3.5.1", Some("abc123"), "4.4", true, None);
        let k2 = cache_key("ggplot2", "3.5.1", Some("abc123"), "4.4", true, None);
        assert_eq!(k1, k2);
        assert!(k1.starts_with("ggplot2-3.5.1-"));
        assert_eq!(k1.len(), "ggplot2-3.5.1-".len() + 32);
    }

    #[test]
    fn cache_key_differs_by_r_version() {
        let k1 = cache_key("pkg", "1.0", Some("abc"), "4.4", true, None);
        let k2 = cache_key("pkg", "1.0", Some("abc"), "4.5", true, None);
        assert_ne!(k1, k2);
    }

    #[test]
    fn cache_key_differs_by_method() {
        let k1 = cache_key("pkg", "1.0", Some("abc"), "4.4", true, None);
        let k2 = cache_key("pkg", "1.0", Some("abc"), "4.4", false, None);
        assert_ne!(k1, k2);
    }

    #[test]
    fn cache_key_differs_by_libr_path() {
        let p1 = PathBuf::from("/home/.uvr/r-versions/4.4.2/lib/libR.dylib");
        let p2 = PathBuf::from("/home/.uvr/r-versions/4.4.3/lib/libR.dylib");
        let k1 = cache_key("pkg", "1.0", Some("abc"), "4.4", true, Some(&p1));
        let k2 = cache_key("pkg", "1.0", Some("abc"), "4.4", true, Some(&p2));
        assert_ne!(k1, k2);
    }

    #[test]
    fn lookup_missing() {
        assert!(lookup("nonexistent", "fake-key-12345678901234567890123456789012").is_none());
    }

    #[test]
    fn lookup_any_finds_source_fallback() {
        let tmp = TempDir::new().unwrap();
        let pkg_dir = tmp.path().join("testpkg");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(
            pkg_dir.join("DESCRIPTION"),
            "Package: testpkg\nVersion: 1.0\n",
        )
        .unwrap();

        // Store under the source key (is_binary=false)
        let source_key = cache_key("testpkg", "1.0", Some("cksum"), "4.5", false, None);
        store(&pkg_dir, &source_key, "testpkg").unwrap();

        // Lookup with binary hint (is_binary=true) — should still find the source entry
        let found = lookup_any("testpkg", "1.0", Some("cksum"), "4.5", true, None);
        assert!(found.is_some());

        // Cleanup
        let _ = std::fs::remove_dir_all(global_packages_dir().join(&source_key));
    }

    #[test]
    fn store_and_lookup_roundtrip() {
        let tmp = TempDir::new().unwrap();

        // Create a fake package directory
        let pkg_dir = tmp.path().join("testpkg");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(
            pkg_dir.join("DESCRIPTION"),
            "Package: testpkg\nVersion: 1.0\n",
        )
        .unwrap();
        std::fs::create_dir_all(pkg_dir.join("R")).unwrap();
        std::fs::write(pkg_dir.join("R/hello.R"), "hello <- function() 1\n").unwrap();

        // Use a unique key to avoid collisions with other tests
        let key = format!(
            "testpkg-1.0-{:032x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );

        // Store
        store(&pkg_dir, &key, "testpkg").unwrap();

        // Lookup
        let cached = lookup("testpkg", &key);
        assert!(cached.is_some());
        let cached_dir = cached.unwrap();
        assert!(cached_dir.join("DESCRIPTION").exists());
        assert!(cached_dir.join("R/hello.R").exists());

        // Cleanup
        let _ = std::fs::remove_dir_all(global_packages_dir().join(&key));
    }

    #[test]
    fn store_replaces_corrupted_entry() {
        let tmp = TempDir::new().unwrap();

        let pkg_dir = tmp.path().join("testpkg");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(
            pkg_dir.join("DESCRIPTION"),
            "Package: testpkg\nVersion: 1.0\n",
        )
        .unwrap();

        let key = format!(
            "testpkg-1.0-corrupt{:024x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );

        // Create a corrupted cache entry (directory exists but no DESCRIPTION)
        let corrupted = global_packages_dir().join(&key);
        std::fs::create_dir_all(corrupted.join("testpkg")).unwrap();
        assert!(lookup("testpkg", &key).is_none()); // no DESCRIPTION

        // Store should replace the corrupted entry
        store(&pkg_dir, &key, "testpkg").unwrap();
        assert!(lookup("testpkg", &key).is_some()); // now valid

        // Cleanup
        let _ = std::fs::remove_dir_all(global_packages_dir().join(&key));
    }

    #[test]
    fn remove_entry_handles_missing_path() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("nope");
        // NotFound should not error.
        remove_entry(&missing).unwrap();
    }

    #[test]
    fn remove_entry_handles_file() {
        let tmp = TempDir::new().unwrap();
        let f = tmp.path().join("thing");
        std::fs::write(&f, "x").unwrap();
        remove_entry(&f).unwrap();
        assert!(!f.exists());
    }

    #[test]
    fn remove_entry_handles_directory() {
        let tmp = TempDir::new().unwrap();
        let d = tmp.path().join("dir");
        std::fs::create_dir_all(d.join("nested")).unwrap();
        std::fs::write(d.join("inner"), "x").unwrap();
        remove_entry(&d).unwrap();
        assert!(!d.exists());
    }

    #[cfg(unix)]
    #[test]
    fn remove_entry_handles_broken_symlink() {
        let tmp = TempDir::new().unwrap();
        let link = tmp.path().join("link");
        // Target never existed — broken symlink.
        std::os::unix::fs::symlink("/does/not/exist/uvr", &link).unwrap();
        assert!(link.symlink_metadata().is_ok());
        remove_entry(&link).unwrap();
        assert!(link.symlink_metadata().is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn clone_to_library_uses_symlink_on_linux() {
        let tmp = TempDir::new().unwrap();
        let cache_pkg = tmp.path().join("cache").join("ggplot2");
        std::fs::create_dir_all(&cache_pkg).unwrap();
        std::fs::write(cache_pkg.join("DESCRIPTION"), "Package: ggplot2\n").unwrap();

        let library = tmp.path().join("library");
        std::fs::create_dir_all(&library).unwrap();

        clone_to_library(&cache_pkg, &library, "ggplot2").unwrap();

        let dest = library.join("ggplot2");
        let md = std::fs::symlink_metadata(&dest).unwrap();
        assert!(
            md.file_type().is_symlink(),
            "expected symlink, got {:?}",
            md.file_type()
        );
        // Package is readable through the link.
        assert!(dest.join("DESCRIPTION").exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn clone_to_library_replaces_real_dir_with_symlink() {
        // Simulates upgrading from an old uvr that recursive-copied: library
        // already holds a real directory. clone_to_library must replace it.
        let tmp = TempDir::new().unwrap();
        let cache_pkg = tmp.path().join("cache").join("xml2");
        std::fs::create_dir_all(&cache_pkg).unwrap();
        std::fs::write(cache_pkg.join("DESCRIPTION"), "Package: xml2\n").unwrap();

        let library = tmp.path().join("library");
        let old = library.join("xml2");
        std::fs::create_dir_all(old.join("R")).unwrap();
        std::fs::write(old.join("DESCRIPTION"), "Package: xml2-stale\n").unwrap();

        clone_to_library(&cache_pkg, &library, "xml2").unwrap();

        assert!(old.symlink_metadata().unwrap().file_type().is_symlink());
        // Now reads from the cache.
        let desc = std::fs::read_to_string(old.join("DESCRIPTION")).unwrap();
        assert!(desc.contains("xml2\n") && !desc.contains("stale"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn clone_to_library_overwrites_existing_symlink() {
        let tmp = TempDir::new().unwrap();
        let cache_a = tmp.path().join("cache-a").join("dplyr");
        let cache_b = tmp.path().join("cache-b").join("dplyr");
        for c in [&cache_a, &cache_b] {
            std::fs::create_dir_all(c).unwrap();
            std::fs::write(c.join("DESCRIPTION"), "Package: dplyr\n").unwrap();
        }

        let library = tmp.path().join("library");
        std::fs::create_dir_all(&library).unwrap();

        clone_to_library(&cache_a, &library, "dplyr").unwrap();
        clone_to_library(&cache_b, &library, "dplyr").unwrap();

        let link = library.join("dplyr");
        let target = std::fs::read_link(&link).unwrap();
        assert_eq!(target, cache_b);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn clone_dir_macos_works() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src_pkg");
        std::fs::create_dir_all(src.join("R")).unwrap();
        std::fs::write(src.join("DESCRIPTION"), "test").unwrap();
        std::fs::write(src.join("R/foo.R"), "foo <- 1").unwrap();

        let dst = tmp.path().join("dst_pkg");
        clone_dir_macos(&src, &dst).unwrap();

        assert!(dst.join("DESCRIPTION").exists());
        assert!(dst.join("R/foo.R").exists());
    }
}
