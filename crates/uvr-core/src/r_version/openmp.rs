//! OpenMP runtime shim for R on macOS.
//!
//! CRAN/P3M macOS binary packages that use OpenMP (Rtsne, dotCall64, mgcv,
//! data.table, …) are compiled with `-fopenmp` but link no OpenMP library
//! themselves: their `__kmpc_*` symbols are left undefined for flat-namespace
//! resolution at `dlopen` time. They only load if something else has already
//! brought `libomp` into the process.
//!
//! Nothing does, on either R uvr runs. The rstudio/r-builds portable builds
//! (#116) ship `libomp.dylib` in `$R_HOME/lib` but link it from nothing. So
//! does CRAN's own framework build: `libR.dylib` carries no `libomp` load
//! command and `SHLIB_OPENMP_CFLAGS` is empty (#261) — the assumption that
//! CRAN R was covered "because R itself links libomp" was wrong, and every
//! such package failed to load there too:
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
//! Two delivery routes, one shim:
//!
//! - **Managed R**: [`ensure_openmp_shim`] appends the block to the install's
//!   own `etc/Rprofile.site` once, at install time and on sync (self-heal).
//! - **Every R** (managed or not): [`runtime_site_profile`] writes the same
//!   block into a uvr-owned profile under `~/.uvr/etc/` and callers point
//!   `R_PROFILE` at it per invocation. That profile first chains to the site
//!   profile R would otherwise have read, so nothing is shadowed, and it
//!   never touches an installation uvr does not own. `uvr run`, `uvr
//!   activate`, and `R CMD INSTALL` all go through it, so a CRAN R is fixed
//!   without root and without editing `/Library/Frameworks`.
//!
//! Known gap: `R --vanilla` skips site profiles entirely, so a deliberately
//! vanilla session still can't load these packages. Matching CRAN exactly
//! would require R itself to be linked against `libomp` upstream.

use std::path::{Path, PathBuf};

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

/// True when `r_home` ships an OpenMP runtime nothing loads, i.e. binary
/// packages built with `-fopenmp` need the shim to resolve their symbols.
///
/// Presence of `lib/libomp.dylib` is the whole test. Loading a runtime that
/// is somehow already in the process is a harmless no-op, so there is no
/// value in the more expensive check (parsing `libR.dylib`'s load commands)
/// and no false-positive cost to over-applying.
pub fn needs_openmp_shim(r_home: &Path) -> bool {
    r_home.join("lib").join("libomp.dylib").exists()
}

/// Environment variable naming the site profile the runtime profile chains
/// to. Blank means "the one R would have read on its own", i.e.
/// `$R_HOME/etc/Rprofile.site`. Set to the user's own `R_PROFILE` when they
/// had one, so uvr's override does not silently discard it.
pub const ORIG_SITE_PROFILE_VAR: &str = "UVR_SITE_PROFILE_ORIG";

/// Preamble of the runtime profile: chain to the site profile that
/// `R_PROFILE` displaced, then fall through to [`SHIM`].
///
/// Sourced into `globalenv()` because that is where R evaluates the site
/// profile (verified against R 4.6, not the base env one might expect). The
/// self-comparison guards against `R_PROFILE` already naming this file — a
/// `uvr run` inside an activated shell — which would otherwise recurse.
/// `try` without `silent` so an error in the user's own site profile is
/// still reported, the way R would report it, without losing the shim.
const RUNTIME_PROFILE_HEADER: &str = r#"# Written by uvr; rewritten on every use, so edits here will not survive.
# R reads this file instead of $R_HOME/etc/Rprofile.site because uvr set
# R_PROFILE for this session. It chains to that file first, then loads the
# bundled OpenMP runtime — the only startup hook that reaches every R this
# project runs (uvr run, activate, R CMD INSTALL's child sessions) without
# editing an R installation uvr does not own. See uvr issue #261.
local({
  self <- Sys.getenv("R_PROFILE")
  orig <- Sys.getenv("UVR_SITE_PROFILE_ORIG")
  if (!nzchar(orig)) orig <- file.path(R.home("etc"), "Rprofile.site")
  if (file.exists(orig)) {
    same <- nzchar(self) && file.exists(self) &&
      identical(normalizePath(orig), normalizePath(self))
    if (!same) try(sys.source(orig, envir = globalenv()))
  }
})
"#;

/// Write the runtime profile into `dir` (creating it) and return its path.
/// Rewritten only when the content differs, so repeated calls are cheap and
/// a profile from an older uvr is refreshed rather than trusted.
pub fn write_runtime_profile(dir: &Path) -> std::io::Result<PathBuf> {
    let path = dir.join("Rprofile.site");
    let content = format!("{RUNTIME_PROFILE_HEADER}{SHIM}");
    if std::fs::read_to_string(&path).ok().as_deref() != Some(content.as_str()) {
        std::fs::create_dir_all(dir)?;
        std::fs::write(&path, content)?;
    }
    Ok(path)
}

