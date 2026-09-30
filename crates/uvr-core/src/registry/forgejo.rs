use semver::Version;
use tracing::debug;

use crate::auth::{git_origin, GitHost};
use crate::error::{Result, UvrError};
use crate::lockfile::PackageSource;
use crate::registry::PackageInfo;

/// Legacy tuple form for a Forgejo-sourced `Remotes:` dependency:
/// `(dep_name, "forgejo::host/owner/repo", optional_ref)`.
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
/// - refs containing whitespace or URL/git metacharacters (`& # ? * ...`),
///   per [`crate::registry::github::is_valid_git_ref`] (#152)
pub fn parse_forgejo_parts(spec: &str) -> Option<ForgejoSpec> {
    let body = spec.strip_prefix("forgejo::").unwrap_or(spec);

    let (path_part, git_ref) = match body.rfind('@') {
        Some(at) => {
            let r = &body[at + 1..];
            if !crate::registry::github::is_valid_git_ref(r) {
                return None;
            }
            (&body[..at], Some(r.to_string()))
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

/// Resolve a Forgejo-hosted R package while deliberately discarding rich
/// `Remotes:` entries before the fallible legacy-tuple adapter.
pub async fn resolve_forgejo_package(
    client: &reqwest::Client,
    host: &str,
    owner: &str,
    repo: &str,
    git_ref: &str,
) -> Result<PackageInfo> {
    resolve_forgejo_package_with_remote_entries(client, host, owner, repo, git_ref)
        .await
        .map(|(info, _)| info)
}

/// Legacy tuple adapter for a Forgejo package's `Remotes:` entries. Nested or
/// bound-unsupported entries return an error rather than losing identity.
pub async fn resolve_forgejo_package_with_remotes(
    client: &reqwest::Client,
    host: &str,
    owner: &str,
    repo: &str,
    git_ref: &str,
) -> Result<(PackageInfo, Vec<ForgejoRemote>)> {
    let (info, remotes) =
        resolve_forgejo_package_with_remote_entries(client, host, owner, repo, git_ref).await?;
    Ok((info, compatible_remotes(remotes)?))
}

pub async fn resolve_forgejo_package_with_remote_entries(
    client: &reqwest::Client,
    host: &str,
    owner: &str,
    repo: &str,
    git_ref: &str,
) -> Result<(PackageInfo, Vec<crate::manifest::RemoteEntry>)> {
    resolve_forgejo_package_with_remote_entries_bound(client, host, owner, repo, git_ref, false)
        .await
}

pub async fn resolve_forgejo_package_with_remote_entries_bound(
    client: &reqwest::Client,
    host: &str,
    owner: &str,
    repo: &str,
    git_ref: &str,
    require_declared_name: bool,
) -> Result<(PackageInfo, Vec<crate::manifest::RemoteEntry>)> {
    let commit_sha = fetch_commit_sha(client, host, owner, repo, git_ref).await?;
    resolve_forgejo_package_with_remote_entries_at_commit_bound(
        client,
        host,
        owner,
        repo,
        &commit_sha,
        require_declared_name,
    )
    .await
}

pub async fn resolve_forgejo_package_with_remote_entries_at_commit_bound(
    client: &reqwest::Client,
    host: &str,
    owner: &str,
    repo: &str,
    commit_sha: &str,
    require_declared_name: bool,
) -> Result<(PackageInfo, Vec<crate::manifest::RemoteEntry>)> {
    let (info, remotes, _) =
        resolve_forgejo_package_with_remote_entries_and_install_dependencies_at_commit_bound(
            client,
            host,
            owner,
            repo,
            commit_sha,
            require_declared_name,
        )
        .await?;
    Ok((info, remotes))
}

/// Resolve a Forgejo package while also returning every install-time
/// DESCRIPTION dependency name used to bind nested GitHub `Remotes:` entries.
/// `PackageInfo::requires` deliberately retains Forgejo's existing
/// Imports/Depends-only behavior.
pub async fn resolve_forgejo_package_with_remote_entries_and_install_dependencies_at_commit_bound(
    client: &reqwest::Client,
    host: &str,
    owner: &str,
    repo: &str,
    commit_sha: &str,
    require_declared_name: bool,
) -> Result<(
    PackageInfo,
    Vec<crate::manifest::RemoteEntry>,
    std::collections::BTreeSet<String>,
)> {
    let origin = git_origin(host);
    let desc_url = format!("{origin}/api/v1/repos/{owner}/{repo}/raw/DESCRIPTION?ref={commit_sha}");
    let desc_req = client
        .get(&desc_url)
        .header("User-Agent", concat!("uvr/", env!("CARGO_PKG_VERSION")));
    let desc_resp = GitHost::Forgejo(host).send(desc_req).await?;
    if !desc_resp.status().is_success() {
        return Err(map_forgejo_error(
            desc_resp.status(),
            host,
            owner,
            repo,
            commit_sha,
        ));
    }
    let desc_text = desc_resp.text().await?;

    let desc_fields = crate::dcf::parse_dcf_fields(&desc_text);
    let pkg_name = description_package_name(
        &desc_fields,
        host,
        owner,
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

    let requires = crate::registry::github::parse_description_runtime_deps(&desc_fields);
    let install_dependencies =
        crate::registry::github::parse_description_install_dependency_names(&desc_fields);
    let remotes = parse_forgejo_remote_entries(&desc_fields);

    let url = format!("{origin}/api/v1/repos/{owner}/{repo}/archive/{commit_sha}.tar.gz");

    debug!("Forgejo {host}/{owner}/{repo}@{commit_sha} → {pkg_name} {version}");

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
            subdirectory: None,
        },
        remotes,
        install_dependencies,
    ))
}

fn description_package_name(
    fields: &std::collections::BTreeMap<String, String>,
    host: &str,
    owner: &str,
    repo: &str,
    commit_sha: &str,
    require_declared_name: bool,
) -> Result<String> {
    if !require_declared_name {
        return Ok(fields
            .get("Package")
            .cloned()
            .unwrap_or_else(|| repo.to_string()));
    }
    let name = fields
        .get("Package")
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            UvrError::Other(format!(
                "DESCRIPTION for Forgejo {host}/{owner}/{repo}@{commit_sha} has no `Package:` \
                 field; cannot validate the bound package identity."
            ))
        })?;
    if !crate::package_name::is_valid(&name) {
        return Err(UvrError::Other(format!(
            "DESCRIPTION for Forgejo {host}/{owner}/{repo}@{commit_sha} declares an invalid \
             `Package:` name '{name}'."
        )));
    }
    Ok(name)
}

