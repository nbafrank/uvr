# Forgejo Remote Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the `forgejo::<host>/<owner>/<repo>[@<ref>]` registry — usable from `uvr add`, `uvr.toml` `git = "..."`, and DESCRIPTION `Remotes:` — that resolves and installs R packages from any Forgejo instance.

**Architecture:** New `crates/uvr-core/src/registry/forgejo.rs` mirrors `github.rs`, parameterized by host. A new `PackageSource::Forgejo { host }` lockfile variant. The `git` field in `DetailedDep` keeps its `Option<String>` shape; the `forgejo::` prefix on the value discriminates which resolver to call. The existing GitHub BFS in `lock.rs` is generalized to a single `resolve_git_deps` that dispatches per-prefix and walks cross-host `Remotes:` chains.

**Tech Stack:** Rust, `reqwest` (HTTP), `serde_json` (commits-endpoint parsing), `serde` (lockfile (de)serialization). No new crate dependencies.

**Spec:** `docs/superpowers/specs/2026-05-26-forgejo-remote-support-design.md`

---

## File Structure

| File | Role |
|------|------|
| `crates/uvr-core/src/lockfile.rs` | Add `Forgejo { host }` variant; serialize as `"forgejo:<host>"`. |
| `crates/uvr-core/src/registry/mod.rs` | Re-export `forgejo` submodule. |
| `crates/uvr-core/src/registry/forgejo.rs` *(new)* | Spec parser, ref→SHA, DESCRIPTION fetch, tarball URL builder, `Remotes:` walker, token lookup. |
| `crates/uvr-core/src/manifest.rs` | `parse_remotes_field` keeps `forgejo::` entries (currently dropped). |
| `crates/uvr/src/commands/add.rs` | `parse_add_spec` recognizes `forgejo::` prefix. `resolve_github_pkg_names` becomes `resolve_git_pkg_names` and dispatches. |
| `crates/uvr/src/commands/lock.rs` | `resolve_github_deps` becomes `resolve_git_deps` and dispatches by prefix; queue uses prefixed strings throughout. |
| `crates/uvr/src/commands/sync.rs` | `source_url` handles `PackageSource::Forgejo { .. }`. |
| `crates/uvr/src/commands/export.rs` | renv-export emits `Source: "Git"` + `RemoteType: "git2r"` + `RemoteUrl` for Forgejo packages. |

---

## Task 1: Add `PackageSource::Forgejo { host }` lockfile variant

**Files:**
- Modify: `crates/uvr-core/src/lockfile.rs:62-108` (enum, `Serialize`, `Deserialize`, `Display`)
- Test: `crates/uvr-core/src/lockfile.rs` (tests module)

- [ ] **Step 1: Write failing round-trip test for the Forgejo variant**

Add inside the `#[cfg(test)] mod tests { ... }` block in `crates/uvr-core/src/lockfile.rs`:

```rust
#[test]
fn round_trip_forgejo_source() {
    let input = r#"
[r]
version = "4.4.2"

[[package]]
name = "mypkg"
version = "0.1.0"
source = "forgejo:codefloe.com"
url = "https://codefloe.com/api/v1/repos/pat-s/mypkg/archive/abc123.tar.gz"
"#;
    let lf: Lockfile = input.parse().expect("parse forgejo source");
    assert_eq!(
        lf.packages[0].source,
        PackageSource::Forgejo {
            host: "codefloe.com".to_string()
        }
    );

    let s = lf.to_toml_string().unwrap();
    assert!(s.contains(r#"source = "forgejo:codefloe.com""#));
    let lf2: Lockfile = s.parse().unwrap();
    assert_eq!(lf, lf2);
}

#[test]
fn forgejo_source_empty_host_falls_to_custom() {
    // Defensive: a malformed `"forgejo:"` (empty host) deserializes
    // to Custom, not to Forgejo with an empty host string.
    let input = r#"
[r]
version = "4.4.2"

[[package]]
name = "x"
version = "0.1.0"
source = "forgejo:"
"#;
    let lf: Lockfile = input.parse().expect("parse");
    assert!(matches!(
        lf.packages[0].source,
        PackageSource::Custom { ref name } if name == "forgejo:"
    ));
}
```

- [ ] **Step 2: Run tests to verify they fail to compile**

Run: `cargo test -p uvr-core --lib lockfile::tests::round_trip_forgejo_source 2>&1 | tail -20`
Expected: compilation error — `PackageSource::Forgejo` variant does not exist.

- [ ] **Step 3: Add the variant and update (de)serialization**