/// The directory holding uvr's runtime profile: `~/.uvr/etc/`.
pub fn runtime_profile_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".uvr").join("etc"))
}

/// The site profile to point `R_PROFILE` at when running `r_home`, or
/// `None` when it needs no shim or the profile cannot be written.
///
/// Best-effort by contract, like [`ensure_openmp_shim`]: a failure degrades
/// to the pre-existing broken-load behaviour and is logged, not raised.
pub fn runtime_site_profile(r_home: &Path) -> Option<PathBuf> {
    if !needs_openmp_shim(r_home) {
        return None;
    }
    let dir = runtime_profile_dir()?;
    match write_runtime_profile(&dir) {
        Ok(path) => Some(path),
        Err(e) => {
            tracing::warn!(
                "Could not write uvr's site profile to {}: {e}. Binary packages built \
                 with OpenMP (Rtsne, mgcv, ...) may fail to load.",
                dir.display()
            );
            None
        }
    }
}

/// Like [`runtime_site_profile`], from a path to `bin/R`.
pub fn runtime_site_profile_for_binary(r_binary: &Path) -> Option<PathBuf> {
    runtime_site_profile(r_home_from_binary(r_binary)?)
}

/// The environment that activates `profile`: `R_PROFILE` itself, plus
/// [`ORIG_SITE_PROFILE_VAR`] carrying whatever `R_PROFILE` the caller's
/// environment already had — unless that is already uvr's own profile (an
/// activated shell), in which case chaining to it would be a cycle and the
/// original is R's default again.
pub fn site_profile_env(profile: &Path) -> [(&'static str, String); 2] {
    let orig = std::env::var("R_PROFILE")
        .ok()
        .filter(|v| !v.trim().is_empty() && Path::new(v) != profile)
        .unwrap_or_default();
    [
        ("R_PROFILE", profile.to_string_lossy().into_owned()),
        (ORIG_SITE_PROFILE_VAR, orig),
    ]
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
    fn needs_shim_iff_libomp_ships() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(needs_openmp_shim(&fake_r_home(tmp.path(), true)));
        let tmp = tempfile::tempdir().unwrap();
        assert!(!needs_openmp_shim(&fake_r_home(tmp.path(), false)));
    }

    #[test]
    fn runtime_profile_chains_then_shims_and_is_stable() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("etc");

        let path = write_runtime_profile(&dir).unwrap();
        assert_eq!(path, dir.join("Rprofile.site"));
        let content = std::fs::read_to_string(&path).unwrap();
        // Chain to the displaced site profile first, then the shim — order
        // matters, since the shim must run even if the chained file errors.
        let chain = content
            .find("sys.source(orig")
            .expect("chains to the original profile");
        let shim = content.find("dyn.load").expect("contains the shim");
        assert!(chain < shim);
        assert!(
            content.contains("globalenv()"),
            "site profiles are sourced into globalenv"
        );
        assert!(content.contains(ORIG_SITE_PROFILE_VAR));
        assert!(content.contains(MARKER));

        // Unchanged content is not rewritten (mtime-stable for tools that
        // watch it); a tampered file is restored.
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        write_runtime_profile(&dir).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            before
        );
        std::fs::write(&path, "options(warn = 2)\n").unwrap();
        write_runtime_profile(&dir).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), content);
    }

    #[test]
    fn runtime_site_profile_is_none_without_libomp() {
        let tmp = tempfile::tempdir().unwrap();
        let r_home = fake_r_home(tmp.path(), false);
        assert_eq!(runtime_site_profile(&r_home), None);
    }

    #[test]
    fn site_profile_env_carries_the_users_r_profile_but_never_itself() {
        let _env = crate::env_vars::env_lock();
        let ours = Path::new("/home/u/.uvr/etc/Rprofile.site");

        std::env::remove_var("R_PROFILE");
        let vars = site_profile_env(ours);
        assert_eq!(vars[0], ("R_PROFILE", ours.to_string_lossy().into_owned()));
        assert_eq!(vars[1], (ORIG_SITE_PROFILE_VAR, String::new()));

        // A user-set site profile is chained to, not discarded.
        std::env::set_var("R_PROFILE", "/etc/R/Rprofile.site");
        assert_eq!(site_profile_env(ours)[1].1, "/etc/R/Rprofile.site");

        // Inside an activated shell R_PROFILE is already ours: chaining to
        // it would be a cycle, so the original is R's default again.
        std::env::set_var("R_PROFILE", ours);
        assert_eq!(site_profile_env(ours)[1].1, "");

        std::env::set_var("R_PROFILE", "   ");
        assert_eq!(site_profile_env(ours)[1].1, "");
        std::env::remove_var("R_PROFILE");
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
