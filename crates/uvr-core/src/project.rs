use std::path::{Path, PathBuf};

use crate::error::{Result, UvrError};
use crate::lockfile::Lockfile;
use crate::manifest::Manifest;

pub const MANIFEST_FILE: &str = "uvr.toml";
pub const LOCK_FILE: &str = "uvr.lock";
pub const R_VERSION_FILE: &str = ".r-version";
pub const DOT_UVR_DIR: &str = ".uvr";
pub const LIBRARY_DIR: &str = "library";

/// Represents a resolved uvr project rooted at a directory containing `uvr.toml`.
#[derive(Debug, Clone)]
pub struct Project {
    pub root: PathBuf,
    pub manifest: Manifest,
}

impl Project {
    /// Walk up from `start` looking for `uvr.toml`.
    pub fn find(start: &Path) -> Result<Self> {
        let mut dir = start.to_path_buf();
        loop {
            let candidate = dir.join(MANIFEST_FILE);
            if candidate.exists() {
                let manifest = Manifest::from_file(&candidate)?;
                return Ok(Project {
                    root: dir,
                    manifest,
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