In `crates/uvr-core/src/lockfile.rs`, change the enum (around line 62-72):

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum PackageSource {
    Cran,
    Bioconductor,
    GitHub,
    /// A Forgejo-hosted package. `host` is the bare hostname (optionally
    /// `host:port`), e.g. `"codefloe.com"`. Serializes as
    /// `"forgejo:<host>"` in the lockfile.
    Forgejo {
        host: String,
    },
    Local,
    /// A custom CRAN-like repository (r-multiverse, r-universe, PPM, etc.)
    Custom {
        name: String,
    },
}
```

Update the `Deserialize` impl (around line 83-95):

```rust
impl<'de> Deserialize<'de> for PackageSource {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(match s.to_lowercase().as_str() {
            "cran" => PackageSource::Cran,
            "bioconductor" => PackageSource::Bioconductor,
            "github" => PackageSource::GitHub,
            "local" => PackageSource::Local,
            _ => {
                // `forgejo:<host>` with a non-empty host → Forgejo variant.
                // Anything else (including a bare `forgejo:`) falls through
                // to Custom so a future typo doesn't silently become a
                // valid Forgejo source with an empty host.
                if let Some(host) = s.strip_prefix("forgejo:") {
                    if !host.is_empty() {
                        return Ok(PackageSource::Forgejo {
                            host: host.to_string(),
                        });
                    }
                }
                PackageSource::Custom { name: s }
            }
        })
    }
}
```

Update the `Display` impl (around line 98-108):

```rust
impl std::fmt::Display for PackageSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackageSource::Cran => write!(f, "cran"),
            PackageSource::Bioconductor => write!(f, "bioconductor"),
            PackageSource::GitHub => write!(f, "github"),
            PackageSource::Forgejo { host } => write!(f, "forgejo:{host}"),
            PackageSource::Local => write!(f, "local"),
            PackageSource::Custom { name } => write!(f, "{name}"),
        }
    }
}
```

- [ ] **Step 4: Run lockfile tests; expect compile failures elsewhere from new variant**

Run: `cargo test -p uvr-core --lib lockfile 2>&1 | tail -20`
Expected: lockfile tests pass. Other crate modules that `match` `PackageSource` exhaustively now fail to compile — list them with:
`cargo build -p uvr-core 2>&1 | grep -E "non-exhaustive|missing|PackageSource" | head -20`

- [ ] **Step 5: Add `Forgejo { .. }` arm to every exhaustive match in `uvr-core`**

The build error from Step 4 lists every site. There should be **zero** sites in `uvr-core` (the enum is only matched in `uvr/`), but if `cargo build -p uvr-core` finds any, add an arm that matches the existing `GitHub` arm's behavior — i.e. treat Forgejo the same as GitHub locally. Re-run the build until it's green.

- [ ] **Step 6: Commit**

```bash
git add crates/uvr-core/src/lockfile.rs
git commit -m "lockfile: add PackageSource::Forgejo { host } variant"
```

---

## Task 2: Forgejo spec parser

**Files:**
- Create: `crates/uvr-core/src/registry/forgejo.rs`
- Modify: `crates/uvr-core/src/registry/mod.rs:1-4`

- [ ] **Step 1: Wire the new module**

In `crates/uvr-core/src/registry/mod.rs`, change the module list from:

```rust
pub mod bioconductor;
pub mod cran;
pub mod github;
pub mod p3m;
```

to:

```rust
pub mod bioconductor;
pub mod cran;
pub mod forgejo;
pub mod github;
pub mod p3m;
```

- [ ] **Step 2: Create `forgejo.rs` with `parse_forgejo_spec` + failing tests**

Create `crates/uvr-core/src/registry/forgejo.rs`:

```rust
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
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test -p uvr-core --lib registry::forgejo 2>&1 | tail -20`
Expected: 6 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/uvr-core/src/registry/mod.rs crates/uvr-core/src/registry/forgejo.rs
git commit -m "registry: scaffold forgejo module with spec parser"
```

---

## Task 3: Forgejo auth-token helper

**Files:**
- Modify: `crates/uvr-core/src/registry/forgejo.rs`

- [ ] **Step 1: Add failing tests for `forgejo_token`**

Append to `crates/uvr-core/src/registry/forgejo.rs` (inside the `tests` module):

```rust
    #[test]
    fn token_lookup_per_host_takes_precedence() {
        // Use unique env var names so this test doesn't clash with anyone
        // else running concurrently.
        let host = "lookup-test-host.example";
        let normalized = "LOOKUP_TEST_HOST_EXAMPLE";
        let per_host_var = format!("UVR_FORGEJO_TOKEN_{normalized}");
        std::env::set_var(&per_host_var, "host-specific");
        std::env::set_var("UVR_FORGEJO_TOKEN", "global");
        assert_eq!(forgejo_token(host).as_deref(), Some("host-specific"));
        std::env::remove_var(&per_host_var);
        assert_eq!(forgejo_token(host).as_deref(), Some("global"));
        std::env::remove_var("UVR_FORGEJO_TOKEN");
        assert_eq!(forgejo_token(host), None);
    }

    #[test]
    fn token_lookup_normalizes_host_with_port() {
        // Port is stripped before normalization so the env var name is
        // stable across `host` vs `host:port`.
        std::env::set_var("UVR_FORGEJO_TOKEN_GIT_LOCAL", "t");
        assert_eq!(forgejo_token("git.local:3000").as_deref(), Some("t"));
        std::env::remove_var("UVR_FORGEJO_TOKEN_GIT_LOCAL");
    }

    #[test]
    fn token_lookup_ignores_empty_or_whitespace() {
        std::env::set_var("UVR_FORGEJO_TOKEN", "   ");
        assert_eq!(forgejo_token("any.host").as_deref(), None);
        std::env::remove_var("UVR_FORGEJO_TOKEN");
    }
```

- [ ] **Step 2: Run tests to verify compile failure**

Run: `cargo test -p uvr-core --lib registry::forgejo 2>&1 | tail -20`
Expected: compile error — `forgejo_token` not defined.

- [ ] **Step 3: Implement `forgejo_token`**

Add to `crates/uvr-core/src/registry/forgejo.rs` (outside the tests module):

```rust
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p uvr-core --lib registry::forgejo 2>&1 | tail -20`
Expected: 9 tests pass. Note: env-touching tests are not parallel-safe across modules; if intermittent failures show up, mark them `#[cfg_attr(test, ignore = "env-mutating")]` and document.

- [ ] **Step 5: Commit**

```bash
git add crates/uvr-core/src/registry/forgejo.rs
git commit -m "registry/forgejo: add UVR_FORGEJO_TOKEN_<HOST> env lookup"
```

---

## Task 4: Forgejo resolver (ref→SHA, DESCRIPTION fetch, tarball URL)

