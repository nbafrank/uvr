# uv feature parity — design

Status: draft — awaiting approval
Author: gdevenyi
Date: 2026-07-23

## Goal

Audit uvr's feature set against [`uv`](https://github.com/astral-sh/uv) and close the
gaps that a real R developer's workflow needs — reshaped to R idioms rather than
copied literally. This document is the umbrella design; each accepted feature gets a
companion forgejo-grade implementation plan under
`docs/superpowers/plans/2026-07-23-uv-parity-NN-*.md`.

Six features are in scope, in three priority tiers:

| Tier | ID | Feature | uv analog |
|------|-----|---------|-----------|
| P0 | F0 | Shell activation | `source .venv/bin/activate` |
| P0 | F1 | Inline per-script dependencies | PEP 723 `# /// script` |
| P0 | F5 | Private-repo authentication | `uv auth` / index credentials |
| P1 | F3 | Dependency sources (path / git-host / url) | `[tool.uv.sources]` |
| P1 | F6 | Ergonomic parity commands | `cache prune/dir/size`, `python dir/find` |
| P2 | F4 | Resolution controls | `--resolution`, `--exclude-newer`, overrides/constraints |

(IDs match the grilling session's candidate numbers; `#2` produced exclusions only, so
there is no F2.)

## Governing principle — R-native workflow parity

