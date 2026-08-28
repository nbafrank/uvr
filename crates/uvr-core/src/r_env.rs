//! The isolated R environment a uvr project runs against.
//!
//! `uvr run` spawns R with a specific set of environment variables that point
//! R at the project's library and shadow anything the machine would otherwise
//! contribute. Shell activation (`uvr activate`) has to export *the same* set
//! into the user's shell, and inline-script runs need it too. Keeping the
//! construction here means those paths cannot drift apart — a variable added
//! for `run` is automatically exported by `activate`.

use std::path::{Path, PathBuf};

/// The OS path-list separator (`;` on Windows, `:` elsewhere).
pub const PATH_SEP: &str = if cfg!(target_os = "windows") {
    ";"
} else {
    ":"
};

/// A resolved, isolated R environment.
///
/// Construct it directly — every input is explicit so the result is a pure
/// function of its fields and can be asserted on in tests without touching
/// the real environment.
#[derive(Debug, Clone, PartialEq)]
pub struct REnv {
    /// Absolute path to the R binary this environment runs.
    pub r_binary: PathBuf,
    /// The project (or fallback) library packages are installed into.
    pub library: PathBuf,
    /// Ephemeral library for `--with` packages, searched *before* `library`.
    pub with_library: Option<PathBuf>,
    /// Raw `UVR_EXTRA_LIBS` value, appended last. See [`REnv::r_libs_user`].
    pub extra_libs: Option<String>,
}

impl REnv {
    /// Directory holding the R binary — what activation prepends to `PATH`
    /// so a bare `R` resolves to this interpreter.
    pub fn r_bin_dir(&self) -> PathBuf {
        self.r_binary
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
    }

    /// R's own `lib/` directory, i.e. `<r_binary>/../../lib`.
    ///
    /// Exported as `DYLD_LIBRARY_PATH` / `LD_LIBRARY_PATH` so compiled
    /// packages can find `libR` at runtime regardless of the install-name
    /// baked into the shared object.
    pub fn r_lib_dir(&self) -> PathBuf {
        self.r_binary
            .parent()
            .and_then(Path::parent)
            .map(|p| p.join("lib"))
            .unwrap_or_default()
    }

    /// The `R_LIBS_USER` search path.
    ///
    /// Order is significant: the `--with` ephemeral library wins over the
    /// project library, and `UVR_EXTRA_LIBS` comes last. The extra-libs
    /// escape hatch exists because `R_LIBS_SITE` is blanked below to isolate
    /// the project — without it a controlled environment (a Docker image, a
    /// shared lab machine, a benchmark harness) could not expose a system
    /// library to uvr at all.
    pub fn r_libs_user(&self) -> String {
        let mut out = match &self.with_library {
            Some(with_lib) => {
                format!("{}{PATH_SEP}{}", with_lib.display(), self.library.display())
            }
            None => self.library.to_string_lossy().into_owned(),
        };
        if let Some(extra) = self.extra_libs.as_deref() {
            let extra = extra.trim();
            if !extra.is_empty() {
                out.push_str(PATH_SEP);
                out.push_str(extra);
            }
        }
        out
    }

