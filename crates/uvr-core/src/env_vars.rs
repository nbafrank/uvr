use std::path::PathBuf;

/// Helper to read an environment variable and ignore it if it's empty or just whitespace.
fn read_env_var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// UVR_ACTIVATE_PROMPT
///
/// Controls whether `source .uvr/activate` prefixes the shell prompt with the
/// project name. Accepts `1`/`true`/`yes` to enable and `0`/`false`/`no` to
/// disable; anything else is ignored. Overrides `[activate] prompt` in
/// `uvr.toml`, so a user can opt out of a project that opts in.
/// Defaults to disabled.
pub fn activate_prompt() -> Option<bool> {
    let raw = read_env_var("UVR_ACTIVATE_PROMPT")?;
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// UVR_CACHE_DIR
///
/// Gets the directory where uvr stores cached packages, environments, and tarballs.
/// Expects a valid absolute or relative directory path.
/// Defaults to `~/.uvr/cache/` if not set.
pub fn cache_dir() -> Option<PathBuf> {
    if let Some(path) = read_env_var("UVR_CACHE_DIR") {
        return Some(PathBuf::from(path));
    }
    dirs::home_dir().map(|h| h.join(".uvr").join("cache"))
}

/// Like [`cache_dir`], but never returns `None`: in a HOME-less environment
/// (sandboxed/scratch containers, some CI runners) it degrades to a
/// `uvr-cache` directory under the system temp dir. Callers used to fall
/// back to `PathBuf::from(".")` instead, scattering cache files into the
/// project/working directory (#161).
pub fn cache_dir_or_temp() -> PathBuf {
    cache_dir().unwrap_or_else(|| {
        let fallback = std::env::temp_dir().join("uvr-cache");
        tracing::warn!(
            "HOME and UVR_CACHE_DIR are unset; using temporary cache at {}",
            fallback.display()
        );
        fallback
    })
}

/// UVR_EXTRA_LIBS
///
/// Allows providing a list of extra R library paths that will be appended
/// to the `R_LIBS_USER` search path when executing `uvr run`.
/// Expects a string of paths separated by the standard OS path separator
/// (`:` on Unix/macOS, `;` on Windows).
pub fn extra_libs() -> Option<String> {
    read_env_var("UVR_EXTRA_LIBS")
}

/// UVR_INSTALL_DIR
///
/// The directory in which to install `uvr` using the standalone installer.
/// Expects a valid absolute or relative directory path.
pub fn install_dir() -> Option<PathBuf> {
    read_env_var("UVR_INSTALL_DIR").map(PathBuf::from)
}

/// UVR_INSTALL_TIMEOUT
///
/// Overrides the default per-package installation timeout limit (which is 30 minutes).
/// Expects a duration string such as `30m`, `2h`, `90s`, or a bare number
/// representing seconds (e.g., `1800`).
pub fn install_timeout() -> Option<String> {
    read_env_var("UVR_INSTALL_TIMEOUT")
}

/// UVR_LIBRARY
///
/// Defines a custom library directory in place of the project-local
/// `.uvr/library/` — both as the install target and for everything that
/// reads the library (`uvr run`, `activate`, doctor, the `.Rprofile`
/// snippet), via `Project::library_path` (#97).
/// Expects a valid absolute or relative directory path.
/// Note: The CLI `--library` argument takes precedence over this variable.
/// Defaults to the project-local `.uvr/library/` directory when unset or
/// empty.
pub fn library() -> Option<PathBuf> {
    read_env_var("UVR_LIBRARY").map(PathBuf::from)
}

/// UVR_USER_ENVIRON
///
/// Opt back in to the user's `~/.Renviron` (and a `./.Renviron` beside the
/// script) inside `uvr run` and a sourced `uvr activate`.
///
/// uvr blanks `R_ENVIRON_USER` by default so a `~/.Renviron` setting
/// `R_LIBS_USER` cannot silently override the project library. That defence
/// is aimed at one variable but costs the whole file, so API tokens,
/// `GITHUB_PAT`, proxy settings, TZ and locale all disappear too (#260) —
/// while `R CMD INSTALL` deliberately keeps them
/// (`installer/r_cmd_install.rs`). Setting this restores the file for the
/// run/activate side, and accepts that a `~/.Renviron` which sets
/// `R_LIBS_USER` will then win over the project library.
///
/// The *site* file (`R_ENVIRON`) stays blanked either way: it is machine
/// configuration rather than the user's, and it is not what #260 asks for.
pub fn user_environ() -> bool {
    matches!(
        read_env_var("UVR_USER_ENVIRON").as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("TRUE") | Some("YES")
    )
}

/// UVR_PACKAGES_DIR
///
/// Gets the directory where uvr stores cached installed-package entries.
/// Expects a valid absolute or relative directory path.
/// Defaults to `~/.uvr/packages/` if not set (see
/// `installer::package_cache::global_packages_dir` for the no-home fallback).
pub fn packages_dir() -> Option<PathBuf> {
    read_env_var("UVR_PACKAGES_DIR").map(PathBuf::from)
}

/// UVR_PROGRESS
///
/// Controls the visibility of progress bars and spinners in the terminal.
/// Acceptable settings:
///     - `always`, `1`, `true`: Forces progress to be drawn, bypassing TTY checks (useful for SSH).
///     - `never`, `0`, `false`: Forces progress to be hidden (useful for CI logs).
/// Defaults to automatically detecting a TTY.
pub fn progress() -> Option<String> {
    read_env_var("UVR_PROGRESS")
}

/// UVR_R_INSTALL_DIR
///
/// Gets the directory where uvr-managed R versions are installed.
/// Expects a valid absolute or relative directory path.
/// Defaults to `~/.uvr/r-versions/` if not set.
pub fn r_install_dir() -> Option<PathBuf> {
    if let Some(path) = read_env_var("UVR_R_INSTALL_DIR") {
        return Some(PathBuf::from(path));
    }
    dirs::home_dir().map(|h| h.join(".uvr").join("r-versions"))
}

/// UVR_REPOS — comma-separated list of CRAN-like repository URLs to use
/// in addition to (and at higher priority than) any `[[sources]]` in
/// `uvr.toml`. Each URL becomes a `[[sources]]` entry whose name is
/// auto-derived from the URL host. Used to inject repos via CI env
/// instead of mutating `uvr.toml`:
///
/// ```sh
/// UVR_REPOS=https://cran.rpkgs.com/arm64/alpine323/latest
/// UVR_REPOS=https://repo1.example/cran,https://repo2.example/cran
/// ```
///
/// Returns the parsed list, or `None` when the env var is unset / empty.
pub fn repos() -> Option<Vec<EnvRepo>> {
    let raw = read_env_var("UVR_REPOS")?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let out: Vec<EnvRepo> = trimmed
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|url| EnvRepo {
            name: derive_name_from_url(url),
            url: url.to_string(),
        })
        .collect();
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvRepo {
    pub name: String,
    pub url: String,
}

/// Derive a stable, human-readable source name from a repo URL.
/// Falls back to the URL if no host can be parsed. Strips any port
/// and lowercases for predictability.
fn derive_name_from_url(url: &str) -> String {
    let after_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let host_end = after_scheme.find('/').unwrap_or(after_scheme.len());
    let host_with_port = &after_scheme[..host_end];
    let host = host_with_port.split(':').next().unwrap_or(host_with_port);
    if host.is_empty() {
        url.to_string()
    } else {
        host.to_lowercase()
    }
}

/// Serializes every test that mutates process-global environment variables.
/// Env vars are shared across the whole test binary, so tests touching the
/// same var (e.g. `test_env_vars` and `test_env_repos` both on `UVR_REPOS`, or
/// `test_env_vars` vs `r_cmd_install`'s timeout test on `UVR_INSTALL_TIMEOUT`)
/// race under the parallel runner and fail intermittently. Any env-mutating
/// test must hold this lock for its whole body. Crate-visible so tests in other
/// modules can share it.
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Acquire [`ENV_LOCK`], recovering from a poisoned mutex (a prior test panic
/// shouldn't cascade into "lock poisoned" failures for every other env test).
#[cfg(test)]
pub(crate) fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    struct EnvGuard {
        backups: std::collections::HashMap<&'static str, Option<String>>,
    }

    impl EnvGuard {
        fn new(vars: &[&'static str]) -> Self {
            let mut backups = std::collections::HashMap::new();
            for &v in vars {
                backups.insert(v, env::var(v).ok());
                env::remove_var(v);
            }
            Self { backups }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (var, val) in &self.backups {
                match val {
                    Some(v) => env::set_var(var, v),
                    None => env::remove_var(var),
                }
            }
        }
    }

    // We run all env var checks in a single test to avoid race conditions
    // since environment variables are global per process.
    #[test]
    fn test_env_vars() {
        let _env = env_lock();
        // Backup original env vars if present so we don't permanently mess up the test runner environment
        let vars_to_test = [
            "UVR_ACTIVATE_PROMPT",
            "UVR_CACHE_DIR",
            "UVR_EXTRA_LIBS",
            "UVR_INSTALL_DIR",
            "UVR_INSTALL_TIMEOUT",
            "UVR_LIBRARY",
            "UVR_PACKAGES_DIR",
            "UVR_PROGRESS",
            "UVR_R_INSTALL_DIR",
            "UVR_REPOS",
        ];

        let _guard = EnvGuard::new(&vars_to_test);

        // 1. Defaults when unset
        let default_cache = cache_dir();
        assert!(default_cache.is_some());
        assert!(default_cache.unwrap().ends_with("cache"));

        assert_eq!(activate_prompt(), None);
        assert_eq!(extra_libs(), None);
        assert_eq!(install_dir(), None);
        assert_eq!(install_timeout(), None);
        assert_eq!(library(), None);
        assert_eq!(packages_dir(), None);
        assert_eq!(progress(), None);

        let default_r_install = r_install_dir();
        assert!(default_r_install.is_some());
        assert!(default_r_install.unwrap().ends_with("r-versions"));

        // 2. Override when set
        env::set_var("UVR_CACHE_DIR", "/custom/cache");
        assert_eq!(cache_dir(), Some(PathBuf::from("/custom/cache")));

        env::set_var("UVR_EXTRA_LIBS", "/custom/libs");
        assert_eq!(extra_libs(), Some("/custom/libs".to_string()));

        env::set_var("UVR_INSTALL_DIR", "/custom/bin");
        assert_eq!(install_dir(), Some(PathBuf::from("/custom/bin")));

        env::set_var("UVR_INSTALL_TIMEOUT", "60s");
        assert_eq!(install_timeout(), Some("60s".to_string()));

        env::set_var("UVR_LIBRARY", "/custom/library");
        assert_eq!(library(), Some(PathBuf::from("/custom/library")));
        // #97: the project's library path follows the override, so the
        // commands that *read* the library look where sync installed.
        let project = crate::project::Project {
            root: PathBuf::from("/proj"),
            manifest: crate::manifest::Manifest::new("t", None),
            manifest_source: crate::project::ManifestSource::Toml,
        };
        assert_eq!(project.library_path(), PathBuf::from("/custom/library"));

        env::set_var("UVR_PACKAGES_DIR", "/custom/packages");
        assert_eq!(packages_dir(), Some(PathBuf::from("/custom/packages")));

        env::set_var("UVR_PROGRESS", "always");
        assert_eq!(progress(), Some("always".to_string()));

        env::set_var("UVR_R_INSTALL_DIR", "/custom/r-versions");
        assert_eq!(r_install_dir(), Some(PathBuf::from("/custom/r-versions")));

        // 3. Empty-string env vars falling through to default
        for &var in &vars_to_test {
            env::set_var(var, "");
        }

        let empty_cache = cache_dir();
        assert!(empty_cache.is_some());
        assert!(empty_cache.unwrap().ends_with("cache"));

        assert_eq!(extra_libs(), None);
        assert_eq!(install_dir(), None);
        assert_eq!(install_timeout(), None);
        assert_eq!(library(), None);
        assert_eq!(packages_dir(), None);
        assert_eq!(progress(), None);
        // …and the project's library path falls back to project-local.
        assert!(project.library_path().ends_with(".uvr/library"));

        let empty_r_install = r_install_dir();
        assert!(empty_r_install.is_some());
        assert!(empty_r_install.unwrap().ends_with("r-versions"));

        // Bonus: Whitespace-only strings falling through to default
        for &var in &vars_to_test {
            env::set_var(var, "   ");
        }
        assert_eq!(extra_libs(), None);
    }

    // #161: the last-resort cache location must never be the working
    // directory. Whatever cache_dir_or_temp resolves to (env override, home,
    // or the temp-dir fallback), it is an absolute path — never `.`.
    #[test]
    fn test_cache_dir_or_temp_never_pollutes_cwd() {
        let _env = env_lock();
        let _guard = EnvGuard::new(&["UVR_CACHE_DIR"]);

        let resolved = cache_dir_or_temp();
        assert!(resolved.is_absolute());
        assert_ne!(resolved, PathBuf::from("."));

        // Explicit override still wins.
        env::set_var("UVR_CACHE_DIR", "/custom/cache");
        assert_eq!(cache_dir_or_temp(), PathBuf::from("/custom/cache"));
    }

    // All repos() checks run in a single test to avoid race conditions with
    // the parallel test runner mutating the same env var.
    #[test]
    fn test_env_repos() {
        let _env = env_lock();
        let _guard = EnvGuard::new(&["UVR_REPOS"]);

        // unset → None
        assert!(repos().is_none());

        // single URL
        env::set_var("UVR_REPOS", "https://cran.rpkgs.com/arm64/alpine323/latest");
        let v = repos().expect("one repo");
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name, "cran.rpkgs.com");
        assert_eq!(v[0].url, "https://cran.rpkgs.com/arm64/alpine323/latest");

        // multiple comma-separated URLs
        env::set_var("UVR_REPOS", "https://a.example/cran,https://b.example/cran");
        let v = repos().expect("two repos");
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].name, "a.example");
        assert_eq!(v[1].name, "b.example");

        // port stripped from name
        env::set_var("UVR_REPOS", "http://localhost:8080/cran");
        let v = repos().expect("one repo with port");
        assert_eq!(v[0].name, "localhost");

        // whitespace-only → None
        env::set_var("UVR_REPOS", "  ");
        assert!(repos().is_none());
    }

    #[test]
    fn test_activate_prompt_parsing() {
        let _env = env_lock();
        let _guard = EnvGuard::new(&["UVR_ACTIVATE_PROMPT"]);

        for on in ["1", "true", "TRUE", "yes", "on", "  1  "] {
            env::set_var("UVR_ACTIVATE_PROMPT", on);
            assert_eq!(activate_prompt(), Some(true), "{on:?} should enable");
        }
        for off in ["0", "false", "FALSE", "no", "off"] {
            env::set_var("UVR_ACTIVATE_PROMPT", off);
            assert_eq!(activate_prompt(), Some(false), "{off:?} should disable");
        }
        // Unrecognized values fall through to None so the manifest still
        // decides, rather than a typo silently forcing the prompt off.
        for junk in ["maybe", "2", "-"] {
            env::set_var("UVR_ACTIVATE_PROMPT", junk);
            assert_eq!(activate_prompt(), None, "{junk:?} should be ignored");
        }
        // Empty / whitespace-only is treated as unset, like every other var.
        env::set_var("UVR_ACTIVATE_PROMPT", "   ");
        assert_eq!(activate_prompt(), None);
        env::remove_var("UVR_ACTIVATE_PROMPT");
        assert_eq!(activate_prompt(), None);
    }
}
