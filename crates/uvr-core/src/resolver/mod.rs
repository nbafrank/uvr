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
    pub system_requirements: Option<String>,
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
        // Track all constraints seen per package for re-resolution on conflict.
        let mut seen_constraints: HashMap<String, Vec<String>> = HashMap::new();

        // Seed from manifest direct dependencies.
        let mut pending: VecDeque<(String, Option<String>)> = manifest
            .dependencies
            .iter()
            .map(|(name, spec)| (name.clone(), spec.version_req().map(str::to_string)))
            .collect();
        for (name, constraint) in &pending {
            queued.insert(name.clone());
            if let Some(c) = constraint {
                if !c.is_empty() && c != "*" {
                    seen_constraints
                        .entry(name.clone())
                        .or_default()
                        .push(c.clone());
                }
            }
        }

        while let Some((name, constraint)) = pending.pop_front() {
            if is_base_package(&name) {
                continue;
            }

            // Record the constraint for future re-resolution checks.
            if let Some(c) = &constraint {
                if !c.is_empty() && c != "*" {
                    let constraints = seen_constraints.entry(name.clone()).or_default();
                    if !constraints.contains(c) {
                        constraints.push(c.clone());
                    }
                }
            }

            // If already resolved, validate the new constraint against the
            // existing version — this is the diamond-dependency case.
            if let Some(existing) = resolution.get(&name) {
                if let Some(c) = &constraint {
                    if !c.is_empty() && c != "*" {
                        let req = parse_version_req(c)?;
                        if !version_matches_req(&existing.version, &req) {
                            // Try re-resolving with the stricter constraint.
                            if let Ok(new_info) =
                                self.registry.resolve_package(&name, Some(c.as_str()))
                            {
                                // Verify the new version satisfies ALL prior constraints.
                                let all_ok = seen_constraints
                                    .get(&name)
                                    .map(|cs| {
                                        cs.iter().all(|prev| {
                                            parse_version_req(prev)
                                                .map(|r| version_matches_req(&new_info.version, &r))
                                                .unwrap_or(false)
                                        })
                                    })
                                    .unwrap_or(true);
                                if all_ok {
                                    // Re-resolve succeeded: update the resolution in place.
                                    resolution.insert(
                                        name.clone(),
                                        ResolvedPackage {
                                            name: name.clone(),
                                            version: new_info.version,
                                            source: new_info.source,
                                            checksum: new_info.checksum,
                                            requires: new_info
                                                .requires
                                                .iter()
                                                .map(|d| d.name.clone())
                                                .collect(),
                                            raw_version: new_info.raw_version,
                                            url: new_info.url,
                                            system_requirements: new_info.system_requirements,
                                        },
                                    );
                                    // Queue the new package's deps for resolution.
                                    for dep in &new_info.requires {
                                        if !is_base_package(&dep.name) {
                                            pending.push_back((
                                                dep.name.clone(),
                                                dep.constraint.clone(),
                                            ));
                                        }
                                    }
                                    continue;
                                }
                            }
                            // Re-resolution failed or new version doesn't satisfy all constraints.
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

            let info = self
                .registry
                .resolve_package(&name, constraint.as_deref())?;

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
                    system_requirements: info.system_requirements,
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
                system_requirements: r.system_requirements,
            })
            .collect();
        packages.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(Lockfile {
            r: crate::lockfile::RVersionPin {
                version: r_version,
                bioc_version: None,
            },
            packages,
        })
    }
}

const BASE_PACKAGES: &[&str] = &[
    "R",
    "base",
    "compiler",
    "datasets",
    "grDevices",
    "graphics",
    "grid",
    "methods",
    "parallel",
    "splines",
    "stats",
    "stats4",
    "tcltk",
    "tools",
    "utils",
];

pub fn is_base_package(name: &str) -> bool {
    BASE_PACKAGES.iter().any(|b| b.eq_ignore_ascii_case(name))
}

/// Check if a version satisfies a requirement, ignoring semver pre-release semantics.
///
/// R's 4-component versions (e.g. `1.18.2.1`) are encoded as semver pre-releases
/// (`1.18.2-4.1`), but semver pre-releases have lower precedence than releases,
/// which breaks constraint matching (e.g. `>=1.13.0` won't match `1.18.2-4.1`).
/// This function strips the pre-release tag before checking the constraint.
pub fn version_matches_req(version: &Version, req: &VersionReq) -> bool {
    if req.matches(version) {
        return true;
    }
    // Try again without pre-release tag
    if !version.pre.is_empty() {
        let stripped = Version::new(version.major, version.minor, version.patch);
        return req.matches(&stripped);
    }
    false
}

/// Parse a version constraint string into a `semver::VersionReq`.
///
/// R version constraints may use 1 or 2 components (e.g. `> 2.4`), but semver
/// requires 3. We pad with `.0` so that `> 2.4` becomes `> 2.4.0`.
pub fn parse_version_req(s: &str) -> Result<VersionReq> {
    let s = s.trim();
    if s == "*" || s.is_empty() {
        return Ok(VersionReq::STAR);
    }
    // Normalize and pad each comparator's version to 3 components.
    // The `-` → `.` substitution is applied only to the version component,
    // not the full string, to avoid mangling operator tokens.
    let padded = normalize_version_in_req(s);
    VersionReq::parse(&padded).map_err(UvrError::Semver)
}

/// Normalize and pad version numbers in a requirement string.
/// - Replaces `-` with `.` in the version component only (R treats them equivalently)
/// - Pads to 3 components so semver parses correctly
///
/// E.g. `"> 2.4"` → `"> 2.4.0"`, `">= 1.1-3"` → `">= 1.1.3"`, `">= 1.0.0"` unchanged.
fn normalize_version_in_req(s: &str) -> String {
    s.split(',')
        .map(|part| {
            let part = part.trim();
            // Find where the version number starts (after operator chars and spaces)
            let ver_start = part
                .find(|c: char| c.is_ascii_digit())
                .unwrap_or(part.len());
            let (prefix, ver) = part.split_at(ver_start);
            // Apply R's `-` → `.` equivalence only to the version part
            let ver = ver.replace('-', ".");
            let dot_count = ver.chars().filter(|&c| c == '.').count();
            match dot_count {
                0 if !ver.is_empty() => format!("{prefix}{ver}.0.0"),
                1 => format!("{prefix}{ver}.0"),
                _ => format!("{prefix}{ver}"),
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Normalize an R version string to semver.
///
/// - Replaces `-` with `.` (e.g. `"1.1-3"` → `"1.1.3"`)
/// - Strips leading zeros from each component (semver forbids them):
///   `"2026.03.11"` → `"2026.3.11"`
/// - Pads to three components
/// - Preserves 4th component as semver pre-release (e.g. `"1.0.12.2"` → `"1.0.12-4.2"`)
///   so that `1.0.12.1` and `1.0.12.2` are distinguishable. The `4.` prefix ensures
///   semver ordering is correct: `1.0.12-4.1 < 1.0.12-4.2`.
///   Note: `raw_version` is always used for URLs, not this normalized form.
pub fn normalize_version(v: &str) -> String {
    let v = v.replace('-', ".");
    let parts: Vec<String> = v
        .split('.')
        .map(|p| {
            // Parse as u64 to strip leading zeros, fall back to raw string.
            p.parse::<u64>()
                .map(|n| n.to_string())
                .unwrap_or_else(|_| p.to_string())
        })
        .collect();
    let base = match parts.len() {
        0 => return "0.0.0".to_string(),
        1 => format!("{}.0.0", parts[0]),
        2 => format!("{}.{}.0", parts[0], parts[1]),
        _ => format!("{}.{}.{}", parts[0], parts[1], parts[2]),
    };
    // R allows 4-component versions (e.g. Rcpp 1.0.12.2, data.table dev builds).
    // Encode the 4th component as a semver pre-release so versions remain
    // distinguishable and correctly ordered.
    if parts.len() >= 4 {
        format!("{base}-4.{}", parts[3])
    } else {
        base
    }
}

/// Sort `packages` into topological install order using their `requires` fields.
/// Packages already installed (not in `all_names`) are treated as satisfied.
pub fn topological_install_order(packages: &[LockedPackage]) -> Result<Vec<&LockedPackage>> {
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

    let order = graph.topological_sort()?;
    let order_index: HashMap<&str, usize> = order
        .iter()
        .enumerate()
        .map(|(i, n)| (n.as_str(), i))
        .collect();

    let mut sorted: Vec<&LockedPackage> = packages.iter().collect();
    sorted.sort_by_key(|p| {
        order_index
            .get(p.name.as_str())
            .copied()
            .unwrap_or(usize::MAX)
    });
    Ok(sorted)
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
        // Date-style versions with leading zeros (e.g. prodlim 2026.03.11)
        assert_eq!(normalize_version("2026.03.11"), "2026.3.11");
        assert_eq!(normalize_version("2023.03.01"), "2023.3.1");
        // 4-component versions (e.g. Rcpp 1.0.12.2) → semver pre-release
        assert_eq!(normalize_version("1.0.12.2"), "1.0.12-4.2");
        assert_eq!(normalize_version("1.0.12.1"), "1.0.12-4.1");
        // 4-component versions are distinguishable via semver ordering
        let v1 = Version::parse(&normalize_version("1.0.12.1")).unwrap();
        let v2 = Version::parse(&normalize_version("1.0.12.2")).unwrap();
        assert!(v1 < v2);
        // Both satisfy >=1.0.12 (pre-release < release, but >=1.0.12-0 matches)
        // and the raw_version is used for URL construction anyway
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

    fn make_pkg(
        name: &str,
        version: &str,
        requires: Vec<(&str, Option<&str>)>,
    ) -> (String, PackageInfo) {
        (
            name.to_string(),
            PackageInfo {
                name: name.to_string(),
                version: Version::parse(version).unwrap(),
                source: PackageSource::Cran,
                checksum: None,
                requires: requires
                    .into_iter()
                    .map(|(n, c)| Dep {
                        name: n.to_string(),
                        constraint: c.map(str::to_string),
                    })
                    .collect(),
                url: format!("https://cran.r-project.org/{name}_{version}.tar.gz"),
                raw_version: None,
                system_requirements: None,
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
                let info = self
                    .packages
                    .get(name)
                    .cloned()
                    .ok_or_else(|| UvrError::PackageNotFound(name.to_string()))?;
                if let Some(c) = constraint {
                    if c != "*" && !c.is_empty() {
                        let req = parse_version_req(c)?;
                        if !version_matches_req(&info.version, &req) {
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
    fn diamond_dep_re_resolves_with_stricter_constraint() {
        // A requires dep >= 1.0.0 (resolves to 1.5.0)
        // B requires dep >= 2.0.0 (1.5.0 doesn't satisfy, but 2.1.0 exists and satisfies both)
        // The resolver should re-resolve dep to 2.1.0 instead of erroring.
        struct MultiVersionRegistry;
        impl PackageRegistry for MultiVersionRegistry {
            fn resolve_package(&self, name: &str, constraint: Option<&str>) -> Result<PackageInfo> {
                match name {
                    "pkgA" => Ok(PackageInfo {
                        name: "pkgA".into(),
                        version: Version::parse("1.0.0").unwrap(),
                        source: PackageSource::Cran,
                        checksum: None,
                        requires: vec![Dep {
                            name: "shared".into(),
                            constraint: Some(">=1.0.0".into()),
                        }],
                        url: "https://example.com/pkgA.tar.gz".into(),
                        raw_version: None,
                        system_requirements: None,
                    }),
                    "pkgB" => Ok(PackageInfo {
                        name: "pkgB".into(),
                        version: Version::parse("1.0.0").unwrap(),
                        source: PackageSource::Cran,
                        checksum: None,
                        requires: vec![Dep {
                            name: "shared".into(),
                            constraint: Some(">=2.0.0".into()),
                        }],
                        url: "https://example.com/pkgB.tar.gz".into(),
                        raw_version: None,
                        system_requirements: None,
                    }),
                    "shared" => {
                        // Return the best version that satisfies the constraint
                        let versions =
                            vec![("2.1.0", ">=2.0.0"), ("2.1.0", ">=1.0.0"), ("1.5.0", "*")];
                        if let Some(c) = constraint {
                            if !c.is_empty() && c != "*" {
                                let req = parse_version_req(c).unwrap();
                                for (ver, _) in &versions {
                                    let v = Version::parse(ver).unwrap();
                                    if version_matches_req(&v, &req) {
                                        return Ok(PackageInfo {
                                            name: "shared".into(),
                                            version: v,
                                            source: PackageSource::Cran,
                                            checksum: None,
                                            requires: vec![],
                                            url: format!("https://example.com/shared_{ver}.tar.gz"),
                                            raw_version: None,
                                            system_requirements: None,
                                        });
                                    }
                                }
                            }
                        }
                        // No constraint or wildcard: return lowest
                        Ok(PackageInfo {
                            name: "shared".into(),
                            version: Version::parse("1.5.0").unwrap(),
                            source: PackageSource::Cran,
                            checksum: None,
                            requires: vec![],
                            url: "https://example.com/shared_1.5.0.tar.gz".into(),
                            raw_version: None,
                            system_requirements: None,
                        })
                    }
                    _ => Err(UvrError::PackageNotFound(name.to_string())),
                }
            }
        }

        let registry = MultiVersionRegistry;
        let resolver = Resolver::new(&registry);
        let mut manifest = Manifest::new("test", None);
        manifest.add_dep("pkgA".into(), DependencySpec::Version("*".into()), false);
        manifest.add_dep("pkgB".into(), DependencySpec::Version("*".into()), false);

        let lockfile = resolver.resolve(&manifest, None).unwrap();
        // shared should be resolved to 2.1.0, not 1.5.0
        let shared = lockfile
            .packages
            .iter()
            .find(|p| p.name == "shared")
            .unwrap();
        assert_eq!(shared.version, "2.1.0");
    }

    #[test]
    fn topological_order_puts_deps_first() {
        use crate::lockfile::LockedPackage;
        let packages = vec![
            LockedPackage {
                name: "ggplot2".into(),
                version: "3.4.4".into(),
                source: PackageSource::Cran,
                raw_version: None,
                url: None,
                checksum: None,
                requires: vec!["dplyr".into(), "rlang".into()],
                system_requirements: None,
            },
            LockedPackage {
                name: "dplyr".into(),
                version: "1.1.4".into(),
                source: PackageSource::Cran,
                raw_version: None,
                url: None,
                checksum: None,
                requires: vec!["rlang".into()],
                system_requirements: None,
            },
            LockedPackage {
                name: "rlang".into(),
                version: "1.1.4".into(),
                source: PackageSource::Cran,
                raw_version: None,
                url: None,
                checksum: None,
                requires: vec![],
                system_requirements: None,
            },
        ];

        let ordered = topological_install_order(&packages).unwrap();
        let pos = |name: &str| ordered.iter().position(|p| p.name == name).unwrap();
        assert!(pos("rlang") < pos("dplyr"));
        assert!(pos("dplyr") < pos("ggplot2"));
    }

    #[test]
    fn strict_greater_than_constraint() {
        // rbibutils 2.4.1 must satisfy > 2.4
        let v = Version::parse(&normalize_version("2.4.1")).unwrap();
        let req = parse_version_req("> 2.4").unwrap();
        assert!(version_matches_req(&v, &req));

        // 2-component: >= 3 should match 3.0.0
        let v2 = Version::parse("3.0.0").unwrap();
        let req2 = parse_version_req(">= 3").unwrap();
        assert!(version_matches_req(&v2, &req2));

        // Already 3-component should still work
        let req3 = parse_version_req(">= 1.0.0").unwrap();
        assert!(version_matches_req(&v, &req3));
    }

    #[test]
    fn four_component_version_matches_constraint() {
        // Regression: data.table 1.18.2.1 → semver 1.18.2-4.1
        // >=1.13.0 must match despite the pre-release tag
        let normalized = normalize_version("1.18.2.1");
        assert_eq!(normalized, "1.18.2-4.1");

        let v = Version::parse(&normalized).unwrap();
        let req = parse_version_req(">=1.13.0").unwrap();
        assert!(version_matches_req(&v, &req));
    }
}
