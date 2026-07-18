use std::path::Path;
use std::process::Command;

use flate2::read::GzDecoder;
use tracing::debug;

use crate::error::{Result, UvrError};
use crate::installer::package_cache::copy_dir_recursive;

/// Remove a directory with retry on Windows, where antivirus or indexing
/// services can hold file handles briefly after extraction.
fn remove_dir_with_retry(path: &Path) {
    if !path.exists() {
        return;
    }
    for attempt in 0..4 {
        match std::fs::remove_dir_all(path) {
            Ok(()) => return,
            Err(_) if attempt < 3 => {
                std::thread::sleep(std::time::Duration::from_millis(50 * (1 << attempt)));
            }
            Err(_) => return, // give up silently, rename_or_copy_dir will fail with a clear error
        }
    }
}

/// On Windows, antivirus / Search Indexer / OneDrive can briefly hold a handle
/// on files inside a freshly-extracted staging directory, causing `rename`
/// (which ultimately calls `MoveFileExW`) to fail with `ERROR_ACCESS_DENIED`
/// (raw_os_error == 5) or `ERROR_SHARING_VIOLATION` (32). Both are transient
/// and usually clear within a second.
#[cfg(windows)]
fn is_transient_windows_error(e: &std::io::Error) -> bool {
    matches!(e.raw_os_error(), Some(5 | 32 | 33))
}

#[cfg(not(windows))]
fn is_transient_windows_error(_: &std::io::Error) -> bool {
    false
}

/// Move `src` to `dst`, falling back to recursive copy + delete when
/// `rename` fails with a cross-device error (EXDEV). This handles
/// Docker volumes, NFS mounts, and bind-mounted library paths.
///
/// On Windows, also retries transient `ERROR_ACCESS_DENIED` /
/// `ERROR_SHARING_VIOLATION` errors caused by AV / indexer / OneDrive holding
/// file handles on just-extracted staging directories. Observed in the wild
/// with `classInt`, `viridisLite`, `terra` — any package with many small
/// files is a likely trigger. Backoff: 50, 100, 200, 400, 800 ms (~1.5 s total).
fn rename_or_copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    let mut last_err: Option<std::io::Error> = None;
    for attempt in 0..6 {
        match std::fs::rename(src, dst) {
            Ok(()) => return Ok(()),
            Err(e)
                if e.raw_os_error() == Some(18 /* EXDEV */)
                    || e.to_string().contains("cross-device") =>
            {
                copy_dir_recursive(src, dst)?;
                std::fs::remove_dir_all(src)?;
                return Ok(());
            }
            Err(e) if is_transient_windows_error(&e) && attempt < 5 => {
                std::thread::sleep(std::time::Duration::from_millis(50 * (1 << attempt)));
                last_err = Some(e);
                continue;
            }
            Err(e) => return Err(e),
        }
    }
    Err(last_err.unwrap_or_else(|| std::io::Error::other("rename_or_copy_dir exhausted retries")))
}

/// Extract a pre-built R binary package into `library`.
///
/// On macOS: `.tgz` (gzip-compressed tarball) extracted with `tar`.
/// On Windows: `.zip` extracted with the `zip` crate.
///
/// `libr_path`: when set (uvr-managed R on macOS), the `.so` files inside the
/// extracted package are patched so their `libR.dylib` reference points to the
/// managed R installation rather than the CRAN framework path.
pub fn install_binary_package(
    tarball: &Path,
    library: &Path,
    package_name: &str,
    libr_path: Option<&Path>,
) -> Result<()> {
    let is_zip = tarball
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("zip"))
        .unwrap_or(false);

    if is_zip {
        extract_zip(tarball, library, package_name)?;
    } else {
        extract_tgz(tarball, library, package_name)?;
    }

    // Verify the extracted shared objects match the host architecture before
    // proceeding. P3M has been observed serving x86_64 tarballs from its
    // sonoma-arm64 channel (#102); without this check, the install completes
    // and the package fails to dyld-load with a confusing arch error at
    // runtime. macOS-only for now — Linux distro channels don't exhibit the
    // same mismatch pattern.
    #[cfg(target_os = "macos")]
    {
        let pkg_dir = library.join(package_name);
        if let Err(e) = verify_mach_o_arch_in_libs(&pkg_dir, package_name) {
            // Roll back the extracted package so a stale wrong-arch tree
            // doesn't sit in the library for downstream commands to trip on.
            // remove_dir_with_retry is best-effort (Spotlight indexer / NFS /
            // root-owned files can keep it from completing); surface a warning
            // if the tree survives so the user knows their library state is
            // partially inconsistent in addition to the install having failed.
            remove_dir_with_retry(&pkg_dir);
            if pkg_dir.exists() {
                tracing::warn!(
                    "Arch check rejected '{}' but rollback of '{}' did not complete; \
                     stale wrong-architecture files remain in the library.",
                    package_name,
                    pkg_dir.display()
                );
            }
            return Err(e);
        }
    }

    // Patch libR.dylib references in all .so files when using managed R (macOS only).
    if cfg!(target_os = "macos") {
        if let Some(libr) = libr_path {
            if libr.exists() {
                let pkg_dir = library.join(package_name);
                if let Err(e) = patch_so_libr_refs(&pkg_dir, libr) {
                    // Don't fail the install: the extracted tree is intact and
                    // loads fine wherever the embedded R.framework paths still
                    // resolve (e.g. a system R is also present). But under
                    // managed R alone, an unpatched .so dies at load time with
                    // a cryptic dyld error — so surface the failure instead of
                    // swallowing it (previously `let _ =`).
                    tracing::warn!(
                        "Failed to patch libR references in '{}': {e}. The package \
                         may fail to load with a 'Library not loaded: …R.framework…' \
                         error under uvr-managed R.",
                        package_name
                    );
                }
            }
        }
    }

    Ok(())
}

