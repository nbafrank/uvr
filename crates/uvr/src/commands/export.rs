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

        // Never export a nested package after losing its exact source identity.
        if pkg.subdirectory.is_some() {
            uvr_core::registry::github::validate_nested_lock_entry(pkg)
                .with_context(|| format!("Cannot export package '{}'", pkg.name))?;
        }

        let remote_info = match &pkg.source {
            PackageSource::GitHub => pkg.url.as_ref().and_then(|u| parse_github_remote(u)),
            _ => None,
        };

        let forgejo_info = match &pkg.source {
            PackageSource::Forgejo { .. } => pkg.url.as_ref().and_then(|u| parse_forgejo_remote(u)),
            _ => None,
        };

        let gitlab_info = match &pkg.source {
            PackageSource::Gitlab { .. } => pkg.url.as_ref().and_then(|u| parse_gitlab_remote(u)),
            _ => None,
        };

        // gitlab's "owner" is a full (possibly nested) namespace path
        // rather than a single segment — same field, different shape.
        let git_host_info = forgejo_info.as_ref().or(gitlab_info.as_ref());
        // renv restores `RemoteType: url` records via `remotes::install_url`.
        let url_source = pkg.source == PackageSource::Url;

        // A `git::` package (#190), as renv itself records a git remote.
        let git_url = match &pkg.source {
            PackageSource::Git { url } => Some(url.clone()),
            _ => None,
        };

        let remote_sha = if pkg.subdirectory.is_some() || git_url.is_some() {
            pkg.checksum
                .as_deref()
                .and_then(|c| c.strip_prefix("git:"))
                .map(str::to_string)
        } else {
            None
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
                .or_else(|| git_host_info.map(|(_, owner, _, _)| owner.clone())),
            remote_repo: remote_info
                .as_ref()
                .map(|(_, repo, _)| repo.clone())
                .or_else(|| git_host_info.map(|(_, _, repo, _)| repo.clone())),
            remote_ref: remote_info
                .as_ref()
                .and_then(|(_, _, r)| r.clone())
                .or_else(|| git_host_info.map(|(_, _, _, sha)| sha.clone())),
            remote_sha,
            remote_subdir: pkg.subdirectory.clone(),
            remote_type: match (&git_url, git_host_info) {
                (Some(_), _) => Some("git".to_string()),
                (None, Some(_)) => Some("git2r".to_string()),
                (None, None) => url_source.then(|| "url".to_string()),
            },
            remote_url: git_url
                .or_else(|| {
                    git_host_info
                        .map(|(host, owner, repo, _)| format!("https://{host}/{owner}/{repo}"))
                })
                .or_else(|| pkg.url.clone().filter(|_| url_source)),
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
/// `export_renv` so we can unit-test the Forgejo/Gitlab mapping without
/// constructing a full `Lockfile`. Forgejo and Gitlab both map to renv's
/// `Source: Git` (the git2r-backed remote) — renv has no Forgejo- or
/// Gitlab-aware type, so a generic Git mapping with `RemoteUrl` set is the
/// most importable shape. The two forges stay distinguishable downstream:
/// `RemoteUrl`/`RemoteUsername`/`RemoteRepo`/`RemoteRef` (built separately
/// from each package's own archive URL) still carry the real host/owner/
/// repo/sha.
fn export_source_and_repository(src: &PackageSource) -> (String, Option<String>) {
    match src {
        PackageSource::Cran => ("Repository".to_string(), Some("CRAN".to_string())),
        PackageSource::Bioconductor => ("Bioconductor".to_string(), None),
        PackageSource::GitHub => ("GitHub".to_string(), None),
        PackageSource::Forgejo { .. } => ("Git".to_string(), None),
        PackageSource::Gitlab { .. } => ("Git".to_string(), None),
        // renv's own spelling for a git remote.
        PackageSource::Git { .. } => ("git".to_string(), None),
        PackageSource::Url => ("URL".to_string(), None),
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

/// Parse a GitLab archive URL into (host, namespace_path, project, sha).
/// Returns `None` if the URL doesn't match the expected
/// `/api/v4/projects/{id}/repository/archive.tar.gz?sha={sha}` shape.
///
/// `namespace_path` is the decoded group/subgroup path (everything before
/// the final project segment) — GitLab's nested groups mean this, unlike
/// Forgejo/GitHub's single-segment owner, can itself contain slashes.
/// `{id}` is percent-encoded (`urlencoding::encode` turns `/` into `%2F`
/// when the URL is built); decoding it back is a plain string replace
/// rather than a full percent-decode, since the only escaped character a
/// project path can contain is that separator — every segment is already
/// restricted to `[alnum].-_` by `parse_gitlab_parts`, all of which sit
/// outside the escaped range.
fn parse_gitlab_remote(url: &str) -> Option<(String, String, String, String)> {
    let (base, query) = url.split_once('?')?;
    let parts: Vec<&str> = base.split('/').collect();
    let api_idx = parts.iter().position(|s| *s == "api")?;
    if parts.get(api_idx + 1).copied()? != "v4" {
        return None;
    }
    if parts.get(api_idx + 2).copied()? != "projects" {
        return None;
    }
    let encoded_id = parts.get(api_idx + 3)?;
    if parts.get(api_idx + 4).copied()? != "repository" {
        return None;
    }
    if parts.get(api_idx + 5).copied()? != "archive.tar.gz" {
        return None;
    }
    let decoded_id = encoded_id.replace("%2F", "/").replace("%2f", "/");
    let (namespace_path, project) = decoded_id.rsplit_once('/')?;
    if namespace_path.is_empty() || project.is_empty() {
        return None;
    }
    let host = parts.get(api_idx.checked_sub(1)?).copied()?;
    if host.is_empty() {
        return None;
    }
    let sha = query.strip_prefix("sha=")?;
    if sha.is_empty() {
        return None;
    }
    Some((
        host.to_string(),
        namespace_path.to_string(),
        project.to_string(),
        sha.to_string(),
    ))
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
    #[serde(rename = "RemoteSha", skip_serializing_if = "Option::is_none")]
    remote_sha: Option<String>,
    #[serde(rename = "RemoteSubdir", skip_serializing_if = "Option::is_none")]
    remote_subdir: Option<String>,
    #[serde(rename = "RemoteUrl", skip_serializing_if = "Option::is_none")]
    remote_url: Option<String>,
    #[serde(rename = "RemoteType", skip_serializing_if = "Option::is_none")]
    remote_type: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use uvr_core::lockfile::LockedPackage;

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
            manifest_fingerprint: None,
            r: RVersionPin {
                version: "4.4.2".to_string(),
                bioc_version: None,
                resolved_as_of: None,
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
                    subdirectory: None,
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
                    subdirectory: None,
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
            manifest_fingerprint: None,
            r: RVersionPin {
                version: "4.4.2".to_string(),
                bioc_version: Some("3.18".to_string()),
                resolved_as_of: None,
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
                    subdirectory: None,
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
                    subdirectory: None,
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
            manifest_fingerprint: None,
            r: RVersionPin {
                version: "4.4.2".to_string(),
                bioc_version: Some("3.18".to_string()),
                resolved_as_of: None,
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
                subdirectory: None,
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
            manifest_fingerprint: None,
            r: RVersionPin {
                version: "4.4.2".to_string(),
                bioc_version: None,
                resolved_as_of: None,
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
                subdirectory: None,
            }],
        };

        let json = export_renv(&lockfile).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["Packages"]["mypkg"]["Source"], "GitHub");
        assert_eq!(parsed["Packages"]["mypkg"]["RemoteUsername"], "user");
        assert_eq!(parsed["Packages"]["mypkg"]["RemoteRepo"], "mypkg");
        assert_eq!(parsed["Packages"]["mypkg"]["RemoteRef"], "main");
        let entry = parsed["Packages"]["mypkg"].as_object().unwrap();
        assert!(!entry.contains_key("RemoteSha"));
        assert!(!entry.contains_key("RemoteSubdir"));
    }

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    fn nested_locked(url: &str, checksum: &str, subdirectory: Option<&str>) -> LockedPackage {
        LockedPackage {
            name: "nested".to_string(),
            version: "0.1.0".to_string(),
            raw_version: None,
            source: PackageSource::GitHub,
            checksum: Some(checksum.to_string()),
            requires: vec![],
            url: Some(url.to_string()),
            system_requirements: None,
            dev: false,
            subdirectory: subdirectory.map(str::to_string),
        }
    }

    fn single_package_lockfile(pkg: LockedPackage) -> Lockfile {
        use uvr_core::lockfile::RVersionPin;
        Lockfile {
            manifest_fingerprint: None,
            r: RVersionPin {
                version: "4.4.2".to_string(),
                bioc_version: None,
                resolved_as_of: None,
            },
            packages: vec![pkg],
        }
    }

    #[test]
    fn export_renv_github_subdirectory_package() {
        let lockfile = single_package_lockfile(nested_locked(
            &format!("https://api.github.com/repos/owner/monorepo/tarball/{COMMIT}"),
            &format!("git:{COMMIT}"),
            Some("pkgs/nested"),
        ));

        let json = export_renv(&lockfile).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let entry = &parsed["Packages"]["nested"];
        assert_eq!(entry["Source"], "GitHub");
        assert_eq!(entry["RemoteUsername"], "owner");
        assert_eq!(entry["RemoteRepo"], "monorepo");
        assert_eq!(entry["RemoteSubdir"], "pkgs/nested");
        assert_eq!(entry["RemoteSha"], COMMIT);
        assert_eq!(entry["RemoteRef"], COMMIT);
    }

    #[test]
    fn export_renv_url_package() {
        // #189: renv's own shape for a `remotes::install_url` package.
        let url = "https://example.org/tpkg_1.2-0.tar.gz";
        let lockfile = single_package_lockfile(LockedPackage {
            name: "tpkg".to_string(),
            version: "1.2.0".to_string(),
            raw_version: Some("1.2-0".to_string()),
            source: PackageSource::Url,
            checksum: Some(format!("sha256:{}", "ab".repeat(32))),
            requires: vec![],
            url: Some(url.to_string()),
            system_requirements: None,
            dev: false,
            subdirectory: None,
        });

        let json = export_renv(&lockfile).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let entry = &parsed["Packages"]["tpkg"];
        assert_eq!(entry["Source"], "URL");
        assert_eq!(entry["Version"], "1.2-0");
        assert_eq!(entry["RemoteType"], "url");
        assert_eq!(entry["RemoteUrl"], url);
        let entry = entry.as_object().unwrap();
        assert!(!entry.contains_key("Repository"));
        assert!(!entry.contains_key("RemoteUsername"));
    }

    #[test]
    fn export_renv_fails_for_invalid_nested_identity() {
        let good_url = format!("https://api.github.com/repos/owner/monorepo/tarball/{COMMIT}");
        let good_checksum = format!("git:{COMMIT}");
        let mut wrong_source = nested_locked(&good_url, &good_checksum, Some("pkgs/nested"));
        wrong_source.source = PackageSource::Cran;
        let cases = [
            wrong_source,
            nested_locked(&good_url, &good_checksum, Some("../escape")),
            nested_locked(&good_url, "git:main", Some("pkgs/nested")),
            nested_locked(
                "https://api.github.com/repos/owner/monorepo/tarball/main",
                &good_checksum,
                Some("pkgs/nested"),
            ),
        ];
        for pkg in cases {
            assert!(
                export_renv(&single_package_lockfile(pkg.clone())).is_err(),
                "should fail rather than downgrade {:?} / {:?} / {:?}",
                pkg.url,
                pkg.checksum,
                pkg.subdirectory
            );
        }
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
            subdirectory: None,
        };
        let (source, repository) = export_source_and_repository(&pkg.source);
        assert_eq!(source, "Git");
        assert_eq!(repository, None);
    }

    #[test]
    fn export_renv_forgejo_package_emits_remote_url_and_type() {
        use uvr_core::lockfile::{LockedPackage, Lockfile, PackageSource, RVersionPin};

        let lockfile = Lockfile {
            manifest_fingerprint: None,
            r: RVersionPin {
                version: "4.4.2".to_string(),
                bioc_version: None,
                resolved_as_of: None,
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
                subdirectory: None,
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
    fn export_gitlab_package() {
        use uvr_core::lockfile::{LockedPackage, PackageSource};

        let pkg = LockedPackage {
            name: "mypkg".into(),
            version: "0.1.0".into(),
            source: PackageSource::Gitlab {
                host: "gitlab.com".into(),
            },
            checksum: Some("git:abc123".into()),
            url: Some(
                "https://gitlab.com/api/v4/projects/my-group%2Fmypkg/repository/archive.tar.gz?sha=abc123"
                    .into(),
            ),
            requires: vec![],
            raw_version: None,
            system_requirements: None,
            dev: false,
            subdirectory: None,
        };
        let (source, repository) = export_source_and_repository(&pkg.source);
        assert_eq!(source, "Git");
        assert_eq!(repository, None);
    }

    #[test]
    fn export_renv_gitlab_package_emits_remote_url_and_type() {
        use uvr_core::lockfile::{LockedPackage, Lockfile, PackageSource, RVersionPin};

        let lockfile = Lockfile {
            manifest_fingerprint: None,
            r: RVersionPin {
                version: "4.4.2".to_string(),
                bioc_version: None,
                resolved_as_of: None,
            },
            packages: vec![LockedPackage {
                name: "mypkg".to_string(),
                version: "0.1.0".to_string(),
                raw_version: None,
                source: PackageSource::Gitlab {
                    host: "gitlab.com".to_string(),
                },
                checksum: Some("git:abc123".to_string()),
                requires: vec![],
                url: Some(
                    "https://gitlab.com/api/v4/projects/my-group%2Fmypkg/repository/archive.tar.gz?sha=abc123"
                        .to_string(),
                ),
                system_requirements: None,
                dev: false,
                subdirectory: None,
            }],
        };

        let json = export_renv(&lockfile).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["Packages"]["mypkg"]["Source"], "Git");
        assert_eq!(parsed["Packages"]["mypkg"]["RemoteType"], "git2r");
        assert_eq!(
            parsed["Packages"]["mypkg"]["RemoteUrl"],
            "https://gitlab.com/my-group/mypkg"
        );
        assert_eq!(parsed["Packages"]["mypkg"]["RemoteUsername"], "my-group");
        assert_eq!(parsed["Packages"]["mypkg"]["RemoteRepo"], "mypkg");
        assert_eq!(parsed["Packages"]["mypkg"]["RemoteRef"], "abc123");
    }

    // #190: renv records a git remote as Source "git", RemoteType "git",
    // RemoteUrl, and the installed commit in RemoteSha, and restores it by
    // fetching RemoteSha from RemoteUrl.
    #[test]
    fn export_renv_generic_git_package_matches_renv() {
        use uvr_core::lockfile::{LockedPackage, Lockfile, PackageSource, RVersionPin};

        let sha = "0123456789abcdef0123456789abcdef01234567";
        let url = "git@bitbucket.org:team/anypkg.git";
        let lockfile = Lockfile {
            manifest_fingerprint: None,
            r: RVersionPin {
                version: "4.4.2".to_string(),
                bioc_version: None,
                resolved_as_of: None,
            },
            packages: vec![LockedPackage {
                name: "anypkg".to_string(),
                version: "0.1.0".to_string(),
                raw_version: None,
                source: PackageSource::Git { url: url.into() },
                checksum: Some(format!("git:{sha}")),
                requires: vec!["jsonlite".into()],
                url: None,
                system_requirements: None,
                dev: false,
                subdirectory: None,
            }],
        };

        let json = export_renv(&lockfile).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let pkg = &parsed["Packages"]["anypkg"];
        assert_eq!(pkg["Source"], "git");
        assert_eq!(pkg["RemoteType"], "git");
        assert_eq!(pkg["RemoteUrl"], url);
        assert_eq!(pkg["RemoteSha"], sha);
        for absent in ["Repository", "RemoteUsername", "RemoteRepo", "RemoteRef"] {
            assert!(pkg.get(absent).is_none(), "{absent}: {pkg}");
        }
    }

    #[test]
    fn parse_gitlab_remote_archive_url() {
        let url = "https://gitlab.com/api/v4/projects/my-group%2Fmypkg/repository/archive.tar.gz?sha=abc123";
        let (host, namespace_path, project, sha) = parse_gitlab_remote(url).unwrap();
        assert_eq!(host, "gitlab.com");
        assert_eq!(namespace_path, "my-group");
        assert_eq!(project, "mypkg");
        assert_eq!(sha, "abc123");
    }

    #[test]
    fn parse_gitlab_remote_archive_url_nested_subgroup() {
        let url = "https://gitlab.com/api/v4/projects/group%2Fsubgroup%2Fmypkg/repository/archive.tar.gz?sha=abc123";
        let (host, namespace_path, project, sha) = parse_gitlab_remote(url).unwrap();
        assert_eq!(host, "gitlab.com");
        assert_eq!(namespace_path, "group/subgroup");
        assert_eq!(project, "mypkg");
        assert_eq!(sha, "abc123");
    }

    #[test]
    fn export_renv_uses_raw_version() {
        use uvr_core::lockfile::{LockedPackage, Lockfile, RVersionPin};

        let lockfile = Lockfile {
            manifest_fingerprint: None,
            r: RVersionPin {
                version: "4.4.2".to_string(),
                bioc_version: None,
                resolved_as_of: None,
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
                subdirectory: None,
            }],
        };

        let json = export_renv(&lockfile).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        // Should use raw_version "1.1-3" not normalized "1.1.3"
        assert_eq!(parsed["Packages"]["scales"]["Version"], "1.1-3");
    }
}
