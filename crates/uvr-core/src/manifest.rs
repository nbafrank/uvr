use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Result, UvrError};

/// Top-level `uvr.toml` structure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub project: ProjectMeta,

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dependencies: BTreeMap<String, DependencySpec>,

    #[serde(
        rename = "dev-dependencies",
        default,
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub dev_dependencies: BTreeMap<String, DependencySpec>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<PackageSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectMeta {
    pub name: String,

    /// SemVer requirement, e.g. `">=4.0.0"`
    #[serde(default)]
    pub r_version: Option<String>,

    #[serde(default)]
    pub description: Option<String>,
}

/// Either a bare version string (`">=3.0.0"`, `"*"`) or a detailed table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum DependencySpec {
    Version(String),
    Detailed(DetailedDep),
}

impl DependencySpec {
    pub fn version_req(&self) -> Option<&str> {
        match self {
            DependencySpec::Version(v) => Some(v),
            DependencySpec::Detailed(d) => d.version.as_deref(),
        }
    }

    pub fn is_bioc(&self) -> bool {
        match self {
            DependencySpec::Version(_) => false,
            DependencySpec::Detailed(d) => d.bioc.unwrap_or(false),
        }
    }

    pub fn git(&self) -> Option<&str> {
        match self {
            DependencySpec::Detailed(d) => d.git.as_deref(),
            _ => None,
        }
    }
}

impl Default for DependencySpec {
    fn default() -> Self {
        DependencySpec::Version("*".to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct DetailedDep {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// `true` = Bioconductor package
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bioc: Option<bool>,

    /// `"user/repo"` — GitHub source
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git: Option<String>,

    /// branch / tag / commit SHA
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rev: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PackageSource {
    pub name: String,
    pub url: String,
}

impl std::str::FromStr for Manifest {
    type Err = crate::error::UvrError;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        toml::from_str(s).map_err(|e| crate::error::UvrError::ManifestParse(e.to_string()))
    }
}

impl Manifest {
    pub fn new(name: impl Into<String>, r_version: Option<String>) -> Self {
        Manifest {
            project: ProjectMeta {
                name: name.into(),
                r_version,
                description: None,
            },
            dependencies: BTreeMap::new(),
            dev_dependencies: BTreeMap::new(),
            sources: Vec::new(),
        }
    }

    pub fn from_file(path: &Path) -> Result<Self> {
        let s = std::fs::read_to_string(path)?;
        s.parse()
    }

    pub fn to_toml_string(&self) -> Result<String> {
        toml::to_string_pretty(self).map_err(UvrError::TomlSer)
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        let s = self.to_toml_string()?;
        atomic_write(path, s.as_bytes())
    }

    /// Add or update a dependency. Returns `true` if a new dep was added.
    pub fn add_dep(&mut self, name: String, spec: DependencySpec, dev: bool) -> bool {
        let map = if dev {
            &mut self.dev_dependencies
        } else {
            &mut self.dependencies
        };
        let new = !map.contains_key(&name);
        map.insert(name, spec);
        new
    }

    pub fn remove_dep(&mut self, name: &str) -> bool {
        let a = self.dependencies.remove(name).is_some();
        let b = self.dev_dependencies.remove(name).is_some();
        a || b
    }
}

/// Write `data` to `path` atomically via a temp file in the same directory.
/// Uses `tempfile::NamedTempFile` for a unique temp name, then renames.
pub fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().unwrap_or(Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(data)?;
    tmp.persist(path)
        .map_err(|e| crate::error::UvrError::Io(e.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[project]
name = "my-project"
r_version = ">=4.0.0"

[dependencies]
ggplot2 = ">=3.0.0"
dplyr = "*"

[dependencies.DESeq2]
bioc = true

[dependencies.myPkg]
git = "user/repo"
rev = "main"
"#;

    #[test]
    fn round_trip() {
        let m: Manifest = SAMPLE.parse().expect("parse");
        assert_eq!(m.project.name, "my-project");
        assert_eq!(m.project.r_version.as_deref(), Some(">=4.0.0"));

        let ggplot2 = m.dependencies.get("ggplot2").unwrap();
        assert!(matches!(ggplot2, DependencySpec::Version(v) if v == ">=3.0.0"));

        let deseq2 = m.dependencies.get("DESeq2").unwrap();
        assert!(deseq2.is_bioc());

        let my_pkg = m.dependencies.get("myPkg").unwrap();
        assert_eq!(my_pkg.git(), Some("user/repo"));

        // Re-serialize and re-parse
        let toml_str = m.to_toml_string().expect("serialize");
        let m2: Manifest = toml_str.parse().expect("reparse");
        assert_eq!(m, m2);
    }

    #[test]
    fn add_remove_dep() {
        let mut m = Manifest::new("test", None);
        assert!(m.add_dep("ggplot2".into(), DependencySpec::Version("*".into()), false));
        assert!(!m.add_dep(
            "ggplot2".into(),
            DependencySpec::Version(">=3.0.0".into()),
            false
        ));
        assert!(m.remove_dep("ggplot2"));
        assert!(!m.remove_dep("ggplot2"));
    }
}
