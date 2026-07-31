use std::collections::HashMap;

use serde::Deserialize;
use tracing::{debug, warn};

use crate::error::Result;
use crate::sysreqs_rules;

/// A resolved system dependency with its apt/rpm package name.
#[derive(Debug, Clone)]
pub struct SysReq {
    /// The system package name, e.g. `"libxml2-dev"`.
    pub package: String,
}

/// Detect the Linux distribution from `/etc/os-release`, normalized onto the
/// vocabulary the sysreqs catalogs use. Returns `id-version` like
/// `"ubuntu-22.04"` or `"redhat-8"`.
pub fn detect_linux_distro() -> Option<String> {
    let os = crate::os_release::detect()?;
    // Both halves are required here, unlike P3M slug detection: the sysreqs
    // API and the vendored rules are keyed by `id-version`, and there is no
    // sensible query for a rolling release that publishes no version. Arch
    // and CachyOS land here and the caller skips the check — the safe
    // direction to fail, since a wrong answer only produces a wrong hint.
    if os.id.is_empty() || os.version_id.is_empty() {
        return None;
    }
    let (distribution, release) = normalize_distro(&os.id, &os.version_id);
    Some(format!("{distribution}-{release}"))
}

/// Map an `/etc/os-release` `(ID, VERSION_ID)` pair onto the names Posit's
/// sysreqs API and the vendored `r-system-requirements` rules are written in.
///
/// Both catalogs speak the Posit vocabulary, not the os-release one, and both
/// reject the raw values RHEL reports (#209):
///
/// ```text
/// ?distribution=rhel&release=8.10  -> {"code":14,"error":"Unsupported system"}
/// ?distribution=redhat&release=8   -> {"requirements":[{"name":"xml2", ...}]}
/// ```
///
/// The vendored rules agree — every RHEL entry is written as `redhat` with
/// `versions: ["8"]`, so a `rhel` / `8.10` host matched no rule either and the
/// local fallback had nothing to offer once the API had bowed out. RHEL hosts
/// therefore got no system dependencies from either source.
///
/// Only the RHEL family is truncated to its major: Ubuntu is keyed `22.04`,
/// openSUSE `15.6` and SLE `12.3` in both catalogs.
pub(crate) fn normalize_distro(id: &str, version_id: &str) -> (String, String) {
    let distribution = match id {
        "rhel" => "redhat",
        // The RHEL rebuilds share Rocky's entries — same repos (crb /
        // powertools), same package names.
        "rocky" | "almalinux" => "rockylinux",
        "sles" => "sle",
        "opensuse-leap" | "opensuse-tumbleweed" => "opensuse",
        other => other,
    };
    let release = match distribution {
        "redhat" | "rockylinux" | "centos" | "fedora" => {
            version_id.split('.').next().unwrap_or(version_id)
        }
        _ => version_id,
    };
    (distribution.to_string(), release.to_string())
}

/// Response from the Posit Package Manager sysreqs API.
#[derive(Debug, Deserialize)]
struct PpmSysreqsResponse {
    #[serde(default)]
    requirements: Vec<PpmRequirement>,
}

#[derive(Debug, Deserialize)]
struct PpmRequirement {
    #[serde(default)]
    requirements: PpmRequirementDetail,
}

#[derive(Debug, Default, Deserialize)]
struct PpmRequirementDetail {
    #[serde(default)]
    packages: Vec<String>,
}

/// Outcome of a sysreqs API lookup.
///
/// `UnsupportedDistro` lets callers tell "no system deps needed" apart from
/// "we couldn't check because Posit's catalog doesn't cover this distro"
/// (e.g. Alpine — see issue #30). Silently treating the latter as the former
/// makes uvr act like it verified sysreqs when it actually skipped them,
/// which bites users whose packages then fail to compile from source.
#[derive(Debug, Clone)]
pub enum SysReqLookup {
    Supported(Vec<SysReq>),
    UnsupportedDistro,
    /// The API couldn't be consulted at all (network failure, non-success
    /// HTTP status, unparseable body). Distinct from `Supported(vec![])` —
    /// "no sysdeps needed" — so callers can fall back to the vendored local
    /// rules instead of silently acting as if the check passed (#148). An
    /// offline CI runner used to get neither API nor local results.
    LookupFailed,
}

/// Detects the Posit PPM "Unsupported system" error body.
///
/// Response shape: `{"code":14,"error":"Unsupported system"}`. Match on the
/// error text rather than the status code, since we've only observed this on
/// non-success responses but don't want to couple to a specific HTTP code.
fn is_unsupported_system_body(body: &str) -> bool {
    body.contains("Unsupported system")
}

