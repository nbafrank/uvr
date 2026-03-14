use std::path::Path;
use std::process::Command;

use crate::error::{Result, UvrError};

/// Extract a pre-built R binary package (`.tgz`) directly into `library`.
///
/// P3M binary packages are gzip-compressed tarballs where the top-level entry
/// is the package directory: `<pkg>/DESCRIPTION`, `<pkg>/R/`, `<pkg>/libs/`, …
/// A plain `tar xf pkg.tgz -C <library>` places it at `<library>/<pkg>/`.
///
/// `libr_path`: when set (uvr-managed R), the `.so` files inside the extracted
/// package are patched so their `libR.dylib` reference points to the managed R
/// installation rather than the CRAN framework path they were compiled against.
/// This is necessary because P3M binary packages embed an absolute framework path
/// (`/Library/Frameworks/R.framework/…`) that only exists for system R installs.
pub fn install_binary_package(
    tarball: &Path,
    library: &Path,
    package_name: &str,
    libr_path: Option<&Path>,
) -> Result<()> {
    let out = Command::new("tar")
        .args([
            "xf",
            &tarball.to_string_lossy(),
            "-C",
            &library.to_string_lossy(),
        ])
        .output()?;

    if !out.status.success() {
        return Err(UvrError::Other(format!(
            "Binary extraction failed for '{}': {}",
            package_name,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }

    // Patch libR.dylib references in all .so files when using managed R.
    if let Some(libr) = libr_path {
        if libr.exists() {
            let pkg_dir = library.join(package_name);
            let _ = patch_so_libr_refs(&pkg_dir, libr);
        }
    }

    Ok(())
}

/// Public entry-point for retroactively patching already-installed packages.
/// Called by `uvr sync` to fix packages that were extracted before patching
/// support was added. Idempotent: no-op if the `.so` already points to `libr_path`.
pub fn patch_installed_so_files(pkg_dir: &Path, libr_path: &Path) {
    let _ = patch_so_libr_refs(pkg_dir, libr_path);
}

/// Walk `<pkg_dir>/libs/` and fix every `.so` that references a `libR.dylib`
/// path other than `libr_path`.
///
/// P3M macOS binaries embed `/Library/Frameworks/R.framework/…/libR.dylib`.
/// When running with a uvr-managed R (not in the framework location), `dyn.load()`
/// fails unless we update that embedded path with `install_name_tool -change`.
/// After modification we re-sign the binary with an ad-hoc signature
/// (Apple Silicon requires all Mach-O binaries to be validly signed).
fn patch_so_libr_refs(pkg_dir: &Path, libr_path: &Path) -> std::io::Result<()> {
    let libs_dir = pkg_dir.join("libs");
    if !libs_dir.exists() {
        return Ok(());
    }

    let new_libr = libr_path.to_string_lossy();

    for entry in std::fs::read_dir(&libs_dir)?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("so") {
            continue;
        }

        // Ask the dynamic linker what libR reference is embedded.
        let Ok(otool_out) = Command::new("otool").args(["-L", &path.to_string_lossy()]).output()
        else {
            continue;
        };
        let otool_text = String::from_utf8_lossy(&otool_out.stdout);

        for line in otool_text.lines() {
            let trimmed = line.trim();
            if !trimmed.contains("libR.dylib") && !trimmed.contains("R.framework") {
                continue;
            }
            // The path is the first whitespace-delimited token on the line.
            let old_libr = trimmed.split_ascii_whitespace().next().unwrap_or("");
            if old_libr.is_empty() || old_libr == new_libr {
                break; // already correct, nothing to do
            }

            let _ = Command::new("install_name_tool")
                .args(["-change", old_libr, &new_libr, &path.to_string_lossy()])
                .status();

            // Re-sign after modification (required on Apple Silicon).
            let _ = Command::new("codesign")
                .args(["--force", "--sign", "-", &path.to_string_lossy()])
                .status();

            break; // only one libR reference per .so
        }
    }

    Ok(())
}