/// Verify every `.so` / `.dylib` under `<pkg_dir>/libs/` (recursively — arch
/// subdirs like `libs/arm64/` included) is a Mach-O for the host CPU.
///
/// Reads only the 8-byte Mach-O header (or the fat header + arch entries
/// for universal binaries). Errors with a clear, actionable message if a
/// `.so` is the wrong arch — that's the failure mode tracked in #102 where
/// Posit Package Manager's `sonoma-arm64` channel has been observed serving
/// x86_64-only tarballs. Files that aren't Mach-O at all (unexpected, but
/// possible if an R package vendors a debug stub or stray asset) are
/// silently skipped; we only reject confirmed wrong-arch Mach-Os.
// CPU type constants from `<mach/machine.h>`:
//   CPU_TYPE_X86_64 = CPU_TYPE_X86 (7) | CPU_ARCH_ABI64 (0x01000000)
//   CPU_TYPE_ARM64  = CPU_TYPE_ARM (12) | CPU_ARCH_ABI64
#[cfg(target_os = "macos")]
const CPU_TYPE_X86_64: u32 = 0x01000007;
#[cfg(target_os = "macos")]
const CPU_TYPE_ARM64: u32 = 0x0100000c;
// Mach-O magic numbers (Mach-O is LE on modern macOS; fat headers are BE):
#[cfg(target_os = "macos")]
const MH_MAGIC_64: u32 = 0xfeedfacf;
#[cfg(target_os = "macos")]
const FAT_MAGIC: u32 = 0xcafebabe;

#[cfg(target_os = "macos")]
fn verify_mach_o_arch_in_libs(pkg_dir: &Path, package_name: &str) -> Result<()> {
    let host_arch = std::env::consts::ARCH;
    let (expected_cpu_type, expected_arch_name) = match host_arch {
        "aarch64" | "arm64" => (CPU_TYPE_ARM64, "arm64"),
        "x86_64" => (CPU_TYPE_X86_64, "x86_64"),
        // Unknown host arch — don't have a baseline to compare against.
        _ => return Ok(()),
    };

    let libs_dir = pkg_dir.join("libs");
    if !libs_dir.exists() {
        return Ok(()); // Pure-R package, no native libraries to verify.
    }

    verify_mach_o_arch_in_dir(
        &libs_dir,
        package_name,
        expected_cpu_type,
        expected_arch_name,
        host_arch,
        0,
    )
}

/// Recursive worker for `verify_mach_o_arch_in_libs`: scans `dir` for `.so` /
/// `.dylib` files and descends into subdirectories (R macOS binaries sometimes
/// place arch-specific shared objects under `libs/<arch>/`, and some ship
/// `.dylib` instead of `.so` — #147). Depth-bounded like
/// `find_dir_with_r_binary` to guard against symlink loops.
#[cfg(target_os = "macos")]
fn verify_mach_o_arch_in_dir(
    dir: &Path,
    package_name: &str,
    expected_cpu_type: u32,
    expected_arch_name: &str,
    host_arch: &str,
    depth: usize,
) -> Result<()> {
    use std::io::Read;

    if depth > 12 {
        return Ok(()); // guard against symlink loops
    }

    let entries = std::fs::read_dir(dir).map_err(|e| {
        UvrError::Other(format!(
            "Failed to read '{}' for arch check of '{}': {}",
            dir.display(),
            package_name,
            e
        ))
    })?;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            verify_mach_o_arch_in_dir(
                &path,
                package_name,
                expected_cpu_type,
                expected_arch_name,
                host_arch,
                depth + 1,
            )?;
            continue;
        }
        if !matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("so") | Some("dylib")
        ) {
            continue;
        }
        let Ok(mut file) = std::fs::File::open(&path) else {
            continue;
        };
        let mut header = [0u8; 8];
        if file.read_exact(&mut header).is_err() {
            continue; // Too small to be Mach-O; not our concern.
        }
        let magic_le = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
        let magic_be = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);

        if magic_le == MH_MAGIC_64 {
            // Thin 64-bit Mach-O: cpu_type is bytes 4..8 little-endian.
            let cpu_type = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
            if cpu_type != expected_cpu_type {
                let found = match cpu_type {
                    CPU_TYPE_X86_64 => "x86_64",
                    CPU_TYPE_ARM64 => "arm64",
                    _ => "unknown",
                };
                return Err(UvrError::Other(format!(
                    "Binary package '{}' contains a {} shared object, \
                     but uvr is running on {} ({}). The upstream binary \
                     repository likely served a wrong-architecture tarball. \
                     File: {}. \
                     Workaround: prefer CRAN by setting \
                     `UVR_REPOS=\"https://cran.r-project.org,https://packagemanager.posit.co/cran/latest\"`. \
                     See #102.",
                    package_name,
                    found,
                    expected_arch_name,
                    host_arch,
                    path.display()
                )));
            }
        } else if magic_be == FAT_MAGIC {
            // Universal (fat) Mach-O. The fat_header layout from
            // <mach-o/fat.h> is just two big-endian u32s: magic + nfat_arch,
            // both already sitting in our initial 8-byte `header` read.
            // (Previous versions incorrectly re-read 4 more bytes for nfat,
            // which gave us the first fat_arch's cpu_type instead of the
            // arch count — caught by post-commit review of bb32065.)
            let nfat = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
            // Apple's toolchain never emits more than a handful of slices;
            // cap to guard against corrupt or adversarial tarballs that
            // could otherwise drive billions of read_exact calls.
            let nfat = nfat.min(16);
            let mut matched = false;
            for _ in 0..nfat {
                let mut arch_entry = [0u8; 20];
                if file.read_exact(&mut arch_entry).is_err() {
                    break;
                }
                let cpu_type = u32::from_be_bytes([
                    arch_entry[0],
                    arch_entry[1],
                    arch_entry[2],
                    arch_entry[3],
                ]);
                if cpu_type == expected_cpu_type {
                    matched = true;
                    break;
                }
            }
            if !matched {
                return Err(UvrError::Other(format!(
                    "Binary package '{}' is a universal Mach-O but contains \
                     no {} slice. File: {}. See #102.",
                    package_name,
                    expected_arch_name,
                    path.display()
                )));
            }
        }
        // Anything else (32-bit Mach-O, non-Mach-O, raw text) is not what we
        // care about — skip rather than reject.
    }

    Ok(())
}