/// Query the Posit Package Manager sysreqs API for system dependencies.
///
/// API: `GET https://packagemanager.posit.co/__api__/repos/1/sysreqs?all=false&pkgname=<name>&distribution=<os>&release=<version>`
///
/// This replaces the archived r-hub sysreqs API with Posit's actively
/// maintained r-system-requirements catalog.
pub async fn resolve_system_deps(
    client: &reqwest::Client,
    package_name: &str,
    distro: &str,
) -> Result<SysReqLookup> {
    let (distribution, release) = distro.split_once('-').unwrap_or((distro, ""));

    let url = "https://packagemanager.posit.co/__api__/repos/1/sysreqs";

    debug!(
        "Querying Posit sysreqs API for {package_name} (distro={distribution}, release={release})"
    );

    let resp = client
        .get(url)
        .query(&[
            ("all", "false"),
            ("pkgname", package_name),
            ("distribution", distribution),
            ("release", release),
        ])
        .send()
        .await;

    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            warn!("Posit sysreqs API request failed: {e}");
            return Ok(SysReqLookup::LookupFailed);
        }
    };

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();

    if !status.is_success() {
        if is_unsupported_system_body(&body) {
            debug!("Posit sysreqs API reports {distribution} is unsupported");
            return Ok(SysReqLookup::UnsupportedDistro);
        }
        warn!("Posit sysreqs API returned {status} for {package_name}");
        return Ok(SysReqLookup::LookupFailed);
    }

    let response: PpmSysreqsResponse = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => {
            warn!("Failed to parse Posit sysreqs API response: {e}");
            return Ok(SysReqLookup::LookupFailed);
        }
    };

    let mut result = Vec::new();
    for req in response.requirements {
        for pkg in req.requirements.packages {
            if !pkg.is_empty() {
                result.push(SysReq { package: pkg });
            }
        }
    }

    Ok(SysReqLookup::Supported(result))
}

/// Check which packages are missing on the system.
/// Uses `dpkg -s` on Debian/Ubuntu, `rpm -q` on Fedora/RHEL/SUSE.
/// If neither package manager is found, returns an empty list (skip check).
pub fn filter_missing(packages: &[SysReq]) -> Vec<&SysReq> {
    let (cmd, args): (&str, &[&str]) = if which::which("dpkg").is_ok() {
        ("dpkg", &["-s"])
    } else if which::which("rpm").is_ok() {
        ("rpm", &["-q"])
    } else if which::which("apk").is_ok() {
        ("apk", &["info", "-e"])
    } else {
        debug!("No supported package manager (dpkg/rpm/apk) found, skipping sysreqs check");
        return vec![];
    };

    packages
        .iter()
        .filter(|req| {
            let output = std::process::Command::new(cmd)
                .args(args)
                .arg(&req.package)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            match output {
                Ok(status) => !status.success(),
                Err(_) => false, // command failed to run — don't report as missing
            }
        })
        .collect()
}

/// Aggregate result of a sysreqs check across many packages.
#[derive(Debug, Default)]
pub struct SysReqsCheck {
    /// Missing system packages keyed by R package name.
    pub missing: HashMap<String, Vec<SysReq>>,
    /// Set when the Posit API reported the distro as unsupported.
    /// When true, `missing` is not authoritative — the check was skipped.
    pub unsupported_distro: bool,
    /// Set when at least one API lookup failed outright (network/HTTP/parse,
    /// #148). Affected packages were checked against the vendored local
    /// rules instead, so `missing` is best-effort rather than authoritative.
    pub lookup_failed: bool,
}

/// R package to check sysreqs for.
#[derive(Debug, Clone)]
pub struct PackageSysReqQuery {
    /// Canonical R package name (e.g. `"xml2"`).
    pub name: String,
    /// Raw `SystemRequirements` field from DESCRIPTION, if any. Used only
    /// when the Posit API rejects the distribution and we fall back to
    /// the vendored `r-system-requirements` rules locally.
    pub system_requirements: Option<String>,
    /// True for Bioconductor-sourced packages. The Posit sysreqs API only
    /// covers CRAN and returns HTTP 500 for Bioc names, so querying it is a
    /// guaranteed per-package WARN with zero information (#202) — Bioc
    /// packages go straight to the vendored local rules instead.
    pub bioc: bool,
}