A uv capability earns a place in uvr **only if a real R developer needs that outcome**,
and it is then reshaped to R idioms. Capabilities that are Python-ecosystem artifacts
with no honest R equivalent are excluded as category errors (see
[Out of scope](#out-of-scope)). Every inclusion and exclusion below is judged against
that one test.

## Parity matrix

The full uv surface, each row judged against the governing principle. Verdicts:
**PARITY** (already equivalent, no work), **BUILD** (accepted gap, designed below),
**EXCLUDE** (category error / out of scope, rationale given).

| uv capability | uvr today | Verdict | Notes |
|---|---|---|---|
| `uv init` | `uvr init` (`--here`, `--r-version`) | PARITY | uv's `--lib/--app/--package/--bare` are packaging modes, not analysis-project modes. |
| `uv add` | `uvr add` (`--dev`, `--bioc`, `--source`, git) | PARITY + BUILD (F3) | Gains `path`/`url`/non-GitHub git via F3. |
| `uv remove` | `uvr remove` | PARITY | |
| `uv sync` | `uvr sync` (`--frozen`, `--no-dev`, `--library`, `--ignore-cache`) | PARITY | `--locked` == `--frozen`. |
| `uv lock` | `uvr lock` (`--upgrade`) | PARITY + BUILD (F4) | Gains `--upgrade-package`, `--resolution`, `--exclude-newer`. |
| `uv run` | `uvr run` (`--r-version`, `--with`) | PARITY + BUILD (F1) | Gains inline-dependency headers + standalone script execution. |
| `uv tree` | `uvr tree` (`--depth`) | PARITY | |
| `uv export` | `uvr export` (renv) | PARITY | renv is R's interchange format; requirements.txt/pylock have no R analog. |
| `uv python …` | `uvr r …` (install/uninstall/list/use/pin/javareconf) | PARITY + BUILD (F6) | Gains `r dir`, `r find`. `r upgrade`/`r update-shell` excluded. |
| `uv venv` | auto-managed `.uvr/library/` | EXCLUDE | No venv object in R; F0 delivers the *activation* half users actually want. |
| `uv cache clean` | `uvr cache clean` (`--package`, `--r-version`) | PARITY | |
| `uv cache prune/dir/size` | — | BUILD (F6) | |
| `uv self update` | `uvr upgrade` / `self-update` | PARITY | |
| `uv self version` | `uvr --version` | PARITY | |
| `uv generate-shell-completion` | `uvr completions` | PARITY | |
| script inline metadata (PEP 723) | — (only `--with`) | BUILD (F1) | The headline gap; live edge over `rv`. |
| `[tool.uv.sources]` git (any host) | GitHub + Forgejo only | BUILD (F3) | Closes #123 (GitLab). |
| `[tool.uv.sources]` path | — | BUILD (F3) | |
| `[tool.uv.sources]` url | — | BUILD (F3) | |
| `[tool.uv.sources]` editable / workspace | — | EXCLUDE | No R live-reload of installed libs; devtools owns it. Workspaces cut. |
| dependency groups (PEP 735) | `dev` only | EXCLUDE | dev/default covers the R case; groups don't map to DESCRIPTION. |
| extras / optional-dependencies | Suggests → dev | EXCLUDE | No `install.packages("pkg[extra]")`; category error. |
| index auth / `uv auth` / keyring | git-host env token only | BUILD (F5) | Private PPM is the enterprise wall. Keyring + `auth` CLI excluded. |
| `--resolution` / `--prerelease` | highest-only | BUILD (F4, lowest) | `--prerelease` excluded — no CRAN prerelease channel. |
| overrides / constraints | — | BUILD (F4) | |
| `--upgrade-package` | via `uvr update <pkg>` | PARITY + BUILD (F4) | Capability exists; F4 adds the `lock --upgrade-package` spelling. |
| universal / cross-platform lockfile | already platform-neutral | PARITY | `LockedPackage` pins source, not OS/arch; binary chosen at sync. |
| `uv build` / `uv publish` | — | EXCLUDE | CRAN/r-universe submission is devtools/usethis territory, manual review. |
| `uvx` / `uv tool …` | — | EXCLUDE | R packages don't ship CLIs. |
| `uv pip …` | import/export renv | EXCLUDE | Imperative escape hatch breaks lockfile-first identity. |
| `uv version` (project version) | — | EXCLUDE | Analysis projects don't self-version; packages carry it in DESCRIPTION. |
| `uv format` / `uv check` / `uv audit` | `uvr doctor` (adjacent) | EXCLUDE | Not uv's core identity; R has air/styler + R CMD check + oysteR. |
| `uvr scan` / `uvr import` / `uvr doctor` | present | (uvr-ahead) | No uv analog; uvr is ahead here. |

## Cross-cutting: the shared environment builder

Two P0 features (F0, F1) and the existing `uvr run` all need the same thing — the
exact set of environment variables that isolate R onto the project's library and
managed interpreter. Today that logic is **inline** in `run()`
(`crates/uvr/src/commands/run.rs:62-102`):

```
R_LIBS_USER = [<with_lib><sep>]<library>[<sep><UVR_EXTRA_LIBS>]
R_LIBS_SITE = ""          # shadow the system site library (isolation)
R_LIBS      = ""
DYLD_LIBRARY_PATH = <r_lib_dir>   # so compiled pkgs find libR at runtime
LD_LIBRARY_PATH   = <r_lib_dir>
R_ENVIRON   = ""          # + process flag --no-environ
```

**Refactor (prerequisite for F0/F1):** extract this into a single pure function in
`uvr-core`:

```rust
// crates/uvr-core/src/r_env.rs (new)
pub struct REnv {
    pub r_binary: PathBuf,        // absolute path to the chosen R
    pub r_bin_dir: PathBuf,       // r_binary.parent()  — for PATH (activation only)
    pub r_lib_dir: PathBuf,       // r_binary.parent().parent()/lib
    pub library: PathBuf,         // project or with-env library
    pub with_library: Option<PathBuf>,
    pub extra_libs: Option<String>,
}

impl REnv {
    /// The (key, value) pairs run.rs sets today. Single source of truth.
    pub fn vars(&self) -> Vec<(String, String)>;
}
```

`run()` is rewritten to build an `REnv` and apply `.vars()` to its `Command`; nothing
about `uvr run`'s behavior changes (verified by its existing tests). F0 emits the same
pairs as shell exports; F1 reuses `ensure_with_env` unchanged. This refactor ships as
Task 1 of both the F0 and F1 plans (whichever lands first; the second no-ops it).

---

## F0 — Shell activation

### Goal

`source .uvr/activate` puts the project's managed R and isolated library into the
current shell, so a bare `R` / `Rscript` uses the project without an `uvr run` prefix —
the interactive-console and RStudio/Positron-terminal workflow. Survives an R-version
change without regeneration.

### Non-goals

- A `uvr venv`-style env-creation primitive (the library is auto-managed).
- `direnv`-style auto-activation on `cd` (possible later layer; not now).
- Modifying the user's `~/.bashrc`/profile (activation is explicit and per-shell).

### Delivery mechanism (decided)

A **sourceable shim that delegates to the dynamic binary**. `uvr init` (and a new
`uvr activate --write-shim`) generates a thin per-shell file:

```sh
# .uvr/activate  (POSIX sh/bash/zsh)
eval "$(command uvr activate --emit sh)"
```

Sourcing it runs `uvr activate --emit <shell>`, which re-reads `uvr.toml`/`.r-version`
**every activation**, so an R-version change can't leave a stale interpreter baked in.
Per-shell shims: `.uvr/activate` (sh/bash/zsh), `.uvr/activate.fish`,
`.uvr/Activate.ps1` — the same four shells as `uvr completions`.

### CLI surface

| Command | Behavior |
|---|---|
| `uvr activate` | Human path: print the one line to source (`source .uvr/activate`) + a hint. |
| `uvr activate --emit <shell>` | Machine path: print `export`/`set` statements for the given shell to stdout. `<shell>` ∈ `sh\|bash\|zsh\|fish\|powershell`. |
| `uvr activate --write-shim` | (Re)write the per-shell shim files into `.uvr/`. Idempotent; also called by `uvr init`. |

`deactivate` is a shell function defined by the emitted block; it restores saved state
and unsets itself. No uvr subcommand needed.

### Emitted environment

`uvr activate --emit sh` prints, in order:

1. Save prior state for `deactivate`: `UVR_OLD_PATH`, `UVR_OLD_R_LIBS_USER`,
   `UVR_OLD_R_LIBS_SITE`, `UVR_OLD_R_LIBS`, `UVR_OLD_R_ENVIRON`,
   `UVR_OLD_DYLD_LIBRARY_PATH`, `UVR_OLD_LD_LIBRARY_PATH`, and (if set) the prompt.
2. `export PATH="<r_bin_dir><sep>$PATH"` — **new vs `run`**: `run` invokes R by absolute
   path and never touches `PATH`; activation must, so a bare `R` resolves to the
   project's R.
3. The `REnv::vars()` pairs from the shared builder (`R_LIBS_USER`, `R_LIBS_SITE=""`,
   `R_LIBS=""`, `DYLD_LIBRARY_PATH`, `LD_LIBRARY_PATH`, `R_ENVIRON=""`).
4. `export UVR_PROJECT="<name>"` and, **opt-in only** (`UVR_ACTIVATE_PROMPT=1` or
   `[activate] prompt = true` in `uvr.toml`), prepend `(<name>) ` to the prompt.
5. `deactivate()` — restores every saved var, unsets the `UVR_*` bookkeeping, resets the
   prompt, and `unset -f deactivate`.

Per-shell differences: fish uses `set -gx` / `set -e` and `functions -e`; PowerShell
uses `$env:` assignment and a `function global:deactivate`.

### Data model

No manifest or lockfile change. Optional `uvr.toml` block for the prompt preference:

```toml
[activate]
prompt = false   # default; true opts into the (project) prompt prefix
```

### Touched files

| File | Change |
|---|---|
| `crates/uvr-core/src/r_env.rs` *(new)* | Shared `REnv` builder (also used by F1 + run). |
| `crates/uvr/src/commands/run.rs:62-102` | Rewrite to build `REnv` and apply `.vars()`. |
| `crates/uvr/src/commands/activate.rs` *(new)* | `--emit <shell>`, `--write-shim`, human path; per-shell emitters. |
| `crates/uvr/src/cli.rs:40-92` | Add `Activate(ActivateArgs)` to `Commands`. |
| `crates/uvr/src/commands/init.rs` | Call `write_shim` after project creation. |
| `crates/uvr-core/src/manifest.rs:27-42` | Optional `[activate] prompt` (new `ActivateMeta`). |
| `.gitignore` template | Add `.uvr/activate*` (generated, like `.uvr/library/`). |

### Error surface

- Outside a project: `uvr activate` errors `No uvr.toml found; run 'uvr init' first.`
- No R satisfies the constraint: same message `uvr run` gives, plus `deactivate` is not
  emitted (activation aborts cleanly, shell untouched).

### Tests

- `activate --emit sh` output contains `PATH=`, `R_LIBS_USER=`, `R_LIBS_SITE=`, and a
  `deactivate()` function; golden-file per shell (sh/fish/powershell).
- Emitted `R_LIBS_*` values equal `REnv::vars()` (shared-builder equivalence — the
  anti-drift test).
- Prompt prefix absent by default, present under `UVR_ACTIVATE_PROMPT=1`.
- Round-trip: emit → source in a subshell → `R RHOME` resolves to the managed R;
  `deactivate` restores `PATH` (integration, `#[ignore]`, needs a managed R).

---

## F1 — Inline per-script dependencies

### Goal

A `.R` script carries its own pinned dependencies (and optional R version) in a header
comment, so `uvr run script.R` runs it **standalone in any directory** — no project, no
setup — and reproduces identically for whoever receives it. The R analog of PEP 723.

### Non-goals

- Inferring dependencies from `library()`/`require()` calls (that is `uvr scan`'s job;
  header is authoritative and explicit).
- Editing project state: a headered run is isolated and never touches `uvr.toml`/`uvr.lock`.
- A published metadata standard: none exists in R, so uvr defines the format, mirroring
  PEP 723's structure.

### Syntax — the header

A fenced comment block, uvr-owned, near the top of the file:

```r
# /// script
# r = ">=4.3"
# dependencies = [
#   "ggplot2>=3.4",
#   "DESeq2 (bioc)",
#   "tidyverse/ggplot2@main",
# ]
# ///

library(ggplot2)
...
```

- Opening `# /// script` and closing `# ///` lines delimit the block; body lines are
  `# `-prefixed TOML.
- `r` — an R version constraint (same grammar as `uvr.toml` `r_version`). Optional.
- `dependencies` — an array of package specs reusing uvr's existing add-spec grammar:
  `name`, `name>=x`, `name (bioc)`, `user/repo@ref`, `forgejo::host/owner/repo@ref`,
  and (via F3) `path`/`url`/other git hosts.

### Behavior

1. `uvr run script.R` reads the file; if a header is present it enters **script mode**;
   if absent, behavior is unchanged (today's `run`).
2. Parse the header into `Vec<(String, DependencySpec)>` (the same types `uvr add` produces).
3. Choose R from the header's `r` constraint (falling back to `--r-version`/project/system).
   **Auto-provision:** if no installed R satisfies it, run the `uvr r install` backend
   for the newest matching version (overridable via `UVR_R_DOWNLOADS=never`, mirroring
   `uv`'s `UV_PYTHON_DOWNLOADS`).
4. Materialize an **isolated ephemeral library** via the existing `ensure_with_env`
   path (`run.rs:138-199`) — hashed cache keyed by R version + the resolved dep set —
   generalized from `&[String]` to `Vec<(String, DependencySpec)>` so header versions
   and sources are honored.
5. Run the script with the shared `REnv` pointing at that ephemeral library. The
   surrounding project, if any, is ignored — that is what makes it reproduce anywhere.

### `uvr add/remove --script`

Edit the header programmatically, mirroring `uv add --script`:

| Command | Behavior |
|---|---|
| `uvr add <pkg…> --script <file>` | Insert/create the header, add specs, keep it sorted. |
| `uvr remove <pkg…> --script <file>` | Drop specs; remove the block if it empties. |

### Data model

No `uvr.toml`/`uvr.lock` change (a headered script is project-independent). New module:

```rust
// crates/uvr-core/src/script_header.rs (new)
pub struct ScriptHeader {
    pub r: Option<String>,                       // R version constraint
    pub dependencies: Vec<(String, DependencySpec)>,
}
pub fn parse(source: &str) -> Result<Option<ScriptHeader>>;   // None = no header
pub fn upsert(source: &str, add: &[(String, DependencySpec)]) -> String;
pub fn remove(source: &str, names: &[String]) -> String;
```

`ensure_with_env` is generalized:

```rust
// run.rs — from:
async fn ensure_with_env(packages: &[String], r_version: &str) -> Result<PathBuf>
// to:
async fn ensure_with_env(deps: &[(String, DependencySpec)], r_version: &str) -> Result<PathBuf>
```

The `--with pkg` CLI path constructs `(pkg, DependencySpec::default())` pairs, preserving
today's behavior and cache keys (hash over the *resolved* spec, so a bare name hashes as
it does now).

### Touched files

| File | Change |
|---|---|
| `crates/uvr-core/src/script_header.rs` *(new)* | Header parse / upsert / remove + tests. |
| `crates/uvr/src/commands/run.rs:11-134` | Detect header → build deps + R constraint; auto-provision R. |
| `crates/uvr/src/commands/run.rs:138-199` | Generalize `ensure_with_env` to `DependencySpec`. |
| `crates/uvr/src/commands/add.rs` | `--script <file>` branch → `script_header::upsert`. |
| `crates/uvr/src/commands/remove.rs` | `--script <file>` branch → `script_header::remove`. |
| `crates/uvr/src/cli.rs:121-179` | Add `--script <FILE>` to `AddArgs`/`RemoveArgs`. |

### Error surface

- Malformed header (unterminated block, bad TOML): `Invalid script header in <file>: <detail>` — hard error, never silently ignored.
- Auto-provision disabled and no R matches: `No R satisfies "<constraint>" for <file>; install one or set UVR_R_DOWNLOADS=auto.`

### Tests

- `parse`: happy header → deps + `r`; no header → `None`; unterminated → `Err`; `(bioc)`
  and `user/repo@ref` specs round-trip to the right `DependencySpec`.
- `upsert`/`remove`: add to a headerless file creates the block; remove-last deletes it;
  ordering stable.
- `--with` still hashes a bare name to today's key (regression: no cache-key churn).
- Integration (`#[ignore]`): a headered script runs standalone in an empty dir.

---

## F5 — Private-repo authentication

### Goal

Let uvr fetch packages from **authenticated** repositories — a private Posit Package
Manager instance, an internal CRAN mirror, or a private git host — so the corporate /
university audience uvr targets can actually use their own repos. Today custom
`[[sources]]` repos are fetched with only a `User-Agent`
(`crates/uvr/src/commands/sync.rs:104-127,788`).

### Non-goals

- Keyring / OS-keychain integration (heavier from a static binary; env + netrc suffice).
- A `uvr auth login` credential-management CLI (env + netrc suffice; possible follow-on).
- **Secrets in `uvr.toml`.** The manifest names a repo; credentials never live in it.

### Credential mechanisms (decided)

Two sources, checked in order, keyed by the repo's `name` (for `[[sources]]`) or host
(for git):

1. **Environment variables** (highest precedence, CI-friendly):
   - Bearer/token: `UVR_REPO_TOKEN_<NAME>` (repo) / existing `GITHUB_PAT`,
     `UVR_FORGEJO_TOKEN_<HOST>`, new `UVR_GITLAB_TOKEN_<HOST>` etc. (git).
   - HTTP basic: `UVR_REPO_USER_<NAME>` + `UVR_REPO_PASSWORD_<NAME>`.
   - `<NAME>`/`<HOST>` normalized as Forgejo already does
     (`registry/forgejo.rs:121-138`): strip port, uppercase, `.`/`-` → `_`.
2. **`~/.netrc`** (fallback, keyed by machine host): standard `machine <host> login <u>
   password <p>`. Covers both repos and git hosts uniformly; what many R users/CI
   already have. Respects `NETRC` env override; ignored if world-readable on Unix
   (a safety refusal, logged).

### Data model

`[[sources]]` gains an **optional** `auth` discriminator naming *which* credential set
to look up — never the secret itself:

```toml
[[sources]]
name = "internal-ppm"
url  = "https://ppm.corp.example/cran/latest"
auth = "token"   # optional: "token" | "basic" | "netrc" (default: try env then netrc)
```

New credential resolver in `uvr-core`:

```rust
// crates/uvr-core/src/auth.rs (new)
pub enum Credential { Bearer(String), Basic { user: String, password: String } }
pub fn resolve(name_or_host: &str, hint: Option<AuthHint>) -> Option<Credential>;
```

### Behavior

`resolve()` is called at every fetch/download against a custom repo or git host, and the
returned `Credential` becomes an `Authorization` header (`Bearer <t>` or Basic). Applied at:

- Custom-registry index fetch — `sync.rs:104-127` (`fetch_custom_registries`).
- Package tarball download — `crates/uvr-core/src/installer/download.rs:305` (the same
  host-scoped-never-forwarded discipline the Forgejo path already uses at `download.rs:318`).
- Git-host DESCRIPTION + tarball fetches (GitLab/Bitbucket added in F3 reuse this).

### Touched files

| File | Change |
|---|---|
| `crates/uvr-core/src/auth.rs` *(new)* | `Credential`, env + netrc resolution, redaction. |
| `crates/uvr-core/src/manifest.rs:99-103` | Optional `auth` field on the `[[sources]]` entry. |
| `crates/uvr/src/commands/sync.rs:104-127` | Attach credentials to custom-registry index fetch. |
| `crates/uvr-core/src/installer/download.rs:305` | Attach credentials to authenticated downloads. |

### Error surface

- 401/403 from a custom repo: `<name> returned <status>; set UVR_REPO_TOKEN_<NAME> or add a ~/.netrc entry for <host>.`
- Never log the credential; redact to `Bearer ***` / `Basic ***` in verbose/trace output.
- World-readable `~/.netrc` on Unix: skip it with a warning (do not fail the whole run).

### Tests

- `resolve`: env token beats netrc; basic user+password pair; host normalization with port;
  whitespace-only env treated as unset (mirrors `forgejo_token`).
- netrc parse: `machine/login/password`, `NETRC` override, permission refusal.
- A fetch against a `mockito` server asserting the `Authorization` header is present and
  correct; and absent when no credential resolves.
- Redaction: verbose output never contains the raw token.

---

## F3 — Dependency sources

### Goal

Extend the sources a single dependency can declare to match real R workflows: a **local
path**, an **arbitrary git host** (GitLab/Bitbucket/generic — closing #123), and a
**direct URL tarball**. Reuses the source-install path and generalizes the
Forgejo-established `<kind>::` prefix scheme.

### Non-goals

- Editable / live-reload installs (R installs are compiled/copied; `devtools::load_all()`
  owns that loop).
- Workspaces / multi-package monorepos.

### Syntax

```toml
[dependencies]
# local path (relative to uvr.toml, or absolute)
mypkg   = { path = "../mypkg" }
# arbitrary git host — new prefixes join github/forgejo
gpkg    = { git = "gitlab::gitlab.com/group/repo",  rev = "v1.2.0" }
bpkg    = { git = "bitbucket::bitbucket.org/team/repo", rev = "main" }
anypkg  = { git = "git::https://git.corp.example/team/repo.git", rev = "abc123" }
# direct source tarball
tpkg    = { url = "https://example.org/tpkg_1.2.0.tar.gz" }
```

CLI: `uvr add ../mypkg` (path sniffed by leading `.`/`/` or existing dir),
`uvr add gitlab::group/repo@ref`, `uvr add https://…/pkg_1.2.tar.gz`.

### Data model

`DetailedDep` (`crates/uvr-core/src/manifest.rs:81-97`) gains two fields; `git` accepts
new prefixes:

```rust
pub struct DetailedDep {
    pub version: Option<String>,
    pub bioc: Option<bool>,
    pub git: Option<String>,   // "user/repo" | "forgejo::…" | NEW "gitlab::…" | "bitbucket::…" | "git::<url>"
    pub rev: Option<String>,
    pub path: Option<String>,  // NEW — local source dir
    pub url: Option<String>,   // NEW — direct source tarball
}
```

`PackageSource` (`crates/uvr-core/src/lockfile.rs:62-78`) — new variants, and the
currently-unused `Local` gains a payload:

```rust
pub enum PackageSource {
    Cran, Bioconductor, GitHub,
    Forgejo  { host: String },
    GitLab   { host: String },   // NEW  ("gitlab:<host>")
    Bitbucket{ host: String },   // NEW  ("bitbucket:<host>")
    Git      { url: String },    // NEW  generic ("git:<url>")
    Url,                          // NEW  ("url"; tarball in LockedPackage.url)
    Local    { path: String },   // CHANGED from unit → carries path ("local:<path>")
    Custom   { name: String },
}
```

Reproducibility notes recorded in the spec and surfaced to users:

- **GitLab/Bitbucket** mirror `forgejo.rs`: REST API for ref→SHA + raw DESCRIPTION +
  archive tarball; lock stores the resolved SHA in `checksum` (`git:<sha>`) and the
  archive URL in `url` — fully reproducible.
- **`git::<url>`** (generic host): resolve via `git ls-remote` + a shallow `git archive`
  / clone; lock the SHA. Requires a `git` binary (documented; `uvr doctor` checks it).
- **`url`** tarball: lock the URL + a `sha256` checksum — fully reproducible.
- **`path`**: lock `source = "local:<path>"`; **not cross-machine reproducible** (the path
  may not exist elsewhere, and local edits aren't checksummed) — the same caveat renv
  carries. `uvr sync --frozen` warns when a lock contains a `Local` entry.

### Behavior

- `parse_add_spec` (`add.rs:11-44`) grows branches: path (leading `.`/`/` or extant dir),
  the new git prefixes (before the bare-`user/repo` GitHub heuristic), and `url`
  (`https?://…(.tar.gz|.tgz)`). The current hard rejection of non-GitHub hosts
  (`add.rs:51-57`) is replaced by dispatch.
- The lock-time BFS (`lock.rs`, `resolve_git_deps`) gains GitLab/Bitbucket/generic arms
  alongside the existing github/forgejo dispatch.
- Install: `url`/git-archive/path all resolve to a source tarball (or dir) fed to the
  existing source-install path; `select_pkg_plan` (`sync.rs:57-95`) still prefers a P3M
  binary for CRAN names and falls back to these source URLs otherwise.
- Private git hosts authenticate via **F5** (`auth::resolve(host)`).

### Touched files

| File | Change |
|---|---|
| `crates/uvr-core/src/manifest.rs:81-97` | `path` + `url` fields; parse/serialize; `[[dependencies]]` round-trip. |
| `crates/uvr-core/src/lockfile.rs:62-78` | New `PackageSource` variants; `Local` → `Local { path }`; (de)serialize. |
| `crates/uvr-core/src/registry/gitlab.rs`, `bitbucket.rs`, `git_generic.rs` *(new)* | Resolvers mirroring `forgejo.rs`. |
| `crates/uvr-core/src/registry/mod.rs` | Register new modules. |
| `crates/uvr/src/commands/add.rs:11-57` | Path/url/git-prefix dispatch; drop non-GitHub rejection. |
| `crates/uvr/src/commands/lock.rs` | BFS dispatch for the new kinds. |
| `crates/uvr/src/commands/sync.rs:57-95,~1512-1536` | `source_url`/`select_pkg_plan` handle new variants; populate `Local { path }`. |
| `crates/uvr/src/commands/export.rs` | Map new variants to the nearest renv `RemoteType`. |

### Tests

- Spec parse for each new kind (path/gitlab/bitbucket/git::/url), including CLI sniffing.
- Lockfile round-trip for every new `PackageSource` variant (+ malformed → `Custom` fallback, per the Forgejo precedent).
- Path dep: install from a fixture source dir; `--frozen` emits the non-reproducible warning.
- URL dep: checksum mismatch is a hard error.
- GitLab resolver against a `mockito` server (ref→SHA→DESCRIPTION→tarball URL).

---

## F6 — Ergonomic parity commands

### Goal

Fill the cheap gaps in uvr's utility surface against `uv cache …` and `uv python …`.

### Commands

| New command | uv analog | Behavior |
|---|---|---|
| `uvr cache prune` | `uv cache prune` | Reclaim stale/orphaned cache without a full wipe. |
| `uvr cache dir` | `uv cache dir` | Print `env_vars::cache_dir()`. |
| `uvr cache size` | `uv cache size` | Print human cache size (the figure `uvr doctor` already computes). |
| `uvr r dir` | `uv python dir` | Print the R install root (`~/.uvr/r-versions` / `UVR_R_INSTALL_DIR`). |
| `uvr r find [constraint]` | `uv python find` | Print the path to the R binary satisfying `constraint` (reuse `find_r_binary`). |

### `cache prune` — definition of "unused"

Removes, without touching live/reachable entries:

1. **Orphans** — download-cache entries with no corresponding extracted package (partial
   or interrupted installs).
2. **Stale-ABI extracts** — extracted packages built for an R minor that is no longer
   installed (mirrors today's `cache clean --r-version` but automatic).
3. `--ci` — additionally drop the raw download cache (compiled extracts kept), mirroring
   `uv cache prune --ci`; safe to run in CI between jobs.

Prints a per-category reclaimed-bytes summary; supports `--dry-run`. Never silently
caps — everything removed is reported.

### Data model

None. `uvr r find`/`r dir` and `cache dir`/`size` are read-only.

### Touched files

| File | Change |
|---|---|
| `crates/uvr/src/cli.rs:452-476,377-396` | `CacheCommands::{Prune,Dir,Size}`; `RCommands::{Dir,Find}`. |
| `crates/uvr/src/commands/cache.rs` | `prune` (orphan/stale-ABI scan, `--ci`, `--dry-run`), `dir`, `size`. |
| `crates/uvr/src/commands/r_cmd/dir.rs`, `find.rs` *(new)* | Print install root; resolve+print interpreter. |
| `crates/uvr/src/commands/r_cmd/mod.rs` | Register subcommands. |

### Tests

- `cache dir`/`size` print the same path/size `doctor` reports (shared helper).
- `r find`: returns the managed R for a satisfied constraint; non-zero + message when none.
- `prune --dry-run` on a fixture cache reports orphans + stale-ABI extracts without deleting;
  real prune removes exactly those and preserves reachable entries.

---

## F4 — Resolution controls

### Goal

Add the resolver knobs uv exposes, scoped to what R's (looser) dependency graph can use.
Accepted in full per the grilling decision, despite low current R demand, because
resolver seams are painful to retrofit.

### Non-goals

- `--prerelease` handling — CRAN has no prerelease channel (devel comes from
  GitHub/r-universe source, already reachable via F3).

### Surface

| Knob | Where | Behavior |
|---|---|---|
| `--resolution {highest,lowest}` | `uvr lock`, `add`, `update`; `[resolution] strategy` | `lowest` picks the *minimum* version satisfying each constraint — verify declared floors. Default `highest` (today's behavior). |
| `--exclude-newer <DATE>` | `uvr lock`; `[resolution] exclude-newer` | Cap resolution to packages available as of `DATE`. Implemented by redirecting CRAN/P3M reads to the dated PPM snapshot URL (`…/cran/<DATE>/`); recorded in the lock for reproducibility. Bioc: documented best-effort. |
| overrides | `[override-dependencies]` in `uvr.toml` | Force a package to an exact version regardless of what any dependency requests. |
| constraints | `[constraint-dependencies]` in `uvr.toml` | Bound a (possibly transitive) package's version *iff* it is pulled in — without adding it as a dependency. |
| `--upgrade-package <NAME>` | `uvr lock` | Targeted re-lock of one package without installing — the missing *spelling* of `uvr update <pkg>`'s existing targeted logic. |

### Data model

New optional manifest tables + `[resolution]` block:

```toml
[resolution]
strategy      = "highest"       # or "lowest"
exclude-newer = "2024-01-01"    # optional

[override-dependencies]
Matrix = "1.6-5"                # force, wins over any requester

[constraint-dependencies]
rlang  = ">=1.1.0"              # applied only if rlang is pulled in
```

Resolver changes (`crates/uvr-core/src/resolver/`, `registry/cran.rs`):

- `CranIndex::get_best` (`cran.rs:202-223`, newest-first sort at `:228`) takes a
  `ResolutionStrategy`; `lowest` selects the *first satisfying from the low end*.
- Overrides extend the existing pin mechanism (`resolver/mod.rs:140-146,225-238`) — an
  override is a pin that also wins over a direct requester (today's pins hold *back*;
  overrides *force*).
- Constraints are an extra predicate in `version_matches_req` (`resolver/mod.rs:429-439`):
  a candidate must satisfy both the requester's requirement and any constraint entry.
- `--exclude-newer` rewrites the effective CRAN/P3M base URL to the snapshot; stored in
  `RVersionPin`'s neighborhood (`lockfile.rs:17-25`) as a new optional lock field
  `resolved_as_of`.
- `lock --upgrade-package NAME` reuses `update.rs`'s `resolve_only_upgraded`
  (`lock.rs:63-70`, `update.rs:61-83`) but skips install.

### Touched files

| File | Change |
|---|---|
| `crates/uvr-core/src/manifest.rs` | `[resolution]`, `[override-dependencies]`, `[constraint-dependencies]`. |
| `crates/uvr-core/src/registry/cran.rs:202-228` | `get_best(strategy)`; snapshot-URL base for exclude-newer. |
| `crates/uvr-core/src/resolver/mod.rs:140-146,225-238,429-439` | Overrides (forcing pins) + constraints predicate. |
| `crates/uvr-core/src/lockfile.rs:17-25` | Optional `resolved_as_of` for exclude-newer reproducibility. |
| `crates/uvr/src/cli.rs:256-279` | `--resolution`, `--exclude-newer`, `--upgrade-package` on `LockArgs`; `--resolution` on `Add`/`Update`. |
| `crates/uvr/src/commands/lock.rs:20-101` | Thread strategy/exclude-newer/targeted-upgrade through `resolve_and_lock`. |

### Error surface

- `--resolution lowest` with a constraint that has no satisfying low version: the normal
  "no version satisfies" resolver error, naming the package + bound.
- Override that contradicts a hard requirement: applied anyway (that is the point);
  logged at `--verbose` as an override taking effect.
- `--exclude-newer` for a source without a dated snapshot (Bioc): warn once, resolve
  against the live index.

### Tests

- `get_best`: `lowest` returns the min satisfying version; `highest` unchanged (regression).
- Override forces a version a dependency would otherwise raise; constraint bounds a
  transitive without adding it.
- `lock --upgrade-package X` re-locks only `X`, holds all others (parallels the existing
  `update <pkg>` test).
- `--exclude-newer` builds the expected snapshot URL and records `resolved_as_of`.

---

## Already at parity (no work)

- **Universal / cross-platform lockfile.** `LockedPackage` (`lockfile.rs:27-56`) pins
  name + version + logical source + a canonical *source* tarball URL + checksum + dep
  names, with **no** OS/arch/binary field; the machine-appropriate P3M binary is chosen
  at sync (`sync.rs:57-95`). One `uvr.lock` serves every platform — uv's universal-lock
  property, which R makes easier (no environment-marker-conditional deps).
- **Targeted upgrade.** `uvr update <pkg>` already re-resolves only the named package and
  holds the rest (`update.rs:61-83`). F4 adds only the `lock --upgrade-package` *spelling*.

## Out of scope

Each excluded with a one-line rationale, designed *not* to be built:

- **`uv build` / `uv publish`** — R package authoring + CRAN/r-universe submission is
  devtools/usethis territory and a manual-review process, not a push.
- **`uvx` / `uv tool`** — R packages almost never ship executables; nothing to run.
- **`uv pip …`** — an imperative escape hatch that breaks uvr's lockfile-first identity;
  `run --with` + `import`/`export` cover the real needs.
- **`uv venv` (create primitive)** — no venv object in R; F0 delivers the activation half.
- **Dependency groups (PEP 735) + extras / optional-dependencies** — dev/default covers
  the R case and maps to DESCRIPTION; extras have no R consumer story (`pkg[extra]`).
- **Editable / live-reload installs + workspaces** — no R live-reload of installed libs;
  monorepos are niche in R.
- **`--prerelease`** — no CRAN prerelease channel.
- **Keyring + `uvr auth` CLI** — env + netrc suffice; keyring is heavy from a static binary.
- **`r upgrade`, `uvr version` / project-version field** — weak R-native fit (see matrix).
- **export-to-DESCRIPTION / DESCRIPTION-as-manifest** — R-native, *no uv analog*; belongs
  to `PLAN.md` #3, not to a uv-parity effort.

## Priority & sequencing

Build order and rationale. F5's credential layer lands before F3 so the private-git-host
half of F3 reuses it; the shared `REnv` refactor is a prerequisite for F0 and F1.

1. **F0 Shell activation** (P0) — you requested it; high interactive-adoption value;
   self-contained once the `REnv` refactor lands.
2. **F1 Inline per-script deps** (P0) — headline differentiator vs `rv`; reuses `REnv`
   + `ensure_with_env`.
3. **F5 Private-repo auth** (P0) — unblocks the corporate/university audience; supplies
   the credential layer F3 depends on.
4. **F3 Dependency sources** (P1) — real workflows + #123; reuses F5's auth.
5. **F6 Ergonomics** (P1) — cheap wins; independent.
6. **F4 Resolution controls** (P2) — full breadth by request; lowest demand, most
   resolver-invasive, so last.

## Cross-cutting testing strategy

- Unit tests are pure (parsers, resolvers, credential/env logic) — no network, mirroring
  the Forgejo work's `mockito`-gated approach.
- Network-touching paths (git-host resolvers, standalone-script run, activation
  round-trip) get `#[ignore]` integration tests, runnable with `cargo test -- --ignored`,
  keeping CI offline-stable.
- Every feature that reshapes an existing code path ships a **regression test proving the
  old path is unchanged** (e.g. `--with` cache keys under F1; `get_best` highest under F4;
  `uvr run` env under the `REnv` refactor).
```