    /// Every environment variable that defines the isolated environment.
    ///
    /// This is the single source of truth: `uvr run` applies these to the R
    /// child process, `uvr activate` exports the same pairs into the shell.
    ///
    /// Note the deliberately-empty values — `R_LIBS_SITE` and `R_LIBS` are
    /// blanked so a system-wide library cannot leak into a project.
    ///
    /// **Both** Renviron variables are blanked by default. `R_ENVIRON` covers
    /// only the *site* file; the per-user `~/.Renviron` is gated separately by
    /// `R_ENVIRON_USER`, and a user whose `~/.Renviron` sets `R_LIBS_USER`
    /// would otherwise have it silently override the project library. `uvr
    /// run` is additionally protected by its `--no-environ` process flag,
    /// but a sourced activation script has no such flag — so blanking the
    /// variables is what actually does the work here.
    ///
    /// `UVR_USER_ENVIRON=1` drops the `R_ENVIRON_USER` blank, restoring the
    /// user's `~/.Renviron` (#260) — the file holds API tokens and locale
    /// settings, not just library paths, and blanking it to defend one
    /// variable takes the rest with it. The site file stays blanked either
    /// way. `uvr run` drops `--no-environ` to match, or the flag would
    /// re-suppress what the variable just restored.
    pub fn vars(&self) -> Vec<(&'static str, String)> {
        let r_lib_dir = self.r_lib_dir().to_string_lossy().into_owned();

        // Prepend R's own lib dir to (DY)LD_LIBRARY_PATH rather than replacing
        // it.  On HPC clusters BLAS/LAPACK are provided by the environment module
        // system (OpenBLAS, MKL, FlexiBLAS) and their paths are already on
        // LD_LIBRARY_PATH.  Clobbering that value causes lazy-loading failures
        // for packages whose `.so` files link against `libRblas.so` /
        // `libRlapack.so` — the install succeeds but `library()` fails.  This
        // is the same fix applied to `R CMD INSTALL` in `build_cmd`; `uvr run`
        // and `uvr activate` must be consistent so the environment produced by
        // a successful `uvr sync` can actually be used.
        //
        // Guard: if `r_lib_dir` is empty (malformed R binary path with no
        // parent), skip it entirely — a leading empty component would resolve to
        // the current working directory, which is worse than nothing.
        let prepend_lib = |var: &'static str| -> String {
            if r_lib_dir.is_empty() {
                return std::env::var(var).unwrap_or_default();
            }
            match std::env::var(var) {
                Ok(existing) if !existing.is_empty() => {
                    format!("{r_lib_dir}{PATH_SEP}{existing}")
                }
                _ => r_lib_dir.clone(),
            }
        };

        let mut vars = vec![
            ("R_LIBS_USER", self.r_libs_user()),
            ("R_LIBS_SITE", String::new()),
            ("R_LIBS", String::new()),
            ("DYLD_LIBRARY_PATH", prepend_lib("DYLD_LIBRARY_PATH")),
            ("LD_LIBRARY_PATH", prepend_lib("LD_LIBRARY_PATH")),
            ("R_ENVIRON", String::new()),
        ];
        if !crate::env_vars::user_environ() {
            vars.push(("R_ENVIRON_USER", String::new()));
        }

        // R reads R_LD_LIBRARY_PATH and prepends it to LD_LIBRARY_PATH in
        // every subprocess it spawns (byte-compilation children, `Rscript`
        // calls inside package build scripts, etc.).  Without this, `uvr run`
        // and a sourced activation script inherit the correct LD_LIBRARY_PATH
        // for the top-level process but any child R spawned by R itself loses
        // the module-provided BLAS/LAPACK paths — manifesting as
        // "libRlapack.so: cannot open shared object file" during lazy loading.
        //
        // Guard: only set R_LD_LIBRARY_PATH when libR.so / libR.dylib is
        // actually present in r_lib_dir.  On Posit portable builds the R
        // wrapper scripts sources `etc/ldpaths`, which sets `R_LD_LIBRARY_PATH
        // = ${R_HOME}/lib` when it is **not already set**.  Since r_lib_dir is
        // `<install_root>/lib` and the correct R_HOME/lib is
        // `<install_root>/lib/R/lib`, pre-setting R_LD_LIBRARY_PATH to the
        // wrong path would suppress ldpaths's correct default and leave exec/R
        // unable to find libR.so at startup.
        let r_lib_path = std::path::Path::new(r_lib_dir.as_str());
        if r_lib_path.join("libR.so").exists() || r_lib_path.join("libR.dylib").exists() {
            vars.push(("R_LD_LIBRARY_PATH", prepend_lib("R_LD_LIBRARY_PATH")));
        }

        vars
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn renv() -> REnv {
        REnv {
            r_binary: PathBuf::from("/opt/R/4.4.2/bin/R"),
            library: PathBuf::from("/proj/.uvr/library"),
            with_library: None,
            extra_libs: None,
        }
    }

    #[test]
    fn libs_user_is_the_library_when_nothing_else_is_set() {
        assert_eq!(renv().r_libs_user(), "/proj/.uvr/library");
    }

