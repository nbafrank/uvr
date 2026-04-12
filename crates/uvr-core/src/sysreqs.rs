use std::collections::HashMap;

use serde::Deserialize;
use tracing::{debug, warn};

use crate::error::Result;

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
) -> Result<Vec<SysReq>> {
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
            return Ok(vec![]);
        }
    };

    if !resp.status().is_success() {
        warn!("Posit sysreqs API returned {}", resp.status());
        return Ok(vec![]);
    }

    let body = resp.text().await.unwrap_or_default();
    let response: PpmSysreqsResponse = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => {
            warn!("Failed to parse Posit sysreqs API response: {e}");
            return Ok(vec![]);
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

    Ok(result)
}

/// Check which packages are missing on the system.
/// Uses `dpkg -s` on Debian/Ubuntu, `rpm -q` on Fedora/RHEL/SUSE.
/// If neither package manager is found, returns an empty list (skip check).
pub fn filter_missing(packages: &[SysReq]) -> Vec<&SysReq> {
    let (cmd, args): (&str, &[&str]) = if which::which("dpkg").is_ok() {
        ("dpkg", &["-s"])
    } else if which::which("rpm").is_ok() {
        ("rpm", &["-q"])
    } else {
        debug!("No supported package manager (dpkg/rpm) found, skipping sysreqs check");
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

/// Resolve and check system dependencies for a set of packages.
/// Returns a map of package name → list of missing system deps.
pub async fn check_system_deps(
    client: &reqwest::Client,
    package_names: &[String],
    distro: &str,
) -> HashMap<String, Vec<SysReq>> {
    let mut missing_by_pkg: HashMap<String, Vec<SysReq>> = HashMap::new();

    for pkg_name in package_names {
        match resolve_system_deps(client, pkg_name, distro).await {
            Ok(resolved) => {
                let missing = filter_missing(&resolved);
                if !missing.is_empty() {
                    missing_by_pkg.insert(pkg_name.clone(), missing.into_iter().cloned().collect());
                }
            }
            Err(e) => {
                warn!("Failed to resolve system deps for {pkg_name}: {e}");
            }
        }
    }

    missing_by_pkg
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
}
