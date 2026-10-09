//! Marker for packages uvr itself put into a library (#255).
//!
//! `uvr sync` prunes only what carries this marker and is absent from
//! `uvr.lock`, so a package installed by `install.packages()` or another tool
//! (carrier resolves the library through `R_LIBS_USER`) is left alone.
//!
//! The file lives under the installed package's `Meta/`, which R ignores
//! apart from its own files. A reinstall by R replaces the whole package
//! directory, so a package the user reinstalls by hand loses the marker.
//!
//! A library entry can be a symlink into the shared package cache (the
//! Linux attach), so the marker may be written into a cache entry that other
//! projects use too. That is correct, because only uvr creates cache entries, and it is
//! why the content does not depend on the project and why an existing marker
//! is never rewritten.

use std::path::{Path, PathBuf};

pub const MARKER_FILENAME: &str = "uvr-installed";

const MARKER_CONTENTS: &str = concat!("uvr/", env!("CARGO_PKG_VERSION"), "\n");

pub fn marker_path(installed_pkg_dir: &Path) -> PathBuf {
    installed_pkg_dir.join("Meta").join(MARKER_FILENAME)
}

/// Whether uvr installed this package. Presence is the whole signal; the
/// content is informational.
pub fn is_marked(installed_pkg_dir: &Path) -> bool {
    std::fs::symlink_metadata(marker_path(installed_pkg_dir)).is_ok_and(|md| md.is_file())
}

/// Stamp `installed_pkg_dir` as uvr-installed. A no-op when it is already
/// stamped, or when there is no package directory to stamp.
pub fn write_marker(installed_pkg_dir: &Path) -> std::io::Result<()> {
    if !installed_pkg_dir.is_dir() || is_marked(installed_pkg_dir) {
        return Ok(());
    }
    let refuse = |p: &Path| {
        std::io::Error::other(format!(
            "refusing to write the uvr marker through '{}'",
            p.display()
        ))
    };
    let meta_dir = installed_pkg_dir.join("Meta");
    match std::fs::symlink_metadata(&meta_dir) {
        Ok(md) if md.is_dir() => {}
        // An installed tree may carry attacker-chosen paths; never write through one.
        Ok(_) => return Err(refuse(&meta_dir)),
        Err(_) => std::fs::create_dir(&meta_dir)?,
    }
    let path = marker_path(installed_pkg_dir);
    // Not a regular file (is_marked said so), so a symlink or a directory.
    if std::fs::symlink_metadata(&path).is_ok() {
        return Err(refuse(&path));
    }
    std::fs::write(&path, MARKER_CONTENTS)
}

/// [`write_marker`], logging instead of failing. An unmarked package is only
/// ever kept, never pruned, so a failed stamp must not fail an install.
pub fn mark(installed_pkg_dir: &Path) {
    if let Err(e) = write_marker(installed_pkg_dir) {
        tracing::debug!(
            "Could not mark {} as uvr-installed: {e}",
            installed_pkg_dir.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_marker_stamps_once_and_keeps_an_existing_stamp() {
        let tmp = tempfile::TempDir::new().unwrap();
        let pkg = tmp.path().join("pkg");
        std::fs::create_dir(&pkg).unwrap();
        assert!(!is_marked(&pkg));

        // Creates Meta/ when missing.
        write_marker(&pkg).unwrap();
        assert!(is_marked(&pkg));

        // A stamp from another uvr version (e.g. in a shared cache entry)
        // is left as it is.
        std::fs::write(marker_path(&pkg), "uvr/0.0.1\n").unwrap();
        write_marker(&pkg).unwrap();
        assert_eq!(
            std::fs::read_to_string(marker_path(&pkg)).unwrap(),
            "uvr/0.0.1\n"
        );
    }

    #[test]
    fn write_marker_skips_a_missing_package_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let pkg = tmp.path().join("never-installed");
        write_marker(&pkg).unwrap();
        assert!(!pkg.exists(), "must not create a package directory");
    }

    #[cfg(unix)]
    #[test]
    fn write_marker_refuses_to_write_through_symlinks() {
        let tmp = tempfile::TempDir::new().unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();

        // Meta itself is a symlink.
        let pkg = tmp.path().join("a");
        std::fs::create_dir(&pkg).unwrap();
        std::os::unix::fs::symlink(&elsewhere, pkg.join("Meta")).unwrap();
        assert!(write_marker(&pkg).is_err());
        assert!(!elsewhere.join(MARKER_FILENAME).exists());

        // The marker path is a symlink.
        let pkg = tmp.path().join("b");
        std::fs::create_dir_all(pkg.join("Meta")).unwrap();
        let target = elsewhere.join("target");
        std::os::unix::fs::symlink(&target, marker_path(&pkg)).unwrap();
        assert!(write_marker(&pkg).is_err());
        assert!(!target.exists());
        assert!(!is_marked(&pkg));
    }
}
