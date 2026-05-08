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
/// Gets the directory where the `uvr` binary itself is installed.
/// Used primarily by the `uvr self-update` command to determine where
/// to place the newly downloaded executable.
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
/// - `always`, `1`, `true`: Forces progress to be drawn, bypassing TTY checks (useful for SSH).
/// - `never`, `0`, `false`: Forces progress to be hidden (useful for CI logs).
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    // We run all env var checks in a single test to avoid race conditions
    // since environment variables are global per process.
    #[test]
    fn test_env_vars() {
        // Backup original env vars if present so we don't permanently mess up the test runner environment
        let vars_to_test = [
            "UVR_CACHE_DIR",
            "UVR_EXTRA_LIBS",
            "UVR_INSTALL_DIR",
            "UVR_INSTALL_TIMEOUT",
            "UVR_LIBRARY",
            "UVR_PROGRESS",
            "UVR_R_INSTALL_DIR",
        ];

        let mut backups = std::collections::HashMap::new();
        for &var in &vars_to_test {
            if let Ok(val) = env::var(var) {
                backups.insert(var, val);
            }
            env::remove_var(var);
        }

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

        // Restore original env vars
        for &var in &vars_to_test {
            if let Some(val) = backups.get(var) {
                env::set_var(var, val);
            } else {
                env::remove_var(var);
            }
        }
    }
}