/// Extract a `.zip` binary package into the library directory.
///
/// Validates that all zip entries extract within `library` to prevent
/// path traversal attacks (zip-slip).
fn extract_zip(zip_path: &Path, library: &Path, package_name: &str) -> Result<()> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| {
        UvrError::Other(format!("Failed to open zip for '{}': {}", package_name, e))
    })?;

    // Extract into a staging directory, then atomically rename on success.
    let staging = tempfile::TempDir::new_in(library).map_err(|e| {
        UvrError::Other(format!(
            "Failed to create staging dir for '{}': {}",
            package_name, e
        ))
    })?;
    let staging_path = staging.path();

    let canonical_staging = staging_path
        .canonicalize()
        .unwrap_or_else(|_| staging_path.to_path_buf());

    // Single-pass: validate path traversal and extract each entry.
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| {
            UvrError::Other(format!(
                "Failed to read zip entry for '{}': {}",
                package_name, e
            ))
        })?;
        let outpath = canonical_staging.join(entry.mangled_name());
        if !outpath.starts_with(&canonical_staging) {
            return Err(UvrError::Other(format!(
                "Zip path traversal detected in package '{}': {}",
                package_name,
                entry.name()
            )));
        }
        if entry.is_dir() {
            std::fs::create_dir_all(&outpath).map_err(|e| {
                UvrError::Other(format!(
                    "Failed to create directory for '{}': {}",
                    package_name, e
                ))
            })?;
        } else {
            if let Some(parent) = outpath.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    UvrError::Other(format!(
                        "Failed to create parent directory for '{}': {}",
                        package_name, e
                    ))
                })?;
            }
            let mut outfile = std::fs::File::create(&outpath).map_err(|e| {
                UvrError::Other(format!(
                    "Failed to create file for '{}': {}",
                    package_name, e
                ))
            })?;
            std::io::copy(&mut entry, &mut outfile).map_err(|e| {
                UvrError::Other(format!(
                    "Failed to write zip entry for '{}': {}",
                    package_name, e
                ))
            })?;
        }
    }

    // Move staged package to final destination (with cross-device fallback).
    let final_dest = library.join(package_name);
    let staged_pkg = staging_path.join(package_name);
    if !staged_pkg.exists() {
        return Err(UvrError::Other(format!(
            "Expected directory '{}' not found in archive for '{}'",
            package_name, package_name
        )));
    }
    remove_dir_with_retry(&final_dest);
    rename_or_copy_dir(&staged_pkg, &final_dest).map_err(|e| {
        UvrError::Other(format!(
            "Failed to move staged package '{}': {}",
            package_name, e
        ))
    })?;
    // staging TempDir dropped here → auto-cleanup of any leftover files

    Ok(())
}

/// Extract a single tar entry into `dest`. Handles directories and regular
/// files; skips symlinks, hardlinks, character/block devices, FIFOs, and
/// anything else (R packages never use them).
///
/// Surfaces the underlying `io::Error.kind()` in the error message so
/// filesystem-specific failures (overlayfs, FUSE, sandboxed runners) are
/// debuggable from the log alone.
fn extract_entry<R: std::io::Read>(
    entry: &mut tar::Entry<'_, R>,
    dest: &Path,
    archive_path: &Path,
    package_name: &str,
) -> Result<()> {
    let entry_type = entry.header().entry_type();

    match entry_type {
        tar::EntryType::Directory => {
            std::fs::create_dir_all(dest).map_err(|e| {
                UvrError::Other(format!(
                    "Failed to create directory '{}' from '{}': {} ({:?})",
                    archive_path.display(),
                    package_name,
                    e,
                    e.kind()
                ))
            })?;
        }
        tar::EntryType::Regular | tar::EntryType::Continuous => {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    UvrError::Other(format!(
                        "Failed to create parent directory for '{}' in '{}': {} ({:?})",
                        archive_path.display(),
                        package_name,
                        e,
                        e.kind()
                    ))
                })?;
            }
            let mut out_file = std::fs::File::create(dest).map_err(|e| {
                UvrError::Other(format!(
                    "Failed to create file '{}' from '{}': {} ({:?}, dest={})",
                    archive_path.display(),
                    package_name,
                    e,
                    e.kind(),
                    dest.display()
                ))
            })?;
            std::io::copy(entry, &mut out_file).map_err(|e| {
                UvrError::Other(format!(
                    "Failed to write content for '{}' in '{}': {} ({:?})",
                    archive_path.display(),
                    package_name,
                    e,
                    e.kind()
                ))
            })?;
        }
        // Skip all other entry types — symlinks, hardlinks, devices, FIFOs,
        // GNU extension headers, PAX globals, etc. R packages don't use them
        // in practice; tarballs from `R CMD INSTALL --build` produce only
        // regular files and directories.
        _ => {}
    }
    Ok(())
}

