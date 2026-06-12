use semver::Version;
use tracing::debug;

use crate::error::{Result, UvrError};
use crate::lockfile::PackageSource;
use crate::manifest::DependencySpec;
use crate::registry::{Dep, PackageInfo};

/// A forgejo-sourced dependency declared in another package's `Remotes:`
/// field. `(dep_name, "forgejo::host/owner/repo", optional_ref)`.
pub type ForgejoRemote = (String, String, Option<String>);

/// A validated Forgejo spec. `git_ref` is `None` when the spec carried no
/// `@ref` segment — callers default it as they see fit (e.g. the registry
/// resolver uses `"HEAD"`, the manifest/CLI parsers keep `None`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgejoSpec {
    pub host: String,
    pub owner: String,
    pub repo: String,
    pub git_ref: Option<String>,
}

/// Parse and validate `"[forgejo::]host/owner/repo[@ref]"` into structured
/// parts. This is the single source of truth for the Forgejo spec shape (#108)
/// — the CLI (`add`), the manifest `Remotes:` parser, and the registry
/// resolver all funnel through it so they accept and reject identical inputs.
///
/// Accepts:
/// - `forgejo::codefloe.com/pat-s/mypkg@v0.1.0`
/// - `codefloe.com/pat-s/mypkg` (no ref → `git_ref = None`)
/// - `git.local:3000/u/r` (port allowed)
///
/// Rejects:
/// - hosts containing a scheme (`https://...`)
/// - empty host, owner, or repo segments
/// - anything other than exactly three path segments
/// - host chars outside `[alnum].-:` or owner/repo chars outside `[alnum].-_`
pub fn parse_forgejo_parts(spec: &str) -> Option<ForgejoSpec> {
    let body = spec.strip_prefix("forgejo::").unwrap_or(spec);

    let (path_part, git_ref) = match body.rfind('@') {
        Some(at) => {
            let r = &body[at + 1..];
            let git_ref = if r.is_empty() {
                None
            } else {
                Some(r.to_string())
            };
            (&body[..at], git_ref)
        }
        None => (body, None),
    };

    if path_part.contains("://") {
        return None;
    }

    let parts: Vec<&str> = path_part.split('/').collect();
    if parts.len() != 3 {
        return None;
    }
    let (host, owner, repo) = (parts[0], parts[1], parts[2]);
    if host.is_empty() || owner.is_empty() || repo.is_empty() {
        return None;
    }
    // Host shape: letters, digits, dot, hyphen, optional :port. Owner/repo
    // shape: letters, digits, dot, hyphen, underscore. Anything else is a
    // user error worth catching before we make a request.
    let host_ok = host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'));
    let seg_ok = |s: &str| {
        s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    };
    if !host_ok || !seg_ok(owner) || !seg_ok(repo) {
        return None;
    }

    Some(ForgejoSpec {
        host: host.to_string(),
        owner: owner.to_string(),
        repo: repo.to_string(),
        git_ref,
    })
}

/// Parse `"forgejo::host/owner/repo[@ref]"` into `(host, owner, repo, ref)`,
/// defaulting a missing ref to `"HEAD"`. Thin wrapper over
/// [`parse_forgejo_parts`] for the registry resolver / BFS, which want a
/// concrete ref to query.
pub fn parse_forgejo_spec(spec: &str) -> Option<(String, String, String, String)> {
    let p = parse_forgejo_parts(spec)?;
    Some((
        p.host,
        p.owner,
        p.repo,
        p.git_ref.unwrap_or_else(|| "HEAD".to_string()),
    ))
}

