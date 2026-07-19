//! OpenMP runtime shim for managed R installations (macOS).
//!
//! CRAN/P3M macOS binary packages that use OpenMP (Rtsne, dotCall64, mgcv,
//! data.table, …) are compiled with `-fopenmp` but link no OpenMP library
//! themselves: their `__kmpc_*` symbols are left undefined for flat-namespace
//! resolution at `dlopen` time. On CRAN's official macOS R that works because
//! R itself is built against `libomp`, so the runtime is already loaded in the
//! process.
//!
//! The rstudio/r-builds portable builds uvr installs since v0.4.0 (#116) ship
//! `libomp.dylib` in `$R_HOME/lib` but link it from nothing, so the symbols
//! never resolve and every such package fails to load:
//!
//! ```text
//! dlopen(.../mgcv.so): symbol not found in flat namespace '___kmpc_barrier'
//! ```
//!
//! There is no way to add a load command to a shipped dylib after the fact,
//! and `DYLD_INSERT_LIBRARIES` is stripped by SIP when `bin/R` (a shell
//! script) execs. What does work is loading the runtime from R's own startup:
//! a site profile that `dyn.load`s `libomp.dylib` into the global namespace
//! before any package is loaded. That covers interactive sessions, `uvr run`,
//! and `R CMD INSTALL`'s lazy-loading child sessions (the path that made
//! `uvr add` fail).
//!
//! Known gap: `R --vanilla` skips site profiles entirely, so a deliberately
//! vanilla session still can't load these packages. Matching CRAN exactly
//! would require R itself to be linked against `libomp` upstream.

use std::path::Path;

/// Marker identifying the block uvr manages, so the profile is written once
/// and user content in an existing `Rprofile.site` is never clobbered.
const MARKER: &str = "# >>> uvr openmp shim >>>";

/// The site-profile block. Resolves `libomp` relative to `R.home()` so the
/// snippet keeps working if the installation is moved (the portable builds
/// are relocatable).
const SHIM: &str = r#"# >>> uvr openmp shim >>>
# Loads the bundled OpenMP runtime so CRAN/P3M binary packages built with
# -fopenmp (Rtsne, dotCall64, mgcv, ...) can resolve their __kmpc_* symbols.
# Without this they fail with "symbol not found in flat namespace".
local({
  lib <- file.path(R.home("lib"), if (.Platform$OS.type == "windows") "libomp.dll" else "libomp.dylib")
  if (file.exists(lib)) try(dyn.load(lib, local = FALSE, now = FALSE), silent = TRUE)
})
# <<< uvr openmp shim <<<
"#;

/// Ensure `r_home`'s site profile loads the bundled OpenMP runtime.
///
/// No-op when the installation ships no `libomp` (Linux portable builds link
/// `libgomp` into the packages themselves, and system R installs are not ours
/// to modify — callers must only pass uvr-managed `R_HOME`s), or when the
/// shim is already present. Returns `true` when the profile was written.
///
/// Best-effort by contract: a failure here degrades to the pre-existing
/// broken-load behaviour, so callers log rather than abort.
pub fn ensure_openmp_shim(r_home: &Path) -> std::io::Result<bool> {
    if !r_home.join("lib").join("libomp.dylib").exists() {
        return Ok(false);
    }
    let etc = r_home.join("etc");
    if !etc.is_dir() {
        return Ok(false);
    }
    let profile = etc.join("Rprofile.site");
    let existing = std::fs::read_to_string(&profile).unwrap_or_default();
    if existing.contains(MARKER) {
        return Ok(false);
    }
    // Append rather than overwrite: a user (or a future uvr feature) may have
    // put their own settings here.
    let updated = if existing.trim().is_empty() {
        SHIM.to_string()
    } else if existing.ends_with('\n') {
        format!("{existing}\n{SHIM}")
    } else {
        format!("{existing}\n\n{SHIM}")
    };
    std::fs::write(&profile, updated)?;
    Ok(true)
}

/// Derive `R_HOME` from a path to an R binary (`<r_home>/bin/R`), returning
/// `None` when the layout doesn't match.
pub fn r_home_from_binary(r_binary: &Path) -> Option<&Path> {
    r_binary.parent()?.parent()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a fake macOS R installation with a `libomp.dylib`.
    fn fake_r_home(dir: &Path, with_libomp: bool) -> std::path::PathBuf {
        let r_home = dir.join("R");
        std::fs::create_dir_all(r_home.join("etc")).unwrap();
        std::fs::create_dir_all(r_home.join("lib")).unwrap();
        if with_libomp {
            std::fs::write(r_home.join("lib").join("libomp.dylib"), b"fake").unwrap();
        }
        r_home
    }

    #[test]
    fn writes_shim_when_libomp_present() {
        let tmp = tempfile::tempdir().unwrap();
        let r_home = fake_r_home(tmp.path(), true);

        assert!(ensure_openmp_shim(&r_home).unwrap());
        let profile = std::fs::read_to_string(r_home.join("etc").join("Rprofile.site")).unwrap();
        assert!(profile.contains("dyn.load"));
        assert!(profile.contains("libomp.dylib"));
    }

    #[test]
    fn is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let r_home = fake_r_home(tmp.path(), true);

        assert!(ensure_openmp_shim(&r_home).unwrap());
        // Second call is a no-op: no duplicate block.
        assert!(!ensure_openmp_shim(&r_home).unwrap());
        let profile = std::fs::read_to_string(r_home.join("etc").join("Rprofile.site")).unwrap();
        assert_eq!(profile.matches("dyn.load").count(), 1);
    }

    #[test]
    fn preserves_existing_profile_content() {
        let tmp = tempfile::tempdir().unwrap();
        let r_home = fake_r_home(tmp.path(), true);
        let profile = r_home.join("etc").join("Rprofile.site");
        std::fs::write(
            &profile,
            "options(repos = c(CRAN = \"https://example.com\"))\n",
        )
        .unwrap();

        assert!(ensure_openmp_shim(&r_home).unwrap());
        let updated = std::fs::read_to_string(&profile).unwrap();
        assert!(
            updated.contains("https://example.com"),
            "user settings must survive"
        );
        assert!(updated.contains("dyn.load"));
    }

    #[test]
    fn no_op_without_libomp() {
        let tmp = tempfile::tempdir().unwrap();
        let r_home = fake_r_home(tmp.path(), false);

        assert!(!ensure_openmp_shim(&r_home).unwrap());
        assert!(!r_home.join("etc").join("Rprofile.site").exists());
    }

    #[test]
    fn r_home_from_binary_strips_bin() {
        let p = std::path::PathBuf::from("/x/.uvr/r-versions/4.6.0/bin/R");
        assert_eq!(
            r_home_from_binary(&p),
            Some(std::path::Path::new("/x/.uvr/r-versions/4.6.0"))
        );
    }
}