    #[test]
    fn with_library_takes_precedence_over_the_project_library() {
        let env = REnv {
            with_library: Some(PathBuf::from("/cache/with-envs/abc/.uvr/library")),
            ..renv()
        };
        assert_eq!(
            env.r_libs_user(),
            format!("/cache/with-envs/abc/.uvr/library{PATH_SEP}/proj/.uvr/library")
        );
    }

    #[test]
    fn extra_libs_are_appended_last() {
        let env = REnv {
            with_library: Some(PathBuf::from("/with")),
            extra_libs: Some("/site/lib".to_string()),
            ..renv()
        };
        assert_eq!(
            env.r_libs_user(),
            format!("/with{PATH_SEP}/proj/.uvr/library{PATH_SEP}/site/lib")
        );
    }

    #[test]
    fn whitespace_only_extra_libs_is_ignored() {
        // A shell exporting `UVR_EXTRA_LIBS=` must not produce a trailing
        // separator, which R would read as an empty (cwd) library entry.
        let env = REnv {
            extra_libs: Some("   ".to_string()),
            ..renv()
        };
        assert_eq!(env.r_libs_user(), "/proj/.uvr/library");
    }

    #[test]
    fn bin_and_lib_dirs_are_derived_from_the_binary() {
        let env = renv();
        assert_eq!(env.r_bin_dir(), PathBuf::from("/opt/R/4.4.2/bin"));
        assert_eq!(env.r_lib_dir(), PathBuf::from("/opt/R/4.4.2/lib"));
    }

