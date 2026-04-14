use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};

use crate::error::{Result, UvrError};

pub struct RCmdInstall {
    pub r_binary: String,
}

impl RCmdInstall {
    pub fn new(r_binary: impl Into<String>) -> Self {
        RCmdInstall {
            r_binary: r_binary.into(),
        }
    }

    /// Run `R CMD INSTALL --library=<lib_path> --no-test-load <tarball>`.
    /// On failure, the captured stderr is included in the error message.
    pub fn install(&self, tarball: &Path, library: &Path, package_name: &str) -> Result<()> {
        let mut cmd = self.build_cmd(tarball, library);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let output = cmd.output()?;

        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let log = if stderr.is_empty() { stdout } else { stderr };
            return Err(UvrError::Other(format!(
                "R CMD INSTALL failed for '{package_name}' (exit {code}):\n{log}"
            )));
        }

        Ok(())
    }

    /// Like `install`, but streams stderr line-by-line to a callback so the
    /// caller can update a progress spinner with compilation output.
    pub fn install_streaming<F>(
        &self,
        tarball: &Path,
        library: &Path,
        package_name: &str,
        on_line: F,
    ) -> Result<()>
    where
        F: Fn(&str),
    {
        let mut cmd = self.build_cmd(tarball, library);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let mut child = cmd.spawn()?;

        // Collect all output for error reporting
        let mut all_stderr = String::new();

        // Read stderr line-by-line to update progress
        if let Some(stderr) = child.stderr.take() {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(|l| l.ok()) {
                all_stderr.push_str(&line);
                all_stderr.push('\n');
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    on_line(trimmed);
                }
            }
        }

        let status = child.wait()?;
        if !status.success() {
            let code = status.code().unwrap_or(-1);
            // Also grab stdout if stderr was empty
            let log = if all_stderr.trim().is_empty() {
                if let Some(mut stdout) = child.stdout.take() {
                    let mut s = String::new();
                    std::io::Read::read_to_string(&mut stdout, &mut s).unwrap_or(0);
                    s
                } else {
                    all_stderr
                }
            } else {
                all_stderr
            };
            return Err(UvrError::Other(format!(
                "R CMD INSTALL failed for '{package_name}' (exit {code}):\n{log}"
            )));
        }

        Ok(())
    }

    fn build_cmd(&self, tarball: &Path, library: &Path) -> Command {
        let lib_str = library.to_string_lossy();
        let tarball_str = tarball.to_string_lossy();

        let r_lib_dir = std::path::Path::new(&self.r_binary)
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.join("lib"))
            .unwrap_or_default();
        let r_lib_str = r_lib_dir.to_string_lossy();

        let mut cmd = Command::new(&self.r_binary);
        cmd.args([
            "CMD",
            "INSTALL",
            &format!("--library={lib_str}"),
            "--no-test-load",
            "--no-staged-install",
            &tarball_str,
        ]);

        if cfg!(target_os = "windows") {
            let mut path_ext = String::new();
            let rtools_candidates: Vec<String> = [
                std::env::var("RTOOLS45_HOME").ok(),
                std::env::var("RTOOLS44_HOME").ok(),
                std::env::var("RTOOLS43_HOME").ok(),
                Some("C:\\rtools45".to_string()),
                Some("C:\\rtools44".to_string()),
                Some("C:\\rtools43".to_string()),
            ]
            .into_iter()
            .flatten()
            .collect();

            for rtools in &rtools_candidates {
                let rtools_path = std::path::Path::new(rtools);
                if rtools_path.exists() {
                    let usr_bin = rtools_path.join("usr").join("bin");
                    let mingw_bin = rtools_path
                        .join("x86_64-w64-mingw32.static.posix")
                        .join("bin");
                    if usr_bin.exists() {
                        path_ext.push_str(&usr_bin.to_string_lossy());
                        path_ext.push(';');
                    }
                    if mingw_bin.exists() {
                        path_ext.push_str(&mingw_bin.to_string_lossy());
                        path_ext.push(';');
                    }
                    break;
                }
            }
            if !path_ext.is_empty() {
                let existing_path = std::env::var("PATH").unwrap_or_default();
                cmd.env("PATH", format!("{path_ext}{existing_path}"));
            }
        } else {
            cmd.env("DYLD_LIBRARY_PATH", r_lib_str.as_ref())
                .env("LD_LIBRARY_PATH", r_lib_str.as_ref());

            if cfg!(target_os = "macos") {
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
                cmd.env("PKG_CONFIG_PATH", brew_pkgconfig)
                    .env("LDFLAGS", format!("-L{brew_lib}"))
                    .env("CPPFLAGS", format!("-I{brew_inc}"));
            }
        }

        cmd
    }
}
