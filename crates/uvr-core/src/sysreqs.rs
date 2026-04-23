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

/// Detect the Linux distribution from `/etc/os-release`.
/// Returns `(id, version_id)` like `("ubuntu", "22.04")`.
pub fn detect_linux_distro() -> Option<String> {
    let content = std::fs::read_to_string("/etc/os-release").ok()?;
    let mut id = None;
    let mut version_id = None;

    for line in content.lines() {
        if let Some(val) = line.strip_prefix("ID=") {
            id = Some(val.trim_matches('"').to_string());
        } else if let Some(val) = line.strip_prefix("VERSION_ID=") {
            version_id = Some(val.trim_matches('"').to_string());
        }
    }

    let id = id?;
    let version_id = version_id?;
    Some(format!("{id}-{version_id}"))
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
            return Ok(SysReqLookup::Supported(vec![]));
        }
    };

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();

    if !status.is_success() {
        if is_unsupported_system_body(&body) {
            debug!("Posit sysreqs API reports {distribution} is unsupported");
            return Ok(SysReqLookup::UnsupportedDistro);
        }
        debug!("Posit sysreqs API returned {status} for {package_name}");
        return Ok(SysReqLookup::Supported(vec![]));
    }

    let response: PpmSysreqsResponse = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => {
            warn!("Failed to parse Posit sysreqs API response: {e}");
            return Ok(SysReqLookup::Supported(vec![]));
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
            Err(e) => {
                warn!("Failed to resolve system deps for {}: {e}", pkg.name);
            }
        }
    }

    if local_fallback {
        let (distribution, version) = distro.split_once('-').unwrap_or((distro, ""));
        for pkg in &packages[tail_start..] {
            let Some(sys_req_text) = pkg.system_requirements.as_deref() else {
                continue;
            };
            let resolved: Vec<SysReq> =
                sysreqs_rules::resolve_local(sys_req_text, distribution, version)
                    .into_iter()
                    .map(|package| SysReq { package })
                    .collect();
            if resolved.is_empty() {
                continue;
            }
            let missing = filter_missing(&resolved);
            if !missing.is_empty() {
                out.missing
                    .insert(pkg.name.clone(), missing.into_iter().cloned().collect());
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
        if cfg!(target_os = "linux") {
            let reqs = vec![SysReq {
                package: "uvr-nonexistent-pkg-12345".to_string(),
            }];
            let missing = filter_missing(&reqs);
            assert_eq!(missing.len(), 1);
        }
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
