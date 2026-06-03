# Forgejo remote support

Status: design approved
Author: pat-s
Date: 2026-05-26

## Goal

Let users install R packages hosted on Forgejo instances (`codefloe.com`, `codeberg.org`, self-hosted) the same way they install GitHub-hosted packages today.

End state:

```sh
uvr add forgejo::codefloe.com/pat-s/mypkg@v0.1.0
```

```toml
# uvr.toml
[dependencies]
mypkg = { git = "forgejo::codefloe.com/pat-s/mypkg", rev = "v0.1.0" }
```

```
# DESCRIPTION
Remotes: forgejo::codefloe.com/pat-s/mypkg@v0.1.0
```

All three forms resolve, lock, and install. Cross-host `Remotes:` chains (a github pkg pointing at a forgejo dep, or vice versa) walk transitively.

## Non-goals

- Gitea, GitLab, Bitbucket, or generic `git::` URL support. Forgejo only.
- HTTP (non-TLS) Forgejo hosts. Always `https://`.
- A new top-level command or a new manifest field. Reuses the existing `git = "..."` field.
- Private-repo auth via netrc, ssh-agent, or git-credential. Environment variables only.

## Syntax

Single canonical form, used identically in three contexts (CLI, manifest, DESCRIPTION):

```
forgejo::<host>[:<port>]/<owner>/<repo>[@<ref>]
```

- `<host>` — hostname or `host:port`. Validated as `[a-z0-9.-]+(?::[0-9]+)?`. No scheme; `https://` is implicit. Hosts containing `/` before the third segment are rejected.
- `<ref>` — branch, tag, or commit SHA. Omitted ref defaults to `HEAD`.

The backward-compatible bare `user/repo` form continues to mean GitHub. The `forgejo::` prefix is mandatory for Forgejo specs — no host-sniffing, no auto-detection.

## Data model

### Manifest

Reuses `DetailedDep.git: Option<String>` verbatim. The prefix on the string discriminates the registry:

- `git = "user/repo"` → GitHub (existing).
- `git = "forgejo::host/owner/repo"` → Forgejo (new).

No schema change. No new manifest field.

### Lockfile

New `PackageSource` variant:

```rust
pub enum PackageSource {
    Cran,
    Bioconductor,
    GitHub,
    Forgejo { host: String },   // new
    Local,
    Custom { name: String },
}
```

Serialization: `Forgejo { host: "codefloe.com" }` ↔ string `"forgejo:codefloe.com"`. The deserializer in `lockfile.rs` recognizes the `forgejo:` prefix, extracts the host, and falls back to `Custom { name }` if the host is empty (defensive — never panics on a malformed lockfile).

## API endpoints

All three calls hit `/api/v1/` on the user-supplied host. No Gitea-only routes. No web-UI routes (`/raw/...`, `/<owner>/<repo>/archive/...`).

| Step | Endpoint | Output |
|------|----------|--------|
| Resolve ref → SHA | `GET https://<host>/api/v1/repos/{owner}/{repo}/commits?sha={ref}&limit=1` | JSON array; first element's `.sha` |
| Fetch DESCRIPTION | `GET https://<host>/api/v1/repos/{owner}/{repo}/raw/DESCRIPTION?ref={sha}` | Raw file |
| Tarball (stored in lockfile, downloaded at sync) | `https://<host>/api/v1/repos/{owner}/{repo}/archive/{sha}.tar.gz` | gzip tarball |

Differences from `github.rs`:

