use anyhow::{Context, Result};
use clap::ValueEnum;
use serde::Serialize;
use std::collections::HashMap;

use uvr_core::lockfile::{Lockfile, PackageSource};
use uvr_core::project::Project;

use crate::ui;
use crate::ui::palette;

pub fn run(format: ExportFormat, output: Option<String>) -> Result<()> {
    let project = Project::find_cwd().context("Not inside a uvr project")?;
    let lockfile = project
        .load_lockfile()
        .context("Failed to read uvr.lock")?
        .ok_or_else(|| anyhow::anyhow!("No lockfile found. Run `uvr lock` first."))?;

    let content = match format {
        ExportFormat::Renv => export_renv(&lockfile)?,
    };

    match output {
        Some(path) => {
            std::fs::write(&path, &content).with_context(|| format!("Failed to write {path}"))?;
            ui::success(format!(
                "Exported {} package(s) to {}",
                lockfile.packages.len(),
                palette::pkg(&path),
            ));
        }
        None => {
            print!("{content}");
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ExportFormat {
    /// Export to renv.lock format
    Renv,
}

/// Export to renv.lock format.
///
/// renv.lock is a JSON file with structure:
/// ```json
/// {
///   "R": { "Version": "4.4.2", "Repositories": [...] },
///   "Packages": {
///     "ggplot2": { "Package": "ggplot2", "Version": "3.5.1", "Source": "Repository", "Repository": "CRAN" },
///     ...
///   }
/// }
/// ```
fn export_renv(lockfile: &Lockfile) -> Result<String> {
    let mut repositories = vec![RenvRepo {
        name: "CRAN".into(),
        url: "https://cloud.r-project.org".into(),
    }];

    // Emit the Bioconductor pin only when the lockfile actually contains
    // Bioconductor packages *and* records the release. renv restores
    // `Source: "Bioconductor"` records via BiocManager repositories that it
    // reconstructs from the top-level `Bioconductor.Version`; without it renv
    // falls back to the installed BiocManager's default release, which can
    // restore from the wrong Bioc version (or fail for older pinned ones).
    let has_bioc = lockfile
        .packages
        .iter()
        .any(|p| matches!(p.source, PackageSource::Bioconductor));
    let bioconductor = match (has_bioc, lockfile.r.bioc_version.as_deref()) {
        (true, Some(version)) => {
            // Match the repository set renv itself writes for a Bioconductor
            // project, all derived from the pinned release.
            repositories.extend(bioc_repositories(version));
            Some(RenvBioconductor {
                version: version.to_string(),
            })
        }
        _ => None,
    };

    let r_section = RenvR {
        version: lockfile.r.version.clone(),
        repositories,
    };

    let mut packages = HashMap::new();
    for pkg in &lockfile.packages {
        let (source, repository) = export_source_and_repository(&pkg.source);

        let version = pkg
            .raw_version
            .as_deref()
            .unwrap_or(&pkg.version)
            .to_string();

        let requirements = if pkg.requires.is_empty() {
            None
        } else {
            Some(pkg.requires.clone())
        };

        let remote_info = match &pkg.source {
            PackageSource::GitHub => pkg.url.as_ref().and_then(|u| parse_github_remote(u)),
            _ => None,
        };

        let forgejo_info = match &pkg.source {
            PackageSource::Forgejo { .. } => pkg.url.as_ref().and_then(|u| parse_forgejo_remote(u)),
            _ => None,
        };

        let entry = RenvPackage {
            package: pkg.name.clone(),
            version,
            source,
            repository,
            requirements,
            remote_username: remote_info
                .as_ref()
                .map(|(user, _, _)| user.clone())
                .or_else(|| forgejo_info.as_ref().map(|(_, owner, _, _)| owner.clone())),
            remote_repo: remote_info
                .as_ref()
                .map(|(_, repo, _)| repo.clone())
                .or_else(|| forgejo_info.as_ref().map(|(_, _, repo, _)| repo.clone())),
            remote_ref: remote_info
                .as_ref()
                .and_then(|(_, _, r)| r.clone())
                .or_else(|| forgejo_info.as_ref().map(|(_, _, _, sha)| sha.clone())),
            remote_url: forgejo_info
                .as_ref()
                .map(|(host, owner, repo, _)| format!("https://{host}/{owner}/{repo}")),
            remote_type: forgejo_info.as_ref().map(|_| "git2r".to_string()),
        };
        packages.insert(pkg.name.clone(), entry);
    }

    let renv_lock = RenvLock {
        r: r_section,
        bioconductor,
        packages,
    };

    serde_json::to_string_pretty(&renv_lock).context("Failed to serialize renv.lock")
}

/// The Bioconductor repository set renv writes into a lockfile's
/// `Repositories`, all pinned to `version` (e.g. "3.18"). Mirrors what renv
/// derives from BiocManager for a Bioconductor project so a restore resolves
/// against the same release the lockfile was captured on.
fn bioc_repositories(version: &str) -> Vec<RenvRepo> {
    [
        ("BioCsoft", "bioc"),
        ("BioCann", "data/annotation"),
        ("BioCexp", "data/experiment"),
        ("BioCworkflows", "workflows"),
        ("BioCbooks", "books"),
    ]
    .into_iter()
    .map(|(name, path)| RenvRepo {
        name: name.into(),
        url: format!("https://bioconductor.org/packages/{version}/{path}"),
    })
    .collect()
}

/// Map a `PackageSource` to renv's (Source, Repository) string pair for
/// the renv.lock export. Extracted from the inline match in
/// `export_renv` so we can unit-test the Forgejo mapping without
/// constructing a full `Lockfile`. Forgejo maps to renv's `Source: Git`
/// (the git2r-backed remote) — renv has no Forgejo-aware type, so a
/// generic Git mapping with `RemoteUrl` set is the most importable shape.
fn export_source_and_repository(src: &PackageSource) -> (String, Option<String>) {
    match src {
        PackageSource::Cran => ("Repository".to_string(), Some("CRAN".to_string())),
        PackageSource::Bioconductor => ("Bioconductor".to_string(), None),
        PackageSource::GitHub => ("GitHub".to_string(), None),
        PackageSource::Forgejo { .. } => ("Git".to_string(), None),
        PackageSource::Local => ("Local".to_string(), None),
        PackageSource::Custom { name } => ("Repository".to_string(), Some(name.clone())),
    }
}

fn parse_github_remote(url: &str) -> Option<(String, String, Option<String>)> {
    // URL like "https://api.github.com/repos/user/repo/tarball/ref"
    // or "user/repo"
    if url.contains("github.com") {
        let parts: Vec<&str> = url.split('/').collect();
        // Find "repos" index or parse user/repo from the URL
        if let Some(pos) = parts.iter().position(|&p| p == "repos") {
            let user = parts.get(pos + 1)?.to_string();
            let repo = parts.get(pos + 2)?.to_string();
            let git_ref = parts.get(pos + 4).map(|s| s.to_string());
            return Some((user, repo, git_ref));
        }
    }
    None
}

/// Parse a Forgejo archive URL into (host, owner, repo, sha). Returns
/// `None` if the URL doesn't match the expected
/// `/api/v1/repos/{owner}/{repo}/archive/{sha}.tar.gz` shape.
fn parse_forgejo_remote(url: &str) -> Option<(String, String, String, String)> {
    let parts: Vec<&str> = url.split('/').collect();
    let api_idx = parts.iter().position(|s| *s == "api")?;
    if parts.get(api_idx + 1).copied()? != "v1" {
        return None;
    }
    if parts.get(api_idx + 2).copied()? != "repos" {
        return None;
    }
    let owner = parts.get(api_idx + 3)?.to_string();
    let repo = parts.get(api_idx + 4)?.to_string();
    if parts.get(api_idx + 5).copied()? != "archive" {
        return None;
    }
    let last = parts.get(api_idx + 6)?.to_string();
    let sha = last.strip_suffix(".tar.gz").unwrap_or(&last).to_string();
    // Host is the path segment immediately before `api` — derive it from
    // api_idx so it stays coupled if the URL prefix ever changes (#106).
    let host = parts.get(api_idx.checked_sub(1)?).copied()?;
    if host.is_empty() {
        return None;
    }
    Some((host.to_string(), owner, repo, sha))
}

#[derive(Serialize)]
struct RenvLock {
    #[serde(rename = "R")]
    r: RenvR,
    #[serde(rename = "Bioconductor", skip_serializing_if = "Option::is_none")]
    bioconductor: Option<RenvBioconductor>,
    #[serde(rename = "Packages")]
    packages: HashMap<String, RenvPackage>,
}

#[derive(Serialize)]
struct RenvBioconductor {
    #[serde(rename = "Version")]
    version: String,
}

#[derive(Serialize)]
struct RenvR {
    #[serde(rename = "Version")]
    version: String,
    #[serde(rename = "Repositories")]
    repositories: Vec<RenvRepo>,
}

#[derive(Serialize)]
struct RenvRepo {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "URL")]
    url: String,
}

#[derive(Serialize)]
struct RenvPackage {
    #[serde(rename = "Package")]
    package: String,
    #[serde(rename = "Version")]
    version: String,
    #[serde(rename = "Source")]
    source: String,
    #[serde(rename = "Repository", skip_serializing_if = "Option::is_none")]
    repository: Option<String>,
    #[serde(rename = "Requirements", skip_serializing_if = "Option::is_none")]
    requirements: Option<Vec<String>>,
    #[serde(rename = "RemoteUsername", skip_serializing_if = "Option::is_none")]
    remote_username: Option<String>,
    #[serde(rename = "RemoteRepo", skip_serializing_if = "Option::is_none")]
    remote_repo: Option<String>,
    #[serde(rename = "RemoteRef", skip_serializing_if = "Option::is_none")]
    remote_ref: Option<String>,
    #[serde(rename = "RemoteUrl", skip_serializing_if = "Option::is_none")]
    remote_url: Option<String>,
    #[serde(rename = "RemoteType", skip_serializing_if = "Option::is_none")]
    remote_type: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_github_remote_api_url() {
        let url = "https://api.github.com/repos/tidyverse/ggplot2/tarball/main";
        let (user, repo, git_ref) = parse_github_remote(url).unwrap();
        assert_eq!(user, "tidyverse");
        assert_eq!(repo, "ggplot2");
        assert_eq!(git_ref, Some("main".to_string()));
    }

    #[test]
    fn parse_github_remote_no_ref() {
        let url = "https://api.github.com/repos/user/pkg/tarball";
        let (user, repo, git_ref) = parse_github_remote(url).unwrap();
        assert_eq!(user, "user");
        assert_eq!(repo, "pkg");
        assert_eq!(git_ref, None);
    }

    #[test]
    fn parse_github_remote_non_github() {
        let url = "https://cran.r-project.org/src/contrib/ggplot2_3.5.1.tar.gz";
        assert!(parse_github_remote(url).is_none());
    }

    #[test]
    fn parse_github_remote_no_repos() {
        let url = "https://github.com/user/repo";
        // No "repos" segment → None
        assert!(parse_github_remote(url).is_none());
    }

    #[test]
    fn export_renv_basic() {
        use uvr_core::lockfile::{LockedPackage, Lockfile, RVersionPin};

        let lockfile = Lockfile {
            r: RVersionPin {
                version: "4.4.2".to_string(),
                bioc_version: None,
            },
            packages: vec![
                LockedPackage {
                    name: "jsonlite".to_string(),
                    version: "1.8.8".to_string(),
                    raw_version: None,
                    source: PackageSource::Cran,
                    checksum: None,
                    requires: vec![],
                    url: None,
                    system_requirements: None,
                    dev: false,
                },
                LockedPackage {
                    name: "DESeq2".to_string(),
                    version: "1.42.0".to_string(),
                    raw_version: None,
                    source: PackageSource::Bioconductor,
                    checksum: None,
                    requires: vec!["BiocGenerics".to_string()],
                    url: None,
                    system_requirements: None,
                    dev: false,
                },
            ],
        };

        let json = export_renv(&lockfile).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["R"]["Version"], "4.4.2");
        assert_eq!(parsed["Packages"]["jsonlite"]["Source"], "Repository");
        assert_eq!(parsed["Packages"]["jsonlite"]["Repository"], "CRAN");
        assert_eq!(parsed["Packages"]["DESeq2"]["Source"], "Bioconductor");
        // Bioconductor packages don't have Repository field
        assert!(parsed["Packages"]["DESeq2"]["Repository"].is_null());
        // bioc_version is None here, so no Bioconductor pin can be emitted.
        assert!(parsed.get("Bioconductor").is_none());
        // Only the CRAN repo — no Bioc repos without a pinned release.
        let repos = parsed["R"]["Repositories"].as_array().unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0]["Name"], "CRAN");
    }

    #[test]
    fn export_renv_bioc_package_with_version_emits_section() {
        use uvr_core::lockfile::{LockedPackage, Lockfile, RVersionPin};

        let lockfile = Lockfile {
            r: RVersionPin {
                version: "4.4.2".to_string(),
                bioc_version: Some("3.18".to_string()),
            },
            packages: vec![
                LockedPackage {
                    name: "jsonlite".to_string(),
                    version: "1.8.8".to_string(),
                    raw_version: None,
                    source: PackageSource::Cran,
                    checksum: None,
                    requires: vec![],
                    url: None,
                    system_requirements: None,
                    dev: false,
                },
                LockedPackage {
                    name: "DESeq2".to_string(),
                    version: "1.42.0".to_string(),
                    raw_version: None,
                    source: PackageSource::Bioconductor,
                    checksum: None,
                    requires: vec![],
                    url: None,
                    system_requirements: None,
                    dev: false,
                },
            ],
        };

        let json = export_renv(&lockfile).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        // Top-level Bioconductor pin present with the lockfile's release.
        assert_eq!(parsed["Bioconductor"]["Version"], "3.18");

        // Repositories include CRAN plus the pinned Bioc repos.
        let repos = parsed["R"]["Repositories"].as_array().unwrap();
        let by_name: std::collections::HashMap<&str, &str> = repos
            .iter()
            .map(|r| (r["Name"].as_str().unwrap(), r["URL"].as_str().unwrap()))
            .collect();
        assert_eq!(by_name["CRAN"], "https://cloud.r-project.org");
        assert_eq!(
            by_name["BioCsoft"],
            "https://bioconductor.org/packages/3.18/bioc"
        );
        assert_eq!(
            by_name["BioCann"],
            "https://bioconductor.org/packages/3.18/data/annotation"
        );
        assert_eq!(
            by_name["BioCexp"],
            "https://bioconductor.org/packages/3.18/data/experiment"
        );
        assert_eq!(
            by_name["BioCworkflows"],
            "https://bioconductor.org/packages/3.18/workflows"
        );
    }

    #[test]
    fn export_renv_no_bioc_package_omits_section_even_with_version() {
        use uvr_core::lockfile::{LockedPackage, Lockfile, RVersionPin};

        // bioc_version is set, but there are no Bioconductor packages, so no
        // spurious Bioconductor section or Bioc repos should be emitted.
        let lockfile = Lockfile {
            r: RVersionPin {
                version: "4.4.2".to_string(),
                bioc_version: Some("3.18".to_string()),
            },
            packages: vec![LockedPackage {
                name: "jsonlite".to_string(),
                version: "1.8.8".to_string(),
                raw_version: None,
                source: PackageSource::Cran,
                checksum: None,
                requires: vec![],
                url: None,
                system_requirements: None,
                dev: false,
            }],
        };

        let json = export_renv(&lockfile).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert!(parsed.get("Bioconductor").is_none());
        let repos = parsed["R"]["Repositories"].as_array().unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0]["Name"], "CRAN");
    }

    #[test]
    fn export_renv_github_package() {
        use uvr_core::lockfile::{LockedPackage, Lockfile, RVersionPin};

        let lockfile = Lockfile {
            r: RVersionPin {
                version: "4.4.2".to_string(),
                bioc_version: None,
            },
            packages: vec![LockedPackage {
                name: "mypkg".to_string(),
                version: "0.1.0".to_string(),
                raw_version: None,
                source: PackageSource::GitHub,
                checksum: None,
                requires: vec![],
                url: Some("https://api.github.com/repos/user/mypkg/tarball/main".to_string()),
                system_requirements: None,
                dev: false,
            }],
        };

        let json = export_renv(&lockfile).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["Packages"]["mypkg"]["Source"], "GitHub");
        assert_eq!(parsed["Packages"]["mypkg"]["RemoteUsername"], "user");
        assert_eq!(parsed["Packages"]["mypkg"]["RemoteRepo"], "mypkg");
        assert_eq!(parsed["Packages"]["mypkg"]["RemoteRef"], "main");
    }

    #[test]
    fn export_forgejo_package() {
        use uvr_core::lockfile::{LockedPackage, PackageSource};

        let pkg = LockedPackage {
            name: "mypkg".into(),
            version: "0.1.0".into(),
            source: PackageSource::Forgejo {
                host: "codefloe.com".into(),
            },
            checksum: Some("git:abc123".into()),
            url: Some("https://codefloe.com/api/v1/repos/pat-s/mypkg/archive/abc123.tar.gz".into()),
            requires: vec![],
            raw_version: None,
            system_requirements: None,
            dev: false,
        };
        let (source, repository) = export_source_and_repository(&pkg.source);
        assert_eq!(source, "Git");
        assert_eq!(repository, None);
    }

    #[test]
    fn export_renv_forgejo_package_emits_remote_url_and_type() {
        use uvr_core::lockfile::{LockedPackage, Lockfile, PackageSource, RVersionPin};

        let lockfile = Lockfile {
            r: RVersionPin {
                version: "4.4.2".to_string(),
                bioc_version: None,
            },
            packages: vec![LockedPackage {
                name: "mypkg".to_string(),
                version: "0.1.0".to_string(),
                raw_version: None,
                source: PackageSource::Forgejo {
                    host: "codefloe.com".to_string(),
                },
                checksum: Some("git:abc123".to_string()),
                requires: vec![],
                url: Some(
                    "https://codefloe.com/api/v1/repos/pat-s/mypkg/archive/abc123.tar.gz"
                        .to_string(),
                ),
                system_requirements: None,
                dev: false,
            }],
        };

        let json = export_renv(&lockfile).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["Packages"]["mypkg"]["Source"], "Git");
        assert_eq!(parsed["Packages"]["mypkg"]["RemoteType"], "git2r");
        assert_eq!(
            parsed["Packages"]["mypkg"]["RemoteUrl"],
            "https://codefloe.com/pat-s/mypkg"
        );
        assert_eq!(parsed["Packages"]["mypkg"]["RemoteUsername"], "pat-s");
        assert_eq!(parsed["Packages"]["mypkg"]["RemoteRepo"], "mypkg");
        assert_eq!(parsed["Packages"]["mypkg"]["RemoteRef"], "abc123");
    }

    #[test]
    fn parse_forgejo_remote_archive_url() {
        let url = "https://codefloe.com/api/v1/repos/pat-s/mypkg/archive/abc123.tar.gz";
        let (host, owner, repo, sha) = parse_forgejo_remote(url).unwrap();
        assert_eq!(host, "codefloe.com");
        assert_eq!(owner, "pat-s");
        assert_eq!(repo, "mypkg");
        assert_eq!(sha, "abc123");
    }

    #[test]
    fn export_renv_uses_raw_version() {
        use uvr_core::lockfile::{LockedPackage, Lockfile, RVersionPin};

        let lockfile = Lockfile {
            r: RVersionPin {
                version: "4.4.2".to_string(),
                bioc_version: None,
            },
            packages: vec![LockedPackage {
                name: "scales".to_string(),
                version: "1.1.3".to_string(),
                raw_version: Some("1.1-3".to_string()),
                source: PackageSource::Cran,
                checksum: None,
                requires: vec![],
                url: None,
                system_requirements: None,
                dev: false,
            }],
        };

        let json = export_renv(&lockfile).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        // Should use raw_version "1.1-3" not normalized "1.1.3"
        assert_eq!(parsed["Packages"]["scales"]["Version"], "1.1-3");
    }
}
