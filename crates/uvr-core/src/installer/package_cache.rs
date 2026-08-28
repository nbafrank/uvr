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
//! - **Linux and Windows**: per-file hardlinks (#247, #248). The library gets
//!   ordinary-looking files that share storage with the cache. Falls back to
//!   a copy when the cache and project sit on different volumes, since a
//!   hardlink cannot cross one.
//!
//! Linux attached a whole-directory symlink until #248. That deduped just as
//! well, but it cost two things hardlinks do not. `uvr cache clean` left every
//! existing project library pointing at deleted targets, because the link died
//! with the target; a hardlinked file outlives the cache entry, so cleaning
//! frees only what nothing else references. And anything that resolves
//! symlinks — `.libPaths()`, `find.package()`, tooling walking the library —
//! reported packages as living in the cache rather than in the project, which
//! is the class of confusion renv spent years unwinding.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tracing::debug;

/// Return the global package cache directory (`~/.uvr/packages/`, or
/// `UVR_PACKAGES_DIR` when set).
///
/// When no home directory can be determined (HOME unset in sandboxes,
/// scratch containers, some CI runners) the cache degrades to a per-boot
/// directory under the system temp dir rather than polluting the current
/// working directory with `./.uvr/packages/`.
pub fn global_packages_dir() -> PathBuf {
    if let Some(dir) = crate::env_vars::packages_dir() {
        return dir;
    }
    match dirs::home_dir() {
        Some(home) => home.join(".uvr").join("packages"),
        None => {
            static WARN_ONCE: std::sync::Once = std::sync::Once::new();
            let fallback = std::env::temp_dir().join("uvr-packages");
            WARN_ONCE.call_once(|| {
                tracing::warn!(
                    "HOME is unset; using temporary package cache at {} \
                     (cache will not persist across reboots)",
                    fallback.display()
                );
            });
            fallback
        }
    }
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
    binary_flavor: Option<&str>,
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
    // Which binary repo the artifact came from. A `jammy` build and a
    // `manylinux_2_28` build of the same package+version are different
    // artifacts linking different libraries, and only one of them loads on a
    // given host (#175) — so they must not share a cache entry.
    //
    // Only hashed for *binary* entries: a source build is compiled on this
    // host and is not repo-specific. Enforced here, not at call sites —
    // the store path passed the session's flavour unconditionally while
    // lookup probed source entries without one, so every source-kind entry
    // on flavoured Linux landed under a key no lookup could produce, and
    // was rebuilt on every warm sync forever (#237). Keying by
    // `is_binary && flavor` keeps macOS/Windows and source keys
    // byte-identical to before; only Linux binary entries carry it, and
    // those are exactly the ones #175 made ambiguous.
    if let (true, Some(flavor)) = (is_binary, binary_flavor) {
        hasher.update(b"|");
        hasher.update(flavor.as_bytes());
    }
    if let Some(p) = libr_path {
        hasher.update(b"|");
        hasher.update(p.to_string_lossy().as_bytes());
    }
    let hash = hex::encode(hasher.finalize());
    format!("{}-{}-{}", name, version, &hash[..32])
}

/// Extract the package name from a cache key (`<name>-<version>-<hash32>`).
///
/// R package names cannot contain hyphens (only letters, digits, and dots),
/// so the name is everything before the *first* hyphen. Splitting from the
/// right would be wrong: package *versions* may contain hyphens (e.g.
/// Matrix "1.6-5"). Returns `None` when `key` does not look like a cache
/// key (missing the trailing 32-hex-char hash or a version segment) — e.g.
/// stray files or temp staging dirs in the cache directory.
pub fn package_name_from_key(key: &str) -> Option<&str> {
    let (name, rest) = key.split_once('-')?;
    // rest = "<version>-<hash32>": require a non-empty version segment and
    // a trailing 32-char hex hash.
    let (version, hash) = rest.rsplit_once('-')?;
    if name.is_empty()
        || version.is_empty()
        || hash.len() != 32
        || !hash.chars().all(|c| c.is_ascii_hexdigit())
    {
        return None;
    }
    Some(name)
}

