pub mod bioconductor;
pub mod cran;
pub mod github;
pub mod p3m;

use semver::Version;

use crate::lockfile::PackageSource;

/// A dependency reference carrying an optional version constraint.
#[derive(Debug, Clone)]
pub struct Dep {
    pub name: String,
    /// Constraint string, e.g. `">=1.0.0"`, or `None` for any version.
    pub constraint: Option<String>,
}

impl Dep {
    pub fn any(name: impl Into<String>) -> Self {
        Dep { name: name.into(), constraint: None }
    }
    pub fn with_constraint(name: impl Into<String>, constraint: impl Into<String>) -> Self {
        Dep { name: name.into(), constraint: Some(constraint.into()) }
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
}
