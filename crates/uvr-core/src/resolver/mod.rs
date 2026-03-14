pub mod graph;

use std::collections::{HashMap, HashSet, VecDeque};

use semver::{Version, VersionReq};

use crate::error::{Result, UvrError};
use crate::lockfile::{LockedPackage, Lockfile, PackageSource};
use crate::manifest::Manifest;
use crate::registry::PackageInfo;

use self::graph::DependencyGraph;

type Resolution = HashMap<String, ResolvedPackage>;

#[derive(Debug, Clone)]
pub struct ResolvedPackage {
    pub name: String,
    pub version: Version,
    pub source: PackageSource,
    pub checksum: Option<String>,
    /// Dep names only (stored in lockfile).
    pub requires: Vec<String>,
    pub url: String,
    pub raw_version: Option<String>,
}

/// Trait to abstract over CRAN / Bioconductor / GitHub registries.
pub trait PackageRegistry {
    fn resolve_package(&self, name: &str, constraint: Option<&str>) -> Result<PackageInfo>;
}

pub struct Resolver<'a> {
    registry: &'a dyn PackageRegistry,
}

impl<'a> Resolver<'a> {
    pub fn new(registry: &'a dyn PackageRegistry) -> Self {
        Resolver { registry }
    }

    /// Resolve all manifest dependencies into a `Lockfile`.
    ///
    /// Handles diamond dependencies: when a package is encountered again with a
    /// new constraint, we check that the already-resolved version satisfies it
    /// rather than treating it as a conflict.
    /// Resolve all manifest dependencies into a `Lockfile`.
    ///
    /// `actual_r_version` should be the version string of the currently-active R
    /// binary (e.g. `"4.4.2"`). When provided it is recorded verbatim in the
    /// lockfile so that `uvr sync` can detect R version changes and re-install.
    /// Falls back to the manifest constraint when `None`.
    pub fn resolve(&self, manifest: &Manifest, actual_r_version: Option<&str>) -> Result<Lockfile> {
        let r_version = actual_r_version
            .map(str::to_string)
            .or_else(|| manifest.project.r_version.clone())
            .unwrap_or_else(|| "*".to_string());

        let mut resolution: Resolution = HashMap::new();
        let mut graph = DependencyGraph::default();
        // Track which names we've pushed into the queue to avoid redundant
        // resolve calls (constraint checks for already-resolved still run).
        let mut queued: HashSet<String> = HashSet::new();

        // Seed from manifest direct dependencies.
        let mut pending: VecDeque<(String, Option<String>)> = manifest
            .dependencies
            .iter()
            .map(|(name, spec)| (name.clone(), spec.version_req().map(str::to_string)))
            .collect();
        for (name, _) in &pending {
            queued.insert(name.clone());
        }

        while let Some((name, constraint)) = pending.pop_front() {
            if is_base_package(&name) {
                continue;
            }

            // If already resolved, just validate the new constraint against the
            // existing version — this is the diamond-dependency case.
            if let Some(existing) = resolution.get(&name) {
                if let Some(c) = &constraint {
                    if !c.is_empty() && c != "*" {
                        let req = parse_version_req(c)?;
                        if !req.matches(&existing.version) {
                            return Err(UvrError::VersionConflict {
                                package: name.clone(),
                                required: c.clone(),
                                conflicting: existing.version.to_string(),
                            });
                        }
                    }
                }
                continue;
            }

            let info = self.registry.resolve_package(&name, constraint.as_deref())?;

            graph.add_node(&name);
            for dep in &info.requires {
                if is_base_package(&dep.name) {
                    continue;
                }
                graph.add_edge(&name, &dep.name);

                if resolution.contains_key(&dep.name) {
                    // Already resolved: still need to validate the constraint.
                    if dep.constraint.is_some() {
                        pending.push_back((dep.name.clone(), dep.constraint.clone()));
                    }
                } else if !queued.contains(&dep.name) {
                    // Not yet queued: queue it with its constraint.
                    queued.insert(dep.name.clone());
                    pending.push_back((dep.name.clone(), dep.constraint.clone()));
                } else {
                    // Already queued but not resolved: push a constraint-check entry
                    // so it gets validated once resolved.
                    if dep.constraint.is_some() {
                        pending.push_back((dep.name.clone(), dep.constraint.clone()));
                    }
                }
            }

            resolution.insert(
                name.clone(),
                ResolvedPackage {
                    name: name.clone(),
                    version: info.version,
                    source: info.source,
                    checksum: info.checksum,
                    requires: info.requires.iter().map(|d| d.name.clone()).collect(),
                    url: info.url,
                    raw_version: info.raw_version,
                },
            );
        }

        // Validate the graph has no cycles (topo sort will error on cycles).
        graph.topological_sort()?;

        // Build packages sorted alphabetically for the lockfile (diffs).
        let mut packages: Vec<LockedPackage> = resolution
            .into_values()
            .map(|r| LockedPackage {
                name: r.name,
                version: r.version.to_string(),
                source: r.source,
                raw_version: r.raw_version,
                url: Some(r.url),
                checksum: r.checksum,
                requires: r.requires,
            })
            .collect();
        packages.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(Lockfile {
            r: crate::lockfile::RVersionPin { version: r_version },
            packages,
        })
    }
}

