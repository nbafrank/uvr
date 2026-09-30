use semver::Version;
use tracing::debug;

use crate::auth::{git_origin, GitHost};
use crate::error::{Result, UvrError};
use crate::lockfile::PackageSource;
use crate::registry::PackageInfo;

/// Legacy tuple form for a GitLab-sourced `Remotes:` dependency:
/// `(dep_name, "gitlab::host/group/.../project", optional_ref)`.
pub type GitlabRemote = (String, String, Option<String>);

/// A validated GitLab spec. Unlike Forgejo/GitHub, GitLab projects
/// routinely live under nested groups (`group/subgroup/.../project`), so
/// `project_path` holds the full namespace path rather than a single
/// `owner` segment. `git_ref` is `None` when the spec carried no `@ref`
/// segment — callers default it as they see fit (e.g. the registry
/// resolver uses `"HEAD"`, the manifest/CLI parsers keep `None`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitlabSpec {
    pub host: String,
    pub project_path: String,
    pub git_ref: Option<String>,
}

impl GitlabSpec {
    /// The project name — the last segment of `project_path`. Used as the
    /// default package name when DESCRIPTION carries no `Package:` field
    /// (mirrors forgejo/github behavior).
    pub fn project_name(&self) -> &str {
        self.project_path
            .rsplit_once('/')
            .map_or(self.project_path.as_str(), |(_, name)| name)
    }
}

/// Parse and validate `"[gitlab::]host/group[/subgroup...]/project[@ref]"`
/// into structured parts. Mirrors [`crate::registry::forgejo::parse_forgejo_parts`]
/// except the middle segment is a namespace *path* rather than a single
/// owner, since GitLab groups nest arbitrarily deep.
///
/// Accepts:
/// - `gitlab::gitlab.com/my-group/mypkg@v0.1.0`
/// - `gitlab.com/my-group/my-subgroup/mypkg` (no ref → `git_ref = None`)
/// - `git.local:3000/g/r` (port allowed)
///
/// Rejects:
/// - hosts containing a scheme (`https://...`)
/// - empty host or any empty path segment
/// - fewer than two path segments (need at least a namespace + project)
/// - host chars outside `[alnum].-:` or path-segment chars outside `[alnum].-_`
/// - GitLab's `group/project/-/subdirectory` package-directory form
/// - refs containing whitespace or URL/git metacharacters (`& # ? * ...`),
///   per [`crate::registry::github::is_valid_git_ref`]
pub fn parse_gitlab_parts(spec: &str) -> Option<GitlabSpec> {
    let body = spec.strip_prefix("gitlab::").unwrap_or(spec);

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

    if path_part.contains("://") || path_part.contains("/-/") {
        return None;
    }

    let parts: Vec<&str> = path_part.split('/').collect();
    // host + at least a two-segment project path (namespace + project).
    if parts.len() < 3 {
        return None;
    }
    let host = parts[0];
    if host.is_empty() {
        return None;
    }
    // Host shape: letters, digits, dot, hyphen, optional :port. Path-segment
    // shape: letters, digits, dot, hyphen, underscore. Anything else is a
    // user error worth catching before we make a request.
    let host_ok = host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'));
    if !host_ok {
        return None;
    }
    let seg_ok = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    };
    if !parts[1..].iter().all(|s| seg_ok(s)) {
        return None;
    }

    Some(GitlabSpec {
        host: host.to_string(),
        project_path: parts[1..].join("/"),
        git_ref,
    })
}

/// Parse `"gitlab::host/group/.../project[@ref]"` into
/// `(host, project_path, ref)`, defaulting a missing ref to `"HEAD"`. Thin
/// wrapper over [`parse_gitlab_parts`] for the registry resolver / BFS,
/// which want a concrete ref to query.
pub fn parse_gitlab_spec(spec: &str) -> Option<(String, String, String)> {
    let p = parse_gitlab_parts(spec)?;
    Some((
        p.host,
        p.project_path,
        p.git_ref.unwrap_or_else(|| "HEAD".to_string()),
    ))
}

