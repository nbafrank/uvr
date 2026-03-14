use std::path::Path;
use std::process::{Command, Stdio};

use crate::error::{Result, UvrError};

pub struct RCmdInstall {
    pub r_binary: String,
}

impl RCmdInstall {
    pub fn new(r_binary: impl Into<String>) -> Self {
        RCmdInstall { r_binary: r_binary.into() }
    }

    /// Run `R CMD INSTALL --library=<lib_path> --no-test-load <tarball>`.
    /// On failure, the captured stderr is included in the error message.
    pub fn install(&self, tarball: &Path, library: &Path, package_name: &str) -> Result<()> {
        let lib_str = library.to_string_lossy();
        let tarball_str = tarball.to_string_lossy();

        // Derive R_HOME from the binary path (<r_home>/bin/R).
        // Set DYLD_LIBRARY_PATH (macOS) / LD_LIBRARY_PATH (Linux) so the dynamic
        // linker can find libR.dylib even when its embedded install-name still points
        // to the original build location (e.g. /Library/Frameworks/R.framework/…).
        let r_lib_dir = std::path::Path::new(&self.r_binary)
            .parent()             // …/bin/
            .and_then(|p| p.parent()) // …/r-versions/4.4.2/
            .map(|p| p.join("lib"))
            .unwrap_or_default();
        let r_lib_str = r_lib_dir.to_string_lossy();

        // On macOS, include Homebrew library/include paths so that packages with
        // system library dependencies (freetype, harfbuzz, etc.) can find them.
        // Homebrew on Apple Silicon installs to /opt/homebrew; Intel Macs use /usr/local.
        let (brew_lib, brew_inc, brew_pkgconfig) = if cfg!(target_arch = "aarch64") {
            (
                "/opt/homebrew/lib",
                "/opt/homebrew/include",
                "/opt/homebrew/lib/pkgconfig",
            )
        } else {
            (
                "/usr/local/lib",
                "/usr/local/include",
                "/usr/local/lib/pkgconfig",
            )
        };

        let output = Command::new(&self.r_binary)
            .args([
                "CMD",
                "INSTALL",
                &format!("--library={lib_str}"),
                "--no-test-load",
                "--no-staged-install", // avoid staging-dir issues with lazy loading on macOS
                &tarball_str,
            ])
            .env("DYLD_LIBRARY_PATH", r_lib_str.as_ref())
            .env("LD_LIBRARY_PATH", r_lib_str.as_ref())
            .env("PKG_CONFIG_PATH", brew_pkgconfig)
            .env("LDFLAGS", format!("-L{brew_lib}"))
            .env("CPPFLAGS", format!("-I{brew_inc}"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()?;

        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            // Combine both streams — R uses both for diagnostics
            let log = if stderr.is_empty() { stdout } else { stderr };
            return Err(UvrError::Other(format!(
                "R CMD INSTALL failed for '{package_name}' (exit {code}):\n{log}"
            )));
        }

        Ok(())
    }
}
