use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::error::{Result, UvrError};
use crate::manifest::atomic_write;

/// Top-level `uvr.lock` structure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Lockfile {
    pub r: RVersionPin,

    /// Sorted alphabetically for deterministic diffs.
    #[serde(rename = "package", default)]
    pub packages: Vec<LockedPackage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RVersionPin {
    pub version: String,

    /// Bioconductor release used during resolution, e.g. `"3.18"`.
    /// Only present when the lockfile includes Bioconductor packages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bioc_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LockedPackage {
    pub name: String,
    pub version: String,
    pub source: PackageSource,

    /// Raw (un-normalized) version string from the registry (e.g. `"1.1-3"`).
    /// Used to reconstruct correct tarball filenames when `url` is absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_version: Option<String>,

    /// Canonical download URL. Stored so `sync` never has to reconstruct it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,

    /// Raw `SystemRequirements` string from DESCRIPTION, if present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_requirements: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum PackageSource {
    Cran,
    Bioconductor,
    GitHub,
    Local,
}

impl std::fmt::Display for PackageSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackageSource::Cran => write!(f, "cran"),
            PackageSource::Bioconductor => write!(f, "bioconductor"),
            PackageSource::GitHub => write!(f, "github"),
            PackageSource::Local => write!(f, "local"),
        }
    }
}

impl std::str::FromStr for Lockfile {
    type Err = UvrError;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        toml::from_str(s).map_err(|e| UvrError::LockfileParse(e.to_string()))
    }
}

impl Lockfile {
    pub fn from_file(path: &Path) -> Result<Self> {
        let s = std::fs::read_to_string(path)?;
        s.parse()
    }

    pub fn to_toml_string(&self) -> Result<String> {
        toml::to_string_pretty(self).map_err(UvrError::TomlSer)
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        let mut sorted = self.clone();
        sorted.packages.sort_by(|a, b| a.name.cmp(&b.name));
        let s = sorted.to_toml_string()?;
        atomic_write(path, s.as_bytes())
    }

    pub fn get_package(&self, name: &str) -> Option<&LockedPackage> {
        self.packages
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
    }

    pub fn upsert_package(&mut self, pkg: LockedPackage) {
        if let Some(existing) = self.packages.iter_mut().find(|p| p.name == pkg.name) {
            *existing = pkg;
        } else {
            self.packages.push(pkg);
        }
        self.packages.sort_by(|a, b| a.name.cmp(&b.name));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[r]
version = "4.3.2"

[[package]]
name = "ggplot2"
version = "3.4.4"
source = "cran"
url = "https://cran.r-project.org/src/contrib/ggplot2_3.4.4.tar.gz"
checksum = "sha256:abc123"
requires = ["dplyr", "scales"]

[[package]]
name = "dplyr"
version = "1.1.4"
source = "cran"
"#;

    #[test]
    fn round_trip() {
        let lf: Lockfile = SAMPLE.parse().expect("parse");
        assert_eq!(lf.r.version, "4.3.2");
        assert_eq!(lf.packages.len(), 2);

        let gg = lf.get_package("ggplot2").unwrap();
        assert_eq!(gg.version, "3.4.4");
        assert_eq!(
            gg.url.as_deref(),
            Some("https://cran.r-project.org/src/contrib/ggplot2_3.4.4.tar.gz")
        );
        assert_eq!(gg.requires, vec!["dplyr", "scales"]);

        let s = lf.to_toml_string().unwrap();
        let lf2: Lockfile = s.parse().unwrap();
        assert_eq!(lf, lf2);
    }

    #[test]
    fn round_trip_with_bioc_version() {
        let input = r#"
[r]
version = "4.3.2"
bioc_version = "3.18"

[[package]]
name = "DESeq2"
version = "1.42.0"
source = "bioconductor"
url = "https://bioconductor.org/packages/3.18/bioc/src/contrib/DESeq2_1.42.0.tar.gz"
"#;
        let lf: Lockfile = input.parse().expect("parse");
        assert_eq!(lf.r.bioc_version.as_deref(), Some("3.18"));
        assert_eq!(lf.packages[0].source, PackageSource::Bioconductor);

        let s = lf.to_toml_string().unwrap();
        let lf2: Lockfile = s.parse().unwrap();
        assert_eq!(lf, lf2);
    }

    #[test]
    fn backward_compat_no_bioc_version() {
        // Old lockfiles without bioc_version should still parse fine.
        let lf: Lockfile = SAMPLE.parse().expect("parse");
        assert!(lf.r.bioc_version.is_none());
    }

    #[test]
    fn backward_compat_no_url() {
        // Old lockfiles without `url` field should still parse
        let old = r#"
[r]
version = "4.3.2"

[[package]]
name = "ggplot2"
version = "3.4.4"
source = "cran"
"#;
        let lf: Lockfile = old.parse().unwrap();
        assert!(lf.get_package("ggplot2").unwrap().url.is_none());
    }
}
