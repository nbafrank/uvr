//! Local fallback sysreqs resolver.
//!
//! Wraps the compile-time rule table generated from
//! `vendor/r-system-requirements/rules/` and exposes a single function —
//! [`resolve_local`] — that turns a raw `SystemRequirements` string into a
//! list of distro-native system package names.
//!
//! Used when the Posit Package Manager sysreqs API reports the distribution
//! as unsupported (e.g. Alpine, see issue #30). Matches upstream semantics:
//!
//! 1. For each rule, compile its patterns once and test them against the
//!    SystemRequirements text (case-insensitive, multi-line).
//! 2. On a pattern hit, iterate that rule's dependency entries and pick the
//!    first one whose constraints match the target `(os, distribution,
//!    version)` triple. Empty `versions` means "any release of that distro."
//! 3. Collect the matched `packages` into the result, deduplicated, in
//!    stable order.

use regex::RegexSet;
use std::sync::OnceLock;

include!(concat!(env!("OUT_DIR"), "/sysreqs_rules_generated.rs"));

/// Per-rule compiled RegexSet. Built lazily on first call to `resolve_local`.
/// Total set size is ~200 patterns; compilation cost is paid once per process.
fn pattern_sets() -> &'static [RegexSet] {
    static CACHE: OnceLock<Vec<RegexSet>> = OnceLock::new();
    CACHE.get_or_init(|| {
        RULES
            .iter()
            .map(|r| {
                // Upstream rules assume case-insensitive, line-aware matching.
                let patterns: Vec<String> = r.patterns.iter().map(|p| format!("(?i){p}")).collect();
                RegexSet::new(&patterns).unwrap_or_else(|e| {
                    panic!("bad vendor regex in rule {}: {e}", r.name);
                })
            })
            .collect()
    })
}

/// What the vendored rules say a package needs on this distribution.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LocalResolution {
    /// Distro-native package names, deduplicated in stable order.
    pub packages: Vec<String>,
    /// Commands to run before installing `packages` (e.g. enabling EPEL).
    pub pre_install: Vec<String>,
    /// Commands to run after installing `packages` (e.g. `R CMD javareconf`).
    pub post_install: Vec<String>,
}

/// Resolve sysreqs against the local rules.
///
/// - `sys_req_text`: the raw `SystemRequirements` field from DESCRIPTION
///   (may be multi-line, free-form; matching is regex-based).
/// - `distribution`: e.g. `"alpine"`, `"ubuntu"`, `"rockylinux"` — the
///   first half of an os-release `id-version` pair.
/// - `version`: e.g. `"3.21"`, `"22.04"` — the second half. Pass an empty
///   string if unknown; rules with empty `versions` still match.
///
/// Returns the matched distro-native system package names, de-duplicated in
/// stable order, along with the pre/post install commands the matched rules
/// carry (also de-duplicated). Empty if no rules match or the distro isn't
/// covered.
pub fn resolve_local(sys_req_text: &str, distribution: &str, version: &str) -> LocalResolution {
    let mut out = LocalResolution::default();
    if sys_req_text.is_empty() {
        return out;
    }

    let sets = pattern_sets();

    for (rule, set) in RULES.iter().zip(sets.iter()) {
        if !set.is_match(sys_req_text) {
            continue;
        }
        // First matching dependency entry wins for this rule — upstream
        // rules are authored so that at most one entry matches any given
        // (distribution, version) pair.
        for dep in rule.dependencies.iter() {
            if dep
                .constraints
                .iter()
                .any(|c| constraint_matches(c, distribution, version))
            {
                for pkg in dep.packages {
                    if !out.packages.iter().any(|x| x == pkg) {
                        out.packages.push((*pkg).to_string());
                    }
                }
                for cmd in dep.pre_install {
                    if !out.pre_install.iter().any(|x| x == cmd) {
                        out.pre_install.push((*cmd).to_string());
                    }
                }
                for cmd in dep.post_install {
                    if !out.post_install.iter().any(|x| x == cmd) {
                        out.post_install.push((*cmd).to_string());
                    }
                }
                break;
            }
        }
    }

    out
}