/// Resolve and check system dependencies for a set of packages.
///
/// Flow:
/// 1. Query Posit's sysreqs API per package.
/// 2. If PPM reports `UnsupportedDistro` (e.g. Alpine), stop querying and
///    fall back to the vendored `r-system-requirements` rules. The fallback
///    matches each package's `SystemRequirements` string against the local
///    rule table. This is the path that addresses issue #30 end-to-end.
/// 3. Filter the resolved deps through the installed package manager
///    (`dpkg`/`rpm`/`apk`) to surface only the ones that are actually
///    missing.
///
/// Returns both the missing-deps map and a flag indicating whether PPM
/// rejected the distribution (set true even when the local fallback fills
/// in results, so callers can mention the provenance if they want).
pub async fn check_system_deps(
    client: &reqwest::Client,
    packages: &[PackageSysReqQuery],
    distro: &str,
) -> SysReqsCheck {
    let mut out = SysReqsCheck::default();
    let mut local_fallback = false;
    let mut tail_start: usize = 0;

    for (idx, pkg) in packages.iter().enumerate() {
        // The Posit API is CRAN-only; Bioc names 500 every time. Check them
        // against the vendored local rules directly — no request, no WARN,
        // and no degraded-check flag, since nothing actually degraded (#202).
        if pkg.bioc {
            check_pkg_local(&mut out, pkg, distro);
            continue;
        }
        match resolve_system_deps(client, &pkg.name, distro).await {
            Ok(SysReqLookup::Supported(resolved)) => {
                let missing = filter_missing(&resolved);
                if !missing.is_empty() {
                    out.missing
                        .insert(pkg.name.clone(), missing.into_iter().cloned().collect());
                }
            }
            Ok(SysReqLookup::UnsupportedDistro) => {
                out.unsupported_distro = true;
                local_fallback = true;
                tail_start = idx;
                break;
            }
            Ok(SysReqLookup::LookupFailed) => {
                // API unreachable/broken for this package (#148): check it
                // against the vendored local rules instead of pretending the
                // check passed, but keep querying the API for the rest — a
                // transient per-request failure shouldn't downgrade the
                // whole run to local rules.
                out.lookup_failed = true;
                check_pkg_local(&mut out, pkg, distro);
            }
            Err(e) => {
                warn!("Failed to resolve system deps for {}: {e}", pkg.name);
            }
        }
    }

    if local_fallback {
        for pkg in &packages[tail_start..] {
            check_pkg_local(&mut out, pkg, distro);
        }
    }

    out
}

