use crate::error::{Result, UvrError};
use crate::r_version::detector::{find_all, RInstallation};
use crate::r_version::downloader::{download_and_install_r, Platform};

pub struct RManager {
    client: reqwest::Client,
}

impl RManager {
    pub fn new(client: reqwest::Client) -> Self {
        RManager { client }
    }

    /// Install a specific R version.
    pub async fn install(&self, version: &str) -> Result<()> {
        let platform = Platform::detect()?;
        download_and_install_r(&self.client, version, platform).await?;
        Ok(())
    }

    /// List installed + system R versions.
    pub fn list(&self) -> Vec<RInstallation> {
        find_all()
    }

    /// Return the path to the R binary for a given version (must be installed).
    pub fn binary_for_version(&self, version: &str) -> Result<std::path::PathBuf> {
        let all = find_all();
        all.into_iter()
            .find(|i| i.version == version)
            .map(|i| i.binary)
            .ok_or_else(|| UvrError::Other(format!("R {version} is not installed")))
    }

    /// Remove a uvr-managed R installation at `~/.uvr/r-versions/<version>/`.
    /// Only touches uvr-managed installs — system R installations are left alone.
    pub fn uninstall(version: &str) -> Result<std::path::PathBuf> {
        if version.is_empty()
            || version.contains('/')
            || version.contains('\\')
            || version.contains("..")
            || version.starts_with('.')
        {
            return Err(UvrError::Other(format!("Invalid R version: {version:?}")));
        }
        let install_dir = dirs::home_dir()
            .ok_or_else(|| UvrError::Other("Cannot determine home directory".into()))?
            .join(".uvr")
            .join("r-versions")
            .join(version);
        if !install_dir.exists() {
            return Err(UvrError::Other(format!(
                "R {version} is not installed at {}",
                install_dir.display()
            )));
        }
        std::fs::remove_dir_all(&install_dir).map_err(|e| {
            UvrError::Other(format!("Failed to remove {}: {e}", install_dir.display()))
        })?;
        Ok(install_dir)
    }
}