/// Filename of the metadata file written inside each cache entry directory
/// (next to the `<package_name>/` subdirectory). Records facts the cache key
/// hashes away — the R minor version and install method — so `uvr cache
/// clean --r-version` can filter entries. Entries created by older uvr
/// versions lack this file and cannot be filtered by R version.
pub const ENTRY_META_FILENAME: &str = ".uvr-meta";

/// Metadata recorded for a cache entry at store time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryMeta {
    /// R minor version the package was installed under, e.g. "4.5".
    pub r_minor: String,
    /// Whether the package was installed from a binary (vs built from source).
    pub is_binary: bool,
}

impl EntryMeta {
    fn to_file_contents(&self) -> String {
        format!(
            "r_minor={}\nkind={}\n",
            self.r_minor,
            if self.is_binary { "binary" } else { "source" }
        )
    }

    fn from_file_contents(contents: &str) -> Option<Self> {
        let mut r_minor = None;
        let mut is_binary = None;
        for line in contents.lines() {
            match line.split_once('=') {
                Some(("r_minor", v)) if !v.is_empty() => r_minor = Some(v.to_string()),
                Some(("kind", v)) => is_binary = Some(v == "binary"),
                // Unknown keys are ignored for forward compatibility.
                _ => {}
            }
        }
        Some(EntryMeta {
            r_minor: r_minor?,
            is_binary: is_binary?,
        })
    }
}

/// Read the metadata file of a cache entry directory, if present and parseable.
pub fn read_entry_meta(entry_dir: &Path) -> Option<EntryMeta> {
    let contents = std::fs::read_to_string(entry_dir.join(ENTRY_META_FILENAME)).ok()?;
    EntryMeta::from_file_contents(&contents)
}

