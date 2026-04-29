use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::error::{Result, UvrError};

/// Default per-package install timeout when neither `--timeout` nor the env var is set.
pub const DEFAULT_INSTALL_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Parse a duration like `30m`, `2h`, `90s`, or a bare number (interpreted as seconds).
/// Returns `None` for unparseable input — callers fall back to the default.
pub fn parse_install_timeout(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (num, suffix) = match s.find(|c: char| !c.is_ascii_digit()) {
        Some(idx) => (&s[..idx], &s[idx..]),
        None => (s, ""),
    };
    let n: u64 = num.parse().ok()?;
    let secs = match suffix.trim() {
        "" | "s" | "sec" | "secs" | "second" | "seconds" => n,
        "m" | "min" | "mins" | "minute" | "minutes" => n.checked_mul(60)?,
        "h" | "hr" | "hrs" | "hour" | "hours" => n.checked_mul(3600)?,
        _ => return None,
    };
    Some(Duration::from_secs(secs))
}

/// Resolve the effective install timeout: explicit override > env var > default.
pub fn effective_install_timeout(explicit: Option<Duration>) -> Duration {
    if let Some(d) = explicit {
        return d;
    }
    if let Ok(env) = std::env::var("UVR_INSTALL_TIMEOUT") {
        if let Some(d) = parse_install_timeout(&env) {
            return d;
        }
    }
    DEFAULT_INSTALL_TIMEOUT
}

/// Remove a stale `00LOCK-<package>/` directory left behind by an aborted
/// `R CMD INSTALL`. Best-effort: if removal fails the next install attempt
/// will surface a clearer error than a silent skip would.
pub fn cleanup_lock_dir(library: &Path, package_name: &str) {
    let lock_dir = library.join(format!("00LOCK-{package_name}"));
    if lock_dir.exists() {
        let _ = std::fs::remove_dir_all(&lock_dir);
    }
}

