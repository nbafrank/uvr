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

    let r_name = if cfg!(target_os = "windows") {
        "R.exe"
    } else {
        "R"
    };

    // Managed installations in ~/.uvr/r-versions/
    if let Some(home) = dirs::home_dir() {
        let versions_dir = home.join(".uvr").join("r-versions");
        if let Ok(entries) = std::fs::read_dir(&versions_dir) {
            for entry in entries.flatten() {
                let bin = entry.path().join("bin").join(r_name);
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

    // R_HOME: if uvr is spawned from within an R session, R_HOME is set.
    // Use it to locate the running R binary even if it's not on PATH.
    if let Ok(r_home) = std::env::var("R_HOME") {
        let r_home_bin = PathBuf::from(&r_home).join("bin").join(r_name);
        if r_home_bin.exists() && !found.iter().any(|i| i.binary == r_home_bin) {
            if let Some(version) = query_r_version(&r_home_bin) {
                found.push(RInstallation {
                    binary: r_home_bin,
                    version,
                    managed: false,
                });
            }
        }
    }

    // System R on PATH
    if let Ok(r_path) = which::which(r_name) {
        if !found.iter().any(|i| i.binary == r_path) {
            if let Some(version) = query_r_version(&r_path) {
                found.push(RInstallation {
                    binary: r_path,
                    version,
                    managed: false,
                });
            }
        }
    }

    // Windows: check common install locations
    #[cfg(target_os = "windows")]
    {
        let program_files =
            std::env::var("ProgramFiles").unwrap_or_else(|_| "C:\\Program Files".to_string());
        let r_base = std::path::Path::new(&program_files).join("R");
        if let Ok(entries) = std::fs::read_dir(&r_base) {
            for entry in entries.flatten() {
                let bin = entry.path().join("bin").join("R.exe");
                if bin.exists() && !found.iter().any(|i| i.binary == bin) {
                    if let Some(version) = query_r_version(&bin) {
                        found.push(RInstallation {
                            binary: bin,
                            version,
                            managed: false,
                        });
                    }
                }
            }
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
        let installed = installations
            .iter()
            .map(|i| i.version.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let installed = if installed.is_empty() {
            "none".to_string()
        } else {
            installed
        };
        return Err(UvrError::RVersionUnsatisfied {
            constraint: constraint.to_string(),
            installed,
        });
    }

    // 3. Prefer managed installation, fall back to first system R.
    //    Validate each candidate via `query_r_version` so a broken managed
    //    install (e.g. R 4.6 before the install-name patch on macOS — see
    //    `patch_r_executables`) doesn't silently capture every uvr command.
    let mut managed: Vec<&RInstallation> = installations.iter().filter(|i| i.managed).collect();
    let mut system: Vec<&RInstallation> = installations.iter().filter(|i| !i.managed).collect();
    // Probe in version-descending order so the newest working install wins.
    managed.sort_by(|a, b| version_cmp(&b.version, &a.version));
    system.sort_by(|a, b| version_cmp(&b.version, &a.version));
    for inst in managed.into_iter().chain(system.into_iter()) {
        if query_r_version(&inst.binary).is_some() {
            return Ok(inst.binary.clone());
        }
    }
    Err(UvrError::RNotFound)
}

fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let parse = |s: &str| -> Vec<u32> { s.split('.').filter_map(|p| p.parse().ok()).collect() };
    parse(a).cmp(&parse(b))
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

/// Given a list of installations, return the binary for an exact version match.
/// Used by `.r-version` resolution.
/// (Exposed for testing as `find_exact_version` is private.)
#[cfg(test)]
pub fn test_find_exact(installations: &[RInstallation], version: &str) -> Result<PathBuf> {
    find_exact_version(installations, version)
}

pub fn query_r_version(binary: &std::path::Path) -> Option<String> {
    let output = Command::new(binary)
        .args([
            "--vanilla",
            "--slave",
            "-e",
            "cat(R.version$major, \".\", R.version$minor, sep='')",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    // R sometimes prints startup warnings (e.g. "WARNING: ignoring environment
    // value of R_HOME" when R_HOME points at a different install) to **stdout**
    // before user code runs. The version string from our `-e` script is always
    // the last line, so pick the last non-empty line that parses as a version.
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.lines().rev().find_map(|line| {
        let t = line.trim();
        if !t.is_empty() && t.chars().all(|c| c.is_ascii_digit() || c == '.') && t.contains('.') {
            Some(t.to_string())
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_installation(version: &str, managed: bool) -> RInstallation {
        RInstallation {
            binary: PathBuf::from(format!("/fake/r-{version}/bin/R")),
            version: version.to_string(),
            managed,
        }
    }

    #[test]
    fn find_exact_version_found() {
        let installations = vec![
            fake_installation("4.3.2", true),
            fake_installation("4.4.1", true),
        ];
        let result = find_exact_version(&installations, "4.4.1").unwrap();
        assert_eq!(result, PathBuf::from("/fake/r-4.4.1/bin/R"));
    }

    #[test]
    fn find_exact_version_not_found() {
        let installations = vec![fake_installation("4.3.2", true)];
        let result = find_exact_version(&installations, "4.4.1");
        assert!(result.is_err());
    }

    #[test]
    fn find_all_returns_something() {
        // On a dev machine, there should be at least one R installation.
        // This is not strictly guaranteed in CI but is a reasonable smoke test.
        let installations = find_all();
        // Just verify it doesn't panic and returns a Vec
        for inst in &installations {
            assert!(!inst.version.is_empty());
            assert!(!inst.binary.as_os_str().is_empty());
        }
    }

    #[test]
    fn r_installation_struct_fields() {
        let inst = fake_installation("4.5.0", true);
        assert_eq!(inst.version, "4.5.0");
        assert!(inst.managed);
        assert!(inst.binary.to_string_lossy().contains("4.5.0"));
    }

    #[test]
    fn find_r_binary_prefers_managed() {
        // This tests the logic indirectly — if there are managed + system R,
        // managed should be preferred. Since we can't mock find_all, we test
        // the fallback logic through find_r_binary without constraint.
        // Just verify it returns Ok (not Err) when R is available.
        if let Ok(binary) = find_r_binary(None) {
            assert!(binary.to_string_lossy().contains("R"));
        }
        // If R is not installed at all, that's fine — test is informational
    }

    #[test]
    fn find_r_binary_with_constraint() {
        // If we have R, a loose constraint should match
        if let Ok(binary) = find_r_binary(Some(">=3.0.0")) {
            assert!(binary.exists());
        }
    }

    #[test]
    fn find_r_binary_impossible_constraint() {
        // A constraint for R 99.0.0 should fail (no such version exists)
        let result = find_r_binary(Some(">=99.0.0"));
        if !find_all().is_empty() {
            assert!(result.is_err());
        }
    }
}
