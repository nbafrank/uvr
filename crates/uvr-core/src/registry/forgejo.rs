use semver::Version;
use tracing::debug;

use crate::error::{Result, UvrError};
use crate::lockfile::PackageSource;
use crate::manifest::DependencySpec;
use crate::registry::{Dep, PackageInfo};

/// A forgejo-sourced dependency declared in another package's `Remotes:`
/// field. `(dep_name, "forgejo::host/owner/repo", optional_ref)`.
pub type ForgejoRemote = (String, String, Option<String>);

/// Parse `"forgejo::host/owner/repo[@ref]"` (or the bare `host/owner/repo[@ref]`
/// once the prefix has been stripped) into `(host, owner, repo, ref)`.
///
/// Accepts:
/// - `forgejo::codefloe.com/pat-s/mypkg@v0.1.0`
/// - `codefloe.com/pat-s/mypkg` (ref defaults to `HEAD`)
/// - `git.local:3000/u/r` (port allowed)
///
/// Rejects:
/// - hosts containing a scheme (`https://...`)
/// - empty host, owner, or repo segments
/// - anything with more than three path segments
pub fn parse_forgejo_spec(spec: &str) -> Option<(String, String, String, String)> {
    let body = spec.strip_prefix("forgejo::").unwrap_or(spec);

    let (path_part, git_ref) = if let Some(at_pos) = body.rfind('@') {
        (&body[..at_pos], body[at_pos + 1..].to_string())
    } else {
        (body, "HEAD".to_string())
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
    // Host shape: letters, digits, dot, hyphen, optional :port. Anything
    // else is a user error worth catching before we make a request.
    let host_ok = host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == ':');
    if !host_ok {
        return None;
    }

    Some((
        host.to_string(),
        owner.to_string(),
        repo.to_string(),
        git_ref,
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
pub(crate) fn forgejo_token(host: &str) -> Option<String> {
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
        let (host, _, _, _) =
            parse_forgejo_spec("forgejo::git.local:3000/u/r").unwrap();
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
}