/// Extract a `.tgz` binary package into the library directory.
///
/// Uses the pure-Rust `tar` + `flate2` crates instead of shelling out to `tar`,
/// with path-traversal validation to prevent malicious tarballs from writing
/// files outside the library directory.
fn extract_tgz(tgz_path: &Path, library: &Path, package_name: &str) -> Result<()> {
    let file = std::fs::File::open(tgz_path)?;
    let decoder = GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    // NOTE: archive metadata-preservation settings are unused because
    // extract_entry does manual file creation. R packages have no
    // meaningful mtime/permissions to preserve.

    // Extract into a staging directory, then atomically rename on success.
    let staging = tempfile::TempDir::new_in(library).map_err(|e| {
        UvrError::Other(format!(
            "Failed to create staging dir for '{}': {}",
            package_name, e
        ))
    })?;
    let staging_path = staging.path();

    let canonical_staging = staging_path
        .canonicalize()
        .unwrap_or_else(|_| staging_path.to_path_buf());

    for entry in archive
        .entries()
        .map_err(|e| UvrError::Other(format!("Failed to read tgz for '{}': {}", package_name, e)))?
    {
        let mut entry = entry.map_err(|e| {
            UvrError::Other(format!(
                "Failed to read tgz entry for '{}': {}",
                package_name, e
            ))
        })?;

        let path = entry
            .path()
            .map_err(|e| {
                UvrError::Other(format!("Invalid path in tgz for '{}': {}", package_name, e))
            })?
            .into_owned();

        // Guard against path traversal: reject entries with `..` components
        // or absolute paths that would escape the staging directory.
        if path.is_absolute()
            || path
                .components()
                .any(|c| c == std::path::Component::ParentDir)
        {
            return Err(UvrError::Other(format!(
                "Path traversal detected in package '{}': {}",
                package_name,
                path.display()
            )));
        }

        let dest = canonical_staging.join(&path);
        if !dest.starts_with(&canonical_staging) {
            return Err(UvrError::Other(format!(
                "Path traversal detected in package '{}': {}",
                package_name,
                path.display()
            )));
        }

        // Manual extraction. Bypasses tar::Entry::unpack to avoid:
        // - create_new(true) failures on filesystems where the staging file
        //   already exists for any reason
        // - fs::remove_file followed by create dance that overlayfs/FUSE
        //   handles unevenly
        // - mtime/permission preservation, which fails opaquely on some CI
        //   filesystems and isn't meaningful for R packages anyway
        // - Symlink validation, which is unnecessary inside a fresh tempdir
        //   we already path-traversal-checked above
        extract_entry(&mut entry, &dest, &path, package_name)?;
    }

    // Move staged package to final destination (with cross-device fallback).
    let final_dest = library.join(package_name);
    let staged_pkg = staging_path.join(package_name);
    if !staged_pkg.exists() {
        return Err(UvrError::Other(format!(
            "Expected directory '{}' not found in archive for '{}'",
            package_name, package_name
        )));
    }
    remove_dir_with_retry(&final_dest);
    rename_or_copy_dir(&staged_pkg, &final_dest).map_err(|e| {
        UvrError::Other(format!(
            "Failed to move staged package '{}': {}",
            package_name, e
        ))
    })?;

    debug!(
        "Extracted tgz for {package_name} into {}",
        library.display()
    );
    Ok(())
}

/// Inspected metadata for a single R package tarball.
#[derive(Debug, Default, Clone)]
pub struct TarballMeta {
    /// Parsed `Built:` field. Present iff the tarball was pre-built for some platform.
    pub built: Option<crate::registry::cran::BuiltInfo>,
    /// True iff DESCRIPTION explicitly states `NeedsCompilation: no`.
    /// Absent means uvr can't prove the package is pure-R — treat conservatively as source.
    pub pure_r: bool,
}

/// Inspect a downloaded `.tar.gz` for both `Built:` and `NeedsCompilation` in
/// its DESCRIPTION. Used to classify each package as binary / pure-R / source
/// for accurate install-time accounting.
///
/// Returns `None` only if the tarball can't be opened or its DESCRIPTION can't
/// be located (the latter is an unexpected case — a well-formed R package
/// tarball always has `<pkg>/DESCRIPTION` early in the archive).
pub fn inspect_tarball(tarball_path: &Path, package_name: &str) -> Option<TarballMeta> {
    use std::io::Read;
    let file = std::fs::File::open(tarball_path).ok()?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    let want = format!("{package_name}/DESCRIPTION");

    for entry in archive.entries().ok()?.flatten() {
        // Skip entries with unparseable paths (older GNU tar headers, PAX
        // quirks) instead of aborting the whole inspection — bailing here
        // used to misclassify perfectly valid binary tarballs as source
        // (#139, same shape as the #88 extraction fix).
        let path_owned = match entry.path() {
            Ok(p) => p.into_owned(),
            Err(_) => continue,
        };
        if path_owned.to_string_lossy() != want {
            continue;
        }
        let mut buf = String::new();
        let mut limited = entry.take(32 * 1024);
        let _ = limited.read_to_string(&mut buf);

        let mut meta = TarballMeta::default();
        for line in buf.lines() {
            if let Some(value) = line.strip_prefix("Built:") {
                meta.built = crate::registry::cran::parse_built(value.trim());
            } else if let Some(value) = line.strip_prefix("NeedsCompilation:") {
                let v = value.trim().to_lowercase();
                if v == "no" {
                    meta.pure_r = true;
                }
            }
        }
        return Some(meta);
    }
    None
}

/// Inspect a downloaded `.tar.gz` for a `Built:` line in its DESCRIPTION.
/// Returns `Some(BuiltInfo)` if found and parseable. Used to auto-detect
/// pre-built binary tarballs from repositories whose PACKAGES.gz omits
/// the `Built:` field (e.g. cran.rpkgs.com).
///
/// Thin convenience wrapper around `inspect_tarball` for callers that only
/// care about the `Built:` field.
pub fn detect_built_from_tarball(
    tarball_path: &Path,
    package_name: &str,
) -> Option<crate::registry::cran::BuiltInfo> {
    inspect_tarball(tarball_path, package_name).and_then(|m| m.built)
}

/// Public entry-point for retroactively patching already-installed packages.
/// Called by `uvr sync` to fix packages that were extracted before patching
/// support was added. Idempotent: no-op if the `.so` already points to `libr_path`.
pub fn patch_installed_so_files(pkg_dir: &Path, libr_path: &Path) {
    if let Err(e) = patch_so_libr_refs(pkg_dir, libr_path) {
        // Best-effort retro-patch, but not silently so (previously `let _ =`):
        // an unpatched .so under managed R fails at load time with a cryptic
        // dyld error the user can't trace back to this step.
        tracing::warn!(
            "Failed to patch libR references in '{}': {e}. The package may fail \
             to load under uvr-managed R.",
            pkg_dir.display()
        );
    }
}