    #[test]
    fn vars_isolate_the_project_from_system_libraries() {
        let _env = crate::env_vars::env_lock();
        // Clear module-provided paths so the test is deterministic.
        std::env::remove_var("LD_LIBRARY_PATH");
        std::env::remove_var("DYLD_LIBRARY_PATH");

        let vars = renv().vars();
        let get = |k: &str| {
            vars.iter()
                .find(|(name, _)| *name == k)
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| panic!("{k} not exported"))
        };
        assert_eq!(get("R_LIBS_USER"), "/proj/.uvr/library");
        // Blank, not absent: these actively shadow system-wide libraries.
        assert_eq!(get("R_LIBS_SITE"), "");
        assert_eq!(get("R_LIBS"), "");
        assert_eq!(get("R_ENVIRON"), "");
        // R_ENVIRON alone covers only the *site* Renviron. Without this, a
        // user whose ~/.Renviron sets R_LIBS_USER silently gets their own
        // library instead of the project's — verified against real R.
        assert_eq!(get("R_ENVIRON_USER"), "");
        // With no inherited value, the lib dir is set directly — no leading colon.
        let lib = PathBuf::from("/opt/R/4.4.2").join("lib");
        let lib = lib.to_string_lossy();
        assert_eq!(get("DYLD_LIBRARY_PATH"), lib.as_ref());
        assert_eq!(get("LD_LIBRARY_PATH"), lib.as_ref());
    }

    #[test]
    fn user_environ_opt_in_keeps_the_site_file_blanked() {
        // #260: `~/.Renviron` carries API tokens, GITHUB_PAT, proxy settings
        // and locale, not only library paths, so blanking it to defend
        // R_LIBS_USER takes the rest with it. The opt-out restores the user
        // file and nothing else — the site file is machine configuration and
        // stays shadowed.
        let _env = crate::env_vars::env_lock();
        std::env::set_var("UVR_USER_ENVIRON", "1");
        let vars = renv().vars();
        std::env::remove_var("UVR_USER_ENVIRON");

        assert!(
            !vars.iter().any(|(k, _)| *k == "R_ENVIRON_USER"),
            "R_ENVIRON_USER must not be exported at all — a blank value \
             suppresses the file just as effectively as a wrong one"
        );
        assert_eq!(
            vars.iter()
                .find(|(k, _)| *k == "R_ENVIRON")
                .map(|(_, v)| v.as_str()),
            Some(""),
            "the site Renviron stays blanked"
        );
    }

    #[test]
    fn user_environ_is_off_by_default() {
        let _env = crate::env_vars::env_lock();
        std::env::remove_var("UVR_USER_ENVIRON");
        let vars = renv().vars();
        assert!(vars
            .iter()
            .any(|(k, v)| *k == "R_ENVIRON_USER" && v.is_empty()));
    }

    #[test]
    fn vars_prepend_r_lib_dir_to_inherited_ld_library_path() {
        // HPC clusters set LD_LIBRARY_PATH to expose BLAS/LAPACK shared
        // libraries (OpenBLAS, MKL, FlexiBLAS).  `vars()` must prepend R's
        // lib dir rather than replacing the value, so the module-provided
        // libraries remain visible to R CMD INSTALL subprocesses, `uvr run`,
        // and activation scripts.
        let _env = crate::env_vars::env_lock();
        let module_blas = "/apps/rocs/OpenBLAS/lib";
        std::env::set_var("LD_LIBRARY_PATH", module_blas);
        std::env::remove_var("DYLD_LIBRARY_PATH");

        let vars = renv().vars();
        let get = |k: &str| {
            vars.iter()
                .find(|(name, _)| *name == k)
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| panic!("{k} not exported"))
        };

        // Use renv().r_lib_dir() to get the path in the OS-native form
        // (backslash on Windows, forward-slash elsewhere) so the expected
        // string matches what vars() actually produces on every platform.
        let r_lib = renv().r_lib_dir().to_string_lossy().into_owned();
        // R's lib dir is first; the module's BLAS path follows.
        assert_eq!(
            get("LD_LIBRARY_PATH"),
            format!("{r_lib}{PATH_SEP}{module_blas}"),
            "LD_LIBRARY_PATH must prepend, not replace"
        );
        // DYLD_LIBRARY_PATH had no inherited value — just R's lib dir.
        assert_eq!(get("DYLD_LIBRARY_PATH"), r_lib);
    }

    #[test]
    fn vars_empty_r_lib_dir_does_not_produce_leading_separator() {
        // A malformed R binary path with no parent gives an empty r_lib_dir.
        // The result must not start with PATH_SEP — a leading empty component
        // resolves to the current directory for the dynamic linker, which is
        // a security hazard.
        let _env = crate::env_vars::env_lock();
        let inherited = "/some/blas/lib";
        std::env::set_var("LD_LIBRARY_PATH", inherited);
        std::env::set_var("DYLD_LIBRARY_PATH", inherited);

        let env = REnv {
            r_binary: PathBuf::from("R"), // no parent → r_lib_dir() is empty
            ..renv()
        };
        let vars = env.vars();
        let get = |k: &str| {
            vars.iter()
                .find(|(name, _)| *name == k)
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| panic!("{k} not exported"))
        };
        let ld = get("LD_LIBRARY_PATH");
        assert!(
            !ld.starts_with(PATH_SEP),
            "LD_LIBRARY_PATH must not start with separator, got: {ld:?}"
        );
    }

    #[test]
    fn a_binary_without_parents_does_not_panic() {
        let env = REnv {
            r_binary: PathBuf::from("R"),
            ..renv()
        };
        assert_eq!(env.r_bin_dir(), PathBuf::from(""));
        assert_eq!(env.r_lib_dir(), PathBuf::from(""));
    }

    #[test]
    fn vars_omits_r_ld_library_path_when_libr_absent() {
        // On Posit portable builds, r_lib_dir() resolves to <install_root>/lib,
        // but libR.so lives in <install_root>/lib/R/lib/ — one level deeper.
        // If uvr sets R_LD_LIBRARY_PATH to the wrong path, R's own `ldpaths`
        // script cannot supply the correct default (it only fires when the var is
        // unset), and exec/R fails to load.  Guard: omit R_LD_LIBRARY_PATH when
        // libR.so / libR.dylib is not present in r_lib_dir so that ldpaths
        // remains free to set it correctly.
        let env = renv(); // r_binary = /opt/R/4.4.2/bin/R; r_lib_dir = /opt/R/4.4.2/lib (no libR.so there)
        let vars = env.vars();
        // R_LD_LIBRARY_PATH must NOT be present when the lib file is absent.
        let has_r_ld = vars.iter().any(|(k, _)| *k == "R_LD_LIBRARY_PATH");
        assert!(
            !has_r_ld,
            "R_LD_LIBRARY_PATH should be omitted when libR.so is not in r_lib_dir"
        );
    }
}
