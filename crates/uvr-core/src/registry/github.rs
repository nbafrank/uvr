use semver::Version;
use tracing::debug;

use crate::auth::{git_origin, GitHost};
use crate::error::{Result, UvrError};
use crate::lockfile::{LockedPackage, PackageSource};
use crate::registry::{Dep, PackageInfo};

/// Legacy tuple form for a GitHub-sourced `Remotes:` dependency:
/// `(dep_name, "user/repo", optional_ref)`. New traversal code uses
/// [`crate::manifest::RemoteEntry`] so bound and nested metadata survives.
pub type GithubRemote = (String, String, Option<String>);

/// Charset gate for git refs before they reach a URL (#152).
///
/// Follows `git check-ref-format` where it matters for us: reject
/// whitespace, control chars, the metacharacters git itself forbids
/// (`~ ^ : ? * [ \`), the sequences `..` and `@{`, plus `&` and `#`,
/// which are legal nowhere in a ref that we'd want to interpolate into
/// a query string or URL path. Legitimate refs (`main`, `v1.2.3`,
/// `feature/x`, `release-2024.01_rc+1`) all pass.
pub(crate) fn is_valid_git_ref(git_ref: &str) -> bool {
    !git_ref.is_empty()
        && !git_ref.contains("..")
        && !git_ref.contains("@{")
        && git_ref.chars().all(|c| {
            !c.is_whitespace()
                && !c.is_control()
                && !matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\' | '&' | '#')
        })
}

/// Parse `"user/repo@ref"` into (user, repo, ref).
///
/// Rejects refs that fail [`is_valid_git_ref`] — a ref with `&`, `#`,
/// `?`, or whitespace would mis-parse the registry API request (#152).
pub fn parse_github_spec(spec: &str) -> Option<(String, String, String)> {
    let (repo_part, git_ref) = if let Some(at_pos) = spec.rfind('@') {
        (&spec[..at_pos], spec[at_pos + 1..].to_string())
    } else {
        (spec, "HEAD".to_string())
    };

    if !is_valid_git_ref(&git_ref) {
        return None;
    }

    let parts: Vec<&str> = repo_part.splitn(2, '/').collect();
    if parts.len() != 2 {
        return None;
    }
    Some((parts[0].to_string(), parts[1].to_string(), git_ref))
}

/// Resolve a GitHub package — fetches commit SHA and DESCRIPTION.
///
/// Thin wrapper that deliberately drops rich `Remotes:` entries before the
/// fallible legacy-tuple adapter. Callers that need to walk remote chains
/// should use [`resolve_github_package_with_remote_entries`] instead.
pub async fn resolve_github_package(
    client: &reqwest::Client,
    user: &str,
    repo: &str,
    git_ref: &str,
) -> Result<PackageInfo> {
    resolve_github_package_with_remote_entries(client, user, repo, git_ref)
        .await
        .map(|(info, _)| info)
}

/// Legacy tuple adapter. Nested or bound-unsupported `Remotes:` entries return
/// an error because `GithubRemote` cannot preserve their identity metadata.
pub async fn resolve_github_package_with_remotes(
    client: &reqwest::Client,
    user: &str,
    repo: &str,
    git_ref: &str,
) -> Result<(PackageInfo, Vec<GithubRemote>)> {
    resolve_github_package_in(client, user, repo, git_ref, None).await
}

/// Legacy tuple adapter with GitHub subdirectory selection for the package
/// being resolved; returned nested remotes remain unrepresentable and error.
pub async fn resolve_github_package_in(
    client: &reqwest::Client,
    user: &str,
    repo: &str,
    git_ref: &str,
    subdirectory: Option<&str>,
) -> Result<(PackageInfo, Vec<GithubRemote>)> {
    let (info, remotes) =
        resolve_github_package_with_remote_entries_in(client, user, repo, git_ref, subdirectory)
            .await?;
    Ok((info, compatible_remotes(remotes)?))
}

pub async fn resolve_github_package_with_remote_entries(
    client: &reqwest::Client,
    user: &str,
    repo: &str,
    git_ref: &str,
) -> Result<(PackageInfo, Vec<crate::manifest::RemoteEntry>)> {
    resolve_github_package_with_remote_entries_in(client, user, repo, git_ref, None).await
}

pub async fn resolve_github_package_with_remote_entries_in(
    client: &reqwest::Client,
    user: &str,
    repo: &str,
    git_ref: &str,
    subdirectory: Option<&str>,
) -> Result<(PackageInfo, Vec<crate::manifest::RemoteEntry>)> {
    if let Some(subdirectory) = subdirectory {
        crate::subdirectory::validate(subdirectory)?;
    }
    let commit_sha = fetch_commit_sha(client, user, repo, git_ref).await?;
    resolve_github_package_with_remote_entries_at_commit(
        client,
        user,
        repo,
        &commit_sha,
        subdirectory,
    )
    .await
}

pub async fn resolve_github_package_with_remote_entries_at_commit(
    client: &reqwest::Client,
    user: &str,
    repo: &str,
    commit_sha: &str,
    subdirectory: Option<&str>,
) -> Result<(PackageInfo, Vec<crate::manifest::RemoteEntry>)> {
    resolve_github_package_with_remote_entries_at_commit_bound(
        client,
        user,
        repo,
        commit_sha,
        subdirectory,
        false,
    )
    .await
}

