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
}
