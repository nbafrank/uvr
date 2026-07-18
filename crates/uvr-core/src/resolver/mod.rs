pub mod graph;

use std::collections::{HashMap, HashSet, VecDeque};

use semver::{Version, VersionReq};

use crate::error::{Result, UvrError};
use crate::lockfile::{LockedPackage, Lockfile, PackageSource};
use crate::manifest::Manifest;
use crate::registry::{Dep, PackageInfo};

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
    ///
    /// `actual_r_version` should be the version string of the currently-active R
    /// binary (e.g. `"4.4.2"`). When provided it is recorded verbatim in the
    /// lockfile so that `uvr sync` can detect R version changes and re-install.
    /// Falls back to the manifest constraint when `None`.
    ///
    /// `pre_resolved` contains packages already resolved outside the registry
    /// chain (e.g. GitHub packages resolved via the GitHub API). These are
    /// injected into the resolution and their transitive deps flow through the
    /// normal registry resolution.
    pub fn resolve(
        &self,
        manifest: &Manifest,
        actual_r_version: Option<&str>,
        pre_resolved: HashMap<String, crate::registry::PackageInfo>,
    ) -> Result<Lockfile> {
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

        // Track which root packages come from dev-dependencies only.
        let mut dev_roots: HashSet<String> = HashSet::new();

        // Seed from manifest direct dependencies + dev-dependencies.
        let mut pending: VecDeque<(String, Option<String>)> = manifest
            .dependencies
            .iter()
            .map(|(name, spec)| (name.clone(), spec.version_req().map(str::to_string)))
            .collect();
        // Add dev-dependencies to the queue.
        for (name, spec) in &manifest.dev_dependencies {
            let constraint = spec.version_req().map(str::to_string);
            pending.push_back((name.clone(), constraint));
            // Only mark as dev root if not also in regular dependencies.
            if !manifest.dependencies.contains_key(name) {
                dev_roots.insert(name.clone());
            }
        }
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
                            // A pre-resolved package must never re-resolve
                            // past its pinned version: for selective-update
                            // pins the locked version IS the resolution
                            // (#127), and for git deps swapping to a registry
                            // version would silently change the source. The
                            // first-encounter path already errors on a
                            // constraint mismatch; without this guard, a
                            // second encounter (pinned package that is both a
                            // direct dep and a transitive dep of the target)
                            // would fall through to a live registry lookup
                            // and quietly override the pin.
                            if pre_resolved.contains_key(&name) {
                                return Err(UvrError::VersionConflict {
                                    package: name.clone(),
                                    required: c.clone(),
                                    conflicting: existing.version.to_string(),
                                });
                            }
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
                                    // Rebuild graph edges for the re-resolved package.
                                    graph.add_node(&name);
                                    for dep in &new_info.requires {
                                        if is_base_package(&dep.name) {
                                            continue;
                                        }
                                        graph.add_edge(&name, &dep.name);
                                        if !queued.contains(&dep.name) {
                                            queued.insert(dep.name.clone());
                                            pending.push_back((
                                                dep.name.clone(),
                                                dep.constraint.clone(),
                                            ));
                                        } else if dep.constraint.is_some() {
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

            // Use pre-resolved info (e.g. GitHub packages) if available,
            // otherwise fall back to the registry chain.
            //
            // The registry path (`resolve_package`) honours the version
            // constraint via its own resolution logic; the pre_resolved
            // path bypasses the registry, so the constraint check has to
            // happen here too — otherwise a `Remotes:`-resolved github
            // version could silently violate a parent's `Imports: foo
            // (>= 1.0.0)` (#84 review).
            let info = if let Some(pi) = pre_resolved.get(&name) {
                if let Some(c) = &constraint {
                    if !c.is_empty() && c != "*" {
                        let req = parse_version_req(c)?;
                        if !version_matches_req(&pi.version, &req) {
                            return Err(UvrError::VersionConflict {
                                package: name.clone(),
                                required: c.clone(),
                                conflicting: pi.version.to_string(),
                            });
                        }
                    }
                }
                pi.clone()
            } else {
                self.registry
                    .resolve_package(&name, constraint.as_deref())?
            };

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

        // Determine which packages are dev-only: reachable exclusively from
        // dev_roots, not from any regular dependency root.
        let non_dev_roots: HashSet<&str> =
            manifest.dependencies.keys().map(String::as_str).collect();
        let dev_only_pkgs: HashSet<String> = find_dev_only_packages(&resolution, &non_dev_roots)
            .into_iter()
            .map(str::to_string)
            .collect();

        // Build packages sorted alphabetically for the lockfile (diffs).
        let mut packages: Vec<LockedPackage> = resolution
            .into_values()
            .map(|r| {
                let is_dev = dev_only_pkgs.contains(&r.name);
                LockedPackage {
                    name: r.name,
                    version: r.version.to_string(),
                    source: r.source,
                    raw_version: r.raw_version,
                    // Empty url = pinned from a legacy lockfile entry that had
                    // none (locked_to_package_info) — keep it absent rather
                    // than round-tripping `""` into the lockfile.
                    url: (!r.url.is_empty()).then_some(r.url),
                    checksum: r.checksum,
                    requires: r.requires,
                    system_requirements: r.system_requirements,
                    dev: is_dev,
                }
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

/// Convert a locked package back into a `PackageInfo` so it can be injected
/// into resolution as a pin via `pre_resolved` (selective `uvr update`,
/// #127). The resolver constraint-checks `pre_resolved` versions (the #84
/// guard), so a freshly-updated package that needs a newer version of a
/// pinned one fails with an explicit `VersionConflict` instead of producing
/// a self-inconsistent lockfile. The guard covers both the first encounter
/// of a pinned name and later, stricter constraints arriving via the
/// diamond path — a pin is never re-resolved past its version. `requires`
/// carries names only (the lockfile stores no constraints), which is fine
/// for pins: their deps are themselves pinned or resolve fresh with real
/// registry constraints.
pub fn locked_to_package_info(p: &LockedPackage) -> Result<PackageInfo> {
    let version = Version::parse(&normalize_version(&p.version)).map_err(|e| {
        UvrError::Other(format!(
            "Locked version {} of {} is not parseable: {e}",
            p.version, p.name
        ))
    })?;
    Ok(PackageInfo {
        name: p.name.clone(),
        version,
        source: p.source.clone(),
        checksum: p.checksum.clone(),
        requires: p
            .requires
            .iter()
            .map(|n| Dep {
                name: n.clone(),
                constraint: None,
            })
            .collect(),
        url: p.url.clone().unwrap_or_default(),
        raw_version: p.raw_version.clone(),
        system_requirements: p.system_requirements.clone(),
    })
}

/// Walk the resolved dependency graph from non-dev roots using BFS.
/// Any package NOT reached is dev-only.
fn find_dev_only_packages<'a>(
    resolution: &'a Resolution,
    non_dev_roots: &HashSet<&str>,
) -> HashSet<&'a str> {
    // BFS from non-dev roots to find all packages reachable from production deps.
    let mut reachable: HashSet<&str> = HashSet::new();
    let mut queue: VecDeque<&str> = non_dev_roots
        .iter()
        .filter(|name| resolution.contains_key(**name))
        .copied()
        .collect();

    while let Some(name) = queue.pop_front() {
        if !reachable.insert(name) {
            continue;
        }
        if let Some(pkg) = resolution.get(name) {
            for req in &pkg.requires {
                if !reachable.contains(req.as_str()) && resolution.contains_key(req.as_str()) {
                    queue.push_back(req);
                }
            }
        }
    }

    // Any resolved package not reachable from non-dev roots is dev-only.
    resolution
        .keys()
        .filter(|name| !reachable.contains(name.as_str()))
        .map(String::as_str)
        .collect()
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
///
/// The version component of each comparator goes through [`normalize_version`],
/// so constraints get the same treatment as versions themselves: R's `-` → `.`
/// equivalence, leading-zero stripping (`>= 0.03-11` → `>= 0.3.11`), padding to
/// 3 components (`> 2.4` → `> 2.4.0`), and 4+-component encoding as a semver
/// pre-release (`>= 1.6.9.27` → `>= 1.6.9-4.27`). CRAN DESCRIPTIONs use such
/// constraints routinely (RcppArmadillo, Matrix, h2o…); they used to fail
/// semver parsing and be silently treated as unconstrained (#149 follow-up).
///
/// Known asymmetry, accepted: a bare `1.6.9` (release) satisfies
/// `>= 1.6.9-4.27` under semver even though R orders `1.6.9 < 1.6.9.27` —
/// the same release-vs-prerelease inversion `normalize_version` already
/// accepts for version ordering. Enforcing the major.minor.patch floor with a
/// narrow sub-patch edge beats dropping the constraint entirely.
fn normalize_version_in_req(s: &str) -> String {
    s.split(',')
        .map(|part| {
            let part = part.trim();
            // Find where the version number starts (after operator chars and spaces)
            let ver_start = part
                .find(|c: char| c.is_ascii_digit())
                .unwrap_or(part.len());
            let (prefix, ver) = part.split_at(ver_start);
            if ver.is_empty() {
                return part.to_string();
            }
            // R writes exact-version constraints as `==`; semver spells it `=`.
            let prefix = prefix.replace("==", "=");
            format!("{prefix}{}", normalize_version(ver))
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
/// - Preserves 4th+ components as semver pre-release (e.g. `"1.0.12.2"` → `"1.0.12-4.2"`,
///   `"1.2.3.4.5"` → `"1.2.3-4.4.5"`) so that `1.0.12.1` and `1.0.12.2` are
///   distinguishable. The `4.` prefix ensures semver ordering is correct:
///   `1.0.12-4.1 < 1.0.12-4.2`.
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
    // R allows 4+-component versions (e.g. Rcpp 1.0.12.2, data.table dev builds).
    // Encode every trailing component as a semver pre-release so versions remain
    // distinguishable and correctly ordered (#130). A 4-component version keeps
    // its historical form (`1.2.3.4` → `1.2.3-4.4`) for lockfile compatibility;
    // 5th+ components append as further dotted pre-release identifiers
    // (`1.2.3.4.5` → `1.2.3-4.4.5`), and semver's numeric identifier ordering
    // keeps `1.2.3-4.4.5 < 1.2.3-4.4.6` and `1.2.3-4.4 < 1.2.3-4.4.5`.
    if parts.len() >= 4 {
        format!("{base}-4.{}", parts[3..].join("."))
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
        // 4-component versions (e.g. Rcpp 1.0.12.2) → semver pre-release.
        // This exact format is load-bearing: it's what existing lockfiles
        // contain, so it must never change (#130 kept it byte-identical).
        assert_eq!(normalize_version("1.0.12.2"), "1.0.12-4.2");
        assert_eq!(normalize_version("1.0.12.1"), "1.0.12-4.1");
        assert_eq!(normalize_version("1.2.3.4"), "1.2.3-4.4");
        // 5th+ components are preserved, not dropped (#130)
        assert_eq!(normalize_version("1.2.3.4.5"), "1.2.3-4.4.5");
        assert_eq!(normalize_version("1.2.3.4.6"), "1.2.3-4.4.6");
        assert_ne!(
            normalize_version("1.2.3.4.5"),
            normalize_version("1.2.3.4.6")
        );
        // 4-component versions are distinguishable via semver ordering
        let v1 = Version::parse(&normalize_version("1.0.12.1")).unwrap();
        let v2 = Version::parse(&normalize_version("1.0.12.2")).unwrap();
        assert!(v1 < v2);
        // 5-component ordering: 4.4 < 4.4.5 < 4.4.6 as pre-release identifiers
        let v4 = Version::parse(&normalize_version("1.2.3.4")).unwrap();
        let v5 = Version::parse(&normalize_version("1.2.3.4.5")).unwrap();
        let v6 = Version::parse(&normalize_version("1.2.3.4.6")).unwrap();
        assert!(v4 < v5);
        assert!(v5 < v6);
        // Both satisfy >=1.0.12 (pre-release < release, but >=1.0.12-0 matches)
        // and the raw_version is used for URL construction anyway
    }

    #[test]
    fn parse_constraints() {
        assert!(parse_version_req("*").is_ok());
        assert!(parse_version_req(">=3.0.0").is_ok());
        assert!(parse_version_req(">= 1.0.0").is_ok());
    }

    #[test]
    fn parse_constraints_with_r_style_versions() {
        // Real CRAN constraint shapes that used to fail semver parsing and be
        // silently dropped as unconstrained (#149 follow-up):
        // 4+ components (RcppArmadillo, h2o, airGR)…
        let req = parse_version_req(">= 1.6.9.27").expect("4-component constraint");
        assert!(req.matches(&Version::parse(&normalize_version("1.6.9.30")).unwrap()));
        assert!(!req.matches(&Version::parse(&normalize_version("1.6.9.20")).unwrap()));
        assert!(req.matches(&Version::parse(&normalize_version("1.7.0")).unwrap()));
        // …dash forms with 4 effective components (Matrix >= 1.2-7.1)…
        let req = parse_version_req(">= 1.2-7.1").expect("dash 4-component constraint");
        assert!(req.matches(&Version::parse(&normalize_version("1.3-0")).unwrap()));
        // …and leading zeros (R2jags >= 0.03-11).
        let req = parse_version_req(">= 0.03-11").expect("leading-zero constraint");
        assert!(req.matches(&Version::parse(&normalize_version("0.5.7")).unwrap()));
        assert!(!req.matches(&Version::parse(&normalize_version("0.2.0")).unwrap()));
        // …and R's `==` exact-version operator (semver spells it `=`).
        let req = parse_version_req("== 0.1.0").expect("double-equals constraint");
        assert!(req.matches(&Version::parse("0.1.0").unwrap()));
        assert!(!req.matches(&Version::parse("0.1.1").unwrap()));
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

        let lockfile = resolver.resolve(&manifest, None, HashMap::new()).unwrap();
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

        let lockfile = resolver.resolve(&manifest, None, HashMap::new()).unwrap();
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

        let result = resolver.resolve(&manifest, None, HashMap::new());
        assert!(result.is_err());
    }

    #[test]
    fn selective_update_pin_conflict_errors() {
        // #127: `uvr update shiny` holds rlang at its locked 1.1.4 via a
        // pin, but the fresh shiny requires rlang >= 2.0.0. The old
        // merge-after-resolve approach silently wrote the inconsistent pair;
        // pinned resolution must surface an explicit conflict instead.
        let registry = MockRegistry {
            packages: HashMap::from([make_pkg("shiny", "2.0.0", vec![("rlang", Some(">=2.0.0"))])]),
        };
        let resolver = Resolver::new(&registry);
        let mut manifest = Manifest::new("test", None);
        manifest.add_dep("shiny".into(), DependencySpec::Version("*".into()), false);

        let locked = LockedPackage {
            name: "rlang".into(),
            version: "1.1.4".into(),
            source: PackageSource::Cran,
            raw_version: None,
            url: Some("https://cran.r-project.org/rlang_1.1.4.tar.gz".into()),
            checksum: None,
            requires: vec![],
            system_requirements: None,
            dev: false,
        };
        let pins = HashMap::from([(
            "rlang".to_string(),
            locked_to_package_info(&locked).unwrap(),
        )]);

        let result = resolver.resolve(&manifest, None, pins);
        assert!(matches!(
            result,
            Err(UvrError::VersionConflict { ref package, .. }) if package == "rlang"
        ));
    }

    #[test]
    fn selective_update_pin_survives_diamond_reresolution() {
        // #127 review blocker: when the pinned package is BOTH a direct
        // manifest dep and a transitive dep of the updated target — the
        // common real-world shape (`Imports: shiny, rlang` where shiny also
        // needs rlang) — it resolves from the pin on first encounter
        // ("rlang" sorts before "shiny" in the manifest BTreeMap), and
        // shiny's stricter constraint then arrives via the diamond path.
        // That path used to consult the registry directly, silently
        // overriding the pin with a fresh version. It must conflict instead.
        let registry = MockRegistry {
            packages: HashMap::from([
                make_pkg("shiny", "2.0.0", vec![("rlang", Some(">=2.0.0"))]),
                make_pkg("rlang", "2.5.0", vec![]), // registry has a satisfying version
            ]),
        };
        let resolver = Resolver::new(&registry);
        let mut manifest = Manifest::new("test", None);
        manifest.add_dep("rlang".into(), DependencySpec::Version("*".into()), false);
        manifest.add_dep("shiny".into(), DependencySpec::Version("*".into()), false);

        let locked = LockedPackage {
            name: "rlang".into(),
            version: "1.1.4".into(),
            source: PackageSource::Cran,
            raw_version: None,
            url: None,
            checksum: None,
            requires: vec![],
            system_requirements: None,
            dev: false,
        };
        let pins = HashMap::from([(
            "rlang".to_string(),
            locked_to_package_info(&locked).unwrap(),
        )]);

        let result = resolver.resolve(&manifest, None, pins);
        assert!(
            matches!(
                result,
                Err(UvrError::VersionConflict { ref package, .. }) if package == "rlang"
            ),
            "pin must surface a conflict, not silently drift: {result:?}"
        );
    }

    #[test]
    fn selective_update_pin_holds_version() {
        // #127 companion: when the updated package is satisfied by the
        // pinned version, resolution keeps the pin even though the registry
        // has a newer release.
        let registry = MockRegistry {
            packages: HashMap::from([
                make_pkg("shiny", "2.0.0", vec![("rlang", Some(">=1.0.0"))]),
                make_pkg("rlang", "2.5.0", vec![]), // newer version available
            ]),
        };
        let resolver = Resolver::new(&registry);
        let mut manifest = Manifest::new("test", None);
        manifest.add_dep("shiny".into(), DependencySpec::Version("*".into()), false);

        let locked = LockedPackage {
            name: "rlang".into(),
            version: "1.1-4".into(), // R dash form must normalize, not error
            source: PackageSource::Cran,
            raw_version: Some("1.1-4".into()),
            url: None, // legacy entry without url
            checksum: None,
            requires: vec![],
            system_requirements: None,
            dev: false,
        };
        let pins = HashMap::from([(
            "rlang".to_string(),
            locked_to_package_info(&locked).unwrap(),
        )]);

        let lockfile = resolver.resolve(&manifest, None, pins).unwrap();
        assert_eq!(lockfile.get_package("rlang").unwrap().version, "1.1.4");
        assert_eq!(lockfile.get_package("shiny").unwrap().version, "2.0.0");
        // A pin without a url must not round-trip `Some("")` into the lockfile.
        assert_eq!(lockfile.get_package("rlang").unwrap().url, None);
    }

    #[test]
    fn pre_resolved_constraint_violation_errors() {
        // #84 review: a Remotes-resolved github package must still satisfy
        // the parent's `Imports:` constraint. Without the pre_resolved
        // constraint check, a github `handyr 0.0.0` would silently install
        // even when the parent declares `Imports: handyr (>= 1.0.0)`.
        let registry = MockRegistry {
            packages: HashMap::from([make_pkg(
                "airquality",
                "0.1.0",
                vec![("handyr", Some(">=1.0.0"))],
            )]),
        };
        let resolver = Resolver::new(&registry);

        let mut manifest = Manifest::new("test", None);
        manifest.add_dep(
            "airquality".into(),
            DependencySpec::Version("*".into()),
            false,
        );

        // Pre-resolved handyr is too old.
        let mut pre_resolved = HashMap::new();
        pre_resolved.insert(
            "handyr".to_string(),
            PackageInfo {
                name: "handyr".to_string(),
                version: Version::parse("0.0.0").unwrap(),
                source: PackageSource::GitHub,
                checksum: None,
                requires: vec![],
                url: "https://api.github.com/...".to_string(),
                raw_version: None,
                system_requirements: None,
            },
        );

        let result = resolver.resolve(&manifest, None, pre_resolved);
        match result {
            Err(UvrError::VersionConflict {
                package,
                required,
                conflicting,
            }) => {
                assert_eq!(package, "handyr");
                assert_eq!(required, ">=1.0.0");
                assert_eq!(conflicting, "0.0.0");
            }
            other => panic!("expected VersionConflict, got {other:?}"),
        }
    }

    #[test]
    fn pre_resolved_constraint_satisfied_succeeds() {
        // Same setup as above but pre_resolved handyr satisfies the
        // constraint — resolve should succeed and use the github source.
        let registry = MockRegistry {
            packages: HashMap::from([make_pkg(
                "airquality",
                "0.1.0",
                vec![("handyr", Some(">=1.0.0"))],
            )]),
        };
        let resolver = Resolver::new(&registry);

        let mut manifest = Manifest::new("test", None);
        manifest.add_dep(
            "airquality".into(),
            DependencySpec::Version("*".into()),
            false,
        );

        let mut pre_resolved = HashMap::new();
        pre_resolved.insert(
            "handyr".to_string(),
            PackageInfo {
                name: "handyr".to_string(),
                version: Version::parse("1.5.0").unwrap(),
                source: PackageSource::GitHub,
                checksum: None,
                requires: vec![],
                url: "https://api.github.com/...".to_string(),
                raw_version: None,
                system_requirements: None,
            },
        );

        let lockfile = resolver
            .resolve(&manifest, None, pre_resolved)
            .expect("resolve should succeed when pre_resolved version satisfies");
        let handyr = lockfile.get_package("handyr").unwrap();
        assert_eq!(handyr.version, "1.5.0");
        assert_eq!(handyr.source, PackageSource::GitHub);
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

        let lockfile = resolver.resolve(&manifest, None, HashMap::new()).unwrap();
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
                dev: false,
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
                dev: false,
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
                dev: false,
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

    #[test]
    fn dev_dependencies_resolved_and_tagged() {
        // ggplot2 is a regular dep, testthat is dev-only.
        // Both should be resolved, but testthat and its exclusive deps should be dev=true.
        let mut packages = HashMap::new();
        packages.extend([
            make_pkg("ggplot2", "3.4.4", vec![("rlang", None)]),
            make_pkg("rlang", "1.1.4", vec![]),
            make_pkg("testthat", "3.2.0", vec![("praise", None)]),
            make_pkg("praise", "1.0.0", vec![]),
        ]);
        let registry = MockRegistry { packages };
        let resolver = Resolver::new(&registry);

        let mut manifest = Manifest::new("test", None);
        manifest.add_dep("ggplot2".into(), DependencySpec::Version("*".into()), false);
        manifest.add_dep("testthat".into(), DependencySpec::Version("*".into()), true); // dev dep

        let lockfile = resolver.resolve(&manifest, None, HashMap::new()).unwrap();
        assert_eq!(lockfile.packages.len(), 4);

        // ggplot2 and rlang are production deps
        assert!(!lockfile.get_package("ggplot2").unwrap().dev);
        assert!(!lockfile.get_package("rlang").unwrap().dev);

        // testthat and praise are dev-only
        assert!(lockfile.get_package("testthat").unwrap().dev);
        assert!(lockfile.get_package("praise").unwrap().dev);
    }

    #[test]
    fn shared_dep_between_dev_and_prod_is_not_dev() {
        // Both ggplot2 (prod) and testthat (dev) depend on rlang.
        // rlang should NOT be marked as dev since it's reachable from prod.
        let mut packages = HashMap::new();
        packages.extend([
            make_pkg("ggplot2", "3.4.4", vec![("rlang", None)]),
            make_pkg("testthat", "3.2.0", vec![("rlang", None)]),
            make_pkg("rlang", "1.1.4", vec![]),
        ]);
        let registry = MockRegistry { packages };
        let resolver = Resolver::new(&registry);

        let mut manifest = Manifest::new("test", None);
        manifest.add_dep("ggplot2".into(), DependencySpec::Version("*".into()), false);
        manifest.add_dep("testthat".into(), DependencySpec::Version("*".into()), true);

        let lockfile = resolver.resolve(&manifest, None, HashMap::new()).unwrap();
        assert_eq!(lockfile.packages.len(), 3);

        assert!(!lockfile.get_package("rlang").unwrap().dev);
        assert!(lockfile.get_package("testthat").unwrap().dev);
    }
}
