use std::path::Path;
use std::process::Command;

use flate2::read::GzDecoder;
use tracing::debug;

use crate::error::{Result, UvrError};

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

    // Patch libR.dylib references in all .so files when using managed R (macOS only).
    if cfg!(target_os = "macos") {
        if let Some(libr) = libr_path {
            if libr.exists() {
                let pkg_dir = library.join(package_name);
                let _ = patch_so_libr_refs(&pkg_dir, libr);
            }
        }
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

    // Atomic rename: staging/<package_name> → library/<package_name>
    let final_dest = library.join(package_name);
    let staged_pkg = staging_path.join(package_name);
    if staged_pkg.exists() {
        let _ = std::fs::remove_dir_all(&final_dest);
        std::fs::rename(&staged_pkg, &final_dest).map_err(|e| {
            UvrError::Other(format!(
                "Failed to move staged package '{}': {}",
                package_name, e
            ))
        })?;
    }
    // staging TempDir dropped here → auto-cleanup of any leftover files

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

        entry.unpack(&dest).map_err(|e| {
            UvrError::Other(format!(
                "Failed to extract '{}' from '{}': {}",
                path.display(),
                package_name,
                e
            ))
        })?;
    }

    // Atomic rename: staging/<package_name> → library/<package_name>
    let final_dest = library.join(package_name);
    let staged_pkg = staging_path.join(package_name);
    if staged_pkg.exists() {
        let _ = std::fs::remove_dir_all(&final_dest);
        std::fs::rename(&staged_pkg, &final_dest).map_err(|e| {
            UvrError::Other(format!(
                "Failed to move staged package '{}': {}",
                package_name, e
            ))
        })?;
    }

    debug!(
        "Extracted tgz for {package_name} into {}",
        library.display()
    );
    Ok(())
}

/// Public entry-point for retroactively patching already-installed packages.
/// Called by `uvr sync` to fix packages that were extracted before patching
/// support was added. Idempotent: no-op if the `.so` already points to `libr_path`.
pub fn patch_installed_so_files(pkg_dir: &Path, libr_path: &Path) {
    let _ = patch_so_libr_refs(pkg_dir, libr_path);
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
            if Command::new("install_name_tool")
                .args(["-change", old_dep, &new_dep, &path.to_string_lossy()])
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
            {
                changed = true;
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
}