pub async fn resolve_github_package_with_remote_entries_at_commit_bound(
    client: &reqwest::Client,
    user: &str,
    repo: &str,
    commit_sha: &str,
    subdirectory: Option<&str>,
    require_declared_name: bool,
) -> Result<(PackageInfo, Vec<crate::manifest::RemoteEntry>)> {
    if let Some(subdirectory) = subdirectory {
        crate::subdirectory::validate(subdirectory)?;
        if !is_full_commit_sha(commit_sha) {
            return Err(UvrError::Other(format!(
                "GitHub returned '{commit_sha}' for {user}/{repo}; a full 40-character \
                 lowercase commit SHA is required to lock a subdirectory package."
            )));
        }
    }

    let desc_url = description_url(user, repo, commit_sha, subdirectory);
    let desc_req = client
        .get(&desc_url)
        .header("User-Agent", concat!("uvr/", env!("CARGO_PKG_VERSION")));
    let desc_resp = GitHost::GitHub.send(desc_req).await?;
    if !desc_resp.status().is_success() {
        return Err(UvrError::Other(match subdirectory {
            Some(sub) => format!(
                "Failed to fetch DESCRIPTION for {user}/{repo}@{commit_sha} in '{sub}' (HTTP {}). \
                 Check that the repository contains a DESCRIPTION file at that subdirectory.",
                desc_resp.status()
            ),
            None => format!(
                "Failed to fetch DESCRIPTION for {user}/{repo}@{commit_sha} (HTTP {}). \
                 Check that the repository contains a DESCRIPTION file at the root.",
                desc_resp.status()
            ),
        }));
    }
    let desc_text = desc_resp.text().await?;

    let desc_fields = crate::dcf::parse_dcf_fields(&desc_text);
    let pkg_name = description_package_name_bound(
        &desc_fields,
        subdirectory,
        user,
        repo,
        commit_sha,
        require_declared_name,
    )?;
    let pkg_version = desc_fields
        .get("Version")
        .cloned()
        .unwrap_or_else(|| "0.0.0".to_string());
    let version = Version::parse(&crate::resolver::normalize_version(&pkg_version))
        .unwrap_or_else(|_| Version::new(0, 0, 0));
    let requires = parse_description_deps(&desc_fields);
    let remotes = parse_github_remote_entries(&desc_fields);
    let url = tarball_url(user, repo, commit_sha);

    debug!("GitHub {user}/{repo}@{commit_sha} → {pkg_name} {version}");

    Ok((
        PackageInfo {
            name: pkg_name,
            version,
            source: PackageSource::GitHub,
            checksum: Some(format!("git:{commit_sha}")),
            requires,
            url,
            raw_version: None,
            system_requirements: None,
            subdirectory: subdirectory.map(str::to_string),
        },
        remotes,
    ))
}

#[cfg(test)]
fn description_package_name(
    fields: &std::collections::BTreeMap<String, String>,
    subdirectory: Option<&str>,
    user: &str,
    repo: &str,
    commit_sha: &str,
) -> Result<String> {
    description_package_name_bound(fields, subdirectory, user, repo, commit_sha, false)
}

fn description_package_name_bound(
    fields: &std::collections::BTreeMap<String, String>,
    subdirectory: Option<&str>,
    user: &str,
    repo: &str,
    commit_sha: &str,
    require_declared_name: bool,
) -> Result<String> {
    if subdirectory.is_none() && !require_declared_name {
        return Ok(fields
            .get("Package")
            .cloned()
            .unwrap_or_else(|| repo.to_string()));
    }
    let location = subdirectory
        .map(|sub| format!(" in '{sub}'"))
        .unwrap_or_default();
    let name = fields
        .get("Package")
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .ok_or_else(|| {
            UvrError::Other(format!(
                "DESCRIPTION for {user}/{repo}@{commit_sha}{location} has no `Package:` \
                 field; cannot validate the bound package identity."
            ))
        })?;
    if !crate::package_name::is_valid(&name) {
        return Err(UvrError::Other(format!(
            "DESCRIPTION for {user}/{repo}@{commit_sha}{location} declares an invalid \
             `Package:` name '{name}'."
        )));
    }
    Ok(name)
}

fn description_url(user: &str, repo: &str, git_ref: &str, subdirectory: Option<&str>) -> String {
    let raw = git_origin("raw.githubusercontent.com");
    match subdirectory {
        Some(sub) => format!(
            "{raw}/{user}/{repo}/{git_ref}/{}/DESCRIPTION",
            crate::subdirectory::encode_segments(sub)
        ),
        None => format!("{raw}/{user}/{repo}/{git_ref}/DESCRIPTION"),
    }
}

pub fn is_full_commit_sha(sha: &str) -> bool {
    sha.len() == 40
        && sha
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, 'a'..='f'))
}

pub fn is_valid_github_repo_spec(spec: &str) -> bool {
    match parse_github_spec(spec) {
        Some((user, repo, _)) => is_repo_segment(&user) && is_repo_segment(&repo),
        None => false,
    }
}