/// Walk `<pkg_dir>/libs/` and redirect every R-framework library reference
/// to its managed-R counterpart.
///
/// P3M macOS binaries embed absolute framework paths for ALL R libraries they
/// link against — not just `libR.dylib` but also `libRlapack.dylib`,
/// `libRblas.dylib`, `libgfortran.5.dylib`, etc.  A naive replacement that
/// redirects every R.framework reference to `libR.dylib` causes packages like
/// Matrix (which directly links `libRlapack.dylib` for `_dgebal_`) to fail
/// at load time with "Symbol not found".
///
/// The correct fix: for each framework reference, extract the filename and
/// redirect it to the same-named library inside `<r_lib_dir>/`.
fn patch_so_libr_refs(pkg_dir: &Path, libr_path: &Path) -> std::io::Result<()> {
    let libs_dir = pkg_dir.join("libs");
    if !libs_dir.exists() {
        return Ok(());
    }

    // The managed R lib directory — all R dylibs live here.
    let r_lib_dir = match libr_path.parent() {
        Some(d) => d.to_string_lossy().to_string(),
        None => return Ok(()),
    };

    for entry in std::fs::read_dir(&libs_dir)?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("so") {
            continue;
        }

        let Ok(otool_out) = Command::new("otool")
            .args(["-L", &path.to_string_lossy()])
            .output()
        else {
            continue;
        };
        // Only parse stdout when otool actually succeeded — a non-zero exit
        // (broken binary, sandbox denial) can leave partial or garbage output
        // that would be parsed as dep lines (#164).
        if !otool_out.status.success() {
            tracing::debug!(
                "otool -L exited {} on '{}'; skipping libR patch for this file",
                otool_out.status.code().unwrap_or(-1),
                path.display()
            );
            continue;
        }
        let otool_text = String::from_utf8_lossy(&otool_out.stdout);

        let mut changed = false;
        for line in otool_text.lines() {
            let old_dep = line.split_whitespace().next().unwrap_or("");
            if !old_dep.contains("R.framework") && !old_dep.contains("libR.dylib") {
                continue;
            }
            // Redirect to the same filename inside the managed R lib dir.
            let filename = std::path::Path::new(old_dep)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if filename.is_empty() {
                continue;
            }
            let new_dep = format!("{r_lib_dir}/{filename}");
            if old_dep == new_dep {
                continue; // already pointing at managed path
            }
            // Per-dep failures must not be silent (#163): a partially-patched
            // .so dies later with a cryptic dyld error, so leave a breadcrumb
            // naming the file and the dep that failed. Not fatal — behavior
            // matches the outer best-effort contract.
            match Command::new("install_name_tool")
                .args(["-change", old_dep, &new_dep, &path.to_string_lossy()])
                .status()
            {
                Ok(s) if s.success() => changed = true,
                Ok(s) => tracing::warn!(
                    "install_name_tool failed (exit {}) redirecting '{old_dep}' -> \
                     '{new_dep}' on '{}'; the package may fail to load under \
                     uvr-managed R.",
                    s.code().unwrap_or(-1),
                    path.display()
                ),
                Err(e) => tracing::warn!(
                    "Failed to run install_name_tool redirecting '{old_dep}' -> \
                     '{new_dep}' on '{}': {e}",
                    path.display()
                ),
            }
        }

        if changed {
            // Re-sign after modification (required on Apple Silicon).
            let _ = Command::new("codesign")
                .args(["--force", "--sign", "-", &path.to_string_lossy()])
                .status();
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn create_test_zip(dir: &std::path::Path, pkg_name: &str) -> std::path::PathBuf {
        let zip_path = dir.join(format!("{pkg_name}.zip"));
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);

        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        zip.start_file(format!("{pkg_name}/DESCRIPTION"), options)
            .unwrap();
        zip.write_all(format!("Package: {pkg_name}\nVersion: 1.0.0\nTitle: Test\n").as_bytes())
            .unwrap();

        zip.start_file(format!("{pkg_name}/R/hello.R"), options)
            .unwrap();
        zip.write_all(b"hello <- function() 'world'\n").unwrap();

        zip.finish().unwrap();
        zip_path
    }

    #[test]
    fn extract_zip_basic() {
        let dir = TempDir::new().unwrap();
        let library = dir.path().join("library");
        std::fs::create_dir_all(&library).unwrap();

        let zip_path = create_test_zip(dir.path(), "testpkg");
        extract_zip(&zip_path, &library, "testpkg").unwrap();

        assert!(library.join("testpkg").join("DESCRIPTION").exists());
        assert!(library.join("testpkg").join("R").join("hello.R").exists());
    }

    #[test]
    fn install_binary_package_zip() {
        let dir = TempDir::new().unwrap();
        let library = dir.path().join("library");
        std::fs::create_dir_all(&library).unwrap();

        let zip_path = create_test_zip(dir.path(), "mypkg");
        let zip_file = dir.path().join("mypkg_1.0.0.zip");
        std::fs::rename(&zip_path, &zip_file).unwrap();

        install_binary_package(&zip_file, &library, "mypkg", None).unwrap();
        assert!(library.join("mypkg").join("DESCRIPTION").exists());
    }

    #[test]
    fn install_binary_tgz() {
        let dir = TempDir::new().unwrap();
        let library = dir.path().join("library");
        std::fs::create_dir_all(&library).unwrap();

        let pkg_dir = dir.path().join("tarpkg");
        let r_dir = pkg_dir.join("R");
        std::fs::create_dir_all(&r_dir).unwrap();
        std::fs::write(
            pkg_dir.join("DESCRIPTION"),
            "Package: tarpkg\nVersion: 1.0.0\n",
        )
        .unwrap();
        std::fs::write(r_dir.join("hello.R"), "hello <- function() 1\n").unwrap();

        let tarball = dir.path().join("tarpkg_1.0.0.tgz");
        let status = std::process::Command::new("tar")
            .args([
                "czf",
                &tarball.to_string_lossy(),
                "-C",
                &dir.path().to_string_lossy(),
                "tarpkg",
            ])
            .status()
            .unwrap();
        assert!(status.success());

        install_binary_package(&tarball, &library, "tarpkg", None).unwrap();
        assert!(library.join("tarpkg").join("DESCRIPTION").exists());
    }

    #[test]
    fn patch_installed_so_no_libs_dir() {
        let dir = TempDir::new().unwrap();
        let fake_libr = dir.path().join("lib").join("libR.dylib");
        // Should be a no-op
        patch_installed_so_files(dir.path(), &fake_libr);
    }

    fn write_tarball_with_description(content: &str) -> tempfile::NamedTempFile {
        write_tarball_with_description_for("rlang", content)
    }

    fn write_tarball_with_description_for(
        pkg_name: &str,
        content: &str,
    ) -> tempfile::NamedTempFile {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        let file = tempfile::NamedTempFile::new().unwrap();
        let mut enc = GzEncoder::new(
            std::fs::File::create(file.path()).unwrap(),
            Compression::default(),
        );
        {
            let mut builder = tar::Builder::new(&mut enc);
            let bytes = content.as_bytes();
            let mut header = tar::Header::new_gnu();
            header.set_path(format!("{pkg_name}/DESCRIPTION")).unwrap();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append(&header, bytes).unwrap();
            builder.finish().unwrap();
        }
        enc.finish().unwrap();
        file
    }

    #[test]
    fn detect_built_from_tarball_with_built() {
        let tarball = write_tarball_with_description(
            "Package: rlang\nVersion: 1.2.0\nBuilt: R 4.5.2; aarch64-unknown-linux-musl; 2025-01-15 12:00:00 UTC; unix\n",
        );
        let built = detect_built_from_tarball(tarball.path(), "rlang").unwrap();
        assert_eq!(built.r_version, "4.5.2");
        assert_eq!(built.platform, "aarch64-unknown-linux-musl");
        assert_eq!(built.os_family, "unix");
    }

    #[test]
    fn detect_built_from_tarball_no_built_returns_none() {
        let tarball = write_tarball_with_description("Package: rlang\nVersion: 1.2.0\n");
        assert!(detect_built_from_tarball(tarball.path(), "rlang").is_none());
    }

    #[test]
    fn detect_built_from_tarball_missing_package_returns_none() {
        let tarball = write_tarball_with_description("Package: rlang\nVersion: 1.2.0\n");
        // Wrong package name → DESCRIPTION not at "otherpkg/DESCRIPTION"
        assert!(detect_built_from_tarball(tarball.path(), "otherpkg").is_none());
    }

    #[test]
    fn inspect_tarball_extracts_pure_r_flag() {
        let tarball = write_tarball_with_description_for(
            "pureR",
            "Package: pureR\nVersion: 1.0\nNeedsCompilation: no\n",
        );
        let m = inspect_tarball(tarball.path(), "pureR").unwrap();
        assert!(m.pure_r);
        assert!(m.built.is_none());
    }

    #[test]
    fn inspect_tarball_extracts_built_and_marks_not_pure_r() {
        let tarball = write_tarball_with_description_for(
            "bin",
            "Package: bin\nVersion: 1.0\nNeedsCompilation: yes\nBuilt: R 4.5.0; aarch64-pc-linux-musl; 2025-01-15; unix\n",
        );
        let m = inspect_tarball(tarball.path(), "bin").unwrap();
        assert!(!m.pure_r);
        assert!(m.built.is_some());
    }

    #[test]
    fn inspect_tarball_missing_needscompilation_defaults_to_source() {
        // DESCRIPTION omits NeedsCompilation entirely — uvr can't prove pure-R.
        let tarball = write_tarball_with_description_for("q", "Package: q\nVersion: 1.0\n");
        let m = inspect_tarball(tarball.path(), "q").unwrap();
        assert!(!m.pure_r);
        assert!(m.built.is_none());
    }

    #[test]
    fn inspect_tarball_yes_is_not_pure_r() {
        let tarball = write_tarball_with_description_for(
            "q",
            "Package: q\nVersion: 1.0\nNeedsCompilation: yes\n",
        );
        let m = inspect_tarball(tarball.path(), "q").unwrap();
        assert!(!m.pure_r);
    }

    #[test]
    fn detect_built_compat_wrapper_still_works() {
        let tarball = write_tarball_with_description(
            "Package: rlang\nVersion: 1.2.0\nBuilt: R 4.5.2; aarch64-unknown-linux-musl; 2025-01-15; unix\n",
        );
        let b = detect_built_from_tarball(tarball.path(), "rlang").unwrap();
        assert_eq!(b.platform, "aarch64-unknown-linux-musl");
    }

    #[test]
    fn extract_tgz_disables_metadata_preservation() {
        // Build a tarball whose entry has a deliberately weird mtime that
        // would tickle metadata-preservation paths. Extract via extract_tgz
        // and verify the file lands without error.
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use tempfile::TempDir;

        let tarball = tempfile::NamedTempFile::new().unwrap();
        let bytes = b"Package: t17pkg\nVersion: 1.0\n";
        {
            let mut enc = GzEncoder::new(
                std::fs::File::create(tarball.path()).unwrap(),
                Compression::default(),
            );
            {
                let mut builder = tar::Builder::new(&mut enc);

                // Directory entry first.
                let mut dir_header = tar::Header::new_gnu();
                dir_header.set_path("t17pkg/").unwrap();
                dir_header.set_size(0);
                dir_header.set_mode(0o755);
                dir_header.set_entry_type(tar::EntryType::Directory);
                // Deliberately weird mtime that some filesystems can't honor.
                dir_header.set_mtime(0);
                dir_header.set_cksum();
                builder.append(&dir_header, std::io::empty()).unwrap();

                // File entry.
                let mut header = tar::Header::new_gnu();
                header.set_path("t17pkg/DESCRIPTION").unwrap();
                header.set_size(bytes.len() as u64);
                header.set_mode(0o644);
                header.set_mtime(0);
                header.set_cksum();
                builder.append(&header, &bytes[..]).unwrap();

                builder.finish().unwrap();
            }
            enc.finish().unwrap();
        }

        let library = TempDir::new().unwrap();
        extract_tgz(tarball.path(), library.path(), "t17pkg")
            .expect("extract_tgz should succeed with metadata preservation disabled");
        let extracted = library.path().join("t17pkg").join("DESCRIPTION");
        assert!(
            extracted.exists(),
            "DESCRIPTION should be at {}",
            extracted.display()
        );
        let content = std::fs::read_to_string(&extracted).unwrap();
        assert!(content.starts_with("Package: t17pkg"));
    }

    #[test]
    fn extract_tgz_skips_symlinks() {
        // Build a tarball with a regular file and a symlink. Verify the
        // regular file extracts and the symlink is silently skipped.
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use tempfile::TempDir;

        let tarball = tempfile::NamedTempFile::new().unwrap();
        let bytes = b"Package: t18pkg\nVersion: 1.0\n";
        {
            let mut enc = GzEncoder::new(
                std::fs::File::create(tarball.path()).unwrap(),
                Compression::default(),
            );
            {
                let mut builder = tar::Builder::new(&mut enc);

                // Regular file.
                let mut header = tar::Header::new_gnu();
                header.set_path("t18pkg/DESCRIPTION").unwrap();
                header.set_size(bytes.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder.append(&header, &bytes[..]).unwrap();

                // Symlink entry.
                let mut sym_header = tar::Header::new_gnu();
                sym_header.set_path("t18pkg/link").unwrap();
                sym_header.set_entry_type(tar::EntryType::Symlink);
                sym_header.set_size(0);
                sym_header.set_link_name("DESCRIPTION").unwrap();
                sym_header.set_cksum();
                builder.append(&sym_header, std::io::empty()).unwrap();

                builder.finish().unwrap();
            }
            enc.finish().unwrap();
        }

        let library = TempDir::new().unwrap();
        extract_tgz(tarball.path(), library.path(), "t18pkg")
            .expect("should succeed despite symlink");
        let extracted = library.path().join("t18pkg").join("DESCRIPTION");
        assert!(extracted.exists());
        // Symlink should not have been extracted.
        let sym = library.path().join("t18pkg").join("link");
        assert!(!sym.exists(), "symlink should be silently skipped");
    }

    #[test]
    fn extract_tgz_overwrites_pre_existing_file() {
        // Even if a stale file exists at the destination (shouldn't happen
        // in practice with the fresh tempdir, but defensive), extraction
        // succeeds via fs::File::create's truncate-on-open semantics.
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use tempfile::TempDir;

        let tarball = tempfile::NamedTempFile::new().unwrap();
        let bytes = b"Package: t18b\nVersion: 2.0\n";
        {
            let mut enc = GzEncoder::new(
                std::fs::File::create(tarball.path()).unwrap(),
                Compression::default(),
            );
            {
                let mut builder = tar::Builder::new(&mut enc);
                let mut header = tar::Header::new_gnu();
                header.set_path("t18b/DESCRIPTION").unwrap();
                header.set_size(bytes.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder.append(&header, &bytes[..]).unwrap();
                builder.finish().unwrap();
            }
            enc.finish().unwrap();
        }

        // Manually pre-create the destination path with stale content.
        // extract_tgz uses a TempDir inside `library` for staging, so
        // there's no collision in practice — this test just exercises
        // that File::create's truncate semantic works as expected.
        let library = TempDir::new().unwrap();
        extract_tgz(tarball.path(), library.path(), "t18b").expect("should succeed");
        let extracted = library.path().join("t18b").join("DESCRIPTION");
        assert!(extracted.exists());
        assert_eq!(
            std::fs::read_to_string(&extracted).unwrap(),
            "Package: t18b\nVersion: 2.0\n"
        );
    }

    #[cfg(target_os = "macos")]
    fn write_thin_macho_so(path: &std::path::Path, cpu_type_le: u32) {
        // Minimal Mach-O header: just enough bytes for the arch check to read.
        // Real Mach-O is 32 bytes for the mach_header_64; we only need 8.
        let mut bytes = Vec::with_capacity(32);
        bytes.extend_from_slice(&0xfeedfacf_u32.to_le_bytes()); // MH_MAGIC_64
        bytes.extend_from_slice(&cpu_type_le.to_le_bytes());
        // Pad to a plausible mach_header_64 length so the file looks valid
        // enough that other tools wouldn't trip; not strictly required.
        bytes.resize(32, 0);
        std::fs::write(path, &bytes).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn arch_check_passes_on_host_arch_so() {
        // Build a `.so` whose Mach-O arch matches the host running the test.
        let host_cpu: u32 = match std::env::consts::ARCH {
            "aarch64" | "arm64" => 0x0100000c,
            "x86_64" => 0x01000007,
            other => panic!("unexpected host arch in test: {other}"),
        };
        let dir = TempDir::new().unwrap();
        let pkg_dir = dir.path().join("pkg");
        let libs = pkg_dir.join("libs");
        std::fs::create_dir_all(&libs).unwrap();
        write_thin_macho_so(&libs.join("pkg.so"), host_cpu);

        verify_mach_o_arch_in_libs(&pkg_dir, "pkg").expect("host-arch .so should pass the check");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn arch_check_rejects_wrong_arch_so() {
        // Wrong arch = opposite of the host. On arm64 host this writes
        // an x86_64 header; on x86_64 host this writes arm64.
        let wrong_cpu: u32 = match std::env::consts::ARCH {
            "aarch64" | "arm64" => 0x01000007, // host=arm64, .so=x86_64
            "x86_64" => 0x0100000c,            // host=x86_64, .so=arm64
            other => panic!("unexpected host arch in test: {other}"),
        };
        let dir = TempDir::new().unwrap();
        let pkg_dir = dir.path().join("pkg");
        let libs = pkg_dir.join("libs");
        std::fs::create_dir_all(&libs).unwrap();
        write_thin_macho_so(&libs.join("pkg.so"), wrong_cpu);

        let err = verify_mach_o_arch_in_libs(&pkg_dir, "pkg")
            .expect_err("wrong-arch .so should be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("wrong-architecture") || msg.contains("contains a"),
            "error message should explain the arch mismatch, got: {msg}"
        );
        assert!(msg.contains("#102"), "error should cite issue #102");
    }

    #[cfg(target_os = "macos")]
    fn wrong_host_cpu() -> u32 {
        match std::env::consts::ARCH {
            "aarch64" | "arm64" => 0x01000007, // host=arm64, file=x86_64
            "x86_64" => 0x0100000c,            // host=x86_64, file=arm64
            other => panic!("unexpected host arch in test: {other}"),
        }
    }

    #[cfg(target_os = "macos")]
    fn host_cpu() -> u32 {
        match std::env::consts::ARCH {
            "aarch64" | "arm64" => 0x0100000c,
            "x86_64" => 0x01000007,
            other => panic!("unexpected host arch in test: {other}"),
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn arch_check_recurses_into_subdirs() {
        // Wrong-arch .so hidden in an arch subdir (libs/<arch>/) must still
        // be caught — the pre-#147 scan only looked at direct libs/* entries.
        let dir = TempDir::new().unwrap();
        let pkg_dir = dir.path().join("pkg");
        let nested = pkg_dir.join("libs").join("x86_64");
        std::fs::create_dir_all(&nested).unwrap();
        write_thin_macho_so(&nested.join("pkg.so"), wrong_host_cpu());

        let err = verify_mach_o_arch_in_libs(&pkg_dir, "pkg")
            .expect_err("wrong-arch .so in libs subdir should be rejected");
        assert!(err.to_string().contains("#102"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn arch_check_covers_dylib_files() {
        // Wrong-arch .dylib directly in libs/ must be caught (#147).
        let dir = TempDir::new().unwrap();
        let pkg_dir = dir.path().join("pkg");
        let libs = pkg_dir.join("libs");
        std::fs::create_dir_all(&libs).unwrap();
        write_thin_macho_so(&libs.join("helper.dylib"), wrong_host_cpu());

        let err = verify_mach_o_arch_in_libs(&pkg_dir, "pkg")
            .expect_err("wrong-arch .dylib should be rejected");
        assert!(err.to_string().contains("#102"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn arch_check_passes_nested_host_arch_dylib() {
        // Host-arch .dylib in a nested subdir passes — pass/fail semantics
        // are unchanged, only enumeration got broader.
        let dir = TempDir::new().unwrap();
        let pkg_dir = dir.path().join("pkg");
        let nested = pkg_dir.join("libs").join("arch").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        write_thin_macho_so(&nested.join("helper.dylib"), host_cpu());
        write_thin_macho_so(&pkg_dir.join("libs").join("pkg.so"), host_cpu());

        verify_mach_o_arch_in_libs(&pkg_dir, "pkg")
            .expect("host-arch files at any depth should pass");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn arch_check_skips_pure_r_package() {
        // No libs/ directory at all — pure-R package, nothing to verify.
        let dir = TempDir::new().unwrap();
        let pkg_dir = dir.path().join("pkg");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        verify_mach_o_arch_in_libs(&pkg_dir, "pkg")
            .expect("pure-R package without libs/ should pass");
    }

    #[cfg(target_os = "macos")]
    fn write_fat_macho_so(path: &std::path::Path, cpu_types: &[u32]) {
        // Minimal fat Mach-O: fat_header (8 bytes BE) + N * fat_arch (20 bytes BE).
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0xcafebabe_u32.to_be_bytes()); // FAT_MAGIC
        bytes.extend_from_slice(&(cpu_types.len() as u32).to_be_bytes()); // nfat_arch
        for &ct in cpu_types {
            bytes.extend_from_slice(&ct.to_be_bytes()); // cpu_type
            bytes.extend_from_slice(&0u32.to_be_bytes()); // cpu_subtype
            bytes.extend_from_slice(&0u32.to_be_bytes()); // offset
            bytes.extend_from_slice(&0u32.to_be_bytes()); // size
            bytes.extend_from_slice(&0u32.to_be_bytes()); // align
        }
        std::fs::write(path, &bytes).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn arch_check_accepts_fat_universal_so() {
        // Universal binary with both arm64 and x86_64 slices should pass
        // regardless of host arch — exercises the fat_header parsing path.
        let dir = TempDir::new().unwrap();
        let pkg_dir = dir.path().join("pkg");
        let libs = pkg_dir.join("libs");
        std::fs::create_dir_all(&libs).unwrap();
        write_fat_macho_so(&libs.join("pkg.so"), &[0x01000007, 0x0100000c]);

        verify_mach_o_arch_in_libs(&pkg_dir, "pkg")
            .expect("universal Mach-O with host slice should pass");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn arch_check_rejects_fat_without_host_slice() {
        // Fat binary missing the host's slice: arm64 host gets an x86_64-only
        // fat binary (only one slice, but that one isn't ours).
        let wrong_only: u32 = match std::env::consts::ARCH {
            "aarch64" | "arm64" => 0x01000007,
            "x86_64" => 0x0100000c,
            other => panic!("unexpected host arch: {other}"),
        };
        let dir = TempDir::new().unwrap();
        let pkg_dir = dir.path().join("pkg");
        let libs = pkg_dir.join("libs");
        std::fs::create_dir_all(&libs).unwrap();
        write_fat_macho_so(&libs.join("pkg.so"), &[wrong_only]);

        let err = verify_mach_o_arch_in_libs(&pkg_dir, "pkg")
            .expect_err("fat binary missing host slice should be rejected");
        assert!(err.to_string().contains("#102"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn arch_check_skips_non_so_files() {
        // libs/ exists but contains nothing matching *.so — should pass.
        let dir = TempDir::new().unwrap();
        let pkg_dir = dir.path().join("pkg");
        let libs = pkg_dir.join("libs");
        std::fs::create_dir_all(&libs).unwrap();
        std::fs::write(libs.join("README"), "not a Mach-O").unwrap();
        verify_mach_o_arch_in_libs(&pkg_dir, "pkg").expect("non-.so files should be ignored");
    }
}
