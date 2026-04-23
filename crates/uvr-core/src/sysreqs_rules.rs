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
/// stable order. Empty if no rules match or the distro isn't covered.
pub fn resolve_local(sys_req_text: &str, distribution: &str, version: &str) -> Vec<String> {
    if sys_req_text.is_empty() {
        return Vec::new();
    }

    let sets = pattern_sets();
    let mut out: Vec<String> = Vec::new();

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
                    if !out.iter().any(|x| x == pkg) {
                        out.push((*pkg).to_string());
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
    match c.distribution {
        Some(d) if d != distribution => return false,
        None => return false,
        _ => {}
    }
    if c.versions.is_empty() {
        return true;
    }
    c.versions.contains(&version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml2_on_alpine_matches_libxml2_dev() {
        // Reproduces the scenario from issue #30.
        let pkgs = resolve_local("libxml2 (>= 2.6.3)", "alpine", "3.21");
        assert!(
            pkgs.iter().any(|p| p == "libxml2-dev"),
            "expected libxml2-dev, got {pkgs:?}"
        );
    }

    #[test]
    fn xml2_on_ubuntu_matches_libxml2_dev() {
        let pkgs = resolve_local("libxml2 (>= 2.6.3)", "ubuntu", "22.04");
        assert!(
            pkgs.iter().any(|p| p == "libxml2-dev"),
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
        assert!(!pkgs.is_empty(), "expected at least one package, got empty");
    }

    #[test]
    fn unknown_distro_returns_empty() {
        let pkgs = resolve_local("libxml2", "haiku", "");
        assert!(pkgs.is_empty(), "expected empty, got {pkgs:?}");
    }

    #[test]
    fn empty_sys_reqs_returns_empty() {
        let pkgs = resolve_local("", "alpine", "3.21");
        assert!(pkgs.is_empty());
    }

    #[test]
    fn dedupes_packages_across_rule_matches() {
        // A string that may match multiple rules shouldn't duplicate packages.
        let pkgs = resolve_local("libxml2 libxml2 libxml2", "alpine", "3.21");
        let n_libxml = pkgs.iter().filter(|p| *p == "libxml2-dev").count();
        assert_eq!(n_libxml, 1);
    }
}