fn is_repo_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

pub fn tarball_url(user: &str, repo: &str, commit_sha: &str) -> String {
    let api = git_origin("api.github.com");
    format!("{api}/repos/{user}/{repo}/tarball/{commit_sha}")
}

pub fn validate_nested_lock_entry(p: &LockedPackage) -> Result<()> {
    if p.source != PackageSource::GitHub && p.subdirectory.is_none() {
        return Ok(());
    }
    let bad = |msg: String| UvrError::Other(format!("Locked package '{}': {msg}", p.name));
    if p.source != PackageSource::GitHub {
        return Err(bad(format!(
            "`subdirectory` is only supported for GitHub packages, not source '{}'.",
            p.source
        )));
    }
    if !crate::package_name::is_valid(&p.name) {
        return Err(bad(format!(
            "`name` is not a valid R package name: '{}'.",
            p.name
        )));
    }
    if let Some(sub) = p.subdirectory.as_deref() {
        crate::subdirectory::validate(sub)?;
    }
    let commit = p
        .checksum
        .as_deref()
        .and_then(|c| c.strip_prefix("git:"))
        .filter(|c| is_full_commit_sha(c))
        .ok_or_else(|| {
            bad("`checksum` must be `git:<40-character lowercase commit SHA>`.".to_string())
        })?;
    let url = p.url.as_deref().unwrap_or_default();
    let rest = url
        .strip_prefix("https://api.github.com/repos/")
        .filter(|rest| !rest.contains(['?', '#']))
        .ok_or_else(|| bad(format!("`url` must be a GitHub tarball URL, got '{url}'.")))?;
    let parts: Vec<&str> = rest.split('/').collect();
    let canonical = matches!(parts.as_slice(), [user, repo, "tarball", sha]
        if is_repo_segment(user) && is_repo_segment(repo) && *sha == commit);
    if !canonical {
        return Err(bad(format!(
            "`url` must be https://api.github.com/repos/<owner>/<repo>/tarball/{commit}, got '{url}'."
        )));
    }
    Ok(())
}

/// Pull github-sourced entries out of a DESCRIPTION's `Remotes:` field.
///
/// Reuses the manifest module's `Remotes:` parser so syntax handled there
/// (`user/repo`, `user/repo@ref`, `github::user/repo`, `pkgname=user/repo`)
/// stays consistent. Non-github prefixes (`bioc::`, `gitlab::`, `url::`,
/// etc.) are filtered out by the manifest parser before we see them.
#[cfg(test)]
fn parse_github_remotes(
    desc_fields: &std::collections::BTreeMap<String, String>,
) -> Result<Vec<GithubRemote>> {
    compatible_remotes(parse_github_remote_entries(desc_fields))
}

fn parse_github_remote_entries(
    desc_fields: &std::collections::BTreeMap<String, String>,
) -> Vec<crate::manifest::RemoteEntry> {
    let Some(remotes_field) = desc_fields.get("Remotes") else {
        return Vec::new();
    };
    crate::manifest::parse_remotes_field_rich(remotes_field)
}

fn compatible_remotes(entries: Vec<crate::manifest::RemoteEntry>) -> Result<Vec<GithubRemote>> {
    crate::manifest::compatible_remote_entries(entries)
}