**Files:**
- Modify: `crates/uvr-core/src/registry/forgejo.rs`

- [ ] **Step 1: Add parse-remotes test (no network needed)**

Append to the tests module of `crates/uvr-core/src/registry/forgejo.rs`:

```rust
    #[test]
    fn parse_forgejo_remotes_filters_non_forgejo() {
        // DESCRIPTION mixing forgejo, github, and gitlab remotes — only
        // forgejo:: entries should come out of the parser.
        let desc = "\
Package: x
Version: 0.1.0
Remotes: forgejo::codefloe.com/pat-s/mypkg@v0.1.0,
    github::user/other,
    gitlab::someone/skipme
";
        let fields = crate::dcf::parse_dcf_fields(desc);
        let remotes = parse_forgejo_remotes(&fields);
        assert_eq!(remotes.len(), 1);
        assert_eq!(remotes[0].0, "mypkg");
        assert_eq!(remotes[0].1, "forgejo::codefloe.com/pat-s/mypkg");
        assert_eq!(remotes[0].2.as_deref(), Some("v0.1.0"));
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p uvr-core --lib registry::forgejo::tests::parse_forgejo_remotes_filters_non_forgejo 2>&1 | tail -20`
Expected: compile error — `parse_forgejo_remotes` not defined.

- [ ] **Step 3: Add the resolver functions + `parse_forgejo_remotes` helper**

This task adds the full resolver. We can't unit-test the HTTP path without a mock server (the codebase has none) — coverage comes from the network-gated integration test in Task 10 and the parser test above.

Append to `crates/uvr-core/src/registry/forgejo.rs`:

```rust
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

    let desc_url = format!(
        "https://{host}/api/v1/repos/{owner}/{repo}/raw/DESCRIPTION?ref={commit_sha}"
    );
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

    let url = format!(
        "https://{host}/api/v1/repos/{owner}/{repo}/archive/{commit_sha}.tar.gz"
    );

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
    let url = format!("https://{host}/api/v1/repos/{owner}/{repo}/commits/{git_ref}");
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

    // Forgejo returns either a JSON commit object (`{ "sha": "...", ... }`)
    // or — when `?stat=false&verification=false` etc. — an array; the
    // `commits/{ref}` endpoint always returns a single object.
    #[derive(serde::Deserialize)]
    struct CommitObj {
        sha: String,
    }
    let body = resp.text().await?;
    let commit: CommitObj = serde_json::from_str(&body).map_err(|e| {
        UvrError::Other(format!(
            "Forgejo {host}/{owner}/{repo}@{git_ref}: could not parse commit JSON ({e}). Body: {}",
            body.chars().take(200).collect::<String>()
        ))
    })?;
    Ok(commit.sha)
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
    let Some(remotes_field) = desc_fields.get("Remotes") else {
        return Vec::new();
    };
    crate::manifest::parse_remotes_field(remotes_field)
        .into_iter()
        .filter_map(|(name, spec)| match spec {
            DependencySpec::Detailed(d) => match d.git {
                Some(g) if g.starts_with("forgejo::") => Some((name, g, d.rev)),
                _ => None,
            },
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
```

- [ ] **Step 4: Run all forgejo tests; expect parse-remotes test still to fail until manifest.rs is updated**

Run: `cargo test -p uvr-core --lib registry::forgejo 2>&1 | tail -20`
Expected: 9 of 10 pass; `parse_forgejo_remotes_filters_non_forgejo` fails because `parse_remotes_field` currently drops the `forgejo::` entry. We fix that in Task 5.

- [ ] **Step 5: Commit**

```bash
git add crates/uvr-core/src/registry/forgejo.rs
git commit -m "registry/forgejo: add resolver, tarball URL, and Remotes walker"
```

---

## Task 5: Teach `parse_remotes_field` to keep `forgejo::` entries

**Files:**
- Modify: `crates/uvr-core/src/manifest.rs:298-374` (`parse_remotes_field` and its docstring)

- [ ] **Step 1: Add a failing manifest-level test**

Append to the existing `tests` module in `crates/uvr-core/src/manifest.rs`:

```rust
    #[test]
    fn parse_remotes_field_keeps_forgejo() {
        let field = "forgejo::codefloe.com/pat-s/mypkg@v0.1.0, github::user/a, gitlab::other/x";
        let v = parse_remotes_field(field);
        let names: Vec<&str> = v.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["mypkg", "a"]);

        // The forgejo entry stores the full `forgejo::host/owner/repo` in
        // the `git` field, with the ref split into `rev`.
        match &v[0].1 {
            DependencySpec::Detailed(d) => {
                assert_eq!(d.git.as_deref(), Some("forgejo::codefloe.com/pat-s/mypkg"));
                assert_eq!(d.rev.as_deref(), Some("v0.1.0"));
            }
            other => panic!("expected Detailed, got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p uvr-core --lib manifest::tests::parse_remotes_field_keeps_forgejo 2>&1 | tail -20`
Expected: FAIL — `names` is `["a"]` because `forgejo::` is filtered out.

- [ ] **Step 3: Extend `parse_remotes_field` to accept the `forgejo::` prefix**

In `crates/uvr-core/src/manifest.rs`, replace the prefix-handling block in `parse_remotes_field` (currently around lines 322-328):

```rust
        // Skip non-GitHub remote types we don't translate yet.
        if let Some((prefix, _)) = entry.split_once("::") {
            if prefix != "github" {
                continue;
            }
        }
        let body = entry.strip_prefix("github::").unwrap_or(entry);
```

with:

