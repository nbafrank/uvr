//! Process-global registry of in-flight `R CMD INSTALL` children, used by the
//! SIGINT / Ctrl+C handler so an interrupted `uvr sync` kills its subprocess
//! and cleans up the `00LOCK-<pkg>/` dir before exiting (#58).
//!
//! Single-threaded install loop today, but the registry is a `Vec` so the
//! design extends to parallel installs without changes here.

use std::path::PathBuf;
use std::sync::Mutex;

use crate::installer::r_cmd_install::cleanup_lock_dir;

#[derive(Debug, Clone)]
pub struct ActiveInstall {
    pub pid: u32,
    pub library: PathBuf,
    pub package_name: String,
}

static ACTIVE: Mutex<Vec<ActiveInstall>> = Mutex::new(Vec::new());

/// Record an in-flight `R CMD INSTALL`. Call this immediately after spawning.
pub fn register(info: ActiveInstall) {
    if let Ok(mut guard) = ACTIVE.lock() {
        guard.push(info);
    }
}

/// Drop the in-flight record for this PID. Call this when the install
/// completes (success or failure) so the SIGINT handler doesn't try to
/// kill an already-finished process.
pub fn unregister(pid: u32) {
    if let Ok(mut guard) = ACTIVE.lock() {
        guard.retain(|a| a.pid != pid);
    }
}

/// Snapshot the current in-flight installs and drain the registry. Used by
/// the signal handler so callbacks fire exactly once per Ctrl+C.
pub fn drain() -> Vec<ActiveInstall> {
    ACTIVE
        .lock()
        .map(|mut g| std::mem::take(&mut *g))
        .unwrap_or_default()
}

/// Kill every in-flight install and remove its `00LOCK-<pkg>/` dir.
/// Call from the SIGINT handler before exiting.
pub fn kill_and_cleanup_all() {
    for info in drain() {
        kill_pid(info.pid);
        cleanup_lock_dir(&info.library, &info.package_name);
    }
}

fn kill_pid(pid: u32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
    #[cfg(windows)]
    {
        use std::process::Command;
        let _ = Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_unregister_roundtrip() {
        // Use an unlikely PID to avoid colliding with parallel test PIDs.
        let pid = 9_999_991;
        register(ActiveInstall {
            pid,
            library: PathBuf::from("/tmp/lib"),
            package_name: "fake".into(),
        });
        unregister(pid);
        assert!(!drain().iter().any(|a| a.pid == pid));
    }
}