/// Look up a package in the global cache.
///
/// Returns the cached package directory if found. When `binary_allowed`, the
/// binary key is probed first, then the source key — this handles the case
/// where P3M reported a binary URL but the download fell back to source. Both
/// kinds are safe to serve then: the source entry was built on this same
/// host/R-minor/libR configuration (it's part of the key). When binaries are
/// unusable here (`--no-binary`, unrecognized distro) only the source key is
/// probed (#165, #175 — see below).
pub fn lookup_any(
    name: &str,
    version: &str,
    checksum: Option<&str>,
    r_minor: &str,
    binary_allowed: bool,
    libr_path: Option<&Path>,
    binary_flavor: Option<&str>,
) -> Option<PathBuf> {
    // Probe the binary variant first when this platform can use P3M binaries
    // at all, then source.
    //
    // When it cannot, a cached *binary* entry must never be served. Those
    // entries exist on such machines only as leftovers from a uvr that
    // mis-identified the distro (#175) — they link shared libraries this
    // system does not have and fail at `library()`. Without this gate the
    // distro fix is inert for exactly the users who already hit the bug,
    // because the broken artifact is still in `~/.uvr/packages/`.
    let variants: &[bool] = if binary_allowed {
        &[true, false]
    } else {
        &[false]
    };
    for &try_binary in variants {
        // Flavour only identifies a *binary* artifact; a source build is
        // compiled here and is not repo-specific.
        let flavor = if try_binary { binary_flavor } else { None };
        let key = cache_key(
            name, version, checksum, r_minor, try_binary, libr_path, flavor,
        );
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

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    {
        // Hardlink each file rather than copying its bytes (#247 for
        // Windows, #248 for Linux). Hardlinks need no privileges on NTFS
        // (unlike symlinks, the reason the Windows path was a copy
        // originally), but they do require one volume — a cache and project
        // on different drives or mounts lands in the fallback below.
        //
        // `remove_entry` above already cleared whatever was here, so a
        // library still holding a pre-#248 directory symlink migrates on its
        // next sync with no separate step.
        match hardlink_dir_recursive(cached_pkg_dir, &dest) {
            Ok(()) => {
                debug!(
                    "hardlinked {} → {}",
                    cached_pkg_dir.display(),
                    dest.display()
                );
                return Ok(());
            }
            Err(e) => {
                debug!("hardlink failed ({}), falling back to copy", e);
                // Partial tree from the failed attempt would otherwise make
                // the copy below merge into it.
                let _ = remove_entry(&dest);
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

fn published_entry_is_equivalent(entry_pkg_dir: &Path, source_pkg_dir: &Path) -> bool {
    let expected = crate::installer::nested_source::read_provenance(source_pkg_dir);
    entry_pkg_dir.join("DESCRIPTION").exists()
        && crate::installer::nested_source::provenance_matches(entry_pkg_dir, expected.as_ref())
}

/// Atomically store a package directory into the global cache.
///
/// Uses a temporary directory + rename so concurrent processes never see
/// a half-written cache entry. If the entry already exists (another process
/// won the race), the temporary copy is discarded.
///
/// When `meta` is given it is written as a [`ENTRY_META_FILENAME`] file into
/// the staging directory, so the rename publishes the entry and its metadata
/// atomically. Metadata is best effort: an entry without it is still usable,
/// it just cannot be filtered by `uvr cache clean --r-version`.
pub fn store(
    source_pkg_dir: &Path,
    key: &str,
    package_name: &str,
    meta: Option<&EntryMeta>,
) -> std::io::Result<()> {
    let packages_dir = global_packages_dir();
    std::fs::create_dir_all(&packages_dir)?;

    let final_dir = packages_dir.join(key);
    if final_dir.exists() {
        if published_entry_is_equivalent(&final_dir.join(package_name), source_pkg_dir) {
            return Ok(());
        }
        if let Err(e) = std::fs::remove_dir_all(&final_dir) {
            if final_dir.exists()
                && !published_entry_is_equivalent(&final_dir.join(package_name), source_pkg_dir)
            {
                tracing::warn!(
                    "Cannot replace invalid cache entry {}: {e}",
                    final_dir.display()
                );
                return Err(std::io::Error::new(
                    e.kind(),
                    format!(
                        "failed to replace invalid cache entry {}: {e}",
                        final_dir.display()
                    ),
                ));
            }
        }
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

    if let Some(meta) = meta {
        if let Err(e) = std::fs::write(
            staging.path().join(ENTRY_META_FILENAME),
            meta.to_file_contents(),
        ) {
            debug!("Failed to write cache entry metadata for {key}: {e}");
        }
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
            if published_entry_is_equivalent(&final_dir.join(package_name), source_pkg_dir) {
                debug!("Cache race for {}, using existing entry", package_name);
                Ok(())
            } else {
                tracing::warn!(
                    "Cache entry {} exists but is invalid and could not be replaced",
                    final_dir.display()
                );
                Err(std::io::Error::new(
                    e.kind(),
                    format!(
                        "invalid cache entry {} could not be replaced: {e}",
                        final_dir.display()
                    ),
                ))
            }
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
/// Recreate `src` at `dst`, hardlinking regular files instead of copying
/// their bytes. Directories are created; symlinks are reproduced as
/// symlinks (Unix) or copied (Windows), same as [`copy_dir_recursive`].
///
/// Used for the Windows attach path (#247), where the alternative is a full
/// byte copy of every package on every warm sync — the other platforms have
/// had an instant path since v0.3 (clonefile on macOS, symlink on Linux).
/// NTFS hardlinks need no special privileges, unlike symlinks.
///
/// Constraints this inherits: hardlinks require both paths on one volume,
/// and the linked inode is shared, so a caller that later writes in place
/// into an attached file would corrupt the cache. R package files are
/// read-only after install, and uvr's extraction writes into the cache
/// before attaching, never through an attached path.
///
/// Returns `Err` on the first failure so the caller can fall back to a
/// plain copy; partial output at `dst` is the caller's to clean up.
// Compiled on every platform so the tree-walking logic stays under test
// everywhere, but macOS clones instead and has no non-test caller.
#[cfg_attr(not(any(target_os = "linux", target_os = "windows")), allow(dead_code))]
pub(crate) fn hardlink_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            #[cfg(unix)]
            {
                let target = std::fs::read_link(&src_path)?;
                std::os::unix::fs::symlink(&target, &dst_path)?;
            }
            #[cfg(not(unix))]
            {
                if src_path.is_dir() {
                    hardlink_dir_recursive(&src_path, &dst_path)?;
                } else {
                    std::fs::copy(&src_path, &dst_path)?;
                }
            }
        } else if ft.is_dir() {
            hardlink_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::hard_link(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

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

/// Recursive size of a directory tree, in bytes.
///
/// Symlinks are never followed (`symlink_metadata` / `entry.file_type()`
/// don't traverse them): cache entries are real directories, and a symlink
/// pointing outside the cache — or a symlink cycle inside a cached package
/// (preserved verbatim by `copy_dir_recursive`) — must not inflate the
/// stats or recurse forever.
pub fn dir_size(path: &Path) -> u64 {
    let md = match std::fs::symlink_metadata(path) {
        Ok(md) => md,
        Err(_) => return 0,
    };
    if md.file_type().is_symlink() {
        return 0;
    }
    if md.is_file() {
        return md.len();
    }
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            if ft.is_symlink() {
                continue;
            }
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
        let k1 = cache_key("ggplot2", "3.5.1", Some("abc123"), "4.4", true, None, None);
        let k2 = cache_key("ggplot2", "3.5.1", Some("abc123"), "4.4", true, None, None);
        assert_eq!(k1, k2);
        assert!(k1.starts_with("ggplot2-3.5.1-"));
        assert_eq!(k1.len(), "ggplot2-3.5.1-".len() + 32);
    }

    #[test]
    fn cache_key_differs_by_r_version() {
        let k1 = cache_key("pkg", "1.0", Some("abc"), "4.4", true, None, None);
        let k2 = cache_key("pkg", "1.0", Some("abc"), "4.5", true, None, None);
        assert_ne!(k1, k2);
    }

    #[test]
    fn cache_key_differs_by_method() {
        let k1 = cache_key("pkg", "1.0", Some("abc"), "4.4", true, None, None);
        let k2 = cache_key("pkg", "1.0", Some("abc"), "4.4", false, None, None);
        assert_ne!(k1, k2);
    }

    #[test]
    fn a_source_entry_ignores_the_binary_flavor() {
        // The store path passes the session's binary flavour for every
        // package it caches; lookup probes source entries with no flavour
        // ("a source build is compiled here and is not repo-specific").
        // Hashing the flavour into a source key therefore stored every
        // source-kind entry on flavoured Linux under a key no lookup could
        // ever produce — an eternal warm-tier miss that re-ran
        // R CMD INSTALL on every sync (#237). The key must make the two
        // sides agree by construction.
        assert_eq!(
            cache_key("pkg", "1.0", Some("abc"), "4.4", false, None, Some("jammy")),
            cache_key("pkg", "1.0", Some("abc"), "4.4", false, None, None)
        );
    }

    #[test]
    fn cache_key_differs_by_binary_flavor() {
        // A jammy build and a manylinux build of the same package+version
        // link different libraries and only one loads on a given host
        // (#175), so they must not collide in the cache.
        let jammy = cache_key("pkg", "1.0", Some("abc"), "4.4", true, None, Some("jammy"));
        let many = cache_key(
            "pkg",
            "1.0",
            Some("abc"),
            "4.4",
            true,
            None,
            Some("manylinux_2_28"),
        );
        assert_ne!(jammy, many);

        // Absent flavour must stay byte-identical to before this field
        // existed, so macOS/Windows and source entries are not invalidated.
        let unflavoured = cache_key("pkg", "1.0", Some("abc"), "4.4", true, None, None);
        assert_ne!(unflavoured, jammy);
        assert_ne!(unflavoured, many);
    }

    #[test]
    fn cache_key_differs_by_libr_path() {
        let p1 = PathBuf::from("/home/.uvr/r-versions/4.4.2/lib/libR.dylib");
        let p2 = PathBuf::from("/home/.uvr/r-versions/4.4.3/lib/libR.dylib");
        let k1 = cache_key("pkg", "1.0", Some("abc"), "4.4", true, Some(&p1), None);
        let k2 = cache_key("pkg", "1.0", Some("abc"), "4.4", true, Some(&p2), None);
        assert_ne!(k1, k2);
    }

    #[test]
    fn package_name_from_key_roundtrips_cache_key() {
        let key = cache_key("ggplot2", "3.5.1", Some("abc123"), "4.4", true, None, None);
        assert_eq!(package_name_from_key(&key), Some("ggplot2"));
        // Dots in names are fine (e.g. data.table).
        let key = cache_key(
            "data.table",
            "1.15.4",
            Some("abc"),
            "4.5",
            false,
            None,
            None,
        );
        assert_eq!(package_name_from_key(&key), Some("data.table"));
    }

    #[test]
    fn package_name_from_key_handles_hyphenated_versions() {
        // R package *versions* may contain hyphens (Matrix "1.6-5"); names
        // cannot, so the name is everything before the first hyphen.
        let key = cache_key("Matrix", "1.6-5", Some("abc"), "4.4", true, None, None);
        assert_eq!(package_name_from_key(&key), Some("Matrix"));
    }

    #[test]
    fn package_name_from_key_rejects_non_keys() {
        let hex32 = "0123456789abcdef0123456789abcdef";
        // No hyphens at all (e.g. tempfile staging dirs like ".tmpAbC123").
        assert_eq!(package_name_from_key(".tmpAbC123"), None);
        // Trailing segment is not a 32-char hex hash.
        assert_eq!(package_name_from_key("not-a-key"), None);
        assert_eq!(package_name_from_key("pkg-1.0-deadbeef"), None);
        // Missing version segment or empty name.
        assert_eq!(package_name_from_key(&format!("pkg-{hex32}")), None);
        assert_eq!(package_name_from_key(&format!("-1.0-{hex32}")), None);
        // Well-formed key parses.
        assert_eq!(
            package_name_from_key(&format!("pkg-1.0-{hex32}")),
            Some("pkg")
        );
    }

    #[test]
    fn entry_meta_file_contents_roundtrip() {
        let meta = EntryMeta {
            r_minor: "4.5".to_string(),
            is_binary: true,
        };
        let contents = meta.to_file_contents();
        assert_eq!(contents, "r_minor=4.5\nkind=binary\n");
        assert_eq!(EntryMeta::from_file_contents(&contents), Some(meta));

        let source = EntryMeta {
            r_minor: "4.4".to_string(),
            is_binary: false,
        };
        assert_eq!(
            EntryMeta::from_file_contents(&source.to_file_contents()),
            Some(source)
        );
    }

    #[test]
    fn entry_meta_parse_tolerates_unknown_keys_and_rejects_incomplete() {
        // Unknown keys from a future uvr are ignored.
        let parsed = EntryMeta::from_file_contents("r_minor=4.5\nkind=source\nfuture_field=zap\n");
        assert_eq!(
            parsed,
            Some(EntryMeta {
                r_minor: "4.5".to_string(),
                is_binary: false,
            })
        );
        // Missing either field → unparseable (treated as legacy).
        assert_eq!(EntryMeta::from_file_contents("r_minor=4.5\n"), None);
        assert_eq!(EntryMeta::from_file_contents("kind=binary\n"), None);
        assert_eq!(EntryMeta::from_file_contents(""), None);
    }

    #[test]
    fn store_writes_meta_and_read_entry_meta_roundtrips() {
        let tmp = TempDir::new().unwrap();
        let pkg_dir = tmp.path().join("testpkg");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(
            pkg_dir.join("DESCRIPTION"),
            "Package: testpkg\nVersion: 1.0\n",
        )
        .unwrap();

        let key = format!(
            "testpkg-1.0-meta{:028x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );

        let meta = EntryMeta {
            r_minor: "4.5".to_string(),
            is_binary: true,
        };
        store(&pkg_dir, &key, "testpkg", Some(&meta)).unwrap();

        let entry_dir = global_packages_dir().join(&key);
        assert_eq!(read_entry_meta(&entry_dir), Some(meta));
        // The entry itself is still a valid lookup target.
        assert!(lookup("testpkg", &key).is_some());

        // Cleanup
        let _ = std::fs::remove_dir_all(&entry_dir);
    }

    #[test]
    fn store_without_meta_leaves_no_meta_file() {
        let tmp = TempDir::new().unwrap();
        let pkg_dir = tmp.path().join("testpkg");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(
            pkg_dir.join("DESCRIPTION"),
            "Package: testpkg\nVersion: 1.0\n",
        )
        .unwrap();

        let key = format!(
            "testpkg-1.0-nometa{:026x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );

        store(&pkg_dir, &key, "testpkg", None).unwrap();

        let entry_dir = global_packages_dir().join(&key);
        assert!(!entry_dir.join(ENTRY_META_FILENAME).exists());
        assert_eq!(read_entry_meta(&entry_dir), None);

        // Cleanup
        let _ = std::fs::remove_dir_all(&entry_dir);
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
        let source_key = cache_key("testpkg", "1.0", Some("cksum"), "4.5", false, None, None);
        store(&pkg_dir, &source_key, "testpkg", None).unwrap();

        // Lookup with binary hint (is_binary=true) — should still find the source entry
        let found = lookup_any("testpkg", "1.0", Some("cksum"), "4.5", true, None, None);
        assert!(found.is_some());

        // Cleanup
        let _ = std::fs::remove_dir_all(global_packages_dir().join(&source_key));
    }

    #[test]
    fn lookup_any_refuses_a_binary_entry_when_binaries_are_unusable() {
        // #175: on a distro P3M doesn't publish for, a cached binary is a
        // leftover from a uvr that mis-identified the distro. Serving it
        // reinstalls a package that cannot load, and would make the distro
        // fix inert for exactly the people who already hit the bug.
        let tmp = TempDir::new().unwrap();
        let pkg_dir = tmp.path().join("binpkg");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(
            pkg_dir.join("DESCRIPTION"),
            "Package: binpkg\nVersion: 1.0\n",
        )
        .unwrap();

        let binary_key = cache_key("binpkg", "1.0", Some("cksum"), "4.5", true, None, None);
        store(&pkg_dir, &binary_key, "binpkg", None).unwrap();

        // Binaries usable here → the entry is served.
        assert!(lookup_any("binpkg", "1.0", Some("cksum"), "4.5", true, None, None).is_some());
        // Binaries NOT usable here → it must be ignored, forcing a source build.
        assert!(
            lookup_any("binpkg", "1.0", Some("cksum"), "4.5", false, None, None).is_none(),
            "a binary cache entry was served on a platform that cannot use binaries"
        );

        let _ = std::fs::remove_dir_all(global_packages_dir().join(&binary_key));
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
        store(&pkg_dir, &key, "testpkg", None).unwrap();

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
        store(&pkg_dir, &key, "testpkg", None).unwrap();
        assert!(lookup("testpkg", &key).is_some()); // now valid

        // Cleanup
        let _ = std::fs::remove_dir_all(global_packages_dir().join(&key));
    }

    #[test]
    fn store_replaces_mismatched_nested_provenance() {
        use crate::installer::nested_source::{self, NestedProvenance};

        let tmp = TempDir::new().unwrap();
        let pkg_dir = tmp.path().join("testpkg");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(
            pkg_dir.join("DESCRIPTION"),
            "Package: testpkg\nVersion: 1.0\n",
        )
        .unwrap();
        let expected = NestedProvenance {
            url:
                "https://api.github.com/repos/o/r/tarball/0123456789abcdef0123456789abcdef01234567"
                    .into(),
            checksum: "git:0123456789abcdef0123456789abcdef01234567".into(),
            subdirectory: "pkgs/testpkg".into(),
        };
        nested_source::write_marker(&pkg_dir, &expected).unwrap();

        let key = format!(
            "testpkg-1.0-nested{:023x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let existing = global_packages_dir().join(&key).join("testpkg");
        std::fs::create_dir_all(&existing).unwrap();
        std::fs::write(
            existing.join("DESCRIPTION"),
            "Package: testpkg\nVersion: 1.0\n",
        )
        .unwrap();
        let wrong = NestedProvenance {
            subdirectory: "pkgs/other".into(),
            ..expected.clone()
        };
        nested_source::write_marker(&existing, &wrong).unwrap();

        store(&pkg_dir, &key, "testpkg", None).unwrap();
        let cached = lookup("testpkg", &key).unwrap();
        assert!(nested_source::provenance_matches(&cached, Some(&expected)));
        let _ = std::fs::remove_dir_all(global_packages_dir().join(&key));
    }

    #[cfg(unix)]
    #[test]
    fn store_errors_when_corrupted_entry_cannot_be_removed() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().unwrap();
        let pkg_dir = tmp.path().join("testpkg");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(
            pkg_dir.join("DESCRIPTION"),
            "Package: testpkg\nVersion: 1.0\n",
        )
        .unwrap();

        let key = format!(
            "testpkg-1.0-poison{:023x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );

        // Corrupted entry (no DESCRIPTION) holding a read-only subdirectory:
        // remove_dir_all can't unlink the file inside a 0o555 dir, so the
        // poisoned entry survives the removal attempt.
        let entry = global_packages_dir().join(&key);
        let locked = entry.join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::write(locked.join("junk"), "x").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();

        // Sanity: the permission lock is a no-op when running as root (some
        // containers). Skip the test in that case.
        if std::fs::remove_file(locked.join("junk")).is_ok() {
            let _ = std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755));
            let _ = std::fs::remove_dir_all(&entry);
            return;
        }

        let result = store(&pkg_dir, &key, "testpkg", None);

        // Restore permissions before asserting so cleanup always succeeds.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::remove_dir_all(&entry).unwrap();

        assert!(
            result.is_err(),
            "store must not report success while a poisoned entry blocks the cache key"
        );
    }

    #[cfg(unix)]
    #[test]
    fn dir_size_skips_symlinks() {
        let tmp = TempDir::new().unwrap();

        // External data that must not count toward the entry's size.
        let external = tmp.path().join("external");
        std::fs::create_dir_all(&external).unwrap();
        std::fs::write(external.join("big"), vec![0u8; 8192]).unwrap();

        let entry = tmp.path().join("entry");
        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(entry.join("real"), b"12345").unwrap();
        std::os::unix::fs::symlink(&external, entry.join("link-dir")).unwrap();
        std::os::unix::fs::symlink(external.join("big"), entry.join("link-file")).unwrap();

        assert_eq!(dir_size(&entry), 5);
        // A symlink passed directly (e.g. a symlinked cache entry) counts as 0.
        assert_eq!(dir_size(&entry.join("link-dir")), 0);
    }

    #[cfg(unix)]
    #[test]
    fn dir_size_terminates_on_symlink_cycle() {
        let tmp = TempDir::new().unwrap();
        let entry = tmp.path().join("entry");
        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(entry.join("f"), b"xx").unwrap();
        // Self-referential loop: entry/loop -> entry.
        std::os::unix::fs::symlink(&entry, entry.join("loop")).unwrap();

        // Must terminate (no unbounded recursion) and not double-count.
        assert_eq!(dir_size(&entry), 2);
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
    fn clone_to_library_hardlinks_on_linux() {
        use std::os::unix::fs::MetadataExt;

        let tmp = TempDir::new().unwrap();
        let cache_pkg = tmp.path().join("cache").join("ggplot2");
        std::fs::create_dir_all(&cache_pkg).unwrap();
        std::fs::write(cache_pkg.join("DESCRIPTION"), "Package: ggplot2\n").unwrap();

        let library = tmp.path().join("library");
        std::fs::create_dir_all(&library).unwrap();

        clone_to_library(&cache_pkg, &library, "ggplot2").unwrap();

        // A real directory, not a link: this is what makes `.libPaths()` and
        // `find.package()` report the project library rather than the cache.
        let dest = library.join("ggplot2");
        let md = std::fs::symlink_metadata(&dest).unwrap();
        assert!(
            !md.file_type().is_symlink(),
            "expected a real directory, got {:?}",
            md.file_type()
        );
        assert!(dest.join("DESCRIPTION").exists());

        // Storage is still shared, so the attach stays near-instant and the
        // dedup across projects is unchanged.
        let cached = std::fs::metadata(cache_pkg.join("DESCRIPTION")).unwrap();
        let attached = std::fs::metadata(dest.join("DESCRIPTION")).unwrap();
        assert_eq!(
            cached.ino(),
            attached.ino(),
            "attach should share the inode"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cache_clean_leaves_a_hardlinked_library_usable() {
        // The reason for #248. Under the old directory-symlink attach this
        // library went dangling the moment the cache entry was removed.
        let tmp = TempDir::new().unwrap();
        let cache_pkg = tmp.path().join("cache").join("rlang");
        std::fs::create_dir_all(cache_pkg.join("R")).unwrap();
        std::fs::write(cache_pkg.join("DESCRIPTION"), "Package: rlang\n").unwrap();
        std::fs::write(cache_pkg.join("R/rlang.rdb"), b"payload").unwrap();

        let library = tmp.path().join("library");
        std::fs::create_dir_all(&library).unwrap();
        clone_to_library(&cache_pkg, &library, "rlang").unwrap();

        std::fs::remove_dir_all(tmp.path().join("cache")).unwrap();

        let dest = library.join("rlang");
        assert_eq!(
            std::fs::read_to_string(dest.join("DESCRIPTION")).unwrap(),
            "Package: rlang\n"
        );
        assert_eq!(std::fs::read(dest.join("R/rlang.rdb")).unwrap(), b"payload");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn clone_to_library_replaces_a_stale_real_dir() {
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

        // Now carries the cached content, and the stale tree is gone.
        let desc = std::fs::read_to_string(old.join("DESCRIPTION")).unwrap();
        assert!(desc.contains("xml2\n") && !desc.contains("stale"));
        assert!(!old.join("R").exists(), "stale subtree must not survive");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn clone_to_library_migrates_a_pre_248_symlink() {
        let tmp = TempDir::new().unwrap();
        let cache_a = tmp.path().join("cache-a").join("dplyr");
        let cache_b = tmp.path().join("cache-b").join("dplyr");
        for c in [&cache_a, &cache_b] {
            std::fs::create_dir_all(c).unwrap();
            std::fs::write(c.join("DESCRIPTION"), "Package: dplyr\n").unwrap();
        }

        let library = tmp.path().join("library");
        std::fs::create_dir_all(&library).unwrap();

        // Stand in for a library written by a pre-#248 uvr.
        let dest = library.join("dplyr");
        std::os::unix::fs::symlink(&cache_a, &dest).unwrap();

        clone_to_library(&cache_b, &library, "dplyr").unwrap();

        assert!(!dest.symlink_metadata().unwrap().file_type().is_symlink());
        assert!(dest.join("DESCRIPTION").exists());
    }

    /// The hardlink attach path is Windows-only in `clone_to_library`, but
    /// the tree-walking logic is platform-independent — exercise it
    /// everywhere so a regression can't hide until it reaches a Windows
    /// user (the platform with the least local testing).
    #[test]
    fn hardlink_dir_recursive_reproduces_the_tree_and_shares_storage() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src_pkg");
        std::fs::create_dir_all(src.join("R")).unwrap();
        std::fs::create_dir_all(src.join("Meta")).unwrap();
        std::fs::write(src.join("DESCRIPTION"), "Package: t\n").unwrap();
        std::fs::write(src.join("R/foo.R"), "foo <- 1").unwrap();
        std::fs::write(src.join("Meta/package.rds"), b"rds").unwrap();

        let dst = tmp.path().join("dst_pkg");
        hardlink_dir_recursive(&src, &dst).unwrap();

        assert_eq!(
            std::fs::read_to_string(dst.join("DESCRIPTION")).unwrap(),
            "Package: t\n"
        );
        assert!(dst.join("R/foo.R").exists());
        assert!(dst.join("Meta/package.rds").exists());

        // Storage is shared, not copied: same inode on Unix. (The Windows
        // equivalent is unobservable through std, hence the Unix gate — the
        // tree-shape assertions above still run everywhere.)
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let a = std::fs::metadata(src.join("R/foo.R")).unwrap();
            let b = std::fs::metadata(dst.join("R/foo.R")).unwrap();
            assert_eq!(a.ino(), b.ino(), "hardlink should share the inode");
            assert_eq!(b.nlink(), 2, "both paths should reference one inode");
        }
    }

    #[test]
    fn hardlink_dir_recursive_reproduces_symlinks_without_following_them() {
        // A cached package can contain SONAME symlinks (#203). They must
        // stay symlinks — following them would duplicate megabytes of
        // vendored shared libraries into every project library.
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src_pkg");
        std::fs::create_dir_all(src.join("libs")).unwrap();
        std::fs::write(src.join("libs/libtbb.so.2"), b"so bytes").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("libtbb.so.2", src.join("libs/libtbb.so")).unwrap();

        let dst = tmp.path().join("dst_pkg");
        hardlink_dir_recursive(&src, &dst).unwrap();

        assert!(dst.join("libs/libtbb.so.2").exists());
        #[cfg(unix)]
        {
            let link = dst.join("libs/libtbb.so");
            assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
            assert_eq!(
                std::fs::read_link(&link).unwrap(),
                std::path::PathBuf::from("libtbb.so.2")
            );
        }
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