pub async fn fetch_commit_sha(
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
    // Percent-encode the ref: it sits in query position (`sha=feat&x`
    // would inject a second parameter), and refs can reach here without
    // going through `parse_forgejo_parts` (lockfile revs, `Remotes:`
    // fields), so encode defensively (#152).
    let encoded_ref = urlencoding::encode(git_ref);
    let url = format!(
        "{}/api/v1/repos/{owner}/{repo}/commits?sha={encoded_ref}&limit=1",
        git_origin(host)
    );
    let req = client
        .get(&url)
        .header("User-Agent", concat!("uvr/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/json");
    let resp = GitHost::Forgejo(host).send(req).await?;

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
            "Forgejo returned {status} for {host}/{owner}/{repo}; {}",
            GitHost::Forgejo(host).denied_advice()
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

#[cfg(test)]
fn parse_forgejo_remotes(
    desc_fields: &std::collections::BTreeMap<String, String>,
) -> Result<Vec<ForgejoRemote>> {
    compatible_remotes(parse_forgejo_remote_entries(desc_fields))
}

fn parse_forgejo_remote_entries(
    desc_fields: &std::collections::BTreeMap<String, String>,
) -> Vec<crate::manifest::RemoteEntry> {
    let Some(remotes_field) = desc_fields.get("Remotes") else {
        return Vec::new();
    };
    crate::manifest::parse_remotes_field_rich(remotes_field)
}

fn compatible_remotes(entries: Vec<crate::manifest::RemoteEntry>) -> Result<Vec<ForgejoRemote>> {
    crate::manifest::compatible_remote_entries(entries)
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
        assert!(parse_forgejo_parts("forgejo::codefloe.com/pat-s/mypkg@").is_none());
    }

    #[test]
    fn install_dependency_names_include_linking_to_without_changing_requires() {
        let fields = crate::dcf::parse_dcf_fields(
            "Package: parent\nImports: imported\nDepends: depended\nLinkingTo: headers\n",
        );
        let requires: Vec<String> =
            crate::registry::github::parse_description_runtime_deps(&fields)
                .into_iter()
                .map(|dependency| dependency.name)
                .collect();
        let install_dependencies =
            crate::registry::github::parse_description_install_dependency_names(&fields);

        assert_eq!(requires, ["imported", "depended"]);
        assert_eq!(
            install_dependencies,
            ["depended", "headers", "imported"]
                .into_iter()
                .map(str::to_string)
                .collect()
        );
    }

    #[test]
    fn bound_package_name_requires_present_legal_description_field() {
        let missing = crate::dcf::parse_dcf_fields("Version: 1.0.0\n");
        let error =
            description_package_name(&missing, "code.example", "team", "repo", "commit", true)
                .unwrap_err()
                .to_string();
        assert!(error.contains("has no `Package:` field"), "{error}");

        let invalid = crate::dcf::parse_dcf_fields("Package: bad name\nVersion: 1.0.0\n");
        let error =
            description_package_name(&invalid, "code.example", "team", "repo", "commit", true)
                .unwrap_err()
                .to_string();
        assert!(error.contains("invalid `Package:` name"), "{error}");

        let valid = crate::dcf::parse_dcf_fields("Package: Alias\nVersion: 1.0.0\n");
        assert_eq!(
            description_package_name(&valid, "code.example", "team", "repo", "commit", true,)
                .unwrap(),
            "Alias"
        );
    }

    #[test]
    fn unbound_package_name_keeps_repository_fallback() {
        let missing = crate::dcf::parse_dcf_fields("Version: 1.0.0\n");
        assert_eq!(
            description_package_name(&missing, "code.example", "team", "repo", "commit", false,)
                .unwrap(),
            "repo"
        );
    }

    #[test]
    fn parts_validates_ref_chars() {
        // #152: a ref with `&` would inject a second query parameter into
        // `?sha={ref}&limit=1`; `#`, `?`, and whitespace break the URL too.
        assert!(parse_forgejo_parts("forgejo::codefloe.com/u/r@feat&x").is_none());
        assert!(parse_forgejo_parts("forgejo::codefloe.com/u/r@v1#frag").is_none());
        assert!(parse_forgejo_parts("forgejo::codefloe.com/u/r@a b").is_none());
        assert!(parse_forgejo_parts("forgejo::codefloe.com/u/r@x?y").is_none());
        // Legitimate ref shapes still pass, including slashed branches.
        assert_eq!(
            parse_forgejo_parts("forgejo::codefloe.com/u/r@main")
                .unwrap()
                .git_ref
                .as_deref(),
            Some("main")
        );
        assert_eq!(
            parse_forgejo_parts("forgejo::codefloe.com/u/r@v1.2.3")
                .unwrap()
                .git_ref
                .as_deref(),
            Some("v1.2.3")
        );
        assert_eq!(
            parse_forgejo_parts("forgejo::codefloe.com/u/r@feature/x")
                .unwrap()
                .git_ref
                .as_deref(),
            Some("feature/x")
        );
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

    // #187: a private repository resolves and downloads with the host's
    // token from the shared resolver, sent as Forgejo's `token` header, on
    // Forgejo's API paths.
    #[cfg(not(target_os = "windows"))]
    #[test]
    fn private_repository_resolves_and_downloads() {
        use crate::auth::{
            test_authorization, test_git_origin, test_private_git_host, test_response, GitEnv,
        };

        const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
        let _env = GitEnv::new(&["UVR_FORGEJO_TOKEN", "UVR_FORGEJO_TOKEN_FORGEJO_TEST"]);
        std::env::set_var("UVR_FORGEJO_TOKEN_FORGEJO_TEST", "fj-tok");
        let (origin, seen) = test_private_git_host("token fj-tok", |path| {
            let ok = |body: &[u8]| test_response("200 OK", "", body);
            match path.strip_prefix("/api/v1/repos/team/privpkg/") {
                Some("commits?sha=main&limit=1") => {
                    ok(format!(r#"[{{"sha":"{SHA}"}}]"#).as_bytes())
                }
                Some(p) if p == format!("raw/DESCRIPTION?ref={SHA}") => {
                    ok(b"Package: privpkg\nVersion: 1.2.3\n")
                }
                Some(p) if p == format!("archive/{SHA}.tar.gz") => ok(b"private tarball"),
                _ => test_response("404 Not Found", "", b""),
            }
        });
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        test_git_origin("forgejo.test:3000", &origin);
        let client = reqwest::Client::new();
        let resolve = || {
            rt.block_on(resolve_forgejo_package(
                &client,
                "forgejo.test:3000",
                "team",
                "privpkg",
                "main",
            ))
        };

        let info = resolve().expect("the token opens the private repository");
        assert_eq!(
            (info.name.as_str(), info.version.to_string()),
            ("privpkg", "1.2.3".into())
        );
        assert_eq!(
            info.url,
            format!("{origin}/api/v1/repos/team/privpkg/archive/{SHA}.tar.gz")
        );
        let tarball = crate::installer::download::test_download(&rt, &info)
            .expect("the tarball downloads with the token");
        assert_eq!(tarball, b"private tarball");
        let heads = seen.lock().unwrap().clone();
        assert_eq!(heads.len(), 3, "{heads:?}");
        for head in &heads {
            assert_eq!(test_authorization(head), Some("token fj-tok"), "{head}");
        }

        // Without a token the repository is hidden; a wrong one is refused.
        std::env::remove_var("UVR_FORGEJO_TOKEN_FORGEJO_TEST");
        let err = resolve().unwrap_err().to_string();
        assert!(err.contains("repository not found"), "{err}");
        std::env::set_var("UVR_FORGEJO_TOKEN", "wrong-tok");
        let err = resolve().unwrap_err().to_string();
        assert!(
            err.contains("refused the token in UVR_FORGEJO_TOKEN."),
            "{err}"
        );
        assert!(!err.contains("wrong-tok"), "{err}");
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
        let remotes = parse_forgejo_remotes(&fields).unwrap();
        let pairs: Vec<(&str, &str)> = remotes
            .iter()
            .map(|(n, g, _)| (n.as_str(), g.as_str()))
            .collect();
        // The malformed unbound GitLab hint is skipped by the legacy adapter.
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
