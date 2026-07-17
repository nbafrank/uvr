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

    /// Install a specific R version. `version` may be partial (`4.5`);
    /// returns the full version actually installed (e.g. `4.5.3`).
    pub async fn install(&self, version: &str) -> Result<String> {
        let platform = Platform::detect()?;
        let install_dir = download_and_install_r(&self.client, version, platform).await?;
        Ok(install_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| version.to_string()))
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
        let base = crate::env_vars::r_install_dir()
            .ok_or_else(|| UvrError::Other("Cannot determine r-versions directory".into()))?;
        let mut install_dir = base.join(version);
        if !install_dir.exists() {
            // Partial version (`4.5`): match managed installs by component
            // prefix, mirroring how pins resolve (#136). Only a unique match
            // is removed — deleting is not the place to guess.
            let mut matches: Vec<String> = std::fs::read_dir(&base)
                .ok()
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|name| crate::r_version::detector::version_matches_prefix(version, name))
                .collect();
            matches.sort();
            match matches.len() {
                0 => {
                    return Err(UvrError::Other(format!(
                        "R {version} is not installed at {}",
                        install_dir.display()
                    )));
                }
                1 => install_dir = base.join(&matches[0]),
                _ => {
                    return Err(UvrError::Other(format!(
                        "R {version} is ambiguous: {} are installed. \
                         Specify the full version to uninstall.",
                        matches.join(", ")
                    )));
                }
            }
        }
        std::fs::remove_dir_all(&install_dir).map_err(|e| {
            UvrError::Other(format!("Failed to remove {}: {e}", install_dir.display()))
        })?;
        Ok(install_dir)
    }
}