/// Check one package's `SystemRequirements` against the vendored
/// `r-system-requirements` rules and record any missing system packages.
/// Shared by the unsupported-distro tail fallback and the per-package
/// API-failure fallback (#148).
fn check_pkg_local(out: &mut SysReqsCheck, pkg: &PackageSysReqQuery, distro: &str) {
    let (distribution, version) = distro.split_once('-').unwrap_or((distro, ""));
    let Some(sys_req_text) = pkg.system_requirements.as_deref() else {
        return;
    };
    let resolved: Vec<SysReq> = sysreqs_rules::resolve_local(sys_req_text, distribution, version)
        .into_iter()
        .map(|package| SysReq { package })
        .collect();
    if resolved.is_empty() {
        return;
    }
    let missing = filter_missing(&resolved);
    if !missing.is_empty() {
        out.missing
            .insert(pkg.name.clone(), missing.into_iter().cloned().collect());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bioc_packages_skip_the_posit_api_entirely() {
        // #202: the Posit sysreqs API 500s for every Bioc name. Bioc-flagged
        // queries must go straight to local rules: no request is made, so
        // neither lookup_failed (API contact failed) nor unsupported_distro
        // can be set — on any machine, online or offline. If this test hangs
        // or flags lookup_failed, the API skip regressed.
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(200))
            .build()
            .unwrap();
        let queries = vec![
            PackageSysReqQuery {
                name: "SummarizedExperiment".into(),
                system_requirements: None,
                bioc: true,
            },
            PackageSysReqQuery {
                name: "Rhtslib".into(),
                system_requirements: Some("libbz2 & liblzma & GNU make".into()),
                bioc: true,
            },
        ];
        let check = check_system_deps(&client, &queries, "ubuntu-22.04").await;
        assert!(!check.lookup_failed, "no API contact → no failed lookups");
        assert!(!check.unsupported_distro);
    }

    #[test]
    fn rhel_normalizes_to_the_posit_vocabulary() {
        // #209: UBI/RHEL report ID="rhel", VERSION_ID="8.10". Posit's API
        // answers only for redhat/8, and every vendored rule is written as
        // `redhat` with versions ["8"] — so the raw pair matched neither.
        assert_eq!(
            normalize_distro("rhel", "8.10"),
            ("redhat".to_string(), "8".to_string())
        );
        assert_eq!(
            normalize_distro("rocky", "9.3"),
            ("rockylinux".to_string(), "9".to_string())
        );
        assert_eq!(
            normalize_distro("almalinux", "10.0"),
            ("rockylinux".to_string(), "10".to_string())
        );
        assert_eq!(
            normalize_distro("sles", "12.3"),
            ("sle".to_string(), "12.3".to_string())
        );
    }

    #[test]
    fn non_rhel_distros_keep_their_full_version() {
        // Ubuntu is keyed "22.04" and openSUSE "15.6" in both catalogs;
        // truncating either to a major would match nothing.
        assert_eq!(
            normalize_distro("ubuntu", "22.04"),
            ("ubuntu".to_string(), "22.04".to_string())
        );
        assert_eq!(
            normalize_distro("opensuse-leap", "15.6"),
            ("opensuse".to_string(), "15.6".to_string())
        );
        assert_eq!(
            normalize_distro("debian", "12"),
            ("debian".to_string(), "12".to_string())
        );
        // Alpine keeps its patch version; `resolve_local` truncates it to
        // major.minor when matching rules (#30).
        assert_eq!(
            normalize_distro("alpine", "3.24.1"),
            ("alpine".to_string(), "3.24.1".to_string())
        );
    }

    #[test]
    fn normalized_rhel_resolves_against_the_local_rules() {
        // The other half of #209: with the normalized pair the vendored rules
        // finally match, so the local fallback works even when the API can't
        // be reached.
        let (distribution, release) = normalize_distro("rhel", "8.10");
        let pkgs = sysreqs_rules::resolve_local("libxml2 (>= 2.6.3)", &distribution, &release);
        assert!(
            pkgs.iter().any(|p| p == "libxml2-devel"),
            "expected libxml2-devel for rhel-8.10, got {pkgs:?}"
        );
        let gdal = sysreqs_rules::resolve_local("GDAL (>= 2.2.3)", &distribution, &release);
        assert!(
            gdal.iter().any(|p| p == "gdal-devel"),
            "expected gdal-devel for rhel-8.10, got {gdal:?}"
        );
    }

    #[test]
    fn detect_distro_format() {
        // This test only makes assertions on Linux
        if cfg!(target_os = "linux") {
            if let Some(distro) = detect_linux_distro() {
                assert!(
                    distro.contains('-'),
                    "expected format 'id-version', got: {distro}"
                );
            }
        }
    }

    #[test]
    fn filter_missing_with_nonexistent_package() {
        if !cfg!(target_os = "linux") {
            return;
        }
        // `filter_missing` documents itself as a no-op without dpkg/rpm/apk,
        // so asserting it reports something requires one of them to exist.
        // The test used to assume every Linux has one and failed on Arch,
        // NixOS, Gentoo and friends — a false alarm for anyone developing
        // uvr there.
        if which::which("dpkg").is_err()
            && which::which("rpm").is_err()
            && which::which("apk").is_err()
        {
            eprintln!("skipping: no dpkg/rpm/apk on this system");
            return;
        }
        let reqs = vec![SysReq {
            package: "uvr-nonexistent-pkg-12345".to_string(),
        }];
        let missing = filter_missing(&reqs);
        assert_eq!(missing.len(), 1);
    }

    #[test]
    fn filter_missing_is_a_no_op_without_a_package_manager() {
        // The other half of the contract: with no supported package manager
        // we report nothing missing rather than guessing. Callers must treat
        // that as "unverified", not "verified clean" — see `check_system_deps`.
        if which::which("dpkg").is_ok()
            || which::which("rpm").is_ok()
            || which::which("apk").is_ok()
        {
            eprintln!("skipping: a supported package manager is present");
            return;
        }
        let reqs = vec![SysReq {
            package: "uvr-nonexistent-pkg-12345".to_string(),
        }];
        assert!(filter_missing(&reqs).is_empty());
    }

    #[test]
    fn detects_ppm_unsupported_system_body() {
        // Observed on Alpine across 3.15–3.21 (issue #30).
        assert!(is_unsupported_system_body(
            r#"{"code":14,"error":"Unsupported system"}"#
        ));
        assert!(!is_unsupported_system_body(r#"{"requirements":[]}"#));
        assert!(!is_unsupported_system_body(""));
    }

    #[test]
    fn local_fallback_resolves_alpine_xml2_requirements() {
        // Direct smoke test of the fallback path: given an Alpine-targeted
        // SystemRequirements string, the vendored rules should produce the
        // apk-compatible package name. This is the invariant issue #30 needs.
        let pkgs = sysreqs_rules::resolve_local("libxml2 (>= 2.9.0)", "alpine", "3.21");
        assert!(
            pkgs.iter().any(|p| p == "libxml2-dev"),
            "expected libxml2-dev in fallback output, got {pkgs:?}"
        );
    }
}