fn constraint_matches(c: &ConstraintStatic, distribution: &str, version: &str) -> bool {
    // We only run the local fallback on Linux, so reject non-linux rules.
    if let Some(os) = c.os {
        if os != "linux" {
            return false;
        }
    }
    // Upstream r-system-requirements semantics: a constraint without a
    // `distribution` applies to every distribution of its `os` (wildcard).
    // This used to be never-match — currently unreachable with the vendored
    // rules (all 27 distribution-less constraints are os: "windows", rejected
    // above), but a vendor sync introducing a linux one would have silently
    // dropped its rule (#166).
    if let Some(d) = c.distribution {
        if d != distribution {
            return false;
        }
    }
    if c.versions.is_empty() {
        return true;
    }
    // Match the host's full `VERSION_ID` against rule versions first; then
    // retry with a major.minor truncation. Upstream rules key on `3.21`,
    // `22.04`, etc., but `/etc/os-release` on Alpine 3.23.4 reports
    // `VERSION_ID="3.23.4"` — without truncation a 3.23.4 host gets zero
    // rule hits even though the rules cover 3.23 (issue #30).
    if c.versions.contains(&version) {
        return true;
    }
    let major_minor = truncate_to_minor(version);
    if let Some(mm) = major_minor.as_deref() {
        return c.versions.contains(&mm);
    }
    false
}