const BASE_PACKAGES: &[&str] = &[
    "R", "base", "stats", "utils", "methods", "graphics", "grDevices",
    "datasets", "tools", "compiler", "grid", "parallel", "splines",
    "tcltk", "translations",
];

pub fn is_base_package(name: &str) -> bool {
    BASE_PACKAGES.iter().any(|b| b.eq_ignore_ascii_case(name))
}

/// Parse a version constraint string into a `semver::VersionReq`.
pub fn parse_version_req(s: &str) -> Result<VersionReq> {
    let s = s.trim();
    if s == "*" || s.is_empty() {
        return Ok(VersionReq::STAR);
    }
    let normalized = s.replace('-', ".");
    VersionReq::parse(&normalized).map_err(UvrError::Semver)
}

/// Normalize an R version string `"1.1-3"` → `"1.1.3"`.
pub fn normalize_version(v: &str) -> String {
    let v = v.replace('-', ".");
    let parts: Vec<&str> = v.split('.').collect();
    match parts.len() {
        0 => "0.0.0".to_string(),
        1 => format!("{}.0.0", parts[0]),
        2 => format!("{}.{}.0", parts[0], parts[1]),
        _ => format!("{}.{}.{}", parts[0], parts[1], parts[2]),
    }
}

/// Sort `packages` into topological install order using their `requires` fields.
/// Packages already installed (not in `all_names`) are treated as satisfied.
pub fn topological_install_order<'a>(
    packages: &'a [LockedPackage],
) -> Vec<&'a LockedPackage> {
    let mut graph = DependencyGraph::default();
    let pkg_set: HashSet<&str> = packages.iter().map(|p| p.name.as_str()).collect();

    for pkg in packages {
        graph.add_node(&pkg.name);
        for req in &pkg.requires {
            if pkg_set.contains(req.as_str()) {
                graph.add_edge(&pkg.name, req);
            }
        }
    }

    let order = graph.topological_sort().unwrap_or_else(|_| {
        packages.iter().map(|p| p.name.clone()).collect()
    });
    let order_index: HashMap<&str, usize> = order
        .iter()
        .enumerate()
        .map(|(i, n)| (n.as_str(), i))
        .collect();

    let mut sorted: Vec<&LockedPackage> = packages.iter().collect();
    sorted.sort_by_key(|p| order_index.get(p.name.as_str()).copied().unwrap_or(usize::MAX));
    sorted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::DependencySpec;
    use crate::registry::Dep;

    #[test]
    fn normalize_versions() {
        assert_eq!(normalize_version("3.4.4"), "3.4.4");
        assert_eq!(normalize_version("1.1-3"), "1.1.3");
        assert_eq!(normalize_version("2.0"), "2.0.0");
        assert_eq!(normalize_version("4"), "4.0.0");
    }

    #[test]
    fn parse_constraints() {
        assert!(parse_version_req("*").is_ok());
        assert!(parse_version_req(">=3.0.0").is_ok());
        assert!(parse_version_req(">= 1.0.0").is_ok());
    }

    struct MockRegistry {
        packages: HashMap<String, PackageInfo>,
    }

    impl PackageRegistry for MockRegistry {
        fn resolve_package(&self, name: &str, _constraint: Option<&str>) -> Result<PackageInfo> {
            self.packages
                .get(name)
                .cloned()
                .ok_or_else(|| UvrError::PackageNotFound(name.to_string()))
        }
    }

    fn make_pkg(name: &str, version: &str, requires: Vec<(&str, Option<&str>)>) -> (String, PackageInfo) {
        (
            name.to_string(),
            PackageInfo {
                name: name.to_string(),
                version: Version::parse(version).unwrap(),
                source: PackageSource::Cran,
                checksum: None,
                requires: requires
                    .into_iter()
                    .map(|(n, c)| Dep { name: n.to_string(), constraint: c.map(str::to_string) })
                    .collect(),
                url: format!("https://cran.r-project.org/{name}_{version}.tar.gz"),
                raw_version: None,
            },
        )
    }

    #[test]
    fn resolve_transitive() {
        let mut packages = HashMap::new();
        packages.extend([
            make_pkg("ggplot2", "3.4.4", vec![("dplyr", None), ("scales", None)]),
            make_pkg("dplyr", "1.1.4", vec![("rlang", Some(">=1.0.0"))]),
            make_pkg("scales", "1.3.0", vec![]),
            make_pkg("rlang", "1.1.4", vec![]),
        ]);
        let registry = MockRegistry { packages };
        let resolver = Resolver::new(&registry);

        let mut manifest = Manifest::new("test", None);
        manifest.add_dep("ggplot2".into(), DependencySpec::Version("*".into()), false);

        let lockfile = resolver.resolve(&manifest, None).unwrap();
        assert_eq!(lockfile.packages.len(), 4);
        assert!(lockfile.get_package("rlang").is_some());
        // URL is stored
        assert!(lockfile.get_package("ggplot2").unwrap().url.is_some());
    }

    #[test]
    fn diamond_dependency_resolved_correctly() {
        // Both ggplot2 and tidyr need rlang, with different minimum versions.
        // The resolver should pick rlang 1.1.4 (satisfies both) without erroring.
        let mut packages = HashMap::new();
        packages.extend([
            make_pkg("ggplot2", "3.4.4", vec![("rlang", Some(">=1.0.0"))]),
            make_pkg("tidyr", "1.3.0", vec![("rlang", Some(">=1.1.0"))]),
            make_pkg("rlang", "1.1.4", vec![]),
        ]);
        let registry = MockRegistry { packages };
        let resolver = Resolver::new(&registry);

        let mut manifest = Manifest::new("test", None);
        manifest.add_dep("ggplot2".into(), DependencySpec::Version("*".into()), false);
        manifest.add_dep("tidyr".into(), DependencySpec::Version("*".into()), false);

        let lockfile = resolver.resolve(&manifest, None).unwrap();
        assert_eq!(lockfile.packages.len(), 3);
        assert_eq!(lockfile.get_package("rlang").unwrap().version, "1.1.4");
    }

    #[test]
    fn genuine_conflict_errors() {
        // ggplot2 needs rlang >= 2.0.0, but only 1.1.4 exists.
        let mut packages = HashMap::new();
        packages.extend([
            make_pkg("ggplot2", "3.4.4", vec![("rlang", Some(">=2.0.0"))]),
            make_pkg("rlang", "1.1.4", vec![]),
        ]);

        struct ConstraintRespectingRegistry {
            packages: HashMap<String, PackageInfo>,
        }
        impl PackageRegistry for ConstraintRespectingRegistry {
            fn resolve_package(&self, name: &str, constraint: Option<&str>) -> Result<PackageInfo> {
                let info = self.packages.get(name)
                    .cloned()
                    .ok_or_else(|| UvrError::PackageNotFound(name.to_string()))?;
                if let Some(c) = constraint {
                    if c != "*" && !c.is_empty() {
                        let req = parse_version_req(c)?;
                        if !req.matches(&info.version) {
                            return Err(UvrError::NoMatchingVersion {
                                package: name.to_string(),
                                constraint: c.to_string(),
                            });
                        }
                    }
                }
                Ok(info)
            }
        }

        let registry = ConstraintRespectingRegistry { packages };
        let resolver = Resolver::new(&registry);
        let mut manifest = Manifest::new("test", None);
        manifest.add_dep("ggplot2".into(), DependencySpec::Version("*".into()), false);

        let result = resolver.resolve(&manifest, None);
        assert!(result.is_err());
    }

    #[test]
    fn topological_order_puts_deps_first() {
        use crate::lockfile::LockedPackage;
        let packages = vec![
            LockedPackage {
                name: "ggplot2".into(), version: "3.4.4".into(),
                source: PackageSource::Cran, raw_version: None, url: None, checksum: None,
                requires: vec!["dplyr".into(), "rlang".into()],
            },
            LockedPackage {
                name: "dplyr".into(), version: "1.1.4".into(),
                source: PackageSource::Cran, raw_version: None, url: None, checksum: None,
                requires: vec!["rlang".into()],
            },
            LockedPackage {
                name: "rlang".into(), version: "1.1.4".into(),
                source: PackageSource::Cran, raw_version: None, url: None, checksum: None,
                requires: vec![],
            },
        ];

        let ordered = topological_install_order(&packages);
        let pos = |name: &str| ordered.iter().position(|p| p.name == name).unwrap();
        assert!(pos("rlang") < pos("dplyr"));
        assert!(pos("dplyr") < pos("ggplot2"));
    }
}