- GitHub's `/repos/{o}/{r}/commits/{ref}` returns a single commit object; Forgejo's `/commits/{ref}` endpoint 404s (the Gitea-compatible route is not exposed by Forgejo's HTTP router), so we use the list-commits endpoint with `?sha=<ref>&limit=1` and read the first element. Accepts branches, tags, and SHAs the same way GitHub's does. One small `serde_json` deserialization per dep.
- All three endpoints share the same `/api/v1/` base. GitHub mixes `api.github.com` (commits, tarball) and `raw.githubusercontent.com` (DESCRIPTION).

## Code organization

### New file: `crates/uvr-core/src/registry/forgejo.rs`

```rust
pub type ForgejoRemote = (String /*pkg*/, String /*"forgejo::host/owner/repo"*/, Option<String>);

pub fn parse_forgejo_spec(spec: &str)
    -> Option<(String /*host*/, String /*owner*/, String /*repo*/, String /*ref*/)>;

pub async fn resolve_forgejo_package(
    client: &reqwest::Client,
    host: &str, owner: &str, repo: &str, git_ref: &str,
) -> Result<PackageInfo>;

pub async fn resolve_forgejo_package_with_remotes(
    client: &reqwest::Client,
    host: &str, owner: &str, repo: &str, git_ref: &str,
) -> Result<(PackageInfo, Vec<ForgejoRemote>)>;

fn forgejo_token(host: &str) -> Option<String>;
fn parse_forgejo_remotes(desc_fields: &BTreeMap<String, String>) -> Vec<ForgejoRemote>;
```

Shape mirrors `github.rs`. Parses DESCRIPTION fields via the existing `crate::dcf::parse_dcf_fields` and `crate::registry::cran::parse_dep_field` helpers — no duplication of that logic.

### Touched files

| File | Change |
|------|--------|
| `crates/uvr-core/src/lockfile.rs` | Add `Forgejo { host }` variant; extend `Serialize`/`Deserialize`/`Display`. |
| `crates/uvr-core/src/registry/mod.rs` | `pub mod forgejo;` |
| `crates/uvr-core/src/manifest.rs::parse_remotes_field` | Keep `forgejo::` entries (currently silently dropped). Stored as `git = "forgejo::host/owner/repo"`. |
| `crates/uvr/src/commands/add.rs::parse_add_spec` | Detect `forgejo::` prefix before the existing `/`-as-github heuristic. |
| `crates/uvr/src/commands/add.rs::resolve_github_pkg_names` | Generalize to dispatch by prefix; rename `resolve_git_pkg_names`. |
| `crates/uvr/src/commands/lock.rs::resolve_github_deps` | Rename `resolve_git_deps`. Dispatch by prefix in the BFS loop. |
| `crates/uvr/src/commands/sync.rs::source_url` | Add `PackageSource::Forgejo { .. }` to the `""` arm (URL is always populated by the resolver). |
| `crates/uvr/src/commands/export.rs` | Map `Forgejo` to renv's `Source: Git` + `RemoteType: git2r` + `RemoteUrl: https://<host>/<owner>/<repo>` + `RemoteUsername/Repo/Ref` for round-trip-into-renv. (renv has no Forgejo-aware source type; `git2r` is the closest renv-understood `RemoteType` for a generic clone-from-URL flow.) |

### Cross-host `Remotes:` traversal

The BFS in `resolve_git_deps` (formerly `resolve_github_deps`) handles cross-registry chains naturally:

- `parse_remotes_field` now emits both `git = "user/repo"` (github) and `git = "forgejo::host/owner/repo"` (forgejo) entries.
- The BFS pops a spec, sniffs its prefix, dispatches to the matching resolver, and pushes the resolver's returned remotes back onto the queue.
- A github pkg whose `Remotes:` lists a forgejo dep — or vice versa — resolves end-to-end without falling through to CRAN.

## Authentication

`forgejo_token(host)` lookup order:

1. Normalize host: strip port, uppercase, replace `.` and `-` with `_`. `codefloe.com` → `CODEFLOE_COM`; `git.local:3000` → `GIT_LOCAL`.
2. `UVR_FORGEJO_TOKEN_<NORMALIZED>` (per-host).
3. `UVR_FORGEJO_TOKEN` (single token; useful when the user only talks to one instance).
4. None → unauthenticated.

Sent as `Authorization: token <value>` (Forgejo's documented header style; not `Bearer`).

Mirrors the `github_token()` helper that landed upstream for GitHub auth — same conventions (env-only, no file lookup, fall back silently to unauthenticated).

## Error surface

| HTTP | Message |
|------|---------|
| 401, 403 | `Forgejo returned {status} for {host}/{owner}/{repo}; set UVR_FORGEJO_TOKEN_<HOST> if the repo is private.` |
| 404 | `Forgejo repository not found: {host}/{owner}/{repo}@{ref}. Check the spec and that the repo exists.` |
| Other 4xx, 5xx | Bubble up via `UvrError::Other` with the upstream body. |
| Transport | Existing reqwest error path. |

Failure policy is unchanged from github:

- Manifest-direct forgejo specs hard-error on resolution failure.
- Transitive `Remotes:`-discovered forgejo specs warn-and-skip; the registry chain takes over.

## Tests

### Unit (`forgejo.rs`)

- `parse_forgejo_spec`:
  - happy: `forgejo::codefloe.com/pat-s/mypkg@v0.1.0` → `("codefloe.com", "pat-s", "mypkg", "v0.1.0")`
  - default ref: missing `@ref` → `"HEAD"`
  - port: `forgejo::git.local:3000/u/r` accepted
  - reject: scheme in host (`https://codefloe.com/u/r`), empty host, missing `/owner/repo`
- `parse_forgejo_remotes`: a DESCRIPTION whose `Remotes:` mixes `github::`, `forgejo::`, and `gitlab::` keeps only github and forgejo entries (no behavior change for github).

### Lockfile round-trip (`lockfile.rs`)

- `PackageSource::Forgejo { host: "codefloe.com" }` ↔ `"forgejo:codefloe.com"`
- Malformed `"forgejo:"` (empty host) deserializes to `Custom { name: "forgejo:" }` (no panic, no silent default to a wrong host).

### CLI/manifest

- `parse_add_spec("forgejo::codefloe.com/pat-s/mypkg@main", false)` → `("mypkg", Detailed { git: "forgejo::codefloe.com/pat-s/mypkg", rev: "main" })`
- `parse_remotes_field("forgejo::codefloe.com/pat-s/mypkg, github::a/b, gitlab::x/y")` keeps the first two, drops the third.

### Lock-time BFS (`lock.rs`)

- Direct forgejo dep resolves end-to-end (with `mockito` for the API).
- A github pkg whose `Remotes:` lists a forgejo dep walks both registries.

### Integration (network-gated, `#[ignore]`)

- Resolve a real public Forgejo repo. Skipped by default to keep CI offline-stable; runnable locally with `cargo test -- --ignored`.

## Out of scope (deferred)

- Generic `git::https://...` URLs (would need host-sniffing or per-host config; revisit if Gitea/GitLab support comes up).
- HTTP (non-TLS) Forgejo hosts (corporate intranet; opt-in flag when asked).
- SSH-based access to private repos.
- `netrc` and `git-credential` integration.
- Forgejo's `/api/forgejo/v1/` namespace (federation, etc.) — not needed for package resolution.
