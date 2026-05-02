use std::path::PathBuf;

/// UVR_CACHE_DIR
/// 
/// Gets the directory where uvr stores cached packages, environments, and tarballs.
/// Expects a valid absolute or relative directory path.
/// Defaults to `~/.uvr/cache/` if not set.
pub fn cache_dir() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("UVR_CACHE_DIR") {
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
    std::env::var("UVR_EXTRA_LIBS").ok()
}

/// UVR_INSTALL_DIR
/// 
/// Gets the directory where the `uvr` binary itself is installed.
/// Used primarily by the `uvr self-update` command to determine where 
/// to place the newly downloaded executable.
/// Expects a valid absolute or relative directory path.
pub fn install_dir() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("UVR_INSTALL_DIR") {
        return Some(PathBuf::from(path));
    }
    // We could fall back to ~/.local/bin but if we are just reading an env var
    // to override the self_update path, maybe it just defaults to None or current_exe parent?
    // Let's provide a function that returns the env var or None.
    None
}

/// UVR_INSTALL_TIMEOUT
/// 
/// Overrides the default per-package installation timeout limit (which is 30 minutes).
/// Expects a duration string such as `30m`, `2h`, `90s`, or a bare number 
/// representing seconds (e.g., `1800`).
pub fn install_timeout() -> Option<String> {
    std::env::var("UVR_INSTALL_TIMEOUT").ok()
}

/// UVR_LIBRARY
/// 
/// Defines a custom target directory for R package installations.
/// Expects a valid absolute or relative directory path.
/// Note: The CLI `--library` argument takes precedence over this variable.
/// Defaults to the project-local `.uvr/library/` directory if neither are provided.
pub fn library() -> Option<PathBuf> {
    std::env::var("UVR_LIBRARY").ok().map(PathBuf::from)
}

/// UVR_PROGRESS
/// 
/// Controls the visibility of progress bars and spinners in the terminal.
/// Acceptable settings:
/// - `always`, `1`, `true`: Forces progress to be drawn, bypassing TTY checks (useful for SSH).
/// - `never`, `0`, `false`: Forces progress to be hidden (useful for CI logs).
/// Defaults to automatically detecting a TTY.
pub fn progress() -> Option<String> {
    std::env::var("UVR_PROGRESS").ok()
}

/// UVR_R_VERSIONS_DIR
/// 
/// Gets the directory where uvr-managed R versions are installed.
/// Expects a valid absolute or relative directory path.
/// Defaults to `~/.uvr/r-versions/` if not set.
pub fn r_versions_dir() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("UVR_R_VERSIONS_DIR") {
        return Some(PathBuf::from(path));
    }
    dirs::home_dir().map(|h| h.join(".uvr").join("r-versions"))
}