```rust
        // Two registries are translated today: github (the bare/`github::`
        // form, returning `git = "owner/repo"`) and forgejo (the
        // `forgejo::host/owner/repo` form, returning the same prefix
        // verbatim in `git` so the lock-time BFS can dispatch on it).
        // Other prefixes (`gitlab::`, `bitbucket::`, `git::`, `url::`,
        // `local::`, `bioc::`) are skipped — the caller keeps whatever
        // version-based spec it already had from `Imports:`.
        let forgejo_body = entry.strip_prefix("forgejo::");
        if let Some(body) = forgejo_body {
            if let Some(parsed) = parse_forgejo_entry(body) {
                result.push(parsed);
            }
            continue;
        }
        if let Some((prefix, _)) = entry.split_once("::") {
            if prefix != "github" {
                continue;
            }
        }
        let body = entry.strip_prefix("github::").unwrap_or(entry);
```

Then add a private helper at the bottom of `crates/uvr-core/src/manifest.rs` (above the `tests` module):

```rust
/// Parse the `host/owner/repo[@ref]` body of a `forgejo::`-prefixed
/// `Remotes:` entry into `(pkg_name, DependencySpec)`. The package name
/// defaults to the repo segment. Returns `None` if the body is malformed.
///
/// The `git` field stores the *full* `"forgejo::host/owner/repo"` string
/// (prefix included) so downstream code that walks `Remotes:` chains can
/// tell forgejo and github specs apart by string prefix.
fn parse_forgejo_entry(body: &str) -> Option<(String, DependencySpec)> {
    // Optional `pkgname=` override.
    let (explicit_name, path) = match body.split_once('=') {
        Some((n, p)) if !n.trim().is_empty() && p.trim().contains('/') => {
            (Some(n.trim().to_string()), p.trim())
        }
        _ => (None, body),
    };

    let (path_no_anchor, rev) = match path.split_once('@') {
        Some((p, r)) => {
            let r = r.split('#').next().unwrap_or(r).trim();
            (p.trim(), if r.is_empty() { None } else { Some(r.to_string()) })
        }
        None => (path.split('#').next().unwrap_or(path).trim(), None),
    };

    let parts: Vec<&str> = path_no_anchor.split('/').collect();
    if parts.len() != 3 || parts.iter().any(|s| s.is_empty()) {
        return None;
    }
    let repo = parts[2];
    let pkg_name = explicit_name.unwrap_or_else(|| repo.to_string());
    if pkg_name.is_empty() {
        return None;
    }

    let spec = DependencySpec::Detailed(DetailedDep {
        git: Some(format!("forgejo::{path_no_anchor}")),
        rev,
        ..Default::default()
    });
    Some((pkg_name, spec))
}
```

Also update the `parse_remotes_field` docstring to mention forgejo (around line 300-309):

```rust
/// Parse an R `Remotes:` field into `(package_name, DependencySpec)` pairs.
///
/// Supports devtools/remotes-style GitHub entries plus uvr's `forgejo::`:
/// - `user/repo` → `git = "user/repo"`
/// - `user/repo@ref` → `git = "user/repo", rev = "ref"`
/// - `github::user/repo[@ref]` → same (explicit prefix)
/// - `pkgname=user/repo[@ref]` → explicit package name binding
/// - `forgejo::host/owner/repo[@ref]` → `git = "forgejo::host/owner/repo", rev = "ref"`
///
/// Entries with other prefixes (`gitlab::`, `bitbucket::`, `git::`,
/// `url::`, `local::`, `bioc::`) are skipped for now — the caller keeps
/// whatever version-based spec it already had from `Imports:`.
///
/// Visible to the github registry so it can walk transitive `Remotes:`
/// chains during `uvr lock` (#84) — and to the forgejo registry for the
/// same reason.
```

- [ ] **Step 4: Run all manifest + forgejo tests**

Run: `cargo test -p uvr-core --lib 'manifest::tests::parse_remotes_field' 2>&1 | tail -20`
Run: `cargo test -p uvr-core --lib registry::forgejo 2>&1 | tail -20`
Expected: both green. The `parse_forgejo_remotes_filters_non_forgejo` test from Task 4 now passes.

- [ ] **Step 5: Commit**

```bash
git add crates/uvr-core/src/manifest.rs
git commit -m "manifest: parse_remotes_field keeps forgejo:: entries"
```

---

## Task 6: `uvr add forgejo::...` CLI parsing

**Files:**
- Modify: `crates/uvr/src/commands/add.rs:11-44` (`parse_add_spec`)

- [ ] **Step 1: Add a failing CLI-parse test**

Append to the `tests` module in `crates/uvr/src/commands/add.rs`:

```rust
    #[test]
    fn parse_forgejo_spec_cli() {
        let (name, spec) =
            parse_add_spec("forgejo::codefloe.com/pat-s/mypkg@main", false).unwrap();
        assert_eq!(name, "mypkg");
        match spec {
            DependencySpec::Detailed(d) => {
                assert_eq!(
                    d.git.as_deref(),
                    Some("forgejo::codefloe.com/pat-s/mypkg")
                );
                assert_eq!(d.rev.as_deref(), Some("main"));
            }
            other => panic!("expected Detailed, got {other:?}"),
        }
    }

    #[test]
    fn parse_forgejo_spec_cli_no_ref() {
        let (name, spec) =
            parse_add_spec("forgejo::codefloe.com/pat-s/mypkg", false).unwrap();
        assert_eq!(name, "mypkg");
        match spec {
            DependencySpec::Detailed(d) => {
                assert_eq!(d.rev, None);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn parse_forgejo_spec_cli_rejects_bad_shape() {
        assert!(parse_add_spec("forgejo::codefloe.com/onlyone", false).is_err());
        assert!(parse_add_spec("forgejo::/pat-s/mypkg", false).is_err());
        assert!(parse_add_spec("forgejo::codefloe.com//mypkg", false).is_err());
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p uvr --lib commands::add::tests::parse_forgejo 2>&1 | tail -20`
Expected: FAIL — current parser treats the spec as a github-style `/`-spec with invalid shape.

