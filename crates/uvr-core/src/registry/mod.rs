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
        Dep { name: name.into(), constraint: None }
    }
    pub fn with_constraint(name: impl Into<String>, constraint: impl Into<String>) -> Self {
        Dep { name: name.into(), constraint: Some(constraint.into()) }
    }
}

/// A registry that tries a primary source and falls back to a secondary on
/// `PackageNotFound`. This routes most packages through CRAN while transparently
/// resolving Bioconductor-only packages (and their transitive deps) without
/// requiring the caller to know which registry a given package lives in.
pub struct CompositeRegistry<'a> {
    primary: &'a dyn PackageRegistry,
    fallback: &'a dyn PackageRegistry,
}

impl<'a> CompositeRegistry<'a> {
    pub fn new(primary: &'a dyn PackageRegistry, fallback: &'a dyn PackageRegistry) -> Self {
        CompositeRegistry { primary, fallback }
    }
}

impl<'a> PackageRegistry for CompositeRegistry<'a> {
    fn resolve_package(&self, name: &str, constraint: Option<&str>) -> Result<PackageInfo> {
        match self.primary.resolve_package(name, constraint) {
            Err(UvrError::PackageNotFound(_)) => self.fallback.resolve_package(name, constraint),
            other => other,
        }
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
}
