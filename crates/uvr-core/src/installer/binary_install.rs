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

        let Ok(otool_out) = Command::new("otool").args(["-L", &path.to_string_lossy()]).output()
        else {
            continue;
        };
        let otool_text = String::from_utf8_lossy(&otool_out.stdout);

        let mut changed = false;
        for line in otool_text.lines() {
            let old_dep = line.trim().split_whitespace().next().unwrap_or("");
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
