use std::path::{Path, PathBuf};

use crate::error::{Result, UvrError};
use crate::lockfile::Lockfile;
use crate::manifest::Manifest;

pub const MANIFEST_FILE: &str = "uvr.toml";
pub const DESCRIPTION_FILE: &str = "DESCRIPTION";
pub const LOCK_FILE: &str = "uvr.lock";
pub const R_VERSION_FILE: &str = ".r-version";
pub const DOT_UVR_DIR: &str = ".uvr";
pub const LIBRARY_DIR: &str = "library";

/// Whether this project's manifest came from `uvr.toml` or a `DESCRIPTION` file.
#[derive(Debug, Clone, PartialEq)]
pub enum ManifestSource {
    /// Standard `uvr.toml` — full read/write support.
    Toml,
    /// R `DESCRIPTION` file — read-only; use `uvr init` to create a `uvr.toml`.
    Description,
}

/// Represents a resolved uvr project rooted at a directory containing `uvr.toml`
/// or an R `DESCRIPTION` file.
#[derive(Debug, Clone)]
pub struct Project {
    pub root: PathBuf,
    pub manifest: Manifest,
    pub manifest_source: ManifestSource,
}

impl Project {
    /// Walk up from `start` looking for `uvr.toml` (preferred) or `DESCRIPTION`.
    pub fn find(start: &Path) -> Result<Self> {
        let mut dir = start.to_path_buf();
        loop {
            let toml_candidate = dir.join(MANIFEST_FILE);
            if toml_candidate.exists() {
                let manifest = Manifest::from_file(&toml_candidate)?;
                return Ok(Project {
                    root: dir,
                    manifest,
                    manifest_source: ManifestSource::Toml,
                });
            }
            let desc_candidate = dir.join(DESCRIPTION_FILE);
            if desc_candidate.exists() {
                let manifest = Manifest::from_description_file(&desc_candidate)?;
                return Ok(Project {
                    root: dir,
                    manifest,
                    manifest_source: ManifestSource::Description,
                });
            }
            if !dir.pop() {
                return Err(UvrError::ManifestNotFound);
            }
        }
    }

    /// Find from the current working directory.
    pub fn find_cwd() -> Result<Self> {
        let cwd = std::env::current_dir()?;
        Self::find(&cwd)
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.root.join(MANIFEST_FILE)
    }

    pub fn lock_path(&self) -> PathBuf {
        self.root.join(LOCK_FILE)
    }

    pub fn dot_uvr_dir(&self) -> PathBuf {
        self.root.join(DOT_UVR_DIR)
    }

    pub fn library_path(&self) -> PathBuf {
        self.dot_uvr_dir().join(LIBRARY_DIR)
    }

    pub fn load_lockfile(&self) -> Result<Option<Lockfile>> {
        let p = self.lock_path();
        if p.exists() {
            Ok(Some(Lockfile::from_file(&p)?))
        } else {
            Ok(None)
        }
    }

    pub fn save_manifest(&self) -> Result<()> {
        if self.manifest_source == ManifestSource::Description {
            return Err(UvrError::Other(
                "Cannot modify DESCRIPTION directly. Run `uvr init` to create a uvr.toml."
                    .to_string(),
            ));
        }
        self.manifest.write(&self.manifest_path())
    }

    pub fn save_lockfile(&self, lf: &Lockfile) -> Result<()> {
        lf.write(&self.lock_path())
    }

    /// Ensure `.uvr/library/` exists.
    pub fn ensure_library_dir(&self) -> Result<()> {
        std::fs::create_dir_all(self.library_path())?;
        Ok(())
    }

    pub fn r_version_pin_path(&self) -> PathBuf {
        self.root.join(R_VERSION_FILE)
    }

    /// Read the exact R version from `.r-version`, if present.
    pub fn read_r_version_pin(&self) -> Option<String> {
        read_r_version_pin_from(&self.root)
    }

    /// Write an exact version to `.r-version`.
    pub fn write_r_version_pin(&self, version: &str) -> Result<()> {
        let path = self.r_version_pin_path();
        crate::manifest::atomic_write(&path, format!("{version}\n").as_bytes())
    }
}