/// Resolve a GitLab-hosted R package while deliberately discarding rich
/// `Remotes:` entries before the fallible legacy-tuple adapter.
pub async fn resolve_gitlab_package(
    client: &reqwest::Client,
    host: &str,
    project_path: &str,
    git_ref: &str,
) -> Result<PackageInfo> {
    resolve_gitlab_package_with_remote_entries(client, host, project_path, git_ref)
        .await
        .map(|(info, _)| info)
}

/// Legacy tuple adapter for a GitLab package's `Remotes:` entries. Nested or
/// bound-unsupported entries return an error rather than losing identity.
pub async fn resolve_gitlab_package_with_remotes(
    client: &reqwest::Client,
    host: &str,
    project_path: &str,
    git_ref: &str,
) -> Result<(PackageInfo, Vec<GitlabRemote>)> {
    let (info, remotes) =
        resolve_gitlab_package_with_remote_entries(client, host, project_path, git_ref).await?;
    Ok((info, compatible_remotes(remotes)?))
}

pub async fn resolve_gitlab_package_with_remote_entries(
    client: &reqwest::Client,
    host: &str,
    project_path: &str,
    git_ref: &str,
) -> Result<(PackageInfo, Vec<crate::manifest::RemoteEntry>)> {
    resolve_gitlab_package_with_remote_entries_bound(client, host, project_path, git_ref, false)
        .await
}

pub async fn resolve_gitlab_package_with_remote_entries_bound(
    client: &reqwest::Client,
    host: &str,
    project_path: &str,
    git_ref: &str,
    require_declared_name: bool,
) -> Result<(PackageInfo, Vec<crate::manifest::RemoteEntry>)> {
    let commit_sha = fetch_commit_sha(client, host, project_path, git_ref).await?;
    resolve_gitlab_package_with_remote_entries_at_commit_bound(
        client,
        host,
        project_path,
        &commit_sha,
        require_declared_name,
    )
    .await
}

pub async fn resolve_gitlab_package_with_remote_entries_at_commit_bound(
    client: &reqwest::Client,
    host: &str,
    project_path: &str,
    commit_sha: &str,
    require_declared_name: bool,
) -> Result<(PackageInfo, Vec<crate::manifest::RemoteEntry>)> {
    let (info, remotes, _) =
        resolve_gitlab_package_with_remote_entries_and_install_dependencies_at_commit_bound(
            client,
            host,
            project_path,
            commit_sha,
            require_declared_name,
        )
        .await?;
    Ok((info, remotes))
}