pub async fn fetch_commit_sha(
    client: &reqwest::Client,
    user: &str,
    repo: &str,
    git_ref: &str,
) -> Result<String> {
    // Percent-encode the ref: it sits in path position, and refs can reach
    // here without going through `parse_github_spec` (lockfile revs,
    // `Remotes:` fields), so encode defensively (#152). GitHub's API
    // accepts `%2F` for the `/` in refs like `feature/x`.
    let encoded_ref = urlencoding::encode(git_ref);
    let api = git_origin("api.github.com");
    let url = format!("{api}/repos/{user}/{repo}/commits/{encoded_ref}");
    let req = client
        .get(&url)
        .header("User-Agent", concat!("uvr/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github.sha");
    let resp = GitHost::GitHub.send(req).await?;

    let status = resp.status();
    if !status.is_success() {
        let advice = match status.as_u16() {
            401 | 403 => format!("; {}", GitHost::GitHub.denied_advice()),
            _ => String::new(),
        };
        return Err(UvrError::Other(format!(
            "GitHub API error for {user}/{repo}@{git_ref}: {status}{advice}"
        )));
    }

    let sha = resp.text().await?;
    Ok(sha.trim().trim_matches('"').to_string())
}

/// Parse install-time dependencies from DESCRIPTION, keeping every distinct constraint.
pub(crate) fn parse_description_deps(
    fields: &std::collections::BTreeMap<String, String>,
) -> Vec<Dep> {
    parse_description_dep_fields(fields, &["Imports", "Depends", "LinkingTo"])
}

pub(crate) fn parse_description_runtime_deps(
    fields: &std::collections::BTreeMap<String, String>,
) -> Vec<Dep> {
    parse_description_dep_fields(fields, &["Imports", "Depends"])
}

pub(crate) fn parse_description_install_dependency_names(
    fields: &std::collections::BTreeMap<String, String>,
) -> std::collections::BTreeSet<String> {
    parse_description_deps(fields)
        .into_iter()
        .map(|dependency| dependency.name)
        .collect()
}

fn parse_description_dep_fields(
    fields: &std::collections::BTreeMap<String, String>,
    dependency_fields: &[&str],
) -> Vec<Dep> {
    let mut deps: Vec<Dep> = Vec::new();
    for field in dependency_fields {
        if let Some(value) = fields.get(*field) {
            let parsed = crate::registry::cran::parse_dep_field(value);
            for d in parsed {
                if crate::resolver::is_base_package(&d.name) {
                    continue;
                }
                match d.req.as_ref().map(|r| r.to_string()) {
                    None => {
                        if !deps.iter().any(|e| e.name == d.name) {
                            deps.push(Dep {
                                name: d.name,
                                constraint: None,
                            });
                        }
                    }
                    Some(c) => {
                        let already = deps.iter().any(|e| {
                            e.name == d.name && e.constraint.as_deref() == Some(c.as_str())
                        });
                        if already {
                            continue;
                        }
                        match deps
                            .iter_mut()
                            .find(|e| e.name == d.name && e.constraint.is_none())
                        {
                            Some(existing) => existing.constraint = Some(c),
                            None => deps.push(Dep {
                                name: d.name,
                                constraint: Some(c),
                            }),
                        }
                    }
                }
            }
        }
    }
    deps
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn description_with_package(package: &str) -> std::collections::BTreeMap<String, String> {
        crate::dcf::parse_dcf_fields(&format!("Package: {package}\nVersion: 1.0.0\n"))
    }

    #[test]
    fn nested_package_name_accepts_legal_names() {
        for ok in ["mypkg", "data.table", "my-pkg_1", "R6"] {
            let name = description_package_name(
                &description_with_package(ok),
                Some("pkgs/nested"),
                "owner",
                "repo",
                TEST_SHA,
            )
            .unwrap();
            assert_eq!(name, ok);
        }
    }

    #[test]
    fn nested_package_name_rejects_illegal_names() {
        for bad in [
            "my pkg",
            "a+b",
            "pkgs/nested",
            "../../outside",
            "a:b",
            ".",
            "..",
        ] {
            let err = description_package_name(
                &description_with_package(bad),
                Some("pkgs/nested"),
                "owner",
                "repo",
                TEST_SHA,
            )
            .unwrap_err()
            .to_string();
            assert!(
                err.contains("invalid `Package:` name"),
                "unexpected error for {bad:?}: {err}"
            );
        }
    }

    #[test]
    fn nested_package_name_rejects_missing_or_empty_field() {
        for desc in ["Version: 1.0.0\n", "Package: \nVersion: 1.0.0\n"] {
            let fields = crate::dcf::parse_dcf_fields(desc);
            let err =
                description_package_name(&fields, Some("pkgs/nested"), "owner", "repo", TEST_SHA)
                    .unwrap_err()
                    .to_string();
            assert!(err.contains("has no `Package:` field"), "unexpected: {err}");
        }
    }

    #[test]
    fn root_package_name_keeps_repo_fallback() {
        let missing = crate::dcf::parse_dcf_fields("Version: 1.0.0\n");
        assert_eq!(
            description_package_name(&missing, None, "owner", "repo", TEST_SHA).unwrap(),
            "repo"
        );
        assert_eq!(
            description_package_name(
                &description_with_package("mypkg"),
                None,
                "owner",
                "repo",
                TEST_SHA
            )
            .unwrap(),
            "mypkg"
        );
    }

    #[test]
    fn bound_root_package_name_requires_a_description_package_field() {
        let missing = crate::dcf::parse_dcf_fields("Version: 1.0.0\n");
        let error = description_package_name_bound(&missing, None, "owner", "repo", TEST_SHA, true)
            .unwrap_err()
            .to_string();
        assert!(error.contains("has no `Package:` field"), "{error}");

        let fields = description_with_package("Alias");
        assert_eq!(
            description_package_name_bound(&fields, None, "owner", "repo", TEST_SHA, true,)
                .unwrap(),
            "Alias"
        );
    }

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
    fn parse_description_deps_linking_to() {
        let desc = "\
Package: mypkg
Version: 1.0.0
LinkingTo: pkgA, pkgB
";
        let fields = crate::dcf::parse_dcf_fields(desc);
        let deps = parse_description_deps(&fields);
        let names: Vec<&str> = deps.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["pkgA", "pkgB"]);
        assert!(deps.iter().all(|d| d.constraint.is_none()));
    }

    #[test]
    fn runtime_description_deps_preserve_non_github_provider_behavior() {
        let fields = crate::dcf::parse_dcf_fields(
            "Package: pkg\nImports: imported\nDepends: depended\nLinkingTo: headers\n",
        );
        let names: Vec<String> = parse_description_runtime_deps(&fields)
            .into_iter()
            .map(|dependency| dependency.name)
            .collect();
        assert_eq!(names, ["imported", "depended"]);
    }

    #[test]
    fn parse_description_deps_linking_to_dedup() {
        let desc = "\
Package: mypkg
Version: 1.0.0
Imports: pkgA (>= 1.0.0), pkgC
LinkingTo: pkgA, pkgB
Depends: R (>= 4.0.0)
";
        let fields = crate::dcf::parse_dcf_fields(desc);
        let deps = parse_description_deps(&fields);
        let names: Vec<&str> = deps.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["pkgA", "pkgC", "pkgB"]);
        let pkg_a = deps.iter().find(|d| d.name == "pkgA").unwrap();
        assert_eq!(pkg_a.constraint.as_deref(), Some(">=1.0.0"));
        assert!(!names.contains(&"R"), "R should be filtered: {names:?}");
    }

    #[test]
    fn parse_description_deps_later_constraint_upgrades_entry() {
        let desc = "\
Package: mypkg
Version: 1.0.0
Imports: pkgA, pkgB
LinkingTo: pkgA (>= 2.0.0)
";
        let fields = crate::dcf::parse_dcf_fields(desc);
        let deps = parse_description_deps(&fields);
        let names: Vec<&str> = deps.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["pkgA", "pkgB"]);
        let pkg_a = deps.iter().find(|d| d.name == "pkgA").unwrap();
        assert_eq!(pkg_a.constraint.as_deref(), Some(">=2.0.0"));
    }

    #[test]
    fn parse_description_deps_keeps_distinct_constraints() {
        let desc = "\
Package: mypkg
Version: 1.0.0
Imports: pkgA (>= 1.0.0)
Depends: pkgA (>= 2.0.0)
LinkingTo: pkgA (>= 1.0.0), pkgA
";
        let fields = crate::dcf::parse_dcf_fields(desc);
        let deps = parse_description_deps(&fields);
        let constraints: Vec<Option<&str>> = deps
            .iter()
            .filter(|d| d.name == "pkgA")
            .map(|d| d.constraint.as_deref())
            .collect();
        assert_eq!(constraints, [Some(">=1.0.0"), Some(">=2.0.0")]);
    }

    #[test]
    fn parse_spec() {
        let (user, repo, git_ref) = parse_github_spec("user/myrepo@main").unwrap();
        assert_eq!(user, "user");
        assert_eq!(repo, "myrepo");
        assert_eq!(git_ref, "main");

        let (_user, _repo, git_ref) = parse_github_spec("tidyverse/ggplot2").unwrap();
        assert_eq!(git_ref, "HEAD");

        // Slashed branch names stay valid (ref split happens before the
        // user/repo split, so extra `/` in the ref is fine).
        let (user, repo, git_ref) = parse_github_spec("user/myrepo@feature/x").unwrap();
        assert_eq!(user, "user");
        assert_eq!(repo, "myrepo");
        assert_eq!(git_ref, "feature/x");

        let (_, _, git_ref) = parse_github_spec("user/myrepo@v1.2.3").unwrap();
        assert_eq!(git_ref, "v1.2.3");
    }

    #[test]
    fn parse_spec_rejects_url_breaking_refs() {
        // #152: refs with URL metacharacters would mis-parse the
        // `/commits/{ref}` request — reject them at parse time.
        assert!(parse_github_spec("user/repo@feat&x").is_none());
        assert!(parse_github_spec("user/repo@v1#frag").is_none());
        assert!(parse_github_spec("user/repo@a b").is_none());
        assert!(parse_github_spec("user/repo@x?y").is_none());
        assert!(parse_github_spec("user/repo@back\\slash").is_none());
        // Trailing `@` yields an empty ref — also rejected.
        assert!(parse_github_spec("user/repo@").is_none());
    }

    #[test]
    fn git_ref_charset_gate() {
        for ok in [
            "main",
            "HEAD",
            "v1.2.3",
            "feature/x",
            "release-2024.01_rc+1",
        ] {
            assert!(is_valid_git_ref(ok), "should accept {ok:?}");
        }
        for bad in [
            "", "feat&x", "v1#f", "a b", "x?y", "a\\b", "a..b", "@{u}", "a~1", "a^2", "re:f",
            "a*b", "a[b", "a\tb",
        ] {
            assert!(!is_valid_git_ref(bad), "should reject {bad:?}");
        }
    }

    #[test]
    fn parse_github_remotes_basic() {
        // Matches the #84 reproducer: `airquality` declares a github
        // sub-dep via Remotes — without parsing this, uvr falls through
        // to CRAN for `handyr` and bails.
        let desc = "\
Package: airquality
Version: 0.0.1
Imports: handyr
Remotes: B-Nilson/handyr
";
        let fields = crate::dcf::parse_dcf_fields(desc);
        let remotes = parse_github_remotes(&fields).unwrap();
        assert_eq!(remotes.len(), 1);
        assert_eq!(remotes[0].0, "handyr");
        assert_eq!(remotes[0].1, "B-Nilson/handyr");
        assert_eq!(remotes[0].2, None);
    }

    #[test]
    fn parse_github_remotes_with_ref_and_prefixes() {
        let desc = "\
Package: x
Version: 0.0.1
Remotes: github::user/a@v1.0.0,
    user/b@main,
    bioc::release/Biobase,
    gitlab::user/c
";
        let fields = crate::dcf::parse_dcf_fields(desc);
        let remotes = parse_github_remotes(&fields).unwrap();
        let names: Vec<&str> = remotes.iter().map(|(n, _, _)| n.as_str()).collect();
        // bioc:: is unsupported and the GitLab hint is malformed, so both skip.
        assert_eq!(names, vec!["a", "b"]);
        assert_eq!(remotes[0].2.as_deref(), Some("v1.0.0"));
        assert_eq!(remotes[1].2.as_deref(), Some("main"));
    }

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn nested_locked(url: &str, checksum: &str, subdirectory: Option<&str>) -> LockedPackage {
        LockedPackage {
            name: "nested".to_string(),
            version: "0.1.0".to_string(),
            source: PackageSource::GitHub,
            raw_version: None,
            url: Some(url.to_string()),
            checksum: Some(checksum.to_string()),
            subdirectory: subdirectory.map(str::to_string),
            requires: Vec::new(),
            system_requirements: None,
            dev: false,
        }
    }

    #[test]
    fn description_url_encodes_subdirectory_segments_only() {
        assert_eq!(
            description_url("o", "r", SHA, None),
            format!("https://raw.githubusercontent.com/o/r/{SHA}/DESCRIPTION")
        );
        assert_eq!(
            description_url("o", "r", SHA, Some("pkgs/my pkg")),
            format!("https://raw.githubusercontent.com/o/r/{SHA}/pkgs/my%20pkg/DESCRIPTION")
        );
        assert_eq!(
            description_url("o", "r", "feature/x", None),
            "https://raw.githubusercontent.com/o/r/feature/x/DESCRIPTION"
        );
    }

    #[test]
    fn full_commit_sha_requires_40_lowercase_hex() {
        assert!(is_full_commit_sha(SHA));
        for bad in [
            "",
            "abc123",
            &SHA[..39],
            &format!("{SHA}0"),
            &SHA.to_uppercase(),
            &format!("{}g", &SHA[..39]),
        ] {
            assert!(!is_full_commit_sha(bad), "should reject {bad:?}");
        }
    }

    #[test]
    fn valid_github_repo_spec_is_fail_closed() {
        for ok in [
            "owner/repo",
            "owner/repo@main",
            "owner/repo@feature/x",
            "o/r.pkg@v1.0",
        ] {
            assert!(is_valid_github_repo_spec(ok), "should accept {ok:?}");
        }
        for bad in [
            "",
            "repo",
            "owner/",
            "/repo",
            "owner/repo@",
            "owner/repo/extra",
            "owner//repo",
            "owner/..@main",
            "gitlab::gitlab.com/g/p",
            "owner/repo@a b",
        ] {
            assert!(!is_valid_github_repo_spec(bad), "should reject {bad:?}");
        }
    }

    #[test]
    fn tarball_url_is_canonical() {
        assert_eq!(
            tarball_url("o", "r", SHA),
            format!("https://api.github.com/repos/o/r/tarball/{SHA}")
        );
    }

    #[test]
    fn nested_lock_entry_accepts_canonical_github_identity() {
        let p = nested_locked(
            &tarball_url("o", "r", SHA),
            &format!("git:{SHA}"),
            Some("pkgs/nested"),
        );
        assert!(validate_nested_lock_entry(&p).is_ok());
    }

    #[test]
    fn root_lock_entry_is_not_validated_as_nested() {
        let mut p = nested_locked("https://example.com/x.tar.gz", "sha256:abc", None);
        p.source = PackageSource::Cran;
        assert!(validate_nested_lock_entry(&p).is_ok());
    }

    #[test]
    fn nested_lock_entry_rejects_inconsistent_identity() {
        let other = "89abcdef0123456789abcdef0123456789abcdef";
        let cases = [
            nested_locked(
                &tarball_url("o", "r", SHA),
                &format!("git:{SHA}"),
                Some("../escape"),
            ),
            nested_locked(
                &tarball_url("o", "r", SHA),
                &format!("git:{}", &SHA[..7]),
                Some("p"),
            ),
            nested_locked(&tarball_url("o", "r", SHA), "sha256:abc", Some("p")),
            nested_locked(
                &tarball_url("o", "r", other),
                &format!("git:{SHA}"),
                Some("p"),
            ),
            nested_locked(
                &format!("https://api.github.com/repos/o/r/zipball/{SHA}"),
                &format!("git:{SHA}"),
                Some("p"),
            ),
            nested_locked(
                &format!("https://codeload.github.com/o/r/tar.gz/{SHA}"),
                &format!("git:{SHA}"),
                Some("p"),
            ),
            nested_locked(
                &format!("https://api.github.com/repos/o/r/tarball/{SHA}/extra"),
                &format!("git:{SHA}"),
                Some("p"),
            ),
            nested_locked(
                &format!("https://api.github.com/repos/o?x/r/tarball/{SHA}"),
                &format!("git:{SHA}"),
                Some("p"),
            ),
            nested_locked(
                &format!("https://api.github.com/repos/o/r/tarball/{SHA}#frag"),
                &format!("git:{SHA}"),
                Some("p"),
            ),
            nested_locked(
                &format!("https://api.github.com/repos/../r/tarball/{SHA}"),
                &format!("git:{SHA}"),
                Some("p"),
            ),
            nested_locked(
                &format!("https://api.github.com/repos//r/tarball/{SHA}"),
                &format!("git:{SHA}"),
                Some("p"),
            ),
        ];
        for p in cases {
            assert!(
                validate_nested_lock_entry(&p).is_err(),
                "should reject {:?} / {:?} / {:?}",
                p.url,
                p.checksum,
                p.subdirectory
            );
        }

        let mut wrong_source = nested_locked(
            &tarball_url("o", "r", SHA),
            &format!("git:{SHA}"),
            Some("pkgs/nested"),
        );
        wrong_source.source = PackageSource::Gitlab {
            host: "gitlab.com".to_string(),
        };
        assert!(validate_nested_lock_entry(&wrong_source).is_err());

        let mut wrong_name = nested_locked(
            &tarball_url("o", "r", SHA),
            &format!("git:{SHA}"),
            Some("pkgs/nested"),
        );
        wrong_name.name = "../escape".to_string();
        assert!(validate_nested_lock_entry(&wrong_name).is_err());
    }

    #[test]
    fn parse_github_remotes_missing_field() {
        let desc = "Package: x\nVersion: 0.0.1\nImports: foo\n";
        let fields = crate::dcf::parse_dcf_fields(desc);
        assert!(parse_github_remotes(&fields).unwrap().is_empty());
    }

    /// GitHub's routes for repository `owner/privpkg` at [`SHA`]: the
    /// commit API, raw.githubusercontent.com, and a tarball API call that
    /// redirects to `codeload` with a token in the URL, as GitHub does.
    #[cfg(not(target_os = "windows"))]
    fn github_route(path: &str, codeload: &str) -> Vec<u8> {
        use crate::auth::test_response;
        let ok = |body: &[u8]| test_response("200 OK", "", body);
        if path == "/repos/owner/privpkg/commits/main" {
            ok(SHA.as_bytes())
        } else if path == format!("/owner/privpkg/{SHA}/DESCRIPTION") {
            ok(b"Package: privpkg\nVersion: 0.3.0\n")
        } else if path == format!("/repos/owner/privpkg/tarball/{SHA}") {
            let location =
                format!("Location: {codeload}/owner/privpkg/tar.gz/{SHA}?token=signed\r\n");
            test_response("302 Found", &location, b"")
        } else {
            test_response("404 Not Found", "", b"")
        }
    }

    /// `(api/raw origin, its request log, codeload's request log)`: the
    /// GitHub hosts on this thread, with `github` as their request handler.
    #[cfg(not(target_os = "windows"))]
    #[allow(clippy::type_complexity)]
    fn fake_github(
        github: impl Fn(&str, &str) -> Vec<u8> + Send + 'static,
    ) -> (
        String,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        use crate::auth::{
            test_authorization, test_git_origin, test_path, test_response, test_server,
        };
        // codeload serves the tarball only with its URL token, and must
        // never see the GitHub credential.
        let (codeload, codeload_seen) = test_server(|head| {
            match (
                test_path(head).ends_with("?token=signed"),
                test_authorization(head),
            ) {
                (true, None) => test_response("200 OK", "", b"private tarball"),
                _ => test_response("403 Forbidden", "", b""),
            }
        });
        let (api, api_seen) = test_server(move |head| github(head, &codeload));
        test_git_origin("api.github.com", &api);
        test_git_origin("raw.githubusercontent.com", &api);
        (api, api_seen, codeload_seen)
    }

    #[cfg(not(target_os = "windows"))]
    fn current_thread() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    // #187: a private repository resolves and installs with GITHUB_PAT from
    // the shared resolver. The tarball request now carries it too (before
    // #187 it did not, so a private repository locked but never synced);
    // the redirect to codeload does not.
    #[cfg(not(target_os = "windows"))]
    #[test]
    fn private_repository_resolves_and_downloads() {
        use crate::auth::{test_authorization, test_path, test_response, GitEnv};

        let _env = GitEnv::new(&[]);
        std::env::set_var("GITHUB_PAT", "gh-tok");
        let rt = current_thread();
        // A private repository: hidden without a token.
        let (_, api_seen, codeload_seen) =
            fake_github(|head, codeload| match test_authorization(head) {
                Some("Bearer gh-tok") => github_route(test_path(head), codeload),
                Some(_) => refused(test_path(head)),
                None => test_response("404 Not Found", "", b""),
            });
        let client = reqwest::Client::new();
        let resolve = || rt.block_on(resolve_github_package(&client, "owner", "privpkg", "main"));

        let info = resolve().expect("GITHUB_PAT opens the private repository");
        assert_eq!(
            (info.name.as_str(), info.version.to_string()),
            ("privpkg", "0.3.0".into())
        );
        let tarball = crate::installer::download::test_download(&rt, &info)
            .expect("the tarball downloads with GITHUB_PAT");
        assert_eq!(tarball, b"private tarball");
        let heads = api_seen.lock().unwrap().clone();
        assert_eq!(heads.len(), 3, "{heads:?}");
        for head in &heads {
            assert_eq!(test_authorization(head), Some("Bearer gh-tok"), "{head}");
        }
        let codeload = codeload_seen.lock().unwrap().clone();
        assert_eq!(codeload.len(), 1);
        assert_eq!(test_authorization(&codeload[0]), None, "{codeload:?}");

        // A wrong token is refused, and the error says which variable.
        std::env::set_var("GITHUB_PAT", "wrong-tok");
        let err = resolve().unwrap_err().to_string();
        assert!(
            err.contains("401 Unauthorized; it refused the token in GITHUB_PAT."),
            "{err}"
        );
        assert!(!err.contains("wrong-tok"), "{err}");
    }

    /// GitHub's answer to a token that it refuses, as github.com gives it:
    /// 401 from the API, but 404 from raw.githubusercontent.com.
    #[cfg(not(target_os = "windows"))]
    fn refused(path: &str) -> Vec<u8> {
        use crate::auth::test_response;
        if path.starts_with("/repos/") {
            test_response("401 Unauthorized", "", b"")
        } else {
            test_response("404 Not Found", "", b"")
        }
    }

    /// A netrc file `name` in `dir` whose GitHub entry GitHub refuses, named
    /// by `NETRC`. Each new file is an entry that uvr has not refused yet.
    #[cfg(not(target_os = "windows"))]
    fn stale_netrc(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, "machine github.com login me password stale-tok\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        std::env::set_var("NETRC", &path);
        path
    }

    // #187: a netrc entry that GitHub refuses (an expired token, say) does
    // not break a public repository: uvr drops the entry for the run and
    // fetches without credentials, as before #186. A variable's token is
    // never dropped.
    #[cfg(not(target_os = "windows"))]
    #[test]
    fn refused_netrc_token_falls_back_to_anonymous() {
        use crate::auth::{test_authorization, test_path, GitEnv};

        let _env = GitEnv::new(&[]);
        let rt = current_thread();
        // A public repository.
        let (_, api_seen, _) = fake_github(|head, codeload| match test_authorization(head) {
            Some(_) => refused(test_path(head)),
            None => github_route(test_path(head), codeload),
        });
        let dir = tempfile::tempdir().unwrap();
        let client = reqwest::Client::new();
        let resolve = || rt.block_on(resolve_github_package(&client, "owner", "privpkg", "main"));
        let description = |repo: &str| {
            rt.block_on(resolve_github_package_with_remote_entries_at_commit(
                &client, "owner", repo, SHA, None,
            ))
        };
        // The `Authorization` of each request since the last call.
        let sent = || -> Vec<Option<String>> {
            std::mem::take(&mut *api_seen.lock().unwrap())
                .iter()
                .map(|head| test_authorization(head).map(str::to_string))
                .collect()
        };
        let stale = || Some("Bearer stale-tok".to_string());

        // The commit API refuses the entry with 401: retried without it,
        // and not sent again in this run.
        stale_netrc(dir.path(), "a");
        let info = resolve().expect("the public repository resolves without the entry");
        assert_eq!(sent(), [stale(), None, None]);

        // raw.githubusercontent.com refuses it with 404: the same.
        stale_netrc(dir.path(), "b");
        description("privpkg").expect("DESCRIPTION without the entry");
        assert_eq!(sent(), [stale(), None]);
        description("privpkg").unwrap();
        assert_eq!(sent(), [None]);

        // A 404 that is a missing file does not drop the entry.
        stale_netrc(dir.path(), "c");
        let err = description("missing").unwrap_err().to_string();
        assert!(err.contains("HTTP 404"), "{err}");
        assert_eq!(sent(), [stale(), None]);
        description("privpkg").unwrap();
        assert_eq!(sent(), [stale(), None]);

        // The tarball download falls back the same way.
        stale_netrc(dir.path(), "d");
        let tarball = crate::installer::download::test_download(&rt, &info)
            .expect("the public tarball downloads without the entry");
        assert_eq!(tarball, b"private tarball");
        assert_eq!(sent(), [stale(), None]);

        // A token from a variable is not dropped: the 401 is the result.
        std::env::set_var("GITHUB_PAT", "stale-env");
        let err = resolve().unwrap_err().to_string();
        assert!(err.contains("refused the token in GITHUB_PAT"), "{err}");
        assert_eq!(sent(), [Some("Bearer stale-env".to_string())]);
    }

    // A private repository with a refused netrc entry: the anonymous retry
    // gets 404, so the error is the 401, and it names the entry.
    #[cfg(not(target_os = "windows"))]
    #[test]
    fn refused_netrc_token_on_a_private_repository_names_the_entry() {
        use crate::auth::{test_authorization, test_path, test_response, GitEnv};

        let _env = GitEnv::new(&[]);
        let rt = current_thread();
        let (_, api_seen, _) = fake_github(|head, _| match test_authorization(head) {
            Some(_) => refused(test_path(head)),
            None => test_response("404 Not Found", "", b""),
        });
        let dir = tempfile::tempdir().unwrap();
        let path = stale_netrc(dir.path(), "netrc");

        let client = reqwest::Client::new();
        let err = rt
            .block_on(resolve_github_package(&client, "owner", "privpkg", "main"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("401 Unauthorized"), "{err}");
        assert!(
            err.contains("refused the password of the `machine github.com` entry in")
                && err.contains(&path.display().to_string()),
            "{err}"
        );
        assert!(!err.contains("stale-tok"), "{err}");
        assert_eq!(api_seen.lock().unwrap().len(), 2, "one retry");
    }
}
