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
    if let Some(env) = crate::env_vars::install_timeout() {
        if let Some(d) = parse_install_timeout(&env) {
            return d;
        }
    }
    DEFAULT_INSTALL_TIMEOUT
}

/// Combine stdout and stderr for an error log. `R CMD INSTALL` splits its
/// output across both streams inconsistently (progress messages and the
/// actual `Error in ... :` cause can land on either one depending on
/// platform/R version), so picking a single stream risks silently dropping
/// the line that explains the failure. Falls back to whichever stream is
/// non-empty when the other is blank, so single-stream output isn't padded
/// with a stray blank line.
fn combine_logs(stdout: &str, stderr: &str) -> String {
    match (stdout.trim().is_empty(), stderr.trim().is_empty()) {
        (true, true) => String::new(),
        (true, false) => stderr.to_string(),
        (false, true) => stdout.to_string(),
        (false, false) => format!("{stdout}\n{stderr}"),
    }
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

/// Gracefully terminate an install child and its descendants. On Unix the
/// streaming child is its own process-group leader (see `build_cmd`), so we
/// SIGTERM the whole group (`-pid`) — this reaches the `make`/`cc`/`Rscript`
/// grandchildren that inherit the stdout pipe. Killing only the direct PID
/// would leave them holding the pipe open, deadlocking the drain (#52/#113).
/// On Windows `taskkill /T` already covers the tree. Best-effort.
fn kill_pid(pid: u32) {
    #[cfg(unix)]
    unsafe {
        // Negative pid → the process group whose leader is `pid`. SIGTERM is
        // graceful; the watchdog escalates to SIGKILL (`hard_kill_group`) if
        // the build ignores TERM.
        libc::kill(-(pid as i32), libc::SIGTERM);
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

/// SIGKILL the install child's process group. Unix-only escalation for a
/// build that ignored the SIGTERM from `kill_pid`. No-op elsewhere (Windows
/// `taskkill /F` already force-kills the tree in `kill_pid`).
#[cfg(unix)]
fn hard_kill_group(pid: u32) {
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
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
                let log = combine_logs(&stdout, &stderr);
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

        // Put the streaming child in its own process group (pre-exec setpgid)
        // so the timeout watchdog can signal the whole build subtree (R + make
        // + cc + Rscript) at once via `kill(-pid)`. Without this, killing only R
        // leaves its grandchildren holding the stdout pipe open and the drain
        // deadlocks (#113). This child is registered with the signal handler
        // below, so detaching it from the terminal's foreground group is safe —
        // uvr's own SIGINT handler kills the group deliberately. Applied only
        // here, NOT in the shared build_cmd: the quiet `install()` path isn't
        // registered for signal cleanup and must stay in the terminal group so
        // Ctrl+C still reaches it.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }

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
                    // Escalate to SIGKILL on the group if a TERM-ignoring build
                    // (or a wedged grandchild) hasn't exited within the grace
                    // period. `completed` flips once child.wait() returns on the
                    // install thread, so we only hard-kill if TERM didn't work.
                    #[cfg(unix)]
                    {
                        let grace = Duration::from_secs(2);
                        let start = Instant::now();
                        while start.elapsed() < grace {
                            if completed.load(Ordering::SeqCst) {
                                return;
                            }
                            std::thread::sleep(Duration::from_millis(100));
                        }
                        if !completed.load(Ordering::SeqCst) {
                            hard_kill_group(pid);
                        }
                    }
                }
            })
        };

        // Collect all output for error reporting
        let mut all_stderr = String::new();

        // Drain stdout on a dedicated thread. `R CMD INSTALL` writes to both
        // stdout and stderr; if we only read stderr (below) the stdout pipe
        // fills its ~64KB kernel buffer on a verbose build and the child blocks
        // forever on write(2), deadlocking against our own `child.wait()` (#52).
        let stdout_drain = child.stdout.take().map(|stdout| {
            std::thread::spawn(move || {
                let mut s = String::new();
                let _ = std::io::Read::read_to_string(&mut BufReader::new(stdout), &mut s);
                s
            })
        });

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
        let all_stdout = stdout_drain.and_then(|h| h.join().ok()).unwrap_or_default();

        // Once the OS reaps the child, the PID can be recycled. Drop the
        // registration immediately so a concurrent SIGINT handler can't snapshot
        // the registry and SIGTERM an unrelated process that inherits the PID.
        // The Deregister Drop guard below stays as a safety net for panic paths.
        crate::signal::unregister(pid);

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
            let log = combine_logs(&all_stdout, &all_stderr);
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

        // Neutralize the project / user .Rprofile during install. Project
        // .Rprofiles often contain `source("renv/activate.R")` (renv pattern)
        // or other side-effecting startup code that aborts R before the
        // install begins. Pointing R_PROFILE_USER at the platform's null
        // device tells R to skip the user/project Rprofile. The library
        // destination is set explicitly via --library, so the suppressed
        // `.libPaths()` call has no install-side effect.
        // (We deliberately leave R_ENVIRON_USER alone — ~/.Renviron often
        // holds load-bearing TZ / locale settings.)
        let null_device = if cfg!(windows) { "NUL" } else { "/dev/null" };
        cmd.env("R_PROFILE_USER", null_device);

        // The *site* profile goes the other way: point it at uvr's own so the
        // OpenMP runtime is loaded in the lazy-loading child sessions too
        // (#261). Inherited by every R this install spawns, which is the
        // point.
        if let Some(profile) =
            crate::r_version::openmp::runtime_site_profile_for_binary(Path::new(&self.r_binary))
        {
            for (k, v) in crate::r_version::openmp::site_profile_env(&profile) {
                cmd.env(k, v);
            }
        }

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
            // Prepend R's own lib dir to (DY)LD_LIBRARY_PATH rather than
            // replacing it. The parent process may carry load-bearing entries
            // — e.g. on HPC clusters where BLAS/LAPACK are provided as
            // separate shared libraries (OpenBLAS, MKL, FlexiBLAS) whose paths
            // are set by the environment module system. Clobbering that value
            // causes R CMD INSTALL's lazy-loading phase to fail with
            // "libRlapack.so: cannot open shared object file" even when the
            // library is available under a module-provided path.
            //
            // This mirrors how the PATH extension is handled above (line
            // `format!("{bin}:{p}")`): prepend, don't replace.
            let prepend_lib = |var: &str| -> String {
                match std::env::var(var) {
                    Ok(existing) if !existing.is_empty() => {
                        format!("{r_lib_str}:{existing}")
                    }
                    _ => r_lib_str.to_string(),
                }
            };
            cmd.env("DYLD_LIBRARY_PATH", prepend_lib("DYLD_LIBRARY_PATH"))
                .env("LD_LIBRARY_PATH", prepend_lib("LD_LIBRARY_PATH"));
            // R reads R_LD_LIBRARY_PATH and prepends it to LD_LIBRARY_PATH in
            // every subprocess it spawns — including the byte-compilation child
            // during `R CMD INSTALL`. On HPC clusters this is the only way to
            // pass the module-provided BLAS/LAPACK paths into R's own children.
            //
            // However, the R wrapper script (`bin/R`) sources `etc/ldpaths`
            // which sets `R_LD_LIBRARY_PATH = ${R_HOME}/lib` when it is not
            // already set. On Posit portable builds the correct `R_HOME/lib` is
            // a subdirectory of `r_lib_dir` (e.g. `lib/R/lib/`), so setting
            // `R_LD_LIBRARY_PATH` to `r_lib_dir` before `ldpaths` runs would
            // suppress `ldpaths`'s correct default and leave `exec/R` unable to
            // find `libR.so`. Only prepend when `libR.so` / `libR.dylib` is
            // actually present in `r_lib_dir`, i.e. we computed the right path.
            let libr_present =
                r_lib_dir.join("libR.so").exists() || r_lib_dir.join("libR.dylib").exists();
            if libr_present {
                cmd.env("R_LD_LIBRARY_PATH", prepend_lib("R_LD_LIBRARY_PATH"));
            }

            // Put the managed R's `bin` on PATH so package build scripts that
            // shell out to `Rscript` / `R` resolve them — e.g. cytolib /
            // RProtoBufLib emit their link flags via `Rscript` during flowCore's
            // build, which otherwise fails with "Rscript: No such file or
            // directory" since uvr's R isn't on the user's PATH. Windows handles
            // its PATH (Rtools) in the branch above.
            if let Some(r_bin_dir) = std::path::Path::new(&self.r_binary).parent() {
                let bin = r_bin_dir.display().to_string();
                // Avoid a trailing colon when PATH is unset/empty — on POSIX an
                // empty PATH entry resolves to the cwd, which we must not add.
                let new_path = match std::env::var("PATH") {
                    Ok(p) if !p.is_empty() => format!("{bin}:{p}"),
                    _ => bin,
                };
                cmd.env("PATH", new_path);
            }

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
    fn combine_logs_prefers_non_empty_single_stream() {
        assert_eq!(combine_logs("", "boom"), "boom");
        assert_eq!(combine_logs("progress", ""), "progress");
        assert_eq!(combine_logs("", ""), "");
    }

    #[test]
    fn combine_logs_merges_both_streams() {
        // Regression: R CMD INSTALL splits progress and the actual `Error
        // in ... :` cause across stdout/stderr inconsistently — dropping
        // either one can hide why lazy loading failed.
        let log = combine_logs(
            "** byte-compile...\nERROR: lazy loading failed",
            "Error in loadNamespace(x) : there is no package called 'foo'",
        );
        assert!(log.contains("ERROR: lazy loading failed"));
        assert!(log.contains("there is no package called 'foo'"));
    }

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
        // Serialize against other UVR_INSTALL_TIMEOUT-mutating tests (env_vars'
        // test_env_vars sets it) — env vars are process-global (#flaky-ci).
        let _env = crate::env_vars::env_lock();
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

    /// Regression for #52: a build that floods stdout beyond the OS pipe buffer
    /// (~64KB) must not deadlock. The streaming installer used to drain only
    /// stderr, so a verbose build (s2, Matrix, StanHeaders) blocked on write(2)
    /// to a full stdout pipe while we waited on it. With a generous timeout the
    /// old code would block until the watchdog killed the child and returned a
    /// timeout error; the fixed code drains stdout concurrently and returns Ok
    /// in milliseconds.
    #[cfg(unix)]
    #[test]
    fn streaming_does_not_deadlock_on_large_stdout() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::TempDir::new().unwrap();
        // Fake `R`: ignore args, write ~256KB to stdout (well past any pipe
        // buffer), a token line to stderr, then exit 0.
        let fake_r = tmp.path().join("R");
        std::fs::write(
            &fake_r,
            "#!/bin/sh\n\
             for i in $(seq 1 5000); do\n\
             echo \"stdout line $i ........................................\"\n\
             done\n\
             echo 'compiling fake 1.0' 1>&2\n\
             exit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake_r, std::fs::Permissions::from_mode(0o755)).unwrap();

        let lib = tmp.path().join("lib");
        std::fs::create_dir_all(&lib).unwrap();
        let tarball = tmp.path().join("fake_1.0.tar.gz");
        std::fs::write(&tarball, b"not a real tarball").unwrap();

        let installer = RCmdInstall::new(fake_r.to_string_lossy().to_string());
        let result = installer.install_streaming(
            &tarball,
            &lib,
            "fake",
            Some(Duration::from_secs(20)),
            |_| {},
        );
        assert!(result.is_ok(), "deadlocked or failed: {result:?}");
    }

    /// Regression for #113: on timeout, a build's grandchildren (make/cc/Rscript)
    /// that inherited the stdout pipe must also be killed, or the stdout drain
    /// thread blocks forever on a pipe nobody closes. The fix makes the child a
    /// process-group leader and signals the whole group (`kill(-pid)`). This
    /// fake `R` backgrounds a long-lived grandchild that holds stdout open and
    /// then hangs; a single-PID kill would orphan the grandchild and the call
    /// would never return. With the group kill it returns a timeout error
    /// promptly. The test *returning at all* is the assertion.
    #[cfg(unix)]
    #[test]
    fn streaming_timeout_kills_grandchildren_holding_pipe() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::TempDir::new().unwrap();
        let fake_r = tmp.path().join("R");
        // `sleep 60 &` is a grandchild that inherits stdout and outlives a kill
        // aimed only at the direct child; the parent then blocks on its own
        // sleep. Both share the child's process group, so a group SIGTERM reaps
        // them together.
        std::fs::write(
            &fake_r,
            "#!/bin/sh\n\
             sleep 60 &\n\
             echo 'building fake 1.0'\n\
             sleep 60\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake_r, std::fs::Permissions::from_mode(0o755)).unwrap();

        let lib = tmp.path().join("lib");
        std::fs::create_dir_all(&lib).unwrap();
        let tarball = tmp.path().join("fake_1.0.tar.gz");
        std::fs::write(&tarball, b"not a real tarball").unwrap();

        let installer = RCmdInstall::new(fake_r.to_string_lossy().to_string());
        let result = installer.install_streaming(
            &tarball,
            &lib,
            "fake",
            Some(Duration::from_secs(2)),
            |_| {},
        );
        // Must return (not hang) with a timeout error.
        assert!(result.is_err(), "expected timeout error, got {result:?}");
        let msg = format!("{:?}", result.unwrap_err());
        assert!(msg.contains("timed out"), "unexpected error: {msg}");
    }
}
