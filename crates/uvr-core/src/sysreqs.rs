use std::collections::HashMap;

use serde::Deserialize;
use tracing::{debug, warn};

use crate::error::Result;

/// A resolved system dependency with its apt package name.
#[derive(Debug, Clone)]
pub struct SysReq {
    /// The apt/deb package name, e.g. `"libxml2-dev"`.
    pub package: String,
}

/// Detect the Linux distribution from `/etc/os-release`.
/// Returns a string like `"ubuntu-22.04"` or `"debian-12"`.
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

/// Response structure from r-hub sysreqs API `GET /map/<string>`.
#[derive(Debug, Deserialize)]
struct SysReqsMapping {
    #[serde(default)]
    dependencies: Option<Vec<SysReqsDep>>,
}

#[derive(Debug, Deserialize)]
struct SysReqsDep {
    #[serde(default)]
    packages: Option<Vec<String>>,
}

/// Query the r-hub sysreqs API to map a `SystemRequirements` string to
/// OS-specific package names.
///
/// API: `GET https://sysreqs.r-hub.io/map/<sysreqs_string>?os=<distro>&os_release=<version>`
pub async fn resolve_system_deps(
    client: &reqwest::Client,
    system_requirements: &str,
    distro: &str,
) -> Result<Vec<SysReq>> {
    // Parse distro string "ubuntu-22.04" → os="ubuntu", os_release="22.04"
    let (os, os_release) = distro.split_once('-').unwrap_or((distro, ""));

    let url = format!(
        "https://sysreqs.r-hub.io/map/{}",
        urlencoding::encode(system_requirements)
    );

    debug!("Querying sysreqs API: {url} (os={os}, release={os_release})");

    let resp = client
        .get(&url)
        .query(&[("os", os), ("os_release", os_release)])
        .send()
        .await;

    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            warn!("sysreqs API request failed: {e}");
            return Ok(vec![]);
        }
    };

    if !resp.status().is_success() {
        warn!("sysreqs API returned {}", resp.status());
        return Ok(vec![]);
    }

    let body = resp.text().await.unwrap_or_default();
    let mappings: Vec<SysReqsMapping> = match serde_json::from_str(&body) {
        Ok(m) => m,
        Err(e) => {
            warn!("Failed to parse sysreqs API response: {e}");
            return Ok(vec![]);
        }
    };

    let mut result = Vec::new();
    for mapping in mappings {
        if let Some(deps) = mapping.dependencies {
            for dep in deps {
                if let Some(packages) = dep.packages {
                    for pkg in packages {
                        if !pkg.is_empty() {
                            result.push(SysReq { package: pkg });
                        }
                    }
                }
            }
        }
    }

    Ok(result)
}

/// Check which packages are missing on the system via `dpkg -s`.
pub fn filter_missing(packages: &[SysReq]) -> Vec<&SysReq> {
    packages
        .iter()
        .filter(|req| {
            let output = std::process::Command::new("dpkg")
                .args(["-s", &req.package])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            match output {
                Ok(status) => !status.success(),
                Err(_) => true, // dpkg not found — assume missing
            }
        })
        .collect()
}

/// Resolve and check system dependencies for a set of packages.
/// Returns a map of package name → list of missing system deps.
pub async fn check_system_deps(
    client: &reqwest::Client,
    packages: &[(String, String)], // (pkg_name, system_requirements)
    distro: &str,
) -> HashMap<String, Vec<SysReq>> {
    let mut missing_by_pkg: HashMap<String, Vec<SysReq>> = HashMap::new();

    for (pkg_name, sysreqs_str) in packages {
        match resolve_system_deps(client, sysreqs_str, distro).await {
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
