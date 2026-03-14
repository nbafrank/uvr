use std::path::PathBuf;
use std::process::Command;

use crate::error::{Result, UvrError};
use crate::project::read_r_version_pin_from;
use crate::resolver::normalize_version;

/// Discovered R installation.
#[derive(Debug, Clone)]
pub struct RInstallation {
    pub binary: PathBuf,
    pub version: String,
    pub managed: bool, // true = installed by uvr
}

/// Find all R installations: managed ones + system R.
pub fn find_all() -> Vec<RInstallation> {
    let mut found = Vec::new();

    // Managed installations in ~/.uvr/r-versions/
    if let Some(home) = dirs::home_dir() {
        let versions_dir = home.join(".uvr").join("r-versions");
        if let Ok(entries) = std::fs::read_dir(&versions_dir) {
            for entry in entries.flatten() {
                let bin = entry.path().join("bin").join("R");
                if bin.exists() {
                    let version = entry.file_name().to_string_lossy().to_string();
                    found.push(RInstallation {
                        binary: bin,
                        version,
                        managed: true,
                    });
                }
            }
        }
    }

    // System R on PATH
    if let Ok(r_path) = which::which("R") {
        if let Some(version) = query_r_version(&r_path) {
            found.push(RInstallation {
                binary: r_path,
                version,
                managed: false,
            });
        }
    }

    found
}

/// Get the R binary to use.
///
/// Resolution order:
/// 1. `.r-version` file (exact pin, walked up from cwd)
/// 2. `version_constraint` from `uvr.toml` (semver requirement)
/// 3. Any managed installation, then system R
pub fn find_r_binary(version_constraint: Option<&str>) -> Result<PathBuf> {
    let installations = find_all();

    if installations.is_empty() {
        return Err(UvrError::RNotFound);
    }

    // 1. Honour .r-version exact pin
    let cwd = std::env::current_dir().unwrap_or_default();
    if let Some(pinned) = read_r_version_pin_from(&cwd) {
        return find_exact_version(&installations, &pinned);
    }

    // 2. Honour semver constraint from uvr.toml
    if let Some(constraint) = version_constraint {
        let req = crate::resolver::parse_version_req(constraint)?;
        for inst in &installations {
            let norm = normalize_version(&inst.version);
            if let Ok(ver) = semver::Version::parse(&norm) {
                if req.matches(&ver) {
                    return Ok(inst.binary.clone());
                }
            }
        }
        let installed = installations.first().map(|i| i.version.as_str()).unwrap_or("unknown");
        return Err(UvrError::RVersionUnsatisfied {
            constraint: constraint.to_string(),
            installed: installed.to_string(),
        });
    }

    // 3. Prefer managed installation, fall back to first system R.
    //    Use the already-fetched list — do NOT call find_all() again.
    let system_fallback = installations.iter().find(|i| !i.managed).map(|i| i.binary.clone());
    installations
        .into_iter()
        .find(|i| i.managed)
        .map(|i| i.binary)
        .or(system_fallback)
        .ok_or(UvrError::RNotFound)
}

fn find_exact_version(installations: &[RInstallation], version: &str) -> Result<PathBuf> {
    installations
        .iter()
        .find(|i| i.version == version)
        .map(|i| i.binary.clone())
        .ok_or_else(|| {
            UvrError::Other(format!(
                "R {version} is pinned in .r-version but not installed. Run: uvr r install {version}"
            ))
        })
}

pub fn query_r_version(binary: &std::path::Path) -> Option<String> {
    let output = Command::new(binary)
        .args(["--vanilla", "--slave", "-e", "cat(R.version$major, \".\", R.version$minor, sep='')"])
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}