/// Resolve a GitLab package while also returning every install-time
/// DESCRIPTION dependency name used to bind nested GitHub `Remotes:` entries.
/// `PackageInfo::requires` deliberately retains GitLab's existing
/// Imports/Depends-only behavior.
pub async fn resolve_gitlab_package_with_remote_entries_and_install_dependencies_at_commit_bound(
    client: &reqwest::Client,
    host: &str,
    project_path: &str,
    commit_sha: &str,
    require_declared_name: bool,
) -> Result<(
    PackageInfo,
    Vec<crate::manifest::RemoteEntry>,
    std::collections::BTreeSet<String>,
)> {
    let project_id = urlencoding::encode(project_path);
    let origin = git_origin(host);
    let desc_url = format!(
        "{origin}/api/v4/projects/{project_id}/repository/files/DESCRIPTION/raw?ref={commit_sha}"
    );
    let desc_req = client
        .get(&desc_url)
        .header("User-Agent", concat!("uvr/", env!("CARGO_PKG_VERSION")));
    let desc_resp = GitHost::GitLab(host).send(desc_req).await?;
    if !desc_resp.status().is_success() {
        return Err(map_gitlab_error(
            desc_resp.status(),
            host,
            project_path,
            commit_sha,
        ));
    }
    let desc_text = desc_resp.text().await?;

    let desc_fields = crate::dcf::parse_dcf_fields(&desc_text);
    let project_name = project_path
        .rsplit_once('/')
        .map_or(project_path, |(_, name)| name);
    let pkg_name = description_package_name(
        &desc_fields,
        host,
        project_path,
        project_name,
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
    let remotes = parse_gitlab_remote_entries(&desc_fields);

    let url =
        format!("{origin}/api/v4/projects/{project_id}/repository/archive.tar.gz?sha={commit_sha}");

    debug!("GitLab {host}/{project_path}@{commit_sha} → {pkg_name} {version}");

    Ok((
        PackageInfo {
            name: pkg_name,
            version,
            source: PackageSource::Gitlab {
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
    project_path: &str,
    project_name: &str,
    commit_sha: &str,
    require_declared_name: bool,
) -> Result<String> {
    if !require_declared_name {
        return Ok(fields
            .get("Package")
            .cloned()
            .unwrap_or_else(|| project_name.to_string()));
    }
    let name = fields
        .get("Package")
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            UvrError::Other(format!(
                "DESCRIPTION for GitLab {host}/{project_path}@{commit_sha} has no `Package:` \
                 field; cannot validate the bound package identity."
            ))
        })?;
    if !crate::package_name::is_valid(&name) {
        return Err(UvrError::Other(format!(
            "DESCRIPTION for GitLab {host}/{project_path}@{commit_sha} declares an invalid \
             `Package:` name '{name}'."
        )));
    }
    Ok(name)
}

pub async fn fetch_commit_sha(
    client: &reqwest::Client,
    host: &str,
    project_path: &str,
    git_ref: &str,
) -> Result<String> {
    let project_id = urlencoding::encode(project_path);
    fetch_commit_sha_by_id(client, host, &project_id, project_path, git_ref).await
}

async fn fetch_commit_sha_by_id(
    client: &reqwest::Client,
    host: &str,
    project_id: &str,
    project_path: &str,
    git_ref: &str,
) -> Result<String> {
    // Unlike Forgejo (whose `/commits/{ref}` 404s and needs a list-commits
    // workaround), GitLab's single-commit endpoint accepts a branch, tag, or
    // SHA directly in the path and returns one commit object. The ref sits
    // in path position here, so percent-encode it — a slashed branch name
    // (`feature/x`) must not be split into extra path segments, and refs
    // can reach here without going through `parse_gitlab_parts` (lockfile
    // revs, `Remotes:` fields), so encode defensively (mirrors forgejo's
    // #152 handling).
    let encoded_ref = urlencoding::encode(git_ref);
    let url = format!(
        "{}/api/v4/projects/{project_id}/repository/commits/{encoded_ref}",
        git_origin(host)
    );
    let req = client
        .get(&url)
        .header("User-Agent", concat!("uvr/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/json");
    let resp = GitHost::GitLab(host).send(req).await?;

    if !resp.status().is_success() {
        return Err(map_gitlab_error(resp.status(), host, project_path, git_ref));
    }

    #[derive(serde::Deserialize)]
    struct CommitObj {
        id: String,
    }
    let body = resp.text().await?;
    let commit: CommitObj = serde_json::from_str(&body).map_err(|e| {
        UvrError::Other(format!(
            "GitLab {host}/{project_path}@{git_ref}: could not parse commit JSON ({e}). Body: {}",
            body.chars().take(200).collect::<String>()
        ))
    })?;
    Ok(commit.id)
}

fn map_gitlab_error(
    status: reqwest::StatusCode,
    host: &str,
    project_path: &str,
    ref_or_sha: &str,
) -> UvrError {
    match status.as_u16() {
        401 | 403 => UvrError::Other(format!(
            "GitLab returned {status} for {host}/{project_path}; {}",
            GitHost::GitLab(host).denied_advice()
        )),
        404 => UvrError::Other(format!(
            "GitLab project not found: {host}/{project_path}@{ref_or_sha}. \
             Check the spec and that the project exists."
        )),
        _ => UvrError::Other(format!(
            "GitLab error for {host}/{project_path}@{ref_or_sha}: HTTP {status}"
        )),
    }
}

#[cfg(test)]
fn parse_gitlab_remotes(
    desc_fields: &std::collections::BTreeMap<String, String>,
) -> Result<Vec<GitlabRemote>> {
    compatible_remotes(parse_gitlab_remote_entries(desc_fields))
}

fn parse_gitlab_remote_entries(
    desc_fields: &std::collections::BTreeMap<String, String>,
) -> Vec<crate::manifest::RemoteEntry> {
    let Some(remotes_field) = desc_fields.get("Remotes") else {
        return Vec::new();
    };
    crate::manifest::parse_remotes_field_rich(remotes_field)
}

fn compatible_remotes(entries: Vec<crate::manifest::RemoteEntry>) -> Result<Vec<GitlabRemote>> {
    crate::manifest::compatible_remote_entries(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_spec_happy() {
        let (host, project_path, git_ref) =
            parse_gitlab_spec("gitlab::gitlab.com/my-group/mypkg@v0.1.0").unwrap();
        assert_eq!(host, "gitlab.com");
        assert_eq!(project_path, "my-group/mypkg");
        assert_eq!(git_ref, "v0.1.0");
    }

    #[test]
    fn parse_spec_nested_subgroups() {
        let (host, project_path, git_ref) =
            parse_gitlab_spec("gitlab::gitlab.com/group/subgroup/mypkg@main").unwrap();
        assert_eq!(host, "gitlab.com");
        assert_eq!(project_path, "group/subgroup/mypkg");
        assert_eq!(git_ref, "main");

        let parsed = parse_gitlab_parts("gitlab::gitlab.com/group/subgroup/mypkg@main").unwrap();
        assert_eq!(parsed.project_name(), "mypkg");
    }

    #[test]
    fn parse_spec_rejects_gitlab_package_directory_form() {
        assert!(parse_gitlab_parts("gitlab::host/group/repo/-/subdir").is_none());
        assert!(parse_gitlab_parts("host/group/repo/-/nested/subdir@main").is_none());
    }

    #[test]
    fn parse_spec_default_ref() {
        let (_h, _p, git_ref) = parse_gitlab_spec("gitlab::gitlab.com/my-group/mypkg").unwrap();
        assert_eq!(git_ref, "HEAD");
    }

    #[test]
    fn parse_spec_with_port() {
        let (host, _, _) = parse_gitlab_spec("gitlab::git.local:3000/g/r").unwrap();
        assert_eq!(host, "git.local:3000");
    }

    #[test]
    fn parse_spec_accepts_unprefixed() {
        // Callers (lock.rs BFS) may strip the prefix before calling us.
        let parsed = parse_gitlab_spec("gitlab.com/my-group/mypkg@main").unwrap();
        assert_eq!(parsed.0, "gitlab.com");
        assert_eq!(parsed.2, "main");
    }

    #[test]
    fn parse_spec_rejects_scheme_in_host() {
        assert!(parse_gitlab_spec("gitlab::https://gitlab.com/g/r").is_none());
    }

    #[test]
    fn parse_spec_rejects_too_few_segments() {
        assert!(parse_gitlab_spec("gitlab::gitlab.com/onlyone").is_none());
        assert!(parse_gitlab_spec("gitlab::onlyhost").is_none());
    }

    #[test]
    fn parse_spec_rejects_empty_segments() {
        assert!(parse_gitlab_parts("gitlab:://g/r").is_none());
        assert!(parse_gitlab_parts("gitlab::gitlab.com//r").is_none());
        assert!(parse_gitlab_parts("gitlab::gitlab.com/g/").is_none());
        assert!(parse_gitlab_parts("gitlab::gitlab.com/g//r").is_none());
    }

    #[test]
    fn parts_ref_is_none_when_absent_head_when_via_spec() {
        // The shared core keeps "no ref" as None; the spec wrapper defaults it
        // to HEAD for the resolver. This is the distinction add/manifest rely on.
        let p = parse_gitlab_parts("gitlab::gitlab.com/my-group/mypkg").unwrap();
        assert_eq!(p.git_ref, None);
        assert_eq!(p.project_path, "my-group/mypkg");
        assert_eq!(
            parse_gitlab_spec("gitlab::gitlab.com/my-group/mypkg")
                .unwrap()
                .2,
            "HEAD"
        );

        let p = parse_gitlab_parts("gitlab::gitlab.com/my-group/mypkg@v1.0").unwrap();
        assert_eq!(p.git_ref.as_deref(), Some("v1.0"));
        assert!(parse_gitlab_parts("gitlab::gitlab.com/my-group/mypkg@").is_none());
    }

    #[test]
    fn bound_package_name_requires_present_legal_description_field() {
        let missing = crate::dcf::parse_dcf_fields("Version: 1.0.0\n");
        let error = description_package_name(
            &missing,
            "gitlab.example",
            "group/repo",
            "repo",
            "commit",
            true,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("has no `Package:` field"), "{error}");

        let invalid = crate::dcf::parse_dcf_fields("Package: bad name\nVersion: 1.0.0\n");
        let error = description_package_name(
            &invalid,
            "gitlab.example",
            "group/repo",
            "repo",
            "commit",
            true,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("invalid `Package:` name"), "{error}");

        let valid = crate::dcf::parse_dcf_fields("Package: Alias\nVersion: 1.0.0\n");
        assert_eq!(
            description_package_name(
                &valid,
                "gitlab.example",
                "group/repo",
                "repo",
                "commit",
                true,
            )
            .unwrap(),
            "Alias"
        );
    }

    #[test]
    fn unbound_package_name_keeps_project_fallback() {
        let missing = crate::dcf::parse_dcf_fields("Version: 1.0.0\n");
        assert_eq!(
            description_package_name(
                &missing,
                "gitlab.example",
                "group/repo",
                "repo",
                "commit",
                false,
            )
            .unwrap(),
            "repo"
        );
    }

    #[test]
    fn parts_validates_ref_chars() {
        // A ref with `&` would inject a second query parameter; `#`, `?`,
        // and whitespace break the URL too.
        assert!(parse_gitlab_parts("gitlab::gitlab.com/g/r@feat&x").is_none());
        assert!(parse_gitlab_parts("gitlab::gitlab.com/g/r@v1#frag").is_none());
        assert!(parse_gitlab_parts("gitlab::gitlab.com/g/r@a b").is_none());
        assert!(parse_gitlab_parts("gitlab::gitlab.com/g/r@x?y").is_none());
        // Legitimate ref shapes still pass, including slashed branches.
        assert_eq!(
            parse_gitlab_parts("gitlab::gitlab.com/g/r@main")
                .unwrap()
                .git_ref
                .as_deref(),
            Some("main")
        );
        assert_eq!(
            parse_gitlab_parts("gitlab::gitlab.com/g/r@v1.2.3")
                .unwrap()
                .git_ref
                .as_deref(),
            Some("v1.2.3")
        );
        assert_eq!(
            parse_gitlab_parts("gitlab::gitlab.com/g/r@feature/x")
                .unwrap()
                .git_ref
                .as_deref(),
            Some("feature/x")
        );
    }

    #[test]
    fn parts_validates_path_segment_chars() {
        // A segment with shell metacharacters is rejected.
        assert!(parse_gitlab_parts("gitlab::gitlab.com/my-group/my;rm -rf").is_none());
        assert!(parse_gitlab_parts("gitlab::gitlab.com/own$er/mypkg").is_none());
        // Underscores, dots, hyphens stay valid, in any segment.
        assert!(parse_gitlab_parts("gitlab::gitlab.com/my-group/my_pkg.v2").is_some());
        assert!(parse_gitlab_parts("gitlab::gitlab.com/a.b-c/sub_d/my_pkg.v2").is_some());
    }

    // #187: a private project in a nested group resolves and downloads with
    // the host's token from the shared resolver, sent as `Bearer`, on
    // GitLab's API v4 paths.
    #[cfg(not(target_os = "windows"))]
    #[test]
    fn private_project_resolves_and_downloads() {
        use crate::auth::{
            test_authorization, test_git_origin, test_private_git_host, test_response, GitEnv,
        };

        const SHA: &str = "89abcdef0123456789abcdef0123456789abcdef";
        let _env = GitEnv::new(&["UVR_GITLAB_TOKEN", "UVR_GITLAB_TOKEN_GITLAB_TEST"]);
        std::env::set_var("UVR_GITLAB_TOKEN_GITLAB_TEST", "gl-tok");
        let (origin, seen) = test_private_git_host("Bearer gl-tok", |path| {
            let ok = |body: &[u8]| test_response("200 OK", "", body);
            match path.strip_prefix("/api/v4/projects/group%2Fsub%2Fprivpkg/repository/") {
                Some("commits/main") => ok(format!(r#"{{"id":"{SHA}"}}"#).as_bytes()),
                Some(p) if p == format!("files/DESCRIPTION/raw?ref={SHA}") => {
                    ok(b"Package: privpkg\nVersion: 2.0.0\n")
                }
                Some(p) if p == format!("archive.tar.gz?sha={SHA}") => ok(b"private tarball"),
                _ => test_response("404 Not Found", "", b""),
            }
        });
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        test_git_origin("gitlab.test:8443", &origin);
        let client = reqwest::Client::new();
        let resolve = || {
            rt.block_on(resolve_gitlab_package(
                &client,
                "gitlab.test:8443",
                "group/sub/privpkg",
                "main",
            ))
        };

        let info = resolve().expect("the token opens the private project");
        assert_eq!(
            (info.name.as_str(), info.version.to_string()),
            ("privpkg", "2.0.0".into())
        );
        let tarball = crate::installer::download::test_download(&rt, &info)
            .expect("the tarball downloads with the token");
        assert_eq!(tarball, b"private tarball");
        let heads = seen.lock().unwrap().clone();
        assert_eq!(heads.len(), 3, "{heads:?}");
        for head in &heads {
            assert_eq!(test_authorization(head), Some("Bearer gl-tok"), "{head}");
        }

        // Without a token the project is hidden; a wrong one is refused.
        std::env::remove_var("UVR_GITLAB_TOKEN_GITLAB_TEST");
        let err = resolve().unwrap_err().to_string();
        assert!(err.contains("project not found"), "{err}");
        std::env::set_var("UVR_GITLAB_TOKEN_GITLAB_TEST", "wrong-tok");
        let err = resolve().unwrap_err().to_string();
        assert!(
            err.contains("refused the token in UVR_GITLAB_TOKEN_GITLAB_TEST."),
            "{err}"
        );
        assert!(!err.contains("wrong-tok"), "{err}");
    }

    #[test]
    fn parse_gitlab_remotes_keeps_all_git_bearing_entries() {
        // A gitlab package's DESCRIPTION may declare git-bearing Remotes
        // pointing at any registry. We pass them all through; the
        // lock-time BFS dispatches per-prefix via classify_git.
        let desc = "\
Package: x
Version: 0.1.0
Remotes: gitlab::gitlab.com/my-group/mypkg@v0.1.0,
    github::user/other,
    forgejo::codefloe.com/pat-s/skipme
";
        let fields = crate::dcf::parse_dcf_fields(desc);
        let remotes = parse_gitlab_remotes(&fields).unwrap();
        let pairs: Vec<(&str, &str)> = remotes
            .iter()
            .map(|(n, g, _)| (n.as_str(), g.as_str()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("mypkg", "gitlab::gitlab.com/my-group/mypkg"),
                ("other", "user/other"),
                ("skipme", "forgejo::codefloe.com/pat-s/skipme"),
            ]
        );
        // The gitlab entry still carries its ref.
        assert_eq!(remotes[0].2.as_deref(), Some("v0.1.0"));
    }
}
