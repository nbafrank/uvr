//! Starting child processes.

use std::io::{ErrorKind, Result};
use std::time::Duration;

/// Run `start` (a `Command::spawn`, `output` or `status` call) again while it
/// fails with `ETXTBSY` (#301). Any other result returns at once.
///
/// On Linux, `execve` of a file fails with `ETXTBSY` while any process holds
/// the file open for writing. A file that was just written and closed can
/// still be open in a child that another thread forked before the close: the
/// descriptor is `O_CLOEXEC`, so that child keeps it only until its own
/// `exec`. The window is short, but the test suite writes fake `R` scripts and
/// runs them while other tests start processes, and it hits the window now and
/// then. A program that uvr has just written and then runs is exposed too.
pub fn retry_text_file_busy<T>(mut start: impl FnMut() -> Result<T>) -> Result<T> {
    let mut delay = Duration::from_millis(10);
    for _ in 0..6 {
        match start() {
            Err(e) if e.kind() == ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(delay);
                delay *= 2;
            }
            result => return result,
        }
    }
    start()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_only_a_busy_executable() {
        let mut calls = 0;
        let result = retry_text_file_busy(|| {
            calls += 1;
            if calls < 3 {
                Err(ErrorKind::ExecutableFileBusy.into())
            } else {
                Ok(calls)
            }
        });
        assert_eq!(result.unwrap(), 3);

        let mut calls = 0;
        let result: Result<()> = retry_text_file_busy(|| {
            calls += 1;
            Err(ErrorKind::NotFound.into())
        });
        assert_eq!(result.unwrap_err().kind(), ErrorKind::NotFound);
        assert_eq!(calls, 1, "only a busy executable is retried");

        let mut calls = 0;
        let result: Result<()> = retry_text_file_busy(|| {
            calls += 1;
            Err(ErrorKind::ExecutableFileBusy.into())
        });
        assert_eq!(result.unwrap_err().kind(), ErrorKind::ExecutableFileBusy);
        assert_eq!(calls, 7, "a file that stays busy is given up on");
    }

    /// The race made certain: a script that this process still holds open for
    /// writing cannot run until the writer closes it.
    #[cfg(target_os = "linux")]
    #[test]
    fn runs_a_script_once_its_writer_closes() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("busy.sh");
        let mut writer = std::fs::File::create(&script).unwrap();
        writer.write_all(b"#!/bin/sh\necho ran\n").unwrap();
        writer.flush().unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        match Command::new(&script).output() {
            Err(e) if e.kind() == ErrorKind::ExecutableFileBusy => {}
            // A kernel that does not refuse this has nothing to retry.
            _ => return,
        }
        let closer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            drop(writer);
        });
        let output = retry_text_file_busy(|| Command::new(&script).output()).unwrap();
        closer.join().unwrap();
        assert_eq!(output.stdout, b"ran\n");
    }
}
