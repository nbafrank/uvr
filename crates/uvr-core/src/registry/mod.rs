pub mod bioconductor;
pub mod cran;
pub mod github;
pub mod p3m;

use semver::Version;

use crate::error::{Result, UvrError};
use crate::lockfile::PackageSource;
use crate::resolver::PackageRegistry;

/// A dependency reference carrying an optional version constraint.
#[derive(Debug, Clone)]
pub struct Dep {
    pub name: String,
    /// Constraint string, e.g. `">=1.0.0"`, or `None` for any version.
    pub constraint: Option<String>,
}

impl Dep {
    pub fn any(name: impl Into<String>) -> Self {
        Dep {
            name: name.into(),
            constraint: None,
        }
    }
    pub fn with_constraint(name: impl Into<String>, constraint: impl Into<String>) -> Self {
        Dep {
            name: name.into(),
            constraint: Some(constraint.into()),
        }
    }
}

/// A registry chain that tries multiple registries in order, falling back
/// to the next on `PackageNotFound`. Used when custom repositories are configured.
pub struct RegistryChain<'a> {
    registries: Vec<&'a dyn PackageRegistry>,
}

impl<'a> RegistryChain<'a> {
    pub fn new(registries: Vec<&'a dyn PackageRegistry>) -> Self {
        RegistryChain { registries }
    }
}

impl<'a> PackageRegistry for RegistryChain<'a> {
    fn resolve_package(&self, name: &str, constraint: Option<&str>) -> Result<PackageInfo> {
        let mut last_err = None;
        for registry in &self.registries {
            match registry.resolve_package(name, constraint) {
                Ok(info) => return Ok(info),
                Err(UvrError::PackageNotFound(_)) => {
                    last_err = Some(UvrError::PackageNotFound(name.to_string()));
                }
                Err(e) => return Err(e),
            }
        }
        Err(last_err.unwrap_or_else(|| UvrError::PackageNotFound(name.to_string())))
    }
}

/// Unified package metadata returned by any registry.
#[derive(Debug, Clone)]
pub struct PackageInfo {
    pub name: String,
    pub version: Version,
    pub source: PackageSource,
    pub checksum: Option<String>,
    /// Dependencies with version constraints (for the resolver).
    pub requires: Vec<Dep>,
    /// Canonical download URL — stored verbatim in the lockfile.
    pub url: String,
    /// Raw (un-normalized) version string from the registry index (e.g. `"1.1-3"`).
    /// Used in tarball URL construction so we never produce broken URLs like
    /// `scales_1.1.3.tar.gz` when the real file is `scales_1.1-3.tar.gz`.
    pub raw_version: Option<String>,
    /// Raw `SystemRequirements` field from DESCRIPTION, if present.
    pub system_requirements: Option<String>,
}