/// Send a TERM-equivalent signal to a child process by PID. On Unix uses
/// SIGTERM (graceful), on Windows uses TerminateProcess (immediate). Best-effort.
fn kill_pid(pid: u32) {
    #[cfg(unix)]
    unsafe {
        // SIGTERM is graceful; the process gets a chance to clean up. If it
        // ignores TERM, the timeout watchdog can be hardened later with SIGKILL.
        libc::kill(pid as i32, libc::SIGTERM);
    }
    #[cfg(windows)]
    {
        use std::process::Command;
        // taskkill /F /T /PID <pid> kills the whole process tree. This catches
        // R's child Rscript / cc.exe sub-processes that would otherwise live on.
        let _ = Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid; // suppress unused warning on exotic targets
    }
}

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
    /// On any failure (timeout, non-zero exit, parse error), the
    /// `00LOCK-<package>/` directory is removed from `library`.
    pub fn install(&self, tarball: &Path, library: &Path, package_name: &str) -> Result<()> {
        let result: Result<()> = (|| {
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
        })();
        if result.is_err() {
            cleanup_lock_dir(library, package_name);
        }
        result
    }

    /// Like `install`, but streams stderr line-by-line to a callback so the
    /// caller can update a progress spinner with compilation output. The
    /// subprocess is killed if it runs longer than `timeout` (or
    /// `UVR_INSTALL_TIMEOUT`, or 30m default) — see #52. On any failure
    /// the `00LOCK-<package>/` dir is cleaned up.
    pub fn install_streaming<F>(
        &self,
        tarball: &Path,
        library: &Path,
        package_name: &str,
        timeout: Option<Duration>,
        on_line: F,
    ) -> Result<()>
    where
        F: Fn(&str),
    {
        let timeout = effective_install_timeout(timeout);
        let result = self.install_streaming_inner(tarball, library, package_name, timeout, on_line);
        if result.is_err() {
            cleanup_lock_dir(library, package_name);
        }
        result
    }

    fn install_streaming_inner<F>(
        &self,
        tarball: &Path,
        library: &Path,
        package_name: &str,
        timeout: Duration,
        on_line: F,
    ) -> Result<()>
    where
        F: Fn(&str),
    {
        let mut cmd = self.build_cmd(tarball, library);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let mut child = cmd.spawn()?;
        let pid = child.id();

        // Register this install so the SIGINT handler can kill the child + clean
        // up its 00LOCK on Ctrl+C (#58). Deregister at the end (success or fail).
        crate::signal::register(crate::signal::ActiveInstall {
            pid,
            library: library.to_path_buf(),
            package_name: package_name.to_string(),
        });
        struct Deregister(u32);
        impl Drop for Deregister {
            fn drop(&mut self) {
                crate::signal::unregister(self.0);
            }
        }
        let _deregister_guard = Deregister(pid);

        // Watchdog: kill the child if `timeout` elapses before completion.
        // The flag tells the watchdog the install thread is done (success or
        // graceful failure), so the watchdog never kills a finished process.
        let completed = Arc::new(AtomicBool::new(false));
        let timed_out = Arc::new(AtomicBool::new(false));
        let watchdog = {
            let completed = Arc::clone(&completed);
            let timed_out = Arc::clone(&timed_out);
            std::thread::spawn(move || {
                let start = Instant::now();
                while start.elapsed() < timeout {
                    if completed.load(Ordering::SeqCst) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
                if !completed.load(Ordering::SeqCst) {
                    timed_out.store(true, Ordering::SeqCst);
                    kill_pid(pid);
                }
            })
        };

        // Collect all output for error reporting
        let mut all_stderr = String::new();

        // Read stderr line-by-line to update progress. When the watchdog kills
        // the child, the pipe closes and this loop exits naturally.
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
        completed.store(true, Ordering::SeqCst);
        let _ = watchdog.join();

        if timed_out.load(Ordering::SeqCst) {
            return Err(UvrError::Other(format!(
                "Install of '{package_name}' timed out after {}s — killed by uvr (#52). \
                 Override with `--timeout <duration>` or `UVR_INSTALL_TIMEOUT=<duration>` \
                 (e.g. 1h).",
                timeout.as_secs()
            )));
        }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_seconds_default() {
        assert_eq!(parse_install_timeout("90"), Some(Duration::from_secs(90)));
        assert_eq!(parse_install_timeout("90s"), Some(Duration::from_secs(90)));
        assert_eq!(
            parse_install_timeout("90 sec"),
            Some(Duration::from_secs(90))
        );
    }

    #[test]
    fn parse_minutes() {
        assert_eq!(
            parse_install_timeout("30m"),
            Some(Duration::from_secs(1800))
        );
        assert_eq!(
            parse_install_timeout("30 min"),
            Some(Duration::from_secs(1800))
        );
    }

    #[test]
    fn parse_hours() {
        assert_eq!(parse_install_timeout("2h"), Some(Duration::from_secs(7200)));
        assert_eq!(
            parse_install_timeout("1 hour"),
            Some(Duration::from_secs(3600))
        );
    }

    #[test]
    fn parse_invalid_returns_none() {
        assert!(parse_install_timeout("").is_none());
        assert!(parse_install_timeout("nope").is_none());
        assert!(parse_install_timeout("30x").is_none());
        assert!(parse_install_timeout("-5m").is_none());
    }

    #[test]
    fn effective_prefers_explicit() {
        let d = effective_install_timeout(Some(Duration::from_secs(42)));
        assert_eq!(d, Duration::from_secs(42));
    }

    #[test]
    fn effective_default_when_none() {
        // Don't tamper with env in this test — just ensure default kicks in
        // when neither explicit nor a parseable env var is set.
        std::env::remove_var("UVR_INSTALL_TIMEOUT");
        let d = effective_install_timeout(None);
        assert_eq!(d, DEFAULT_INSTALL_TIMEOUT);
    }

    #[test]
    fn cleanup_lock_dir_removes_directory() {
        let tmp = tempfile::TempDir::new().unwrap();
        let lib = tmp.path();
        let lock = lib.join("00LOCK-foo");
        std::fs::create_dir_all(lock.join("nested")).unwrap();
        std::fs::write(lock.join("file"), "x").unwrap();
        cleanup_lock_dir(lib, "foo");
        assert!(!lock.exists());
    }

    #[test]
    fn cleanup_lock_dir_handles_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Should not panic when nothing's there.
        cleanup_lock_dir(tmp.path(), "doesnotexist");
    }
}