- [ ] **Step 3: Add `forgejo::` branch to `parse_add_spec`**

In `crates/uvr/src/commands/add.rs`, replace the start of `parse_add_spec` (currently around line 11-44):

```rust
fn parse_add_spec(raw: &str, bioc: bool) -> Result<(String, DependencySpec)> {
    // GitHub: contains '/'
    if raw.contains('/') {
        ...
```

with:

```rust
fn parse_add_spec(raw: &str, bioc: bool) -> Result<(String, DependencySpec)> {
    // Forgejo: explicit `forgejo::host/owner/repo[@ref]` prefix. Checked
    // before the bare `user/repo` heuristic below so a forgejo spec
    // doesn't get misclassified as a malformed GitHub spec.
    if let Some(body) = raw.strip_prefix("forgejo::") {
        let (path_part, git_ref) = if let Some(at) = body.rfind('@') {
            (&body[..at], Some(body[at + 1..].to_string()))
        } else {
            (body, None)
        };

        let parts: Vec<&str> = path_part.split('/').collect();
        if parts.len() != 3 || parts.iter().any(|s| s.is_empty()) {
            anyhow::bail!(
                "Invalid Forgejo spec '{raw}'. Expected: forgejo::host/owner/repo or forgejo::host/owner/repo@ref"
            );
        }
        let name = parts[2].to_string();
        if !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '.' || c == '-' || c == '_')
        {
            anyhow::bail!("Invalid package name '{name}' extracted from Forgejo spec '{raw}'");
        }

        let spec = DependencySpec::Detailed(DetailedDep {
            git: Some(format!("forgejo::{path_part}")),
            rev: git_ref,
            ..Default::default()
        });
        return Ok((name, spec));
    }

    // GitHub: contains '/'
    if raw.contains('/') {
        ...
```

(Keep the rest of `parse_add_spec` unchanged; only the new `forgejo::` block is prepended.)

- [ ] **Step 4: Run tests**

Run: `cargo test -p uvr --lib commands::add::tests 2>&1 | tail -20`
Expected: all `parse_*` tests pass (existing + 3 new).

- [ ] **Step 5: Commit**

```bash
git add crates/uvr/src/commands/add.rs
git commit -m "add: recognize forgejo:: prefix in parse_add_spec"
```

---

## Task 7: Dispatch DESCRIPTION-name lookup by prefix

**Files:**
- Modify: `crates/uvr/src/commands/add.rs:142,227-310` (`resolve_github_pkg_names` and its call site)

The existing `resolve_github_pkg_names` post-processes manifest entries to swap the URL-derived basename for the actual `Package:` field. We extend it to forgejo and rename it for clarity.

- [ ] **Step 1: Rename the function and add a forgejo dispatch arm**

In `crates/uvr/src/commands/add.rs`:

1. Rename the function from `resolve_github_pkg_names` to `resolve_git_pkg_names`.
2. Update the single call site (currently at line 142) from `resolve_github_pkg_names(&mut parsed).await;` to `resolve_git_pkg_names(&mut parsed).await;`.
3. Inside the function (currently iterating over deps where `d.git.is_some()`), branch by prefix:

Replace the current body of the inner `for idx in needs_resolve { ... }` loop with:

```rust
    for idx in needs_resolve {
        let (provisional_name, spec) = &parsed[idx];
        let DependencySpec::Detailed(d) = spec else {
            continue;
        };
        let Some(git) = d.git.as_deref() else {
            continue;
        };
        let git_ref_owned = d.rev.as_deref().unwrap_or("HEAD").to_string();

        // Build the raw-DESCRIPTION URL appropriate for the registry.
        let desc_url = if let Some(body) = git.strip_prefix("forgejo::") {
            // body = "host/owner/repo"
            let parts: Vec<&str> = body.split('/').collect();
            if parts.len() != 3 || parts.iter().any(|s| s.is_empty()) {
                continue;
            }
            format!(
                "https://{host}/api/v1/repos/{owner}/{repo}/raw/DESCRIPTION?ref={r}",
                host = parts[0],
                owner = parts[1],
                repo = parts[2],
                r = git_ref_owned,
            )
        } else {
            // github: `user/repo`
            let spec_str = format!("{git}@{git_ref_owned}");
            let Some((user, repo, resolved_ref)) = parse_github_spec(&spec_str) else {
                continue;
            };
            format!("https://raw.githubusercontent.com/{user}/{repo}/{resolved_ref}/DESCRIPTION")
        };

        match client
            .get(&desc_url)
            .header("User-Agent", concat!("uvr/", env!("CARGO_PKG_VERSION")))
            .send()
            .await
            .and_then(|r| r.error_for_status())
        {
            Ok(resp) => {
                let text = resp.text().await.unwrap_or_default();
                let fields = uvr_core::dcf::parse_dcf_fields(&text);
                if let Some(actual) = fields.get("Package") {
                    let actual = actual.trim().to_string();
                    if !actual.is_empty() && actual != *provisional_name {
                        ui::bullet_dim(format!(
                            "{} → {} (Package: field in DESCRIPTION)",
                            palette::dim(provisional_name),
                            palette::pkg(&actual)
                        ));
                        parsed[idx].0 = actual;
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    "DESCRIPTION fetch failed for {git}@{git_ref_owned}: {e}; using {provisional_name} as the package name"
                );
                fetch_failures += 1;
            }
        }
    }
```

