use std::path::PathBuf;

/// Helper to read an environment variable and ignore it if it's empty or just whitespace.
fn read_env_var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
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
/// Defines a custom target directory for R package installations.
/// Expects a valid absolute or relative directory path.
/// Note: The CLI `--library` argument takes precedence over this variable.
/// Defaults to the project-local `.uvr/library/` directory if neither are provided.
pub fn library() -> Option<PathBuf> {
    read_env_var("UVR_LIBRARY").map(PathBuf::from)
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
            "UVR_CACHE_DIR",
            "UVR_EXTRA_LIBS",
            "UVR_INSTALL_DIR",
            "UVR_INSTALL_TIMEOUT",
            "UVR_LIBRARY",
            "UVR_PROGRESS",
            "UVR_R_INSTALL_DIR",
            "UVR_REPOS",
        ];

        let _guard = EnvGuard::new(&vars_to_test);

        // 1. Defaults when unset
        let default_cache = cache_dir();
        assert!(default_cache.is_some());
        assert!(default_cache.unwrap().ends_with("cache"));

        assert_eq!(extra_libs(), None);
        assert_eq!(install_dir(), None);
        assert_eq!(install_timeout(), None);
        assert_eq!(library(), None);
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
        assert_eq!(progress(), None);

        let empty_r_install = r_install_dir();
        assert!(empty_r_install.is_some());
        assert!(empty_r_install.unwrap().ends_with("r-versions"));

        // Bonus: Whitespace-only strings falling through to default
        for &var in &vars_to_test {
            env::set_var(var, "   ");
        }
        assert_eq!(extra_libs(), None);
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
}
