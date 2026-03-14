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

    let desc_url = format!(
        "https://raw.githubusercontent.com/{user}/{repo}/{commit_sha}/DESCRIPTION"
    );
    let desc_text = client
        .get(&desc_url)
        .header("User-Agent", concat!("uvr/", env!("CARGO_PKG_VERSION")))
        .send()
        .await?
        .text()
        .await?;

    let pkg_name = parse_description_field(&desc_text, "Package")
        .unwrap_or_else(|| repo.to_string());
    let pkg_version = parse_description_field(&desc_text, "Version")
        .unwrap_or_else(|| "0.0.0".to_string());
    let version = Version::parse(&crate::resolver::normalize_version(&pkg_version))
        .unwrap_or_else(|_| Version::new(0, 0, 0));

    // Parse dependencies from DESCRIPTION
    let requires = parse_description_deps(&desc_text);

    let url = format!(
        "https://api.github.com/repos/{user}/{repo}/tarball/{commit_sha}"
    );

    info!("GitHub {user}/{repo}@{git_ref} → {pkg_name} {version} ({commit_sha})");

    Ok(PackageInfo {
        name: pkg_name,
        version,
        source: PackageSource::GitHub,
        checksum: Some(format!("git:{commit_sha}")),
        requires,
        url,
        raw_version: None, // GitHub packages don't have CRAN-style dash versions
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

fn parse_description_field(text: &str, field: &str) -> Option<String> {
    let prefix = format!("{field}:");
    text.lines()
        .find(|l| l.starts_with(&prefix))
        .map(|l| l[prefix.len()..].trim().to_string())
}

/// Parse `Imports` and `Depends` from a DESCRIPTION file into `Dep` values.
fn parse_description_deps(text: &str) -> Vec<Dep> {
    let mut deps = Vec::new();
    for field in &["Imports", "Depends"] {
        if let Some(value) = parse_description_field(text, field) {
            let parsed = crate::registry::cran::parse_dep_field(&value);
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
    fn parse_spec() {
        let (user, repo, git_ref) = parse_github_spec("user/myrepo@main").unwrap();
        assert_eq!(user, "user");
        assert_eq!(repo, "myrepo");
        assert_eq!(git_ref, "main");

        let (_user, _repo, git_ref) = parse_github_spec("tidyverse/ggplot2").unwrap();
        assert_eq!(git_ref, "HEAD");
    }
}
