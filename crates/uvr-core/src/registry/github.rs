use semver::Version;
use tracing::info;

use crate::error::{Result, UvrError};
use crate::lockfile::PackageSource;
use crate::registry::{Dep, PackageInfo};

/// Parse `"user/repo@ref"` into (user, repo, ref).
pub fn parse_github_spec(spec: &str) -> Option<(String, String, String)> {
    let (repo_part, git_ref) = if let Some(at_pos) = spec.rfind('@') {
        (&spec[..at_pos], spec[at_pos + 1..].to_string())
    } else {
        (spec, "HEAD".to_string())
    };

    let parts: Vec<&str> = repo_part.splitn(2, '/').collect();
    if parts.len() != 2 {
        return None;
    }
    Some((parts[0].to_string(), parts[1].to_string(), git_ref))
}

/// Resolve a GitHub package — fetches commit SHA and DESCRIPTION.
pub async fn resolve_github_package(
    client: &reqwest::Client,
    user: &str,
    repo: &str,
    git_ref: &str,
) -> Result<PackageInfo> {
    let commit_sha = fetch_commit_sha(client, user, repo, git_ref).await?;

    let desc_url =
        format!("https://raw.githubusercontent.com/{user}/{repo}/{commit_sha}/DESCRIPTION");
    let desc_resp = client
        .get(&desc_url)
        .header("User-Agent", concat!("uvr/", env!("CARGO_PKG_VERSION")))
        .send()
        .await?;
    if !desc_resp.status().is_success() {
        return Err(UvrError::Other(format!(
            "Failed to fetch DESCRIPTION for {user}/{repo}@{commit_sha} (HTTP {}). \
             Check that the repository contains a DESCRIPTION file at the root.",
            desc_resp.status()
        )));
    }
    let desc_text = desc_resp.text().await?;

    let desc_fields = crate::dcf::parse_dcf_fields(&desc_text);
    let pkg_name = desc_fields
        .get("Package")
        .cloned()
        .unwrap_or_else(|| repo.to_string());
    let pkg_version = desc_fields
        .get("Version")
        .cloned()
        .unwrap_or_else(|| "0.0.0".to_string());
    let version = Version::parse(&crate::resolver::normalize_version(&pkg_version))
        .unwrap_or_else(|_| Version::new(0, 0, 0));

    // Parse dependencies from DESCRIPTION
    let requires = parse_description_deps(&desc_fields);

    let url = format!("https://api.github.com/repos/{user}/{repo}/tarball/{commit_sha}");

    info!("GitHub {user}/{repo}@{git_ref} → {pkg_name} {version} ({commit_sha})");

    Ok(PackageInfo {
        name: pkg_name,
        version,
        source: PackageSource::GitHub,
        checksum: Some(format!("git:{commit_sha}")),
        requires,
        url,
        raw_version: None, // GitHub packages don't have CRAN-style dash versions
        system_requirements: None,
    })
}

async fn fetch_commit_sha(
    client: &reqwest::Client,
    user: &str,
    repo: &str,
    git_ref: &str,
) -> Result<String> {
    let url = format!("https://api.github.com/repos/{user}/{repo}/commits/{git_ref}");
    let resp = client
        .get(&url)
        .header("User-Agent", concat!("uvr/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github.sha")
        .send()
        .await?;

    if !resp.status().is_success() {
        return Err(UvrError::Other(format!(
            "GitHub API error for {user}/{repo}@{git_ref}: {}",
            resp.status()
        )));
    }

    let sha = resp.text().await?;
    Ok(sha.trim().trim_matches('"').to_string())
}

/// Parse `Imports` and `Depends` from parsed DESCRIPTION fields into `Dep` values.
fn parse_description_deps(fields: &std::collections::BTreeMap<String, String>) -> Vec<Dep> {
    let mut deps = Vec::new();
    for field in &["Imports", "Depends"] {
        if let Some(value) = fields.get(*field) {
            let parsed = crate::registry::cran::parse_dep_field(value);
            for d in parsed {
                if !crate::resolver::is_base_package(&d.name) {
                    deps.push(Dep {
                        name: d.name,
                        constraint: d.req.as_ref().map(|r| r.to_string()),
                    });
                }
            }
        }
    }
    deps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_multiline_description_deps() {
        let desc = "\
Package: mypkg
Version: 1.0.0
Imports: cli (>= 3.4.0), generics,
    glue,
    lifecycle (>= 1.0.3),
    rlang (>= 1.1.0)
Depends: R (>= 3.5.0)
";
        let fields = crate::dcf::parse_dcf_fields(desc);
        let deps = parse_description_deps(&fields);
        let names: Vec<&str> = deps.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"cli"), "missing cli: {names:?}");
        assert!(names.contains(&"generics"), "missing generics: {names:?}");
        assert!(names.contains(&"glue"), "missing glue: {names:?}");
        assert!(names.contains(&"lifecycle"), "missing lifecycle: {names:?}");
        assert!(names.contains(&"rlang"), "missing rlang: {names:?}");
        // R itself should be filtered out as a base package
        assert!(!names.contains(&"R"), "R should be filtered: {names:?}");
    }

    #[test]
    fn parse_description_deps_empty() {
        let desc = "Package: mypkg\nVersion: 1.0.0\n";
        let fields = crate::dcf::parse_dcf_fields(desc);
        let deps = parse_description_deps(&fields);
        assert!(deps.is_empty());
    }

    #[test]
    fn parse_spec() {
        let (user, repo, git_ref) = parse_github_spec("user/myrepo@main").unwrap();
        assert_eq!(user, "user");
        assert_eq!(repo, "myrepo");
        assert_eq!(git_ref, "main");

        let (_user, _repo, git_ref) = parse_github_spec("tidyverse/ggplot2").unwrap();
        assert_eq!(git_ref, "HEAD");
    }
}
