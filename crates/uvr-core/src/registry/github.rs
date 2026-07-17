use semver::Version;
use tracing::debug;

use crate::error::{Result, UvrError};
use crate::lockfile::PackageSource;
use crate::manifest::DependencySpec;
use crate::registry::{Dep, PackageInfo};

/// A github-sourced dependency declared in another package's `Remotes:`
/// field. `(dep_name, "user/repo", optional_ref)`. Used by the lock
/// command to walk transitive `Remotes:` chains so a github package
/// whose DESCRIPTION names another github package via `Remotes:` can
/// be resolved without falling through to CRAN (#84).
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
/// Thin wrapper that drops the `Remotes:` chain. Callers that need to
/// walk transitive github→github deps should use
/// [`resolve_github_package_with_remotes`] instead.
pub async fn resolve_github_package(
    client: &reqwest::Client,
    user: &str,
    repo: &str,
    git_ref: &str,
) -> Result<PackageInfo> {
    resolve_github_package_with_remotes(client, user, repo, git_ref)
        .await
        .map(|(info, _)| info)
}

/// Resolve a GitHub package and return its `Remotes:`-declared github deps.
///
/// `Remotes:` entries in a package's DESCRIPTION tell R-tooling that a dep
/// listed in `Imports:`/`Depends:` should be installed from a non-CRAN
/// source. Without honouring them, uvr's resolver falls through to CRAN
/// for every transitive dep and bails on github-only packages with
/// "Package not found" (#84).
pub async fn resolve_github_package_with_remotes(
    client: &reqwest::Client,
    user: &str,
    repo: &str,
    git_ref: &str,
) -> Result<(PackageInfo, Vec<GithubRemote>)> {
    let commit_sha = fetch_commit_sha(client, user, repo, git_ref).await?;

    let desc_url =
        format!("https://raw.githubusercontent.com/{user}/{repo}/{commit_sha}/DESCRIPTION");
    let mut desc_req = client
        .get(&desc_url)
        .header("User-Agent", concat!("uvr/", env!("CARGO_PKG_VERSION")));
    if let Some(tok) = github_token() {
        desc_req = desc_req.bearer_auth(tok);
    }
    let desc_resp = desc_req.send().await?;
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

    // Parse `Remotes:` so the caller can recurse into github→github chains.
    let remotes = parse_github_remotes(&desc_fields);

    let url = format!("https://api.github.com/repos/{user}/{repo}/tarball/{commit_sha}");

    debug!("GitHub {user}/{repo}@{git_ref} → {pkg_name} {version} ({commit_sha})");

    Ok((
        PackageInfo {
            name: pkg_name,
            version,
            source: PackageSource::GitHub,
            checksum: Some(format!("git:{commit_sha}")),
            requires,
            url,
            raw_version: None, // GitHub packages don't have CRAN-style dash versions
            system_requirements: None,
        },
        remotes,
    ))
}

/// Pull github-sourced entries out of a DESCRIPTION's `Remotes:` field.
///
/// Reuses the manifest module's `Remotes:` parser so syntax handled there
/// (`user/repo`, `user/repo@ref`, `github::user/repo`, `pkgname=user/repo`)
/// stays consistent. Non-github prefixes (`bioc::`, `gitlab::`, `url::`,
/// etc.) are filtered out by the manifest parser before we see them.
fn parse_github_remotes(
    desc_fields: &std::collections::BTreeMap<String, String>,
) -> Vec<GithubRemote> {
    let Some(remotes_field) = desc_fields.get("Remotes") else {
        return Vec::new();
    };
    crate::manifest::parse_remotes_field(remotes_field)
        .into_iter()
        .filter_map(|(name, spec)| match spec {
            DependencySpec::Detailed(d) => d.git.map(|repo| (name, repo, d.rev)),
            DependencySpec::Version(_) => None,
        })
        .collect()
}

async fn fetch_commit_sha(
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
    let url = format!("https://api.github.com/repos/{user}/{repo}/commits/{encoded_ref}");
    let mut req = client
        .get(&url)
        .header("User-Agent", concat!("uvr/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github.sha");
    if let Some(tok) = github_token() {
        req = req.bearer_auth(tok);
    }
    let resp = req.send().await?;

    if !resp.status().is_success() {
        return Err(UvrError::Other(format!(
            "GitHub API error for {user}/{repo}@{git_ref}: {}",
            resp.status()
        )));
    }

    let sha = resp.text().await?;
    Ok(sha.trim().trim_matches('"').to_string())
}

/// Look up a GitHub API token to attach to requests. Reads `GITHUB_PAT`
/// first (renv/devtools convention) and falls back to `GITHUB_TOKEN`
/// (Actions / generic CI convention). Without a token GitHub's
/// unauthenticated rate limit is 60 req/hr shared by everyone behind
/// the same egress IP — easy to exhaust on a CI runner walking an
/// `renv.lock` with several github deps (#95).
fn github_token() -> Option<String> {
    for var in ["GITHUB_PAT", "GITHUB_TOKEN"] {
        if let Ok(v) = std::env::var(var) {
            let t = v.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    None
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
        let remotes = parse_github_remotes(&fields);
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
        let remotes = parse_github_remotes(&fields);
        let names: Vec<&str> = remotes.iter().map(|(n, _, _)| n.as_str()).collect();
        // bioc:: and gitlab:: get filtered by the shared parser.
        assert_eq!(names, vec!["a", "b"]);
        assert_eq!(remotes[0].2.as_deref(), Some("v1.0.0"));
        assert_eq!(remotes[1].2.as_deref(), Some("main"));
    }

    #[test]
    fn parse_github_remotes_missing_field() {
        let desc = "Package: x\nVersion: 0.0.1\nImports: foo\n";
        let fields = crate::dcf::parse_dcf_fields(desc);
        assert!(parse_github_remotes(&fields).is_empty());
    }
}