/// Truncate a `major.minor.patch` string to `major.minor`. Returns `None`
/// when the input has fewer than three dot-separated components (no patch
/// to strip).
fn truncate_to_minor(v: &str) -> Option<String> {
    let mut parts = v.split('.');
    let major = parts.next()?;
    let minor = parts.next()?;
    parts.next()?; // require at least 3 components
    Some(format!("{major}.{minor}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distribution_less_linux_constraint_is_wildcard() {
        // #166: upstream semantics say no `distribution` = every distro of
        // that os. (Currently unreachable via the vendored rules — all
        // distribution-less constraints are os: "windows" — so this guards
        // the helper directly against a future vendor sync.)
        let wildcard = ConstraintStatic {
            os: Some("linux"),
            distribution: None,
            versions: &[],
        };
        assert!(constraint_matches(&wildcard, "ubuntu", "22.04"));
        assert!(constraint_matches(&wildcard, "alpine", "3.21"));

        let windows = ConstraintStatic {
            os: Some("windows"),
            distribution: None,
            versions: &[],
        };
        assert!(!constraint_matches(&windows, "ubuntu", "22.04"));
    }

    #[test]
    fn xml2_on_alpine_matches_libxml2_dev() {
        // Reproduces the scenario from issue #30.
        let pkgs = resolve_local("libxml2 (>= 2.6.3)", "alpine", "3.21");
        assert!(
            pkgs.packages.iter().any(|p| p == "libxml2-dev"),
            "expected libxml2-dev, got {pkgs:?}"
        );
    }

    #[test]
    fn alpine_full_version_id_normalizes_to_minor() {
        // pat-s's repro from #30: Alpine 3.23.4 reports VERSION_ID="3.23.4"
        // but rules key on "3.23". The fallback truncates the patch.
        let pkgs = resolve_local("libxml2 (>= 2.6.3)", "alpine", "3.23.4");
        assert!(
            pkgs.packages.iter().any(|p| p == "libxml2-dev"),
            "expected libxml2-dev for alpine-3.23.4, got {pkgs:?}"
        );
    }

    #[test]
    fn truncate_to_minor_strips_patch() {
        assert_eq!(truncate_to_minor("3.23.4").as_deref(), Some("3.23"));
        assert_eq!(truncate_to_minor("22.04.1").as_deref(), Some("22.04"));
        // No truncation when fewer than 3 components.
        assert!(truncate_to_minor("3.23").is_none());
        assert!(truncate_to_minor("22").is_none());
    }

    #[test]
    fn xml2_on_ubuntu_matches_libxml2_dev() {
        let pkgs = resolve_local("libxml2 (>= 2.6.3)", "ubuntu", "22.04");
        assert!(
            pkgs.packages.iter().any(|p| p == "libxml2-dev"),
            "expected libxml2-dev, got {pkgs:?}"
        );
    }

    #[test]
    fn curl_on_alpine_matches_curl_dev() {
        // `curl` rule in the vendor tree maps to `curl-dev` on alpine.
        let pkgs = resolve_local(
            "libcurl: libcurl-openssl-dev (deb), libcurl-devel (rpm)",
            "alpine",
            "3.21",
        );
        assert!(
            !pkgs.packages.is_empty(),
            "expected at least one package, got empty"
        );
    }

    #[test]
    fn unknown_distro_returns_empty() {
        let pkgs = resolve_local("libxml2", "haiku", "");
        assert!(pkgs.packages.is_empty(), "expected empty, got {pkgs:?}");
    }

    #[test]
    fn empty_sys_reqs_returns_empty() {
        let pkgs = resolve_local("", "alpine", "3.21");
        assert!(pkgs.packages.is_empty());
    }

    #[test]
    fn dedupes_packages_across_rule_matches() {
        // A string that may match multiple rules shouldn't duplicate packages.
        let pkgs = resolve_local("libxml2 libxml2 libxml2", "alpine", "3.21");
        let n_libxml = pkgs.packages.iter().filter(|p| *p == "libxml2-dev").count();
        assert_eq!(n_libxml, 1);
    }

    #[test]
    fn gdal_carries_pre_install_on_rockylinux_but_not_alpine() {
        // rules/gdal.json: the rockylinux 9/10 entry needs the CRB repo
        // enabled first (dnf-plugins-core + config-manager --set-enabled
        // crb); the alpine entry needs nothing.
        // (The vendored gdal.json rockylinux 9/10 entry does not mention
        // EPEL, unlike the sibling geos.json rule; asserting on "crb" here
        // matches the actual vendored data.)
        // Rule names in the generated table are file stems (build.rs uses
        // `path.file_stem()`), so this is "gdal", not "gdal.json".
        let gdal = RULES
            .iter()
            .find(|r| r.name == "gdal")
            .expect("gdal rule is vendored");
        let rocky_dep = gdal
            .dependencies
            .iter()
            .find(|d| {
                d.constraints.iter().any(|c| {
                    c.distribution == Some("rockylinux") && c.versions.contains(&"9")
                })
            })
            .expect("rockylinux 9 entry");
        assert!(
            rocky_dep.pre_install.iter().any(|c| c.contains("crb")),
            "rockylinux gdal needs the CRB repo enabled first"
        );

        let alpine_dep = gdal_dep_for(gdal, "alpine");
        assert!(alpine_dep.pre_install.is_empty());
    }

    fn gdal_dep_for<'a>(rule: &'a RuleStatic, distro: &str) -> &'a DependencyStatic {
        rule.dependencies
            .iter()
            .find(|d| {
                d.constraints
                    .iter()
                    .any(|c| c.distribution == Some(distro))
            })
            .expect("dependency entry for distro")
    }

    #[test]
    fn pre_install_is_deduplicated_across_matched_rules() {
        // sf's SystemRequirements matches gdal, geos and proj. In the VENDORED
        // snapshot all three carry `dnf install -y dnf-plugins-core` and
        // `dnf config-manager --set-enabled crb`; only geos.json also carries
        // `dnf install -y epel-release`.
        let r = resolve_local(
            "GDAL (>= 2.0.1), GEOS (>= 3.4.0), PROJ (>= 4.8.0), sqlite3",
            "rockylinux",
            "9",
        );
        // Assert on a command that ALL THREE matched rules carry, otherwise
        // the test proves nothing: in the vendored snapshot `epel-release`
        // appears only in geos.json, so counting it would pass trivially even
        // with dedup removed. `--set-enabled crb` and `dnf-plugins-core` are
        // in gdal.json, geos.json and proj.json alike.
        let crb: Vec<&String> = r
            .pre_install
            .iter()
            .filter(|c| c.contains("--set-enabled crb"))
            .collect();
        assert_eq!(crb.len(), 1, "crb enable must be deduplicated across rules");
        let plugins: Vec<&String> = r
            .pre_install
            .iter()
            .filter(|c| c.contains("dnf-plugins-core"))
            .collect();
        assert_eq!(plugins.len(), 1, "dnf-plugins-core must be deduplicated");
        assert!(r.packages.iter().any(|p| p.starts_with("gdal")));
    }

    #[test]
    fn alpine_resolution_carries_no_setup_commands() {
        let r = resolve_local("GDAL (>= 2.0.1)", "alpine", "3.23.5");
        assert_eq!(r.packages, vec!["gdal-dev", "gdal-tools"]);
        assert!(r.pre_install.is_empty());
        assert!(r.post_install.is_empty());
    }

    #[test]
    fn java_carries_a_post_install_command() {
        let r = resolve_local("Java JDK 8 or higher", "ubuntu", "22.04");
        assert!(
            r.post_install.iter().any(|c| c.contains("javareconf")),
            "rJava needs `R CMD javareconf` after install"
        );
    }
}