/// Walk up from `dir` looking for `.r-version` and return its trimmed contents.
pub fn read_r_version_pin_from(dir: &Path) -> Option<String> {
    let mut current = dir.to_path_buf();
    loop {
        let candidate = current.join(R_VERSION_FILE);
        if candidate.exists() {
            let content = std::fs::read_to_string(&candidate).ok()?;
            let version = content.trim().to_string();
            if !version.is_empty() {
                return Some(version);
            }
        }
        if !current.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_manifest(dir: &std::path::Path, name: &str) {
        let content = format!("[project]\nname = \"{name}\"\n\n[dependencies]\n");
        std::fs::write(dir.join(MANIFEST_FILE), content).unwrap();
    }

    #[test]
    fn find_project_in_current_dir() {
        let dir = TempDir::new().unwrap();
        write_manifest(dir.path(), "test-proj");
        let project = Project::find(dir.path()).unwrap();
        assert_eq!(project.manifest.project.name, "test-proj");
        assert_eq!(project.manifest_source, ManifestSource::Toml);
    }

    #[test]
    fn find_project_walks_up() {
        let dir = TempDir::new().unwrap();
        write_manifest(dir.path(), "parent-proj");
        let sub = dir.path().join("sub").join("deep");
        std::fs::create_dir_all(&sub).unwrap();
        let project = Project::find(&sub).unwrap();
        assert_eq!(project.manifest.project.name, "parent-proj");
        assert_eq!(project.root, dir.path());
    }

    #[test]
    fn find_project_not_found() {
        let dir = TempDir::new().unwrap();
        let result = Project::find(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn find_description_fallback() {
        let dir = TempDir::new().unwrap();
        let desc = "Package: mypkg\nTitle: Test\nVersion: 1.0.0\n";
        std::fs::write(dir.path().join(DESCRIPTION_FILE), desc).unwrap();
        let project = Project::find(dir.path()).unwrap();
        assert_eq!(project.manifest_source, ManifestSource::Description);
        assert_eq!(project.manifest.project.name, "mypkg");
    }

    #[test]
    fn toml_preferred_over_description() {
        let dir = TempDir::new().unwrap();
        write_manifest(dir.path(), "toml-proj");
        let desc = "Package: desc-proj\nTitle: Test\nVersion: 1.0.0\n";
        std::fs::write(dir.path().join(DESCRIPTION_FILE), desc).unwrap();
        let project = Project::find(dir.path()).unwrap();
        assert_eq!(project.manifest_source, ManifestSource::Toml);
        assert_eq!(project.manifest.project.name, "toml-proj");
    }

    #[test]
    fn path_helpers() {
        let dir = TempDir::new().unwrap();
        write_manifest(dir.path(), "paths-test");
        let project = Project::find(dir.path()).unwrap();
        assert_eq!(project.manifest_path(), dir.path().join("uvr.toml"));
        assert_eq!(project.lock_path(), dir.path().join("uvr.lock"));
        assert_eq!(project.dot_uvr_dir(), dir.path().join(".uvr"));
        assert_eq!(
            project.library_path(),
            dir.path().join(".uvr").join("library")
        );
    }

    #[test]
    fn load_lockfile_none_when_missing() {
        let dir = TempDir::new().unwrap();
        write_manifest(dir.path(), "no-lock");
        let project = Project::find(dir.path()).unwrap();
        assert!(project.load_lockfile().unwrap().is_none());
    }

    #[test]
    fn ensure_library_dir_creates_dirs() {
        let dir = TempDir::new().unwrap();
        write_manifest(dir.path(), "lib-test");
        let project = Project::find(dir.path()).unwrap();
        assert!(!project.library_path().exists());
        project.ensure_library_dir().unwrap();
        assert!(project.library_path().exists());
    }

    #[test]
    fn save_manifest_rejects_description_source() {
        let dir = TempDir::new().unwrap();
        let desc = "Package: mypkg\nTitle: Test\nVersion: 1.0.0\n";
        std::fs::write(dir.path().join(DESCRIPTION_FILE), desc).unwrap();
        let project = Project::find(dir.path()).unwrap();
        assert!(project.save_manifest().is_err());
    }

    #[test]
    fn write_and_read_r_version_pin() {
        let dir = TempDir::new().unwrap();
        write_manifest(dir.path(), "pin-test");
        let project = Project::find(dir.path()).unwrap();
        assert!(project.read_r_version_pin().is_none());
        project.write_r_version_pin("4.3.2").unwrap();
        assert_eq!(project.read_r_version_pin(), Some("4.3.2".to_string()));
    }

    #[test]
    fn read_r_version_pin_walks_up() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join(R_VERSION_FILE), "4.4.1\n").unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(read_r_version_pin_from(&sub), Some("4.4.1".to_string()));
    }

    #[test]
    fn read_r_version_pin_ignores_empty() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join(R_VERSION_FILE), "  \n").unwrap();
        // Empty content → treated as no pin (walks up further, finds nothing)
        // Since we're in a tmpdir, there's no parent .r-version, so returns None
        assert!(read_r_version_pin_from(dir.path()).is_none());
    }
}