Note the `parse_github_spec` import in `add.rs` already exists; no new use needed.

Update the function's docstring to reflect both registries — change `GitHub-sourced dep` to `git-sourced dep (github or forgejo)`.

- [ ] **Step 2: Run all add-command tests**

Run: `cargo test -p uvr --lib commands::add 2>&1 | tail -20`
Expected: green. The function rename doesn't affect existing tests since they only call `parse_add_spec`.

- [ ] **Step 3: Build the whole workspace**

Run: `cargo build 2>&1 | tail -20`
Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add crates/uvr/src/commands/add.rs
git commit -m "add: resolve_git_pkg_names dispatches github vs forgejo by prefix"
```

---

## Task 8: Dispatch the lock-time BFS by prefix

**Files:**
- Modify: `crates/uvr/src/commands/lock.rs:11,134,217-300` (`resolve_github_deps` → `resolve_git_deps`)

- [ ] **Step 1: Add a unit test for the dispatcher's prefix discrimination**

The BFS itself hits the network; we can't unit-test the full loop without a mock server. But we can extract a tiny helper that classifies a `git` string and unit-test that. Add this helper to `crates/uvr/src/commands/lock.rs`:

```rust
/// Which registry to query for a `git = "..."` manifest value.
#[derive(Debug, PartialEq)]
enum GitKind {
    GitHub,
    Forgejo,
}

fn classify_git(git: &str) -> GitKind {
    if git.starts_with("forgejo::") {
        GitKind::Forgejo
    } else {
        GitKind::GitHub
    }
}
```

And, in the `tests` module of the same file (or add a `#[cfg(test)] mod tests { ... }` if one isn't there):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_git_distinguishes_prefixes() {
        assert_eq!(classify_git("tidyverse/ggplot2"), GitKind::GitHub);
        assert_eq!(classify_git("user/repo"), GitKind::GitHub);
        assert_eq!(
            classify_git("forgejo::codefloe.com/pat-s/mypkg"),
            GitKind::Forgejo
        );
    }
}
```

- [ ] **Step 2: Run the test to verify it passes (no prior compile failure expected — helper is new)**

Run: `cargo test -p uvr --lib commands::lock::tests::classify_git 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 3: Rename `resolve_github_deps` → `resolve_git_deps` and add the dispatch**

In `crates/uvr/src/commands/lock.rs`:

1. Update the import at line 11 to add `resolve_forgejo_package_with_remotes` and `parse_forgejo_spec`:

```rust
use uvr_core::registry::forgejo::{parse_forgejo_spec, resolve_forgejo_package_with_remotes};
use uvr_core::registry::github::{parse_github_spec, resolve_github_package_with_remotes};
```

2. Rename the call at line 134:

```rust
    let github_fut = resolve_github_deps(client, &project.manifest);
```

to:

```rust
    let git_fut = resolve_git_deps(client, &project.manifest);
```