/// Look up a Forgejo API token from the environment.
///
/// Lookup order:
/// 1. `UVR_FORGEJO_TOKEN_<NORMALIZED_HOST>` — per-host.
/// 2. `UVR_FORGEJO_TOKEN` — single token for users with one instance.
///
/// Host normalization: strip `:port`, uppercase, replace `.` and `-`
/// with `_`. E.g. `codefloe.com` → `CODEFLOE_COM`, `git.local:3000` →
/// `GIT_LOCAL`. Whitespace-only env values are treated as unset so a
/// shell that exports `UVR_FORGEJO_TOKEN=` doesn't fail authenticated
/// requests with a literal empty bearer.
pub fn forgejo_token(host: &str) -> Option<String> {
    let host_no_port = host.split(':').next().unwrap_or(host);
    let normalized: String = host_no_port
        .to_ascii_uppercase()
        .chars()
        .map(|c| if c == '.' || c == '-' { '_' } else { c })
        .collect();
    let per_host = format!("UVR_FORGEJO_TOKEN_{normalized}");
    for var in [per_host.as_str(), "UVR_FORGEJO_TOKEN"] {
        if let Ok(v) = std::env::var(var) {
            let t = v.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    None
}

/// Resolve a Forgejo-hosted R package: fetch commit SHA, fetch DESCRIPTION,
/// build a tarball URL for the lockfile.
pub async fn resolve_forgejo_package(
    client: &reqwest::Client,
    host: &str,
    owner: &str,
    repo: &str,
    git_ref: &str,
) -> Result<PackageInfo> {
    resolve_forgejo_package_with_remotes(client, host, owner, repo, git_ref)
        .await
        .map(|(info, _)| info)
}

/// Resolve a Forgejo package and return its `Remotes:`-declared forgejo deps,
/// so callers can walk transitive forgejo→forgejo chains during `uvr lock`.
pub async fn resolve_forgejo_package_with_remotes(
    client: &reqwest::Client,
    host: &str,
    owner: &str,
    repo: &str,
    git_ref: &str,
) -> Result<(PackageInfo, Vec<ForgejoRemote>)> {
    let commit_sha = fetch_commit_sha(client, host, owner, repo, git_ref).await?;

    let desc_url =
        format!("https://{host}/api/v1/repos/{owner}/{repo}/raw/DESCRIPTION?ref={commit_sha}");
    let mut desc_req = client
        .get(&desc_url)
        .header("User-Agent", concat!("uvr/", env!("CARGO_PKG_VERSION")));
    if let Some(tok) = forgejo_token(host) {
        desc_req = desc_req.header("Authorization", format!("token {tok}"));
    }
    let desc_resp = desc_req.send().await?;
    if !desc_resp.status().is_success() {
        return Err(map_forgejo_error(
            desc_resp.status(),
            host,
            owner,
            repo,
            &commit_sha,
        ));
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

    let requires = parse_description_deps(&desc_fields);
    let remotes = parse_forgejo_remotes(&desc_fields);

    let url = format!("https://{host}/api/v1/repos/{owner}/{repo}/archive/{commit_sha}.tar.gz");

    debug!("Forgejo {host}/{owner}/{repo}@{git_ref} → {pkg_name} {version} ({commit_sha})");

    Ok((
        PackageInfo {
            name: pkg_name,
            version,
            source: PackageSource::Forgejo {
                host: host.to_string(),
            },
            checksum: Some(format!("git:{commit_sha}")),
            requires,
            url,
            raw_version: None,
            system_requirements: None,
        },
        remotes,
    ))
}

async fn fetch_commit_sha(
    client: &reqwest::Client,
    host: &str,
    owner: &str,
    repo: &str,
    git_ref: &str,
) -> Result<String> {
    // Forgejo's `/commits/{ref}` endpoint 404s (it exists in Gitea's API
    // surface but Forgejo's HTTP routing rejects it). The list-commits
    // endpoint with `?sha=<ref>&limit=1` is the supported way to resolve
    // a ref to a SHA — it accepts branches, tags, and SHAs and returns a
    // JSON array of commit objects.
    let url = format!("https://{host}/api/v1/repos/{owner}/{repo}/commits?sha={git_ref}&limit=1");
    let mut req = client
        .get(&url)
        .header("User-Agent", concat!("uvr/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/json");
    if let Some(tok) = forgejo_token(host) {
        req = req.header("Authorization", format!("token {tok}"));
    }
    let resp = req.send().await?;

    if !resp.status().is_success() {
        return Err(map_forgejo_error(resp.status(), host, owner, repo, git_ref));
    }

    #[derive(serde::Deserialize)]
    struct CommitObj {
        sha: String,
    }
    let body = resp.text().await?;
    let commits: Vec<CommitObj> = serde_json::from_str(&body).map_err(|e| {
        UvrError::Other(format!(
            "Forgejo {host}/{owner}/{repo}@{git_ref}: could not parse commit list JSON ({e}). Body: {}",
            body.chars().take(200).collect::<String>()
        ))
    })?;
    commits.into_iter().next().map(|c| c.sha).ok_or_else(|| {
        UvrError::Other(format!(
            "Forgejo {host}/{owner}/{repo}@{git_ref}: commit list was empty. The ref may not exist."
        ))
    })
}

fn map_forgejo_error(
    status: reqwest::StatusCode,
    host: &str,
    owner: &str,
    repo: &str,
    ref_or_sha: &str,
) -> UvrError {
    match status.as_u16() {
        401 | 403 => UvrError::Other(format!(
            "Forgejo returned {status} for {host}/{owner}/{repo}; \
             set UVR_FORGEJO_TOKEN_<HOST> if the repo is private."
        )),
        404 => UvrError::Other(format!(
            "Forgejo repository not found: {host}/{owner}/{repo}@{ref_or_sha}. \
             Check the spec and that the repo exists."
        )),
        _ => UvrError::Other(format!(
            "Forgejo error for {host}/{owner}/{repo}@{ref_or_sha}: HTTP {status}"
        )),
    }
}

fn parse_forgejo_remotes(
    desc_fields: &std::collections::BTreeMap<String, String>,
) -> Vec<ForgejoRemote> {
    // Return ALL git-bearing entries — both `git = "user/repo"` (github)
    // and `git = "forgejo::host/owner/repo"` (forgejo). The lock-time BFS
    // dispatches by prefix via `classify_git`, so a forgejo package whose
    // DESCRIPTION declares `Remotes: github::user/repo` walks correctly
    // into the github registry. Mirrors `github::parse_github_remotes`.
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

/// Same shape as `github::parse_description_deps` — small enough that
/// reusing it via a shared helper isn't worth the cross-module coupling.
fn parse_description_deps(fields: &std::collections::BTreeMap<String, String>) -> Vec<Dep> {
    let mut deps = Vec::new();
    for field in &["Imports", "Depends"] {
        if let Some(value) = fields.get(*field) {
            for d in crate::registry::cran::parse_dep_field(value) {
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
    fn parse_spec_happy() {
        let (host, owner, repo, git_ref) =
            parse_forgejo_spec("forgejo::codefloe.com/pat-s/mypkg@v0.1.0").unwrap();
        assert_eq!(host, "codefloe.com");
        assert_eq!(owner, "pat-s");
        assert_eq!(repo, "mypkg");
        assert_eq!(git_ref, "v0.1.0");
    }

    #[test]
    fn parse_spec_default_ref() {
        let (_h, _o, _r, git_ref) =
            parse_forgejo_spec("forgejo::codefloe.com/pat-s/mypkg").unwrap();
        assert_eq!(git_ref, "HEAD");
    }

    #[test]
    fn parse_spec_with_port() {
        let (host, _, _, _) = parse_forgejo_spec("forgejo::git.local:3000/u/r").unwrap();
        assert_eq!(host, "git.local:3000");
    }

    #[test]
    fn parse_spec_accepts_unprefixed() {
        // Callers (lock.rs BFS) may strip the prefix before calling us.
        let parsed = parse_forgejo_spec("codefloe.com/pat-s/mypkg@main").unwrap();
        assert_eq!(parsed.0, "codefloe.com");
        assert_eq!(parsed.3, "main");
    }

    #[test]
    fn parse_spec_rejects_scheme_in_host() {
        assert!(parse_forgejo_spec("forgejo::https://codefloe.com/u/r").is_none());
    }

    #[test]
    fn parse_spec_rejects_wrong_segment_count() {
        assert!(parse_forgejo_spec("forgejo::codefloe.com/u").is_none());
        assert!(parse_forgejo_spec("forgejo::codefloe.com/u/r/extra").is_none());
    }

    #[test]
    fn parse_spec_rejects_empty_segments() {
        assert!(parse_forgejo_spec("forgejo:://u/r").is_none());
        assert!(parse_forgejo_spec("forgejo::codefloe.com//r").is_none());
        assert!(parse_forgejo_spec("forgejo::codefloe.com/u/").is_none());
    }

    #[test]
    fn parts_ref_is_none_when_absent_head_when_via_spec() {
        // The shared core keeps "no ref" as None; the spec wrapper defaults it
        // to HEAD for the resolver. This is the distinction add/manifest rely on.
        let p = parse_forgejo_parts("forgejo::codefloe.com/pat-s/mypkg").unwrap();
        assert_eq!(p.git_ref, None);
        assert_eq!(p.repo, "mypkg");
        assert_eq!(
            parse_forgejo_spec("forgejo::codefloe.com/pat-s/mypkg")
                .unwrap()
                .3,
            "HEAD"
        );

        let p = parse_forgejo_parts("forgejo::codefloe.com/pat-s/mypkg@v1.0").unwrap();
        assert_eq!(p.git_ref.as_deref(), Some("v1.0"));
    }

    #[test]
    fn parts_validates_owner_and_repo_chars() {
        // Unified host + owner + repo validation (#108): a segment with shell
        // metacharacters is rejected, not silently accepted as it was by the
        // pre-consolidation parsers that skipped owner/repo checks.
        assert!(parse_forgejo_parts("forgejo::codefloe.com/pat-s/my;rm -rf").is_none());
        assert!(parse_forgejo_parts("forgejo::codefloe.com/own$er/mypkg").is_none());
        // Underscores, dots, hyphens in owner/repo stay valid.
        assert!(parse_forgejo_parts("forgejo::codefloe.com/pat-s/my_pkg.v2").is_some());
    }

    // All token lookup tests are combined into a single test to avoid races
    // from env-mutation across parallel test threads (std::env is global).
    #[test]
    fn token_lookup() {
        // --- sub-test: per-host var takes precedence over global ---
        let host = "lookup-test-host.example";
        let per_host_var = "UVR_FORGEJO_TOKEN_LOOKUP_TEST_HOST_EXAMPLE";
        std::env::set_var(per_host_var, "host-specific");
        std::env::set_var("UVR_FORGEJO_TOKEN", "global");
        assert_eq!(forgejo_token(host).as_deref(), Some("host-specific"));
        std::env::remove_var(per_host_var);
        assert_eq!(forgejo_token(host).as_deref(), Some("global"));
        std::env::remove_var("UVR_FORGEJO_TOKEN");
        assert_eq!(forgejo_token(host), None);

        // --- sub-test: port is stripped before normalization ---
        // Port is stripped before normalization so the env var name is
        // stable across `host` vs `host:port`.
        std::env::set_var("UVR_FORGEJO_TOKEN_GIT_LOCAL", "t");
        assert_eq!(forgejo_token("git.local:3000").as_deref(), Some("t"));
        std::env::remove_var("UVR_FORGEJO_TOKEN_GIT_LOCAL");

        // --- sub-test: whitespace-only values are treated as unset ---
        std::env::set_var("UVR_FORGEJO_TOKEN", "   ");
        assert_eq!(forgejo_token("any.host").as_deref(), None);
        std::env::remove_var("UVR_FORGEJO_TOKEN");
    }

    #[test]
    fn parse_forgejo_remotes_keeps_all_git_bearing_entries() {
        // A forgejo package's DESCRIPTION may declare git-bearing Remotes
        // pointing at either registry. We pass them all through; the
        // lock-time BFS dispatches per-prefix via classify_git.
        let desc = "\
Package: x
Version: 0.1.0
Remotes: forgejo::codefloe.com/pat-s/mypkg@v0.1.0,
    github::user/other,
    gitlab::someone/skipme
";
        let fields = crate::dcf::parse_dcf_fields(desc);
        let remotes = parse_forgejo_remotes(&fields);
        let pairs: Vec<(&str, &str)> = remotes
            .iter()
            .map(|(n, g, _)| (n.as_str(), g.as_str()))
            .collect();
        // gitlab:: is dropped by parse_remotes_field; forgejo and github survive.
        assert_eq!(
            pairs,
            vec![
                ("mypkg", "forgejo::codefloe.com/pat-s/mypkg"),
                ("other", "user/other"),
            ]
        );
        // The forgejo entry still carries its ref.
        assert_eq!(remotes[0].2.as_deref(), Some("v0.1.0"));
    }
}