…and update the binding receiver further down where `let github_resolved = github_fut.await?;` appears (search for it; it's a few lines later). Rename the binding to `git_resolved` and update any downstream reference.

3. Replace the body of the BFS (currently the `while let Some(spec) = queue.pop_front()` block) so the per-item resolution dispatches:

```rust
    while let Some(spec) = queue.pop_front() {
        if !visited_specs.insert(spec.clone()) {
            continue;
        }
        let is_direct = direct_specs.contains(&spec);

        // GithubRemote and ForgejoRemote are both aliases for
        // `(String, String, Option<String>)`, so this match unifies into a
        // single `Result<(PackageInfo, Vec<(String, String, Option<String>)>), UvrError>`.
        // The middle string carries the canonical next-hop spec for each
        // registry: `"user/repo"` for github, `"forgejo::host/owner/repo"`
        // for forgejo, so the BFS classifier on the next pop works
        // correctly with no further rewriting.
        let resolved = match classify_git(&spec) {
            GitKind::Forgejo => {
                let body = spec.strip_prefix("forgejo::").unwrap_or(&spec);
                let Some((host, owner, repo, git_ref)) = parse_forgejo_spec(body) else {
                    continue;
                };
                resolve_forgejo_package_with_remotes(client, &host, &owner, &repo, &git_ref).await
            }
            GitKind::GitHub => {
                let Some((user, repo, git_ref)) = parse_github_spec(&spec) else {
                    continue;
                };
                resolve_github_package_with_remotes(client, &user, &repo, &git_ref).await
            }
        };

        let (info, remotes) = match resolved {
            Ok(pair) => pair,
            Err(e) if is_direct => {
                return Err(e).with_context(|| format!("Failed to resolve git package {spec}"));
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to fetch git Remote {spec} ({e}); falling back to registry resolution"
                );
                continue;
            }
        };

        if let Some(existing) = pre_resolved.get(&info.name) {
            tracing::warn!(
                "git package {} already resolved from a different spec ({}); discarding {}",
                info.name,
                existing.checksum.as_deref().unwrap_or("?"),
                spec
            );
        } else {
            pre_resolved.insert(info.name.clone(), info);
        }

        for (_dep_name, repo_path, rev) in remotes {
            // `repo_path` is already in canonical form:
            //   github  → "user/repo"
            //   forgejo → "forgejo::host/owner/repo"
            // so we don't need to re-prefix.
            let next_spec = match rev {
                Some(r) => format!("{repo_path}@{r}"),
                None => repo_path,
            };
            if !visited_specs.contains(&next_spec) {
                queue.push_back(next_spec);
            }
        }
    }
```

4. Rename the function signature itself: `async fn resolve_github_deps(...)` → `async fn resolve_git_deps(...)`. Update the existing docstring to mention forgejo alongside github.

- [ ] **Step 4: Build and run all existing lock tests**

Run: `cargo build 2>&1 | tail -20`
Run: `cargo test -p uvr --lib commands::lock 2>&1 | tail -20`
Expected: clean build; existing lock tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/uvr/src/commands/lock.rs
git commit -m "lock: resolve_git_deps dispatches github vs forgejo by prefix"
```

---

## Task 9: `source_url` handles Forgejo

**Files:**
- Modify: `crates/uvr/src/commands/sync.rs:1387-1405` (the `source_url` function)

- [ ] **Step 1: Run the build to see the exhaustive-match failure**

Run: `cargo build 2>&1 | grep -E "(non-exhaustive|missing variant|Forgejo)" | head -20`
Expected: a `non-exhaustive patterns: ` ... `Forgejo` error at `source_url`.

- [ ] **Step 2: Add the Forgejo arm**

In `crates/uvr/src/commands/sync.rs`, change (around line 1387-1405):

```rust
    match pkg.source {
        PackageSource::Cran => format!(...),
        PackageSource::Bioconductor => { ... }
        PackageSource::GitHub | PackageSource::Local => String::new(),
        PackageSource::Custom { .. } => { ... }
    }
```

to:

```rust
    match pkg.source {
        PackageSource::Cran => format!(...),
        PackageSource::Bioconductor => { ... }
        // Forgejo, GitHub, and Local always have `url` populated by the
        // resolver (or are file:// paths handled elsewhere); the
        // `if let Some(url) ...` guard at the top of this function takes
        // the URL straight from `pkg.url`. If we reach this arm with no
        // URL, something earlier mis-resolved; return empty and let the
        // sync surface a clear download error.
        PackageSource::Forgejo { .. }
        | PackageSource::GitHub
        | PackageSource::Local => String::new(),
        PackageSource::Custom { .. } => { ... }
    }
```

(Keep the existing match-arm bodies; only the patterns change.)

- [ ] **Step 3: Build cleanly**

Run: `cargo build 2>&1 | tail -10`
Expected: clean.

- [ ] **Step 4: Run lockfile-equivalence test path (already exercised by sync tests)**

Run: `cargo test -p uvr --lib commands::sync 2>&1 | tail -20`
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add crates/uvr/src/commands/sync.rs
git commit -m "sync: source_url leaves Forgejo URL untouched (resolver populates it)"
```

---

## Task 10: renv export — emit Forgejo as `Source: Git`

**Files:**
- Modify: `crates/uvr/src/commands/export.rs:69-93`

renv has no `Forgejo` source type. The closest renv-understood mapping is `Source: "Git"` (renv's git2r-backed remote) with `RemoteUrl` set to the public clone URL.

- [ ] **Step 1: Add a failing test**

Append to the `tests` module in `crates/uvr/src/commands/export.rs`:

```rust
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
            url: Some(
                "https://codefloe.com/api/v1/repos/pat-s/mypkg/archive/abc123.tar.gz".into(),
            ),
            requires: vec![],
            raw_version: None,
            system_requirements: None,
            dev: false,
        };
        let (source, repository) = export_source_and_repository(&pkg.source);
        assert_eq!(source, "Git");
        assert_eq!(repository, None);
    }
```

Also add a small public-in-module helper that we'll call from the existing `match`:

```rust
    fn export_source_and_repository(src: &uvr_core::lockfile::PackageSource) -> (String, Option<String>) {
        match src {
            uvr_core::lockfile::PackageSource::Cran => ("Repository".to_string(), Some("CRAN".to_string())),
            uvr_core::lockfile::PackageSource::Bioconductor => ("Bioconductor".to_string(), None),
            uvr_core::lockfile::PackageSource::GitHub => ("GitHub".to_string(), None),
            uvr_core::lockfile::PackageSource::Forgejo { .. } => ("Git".to_string(), None),
            uvr_core::lockfile::PackageSource::Local => ("Local".to_string(), None),
            uvr_core::lockfile::PackageSource::Custom { name } => ("Repository".to_string(), Some(name.clone())),
        }
    }
```

- [ ] **Step 2: Run to verify failure (compile error: helper not in scope)**

Run: `cargo test -p uvr --lib commands::export::tests::export_forgejo_package 2>&1 | tail -20`
Expected: compile error.

- [ ] **Step 3: Extract the helper into module scope and call it**

Move `export_source_and_repository` out of the `tests` module into `crates/uvr/src/commands/export.rs` proper (just above the renv export function). Replace the existing inline `match` (around line 69) that produces `(source, repository)` with a call to this helper.

Also extend `parse_github_remote`'s caller block (around line 89) so Forgejo packages get a `RemoteUrl` populated for renv re-import. Add a `parse_forgejo_remote` sibling that extracts host/owner/repo/sha from the archive URL:

```rust
fn parse_forgejo_remote(url: &str) -> Option<(String /*host*/, String, String, String)> {
    // URL shape: https://<host>/api/v1/repos/<owner>/<repo>/archive/<sha>.tar.gz
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
    let host = parts.get(2).copied()?.to_string();
    Some((host, owner, repo, sha))
}
```

And in the package-emission loop, populate the renv fields for Forgejo packages:

```rust
        let remote_info = match &pkg.source {
            PackageSource::GitHub => pkg.url.as_ref().and_then(|u| parse_github_remote(u)),
            PackageSource::Forgejo { .. } => None, // handled below via remote_url
            _ => None,
        };

        let forgejo_info = match &pkg.source {
            PackageSource::Forgejo { .. } => pkg.url.as_ref().and_then(|u| parse_forgejo_remote(u)),
            _ => None,
        };
```

Extend the `RenvPackage` struct with a `RemoteUrl` field (so the renv re-importer can clone the right URL):

```rust
    #[serde(rename = "RemoteUrl", skip_serializing_if = "Option::is_none")]
    remote_url: Option<String>,
```

And populate it for forgejo:

```rust
        let entry = RenvPackage {
            ...
            remote_username: remote_info
                .as_ref()
                .map(|(user, _, _)| user.clone())
                .or_else(|| forgejo_info.as_ref().map(|(_, owner, _, _)| owner.clone())),
            remote_repo: remote_info
                .as_ref()
                .map(|(_, repo, _)| repo.clone())
                .or_else(|| forgejo_info.as_ref().map(|(_, _, repo, _)| repo.clone())),
            remote_ref: remote_info
                .as_ref()
                .and_then(|(_, _, r)| r.clone())
                .or_else(|| forgejo_info.as_ref().map(|(_, _, _, sha)| sha.clone())),
            remote_url: forgejo_info
                .as_ref()
                .map(|(host, owner, repo, _)| format!("https://{host}/{owner}/{repo}")),
        };
```

Add a test for the URL parser:

```rust
    #[test]
    fn parse_forgejo_remote_archive_url() {
        let url = "https://codefloe.com/api/v1/repos/pat-s/mypkg/archive/abc123.tar.gz";
        let (host, owner, repo, sha) = parse_forgejo_remote(url).unwrap();
        assert_eq!(host, "codefloe.com");
        assert_eq!(owner, "pat-s");
        assert_eq!(repo, "mypkg");
        assert_eq!(sha, "abc123");
    }
```

- [ ] **Step 4: Run export tests + workspace build**

Run: `cargo test -p uvr --lib commands::export 2>&1 | tail -30`
Run: `cargo build 2>&1 | tail -10`
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add crates/uvr/src/commands/export.rs
git commit -m "export: emit Forgejo packages as renv Source: Git with RemoteUrl"
```

---

## Task 11: Network-gated integration test against a real Forgejo host

**Files:**
- Create: `crates/uvr-core/tests/forgejo_live.rs`

This test is ignored by default so CI stays offline-stable. Runnable locally with `cargo test -- --ignored`.

- [ ] **Step 1: Create the test file**

Create `crates/uvr-core/tests/forgejo_live.rs`:

```rust
//! Network-gated test: resolve a real public Forgejo repo end-to-end.
//!
//! Skipped by default. Run with:
//!     cargo test -p uvr-core --test forgejo_live -- --ignored

use uvr_core::registry::forgejo::resolve_forgejo_package;

#[tokio::test]
#[ignore = "requires network access to codeberg.org"]
async fn resolve_public_forgejo_repo() {
    let client = reqwest::Client::builder()
        .user_agent("uvr-test")
        .build()
        .expect("build client");

    // codeberg.org is a public Forgejo instance. Pick a small public R
    // package repo at a stable tag. If the chosen repo is removed or
    // renamed, swap to any other small public Forgejo-hosted R package
    // — the test is exercising the API surface, not this specific repo.
    let info = resolve_forgejo_package(
        &client,
        "codeberg.org",
        "Codeberg",       // owner — placeholder; replace with a real org/user hosting an R pkg
        "Documentation",  // repo  — placeholder; replace likewise
        "main",
    )
    .await
    .expect("resolve");

    assert!(!info.name.is_empty(), "package name from DESCRIPTION");
    assert!(
        info.url
            .starts_with("https://codeberg.org/api/v1/repos/"),
        "archive URL pinned to /api/v1/: {}",
        info.url
    );
    assert!(
        info.url.ends_with(".tar.gz"),
        "archive URL ends in .tar.gz"
    );
}
```

- [ ] **Step 2: Verify it builds and runs only with `--ignored`**

Run: `cargo test -p uvr-core --test forgejo_live 2>&1 | tail -10`
Expected: 0 tests run (ignored).

Run (optional, when online): `cargo test -p uvr-core --test forgejo_live -- --ignored 2>&1 | tail -20`
Expected: passes against the chosen public Forgejo repo. **Before merging**, replace the owner/repo placeholders in the test with a real public Forgejo-hosted R package and verify locally.

- [ ] **Step 3: Commit**

```bash
git add crates/uvr-core/tests/forgejo_live.rs
git commit -m "test: live Forgejo resolver integration test (ignored)"
```

---

## Task 12: Final verification

- [ ] **Step 1: Full build + test pass**

Run: `cargo build 2>&1 | tail -10`
Run: `cargo test --workspace 2>&1 | tail -20`
Expected: clean build, all tests green (the `forgejo_live` test stays ignored).

- [ ] **Step 2: Manual smoke (optional, requires network + a real public Forgejo R package)**

In a scratch dir:

```sh
cd /tmp && mkdir uvr-forgejo-smoke && cd uvr-forgejo-smoke
cargo run -p uvr -- init smoke
cargo run -p uvr -- add forgejo::<host>/<owner>/<r-package>@<ref>
cat uvr.toml uvr.lock
```

Expected: `uvr.toml` contains `git = "forgejo::<host>/<owner>/<repo>"`; `uvr.lock` contains `source = "forgejo:<host>"` and a `url = "https://<host>/api/v1/repos/.../archive/<sha>.tar.gz"`.

- [ ] **Step 3: Run cargo fmt**

Run: `cargo fmt --all 2>&1 | tail -5`
Expected: no output (or `cargo fmt --all -- --check` shows clean).

- [ ] **Step 4: Final commit, only if Step 3 changed anything**

```bash
git diff --stat
# if there are formatting changes:
git add -u
git commit -m "style: cargo fmt"
```
