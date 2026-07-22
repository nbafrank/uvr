# Changelog

User-facing notes, latest first. For per-commit detail see `git log` or the
release page on GitHub. Issue numbers reference https://github.com/nbafrank/uvr/issues/.

## Unreleased

Pure tracking section — fixes and small features land here between tags.

### Fixes

- R version detection no longer inherits `R_HOME`/`R_LIBS*` from an
  enclosing R session (RStudio terminal, `system("uvr …")`), where a
  stale `R_HOME` from a different install could break or confuse the
  spawned R (#128; hardens the #99 scenario).
- `uvr sync` detects R exactly once per run — the R-switch check and the
  installer previously each spawned R (~250ms), with a tiny window to
  observe different Rs.
- `uvr cache clean` byte counts no longer follow symlinks (the deletion
  logic never did; this aligns the reported sizes).

## v0.4.2 (2026-07-22)

Round two of @gdevenyi's codebase audit (#127–#172): roughly twenty more
confirmed issues fixed across the resolver, registry caches, package
cache, and macOS binary installs — plus two requests from @B-Nilson
(#85, #92), an automatic Bioconductor fallback for `uvr add`, and a fix
for OpenMP-linked binary packages on uvr-managed macOS R installs. The
batch passed an adversarial review before tagging.

### Features

- `uvr cache clean` gains `--package <name>` and `--r-version <minor>`
  filters (repeatable/comma-separated), so troubleshooting one package or
  retiring one R series no longer costs the whole cache (#92, thanks
  @B-Nilson). Cache entries now record their R version; entries from older
  uvr versions can't be version-filtered and are reported as skipped.
- `uvr add <pkg>` for a package that isn't on CRAN but is on Bioconductor
  now adds it from Bioconductor automatically — with any version
  constraint preserved — instead of asking you to retype the command with
  `--bioc`. This matters most from `uvr::add()` inside an R session,
  where re-running a CLI command isn't an option.

### Fixes

- macOS: uvr-managed R installs now load the bundled OpenMP runtime, so
  P3M binary packages built with `-fopenmp` (Rtsne, mgcv, dotCall64, …)
  load instead of failing with "symbol not found in flat namespace".
  `uvr sync` self-heals installs made by earlier uvr versions, and the
  managed-R checks honor `UVR_R_INSTALL_DIR` for relocated installs.
- Switching a project's active R minor no longer wipes the library on
  every sync: `uvr sync` now re-resolves the lockfile for the new R (the
  old resolution carried per-minor binary URLs and Bioc releases) and the
  wipe fires exactly once per actual switch (#85, thanks @B-Nilson).
- R-style version constraints that semver can't parse directly — 4+
  components (`>= 0.9.100.5.0`), dash forms (`>= 1.2-7.1`), leading zeros
  (`>= 0.03-11`), and `==` — are now normalized and enforced instead of
  being silently treated as unconstrained (#149 follow-up).
- New `UVR_PACKAGES_DIR` env var overrides the installed-package cache
  location (`~/.uvr/packages/`), mirroring `UVR_CACHE_DIR`; the `cargo
  test` suite uses it for isolation and no longer wipes the developer's
  real `~/.uvr` cache (nor the CI runner's on Windows, where
  `HOME`/`USERPROFILE` overrides don't reach `dirs::home_dir()`).
- System-dependency checks distinguish "the sysreqs API was unreachable"
  from "no sysdeps needed": failed lookups now fall back to the vendored
  local rules and `uvr sync` says the check was degraded (#148).
- macOS arch verification of binary packages now recurses into `libs/`
  subdirectories and covers `.dylib` files (#147); `install_name_tool`/
  `otool` failures during library patching are logged instead of silently
  swallowed (#163, #164); a corrupt tar entry no longer aborts binary
  tarball inspection (#139).
- Registry caches no longer record a fresh ETag when the cache data write
  failed, which could make later conditional requests serve stale content
  (part of #168); unparseable dependency constraints and index entries are
  logged when dropped (#149, #150).
- The package cache reports an error instead of silent success when a
  corrupted entry can't be replaced (#141), and cache size stats no longer
  follow symlinks (#142).
- HOME-less environments (sandboxes, some CI) now cache under the system
  temp dir instead of polluting the working directory (#161, main sites).
- 5th+ version components are preserved during normalization instead of
  silently dropped (#130).
- Warnings now fire when the Bioconductor release is derived from a default
  R 4.4 because R couldn't be detected (#154), and when a `.r-version` pin
  contradicts the `uvr.toml` constraint (#156, pin still wins).
- `uvr add gitlab.com/user/repo` explains that the host is unsupported and
  points at #123 instead of claiming an "Invalid GitHub spec" (#145).

## v0.4.1 (2026-07-17)

Fix batch driven by @gdevenyi's codebase audit (#127–#172): 12 confirmed
issues fixed, headlined by partial R version support (`uvr r install 4.5`)
and selective-update lockfile consistency. Every fix was verified against
the audit's claims and the batch passed an adversarial review before
tagging.

### Features

- Partial R versions everywhere: `uvr r install 4.5` installs the newest
  published 4.5.x, and a `4.5` pin in `.r-version` matches the newest
  installed 4.5.x (#136, #170). Ambiguous partial uninstalls list the
  candidates instead of guessing.
- `uvr export renv` pins the Bioconductor release (`Bioconductor.Version` +
  release-pinned BioC repos) when the lockfile contains Bioc packages (#144).

### Fixes

- `uvr update <pkg>` now validates the update against the packages held at
  their locked versions instead of silently writing a lockfile where the
  updated package needs newer deps than the ones locked (#127).
- `uvr r use`/`pin` reject invalid versions (`--`, `4-5-2`) instead of
  writing pins that can never match (#171); noise lines can no longer be
  mistaken for R version output (#159).
- Binary package cache entries are now integrity-checked on every hit via
  sha256 sidecars pinned to the first-seen bytes; missing sidecars are
  backfilled instead of staying permanently unverified (#129, #140).
- `uvr cache clean` actually removes directories (e.g. `with-envs/`) and
  reports only what was really deleted (#143).
- A `Remotes:` override no longer drops the version constraint declared in
  `Imports`/`Depends` (#132).
- Git refs are validated and percent-encoded before landing in GitHub or
  Forgejo API URLs (#152).
- R downloads time out on stalled connections instead of hanging forever;
  slow-but-moving downloads are unaffected (#133).
- Windows: system R is also detected in `ProgramFiles(x86)` and per-user
  `LOCALAPPDATA\Programs\R` installs (#158).
- `uvr run --with` refuses to build an environment when the R version can't
  be determined, instead of silently reusing a version-agnostic cache (#160).

## v0.4.0 (2026-07-10)

Major release: R installation now uses the portable, relocatable builds
published by [rstudio/r-builds](https://github.com/rstudio/r-builds) on all
platforms (#116, thanks @gdevenyi), and P3M binary packages install with the
correct architecture again on Apple Silicon for R ≤ 4.5 (#102, #98).

### Breaking changes

- **Linux now requires glibc ≥ 2.34** for `uvr r install` (the
  `manylinux_2_34` portable-build floor). Ubuntu 20.04, RHEL 8, and Debian 11
  are below it and no longer supported by uvr's R installer — use your
  system package manager's R or build from source. uvr detects an old glibc
  up front and says so instead of failing mid-download. Alpine/musl is
  supported via the `musllinux_1_2` builds.
- **R versions older than 4.1.0 can no longer be installed on macOS and
  Windows** — the portable-build CDN doesn't publish them (Linux has no
  floor). uvr errors clearly on a pre-4.1 request and clamps `uvr r list
  --all` accordingly.
- **`uvr r install --distribution` is deprecated and ignored** (hidden from
  help, warns when used). The portable builds make the distro choice moot.

### Features

- **Portable R installs on every platform** (#116, thanks @gdevenyi):
  extract-and-run archives from `cdn.posit.co/r` replace the CRAN `.pkg`
  flow on macOS and the distro-installer flow on Linux. No more Mach-O
  install-name surgery, `Makeconf` prefix patching, or ad-hoc re-signing —
  R resolves its own `R_HOME` at runtime. Windows installs from a plain
  `.zip`, no admin rights involved. Supersedes #96 and #100.
- **Broken R installs self-heal** (#99, thanks @mvuorre): `uvr r install`
  now validates an existing install by running `R --version` instead of
  trusting file existence; a half-patched or broken tree is removed and
  reinstalled instead of wedging every subsequent command.
- **Atomic R installs**: extraction stages next to the install directory and
  lands with a single rename, so an interrupted install can never leave a
  half-populated R that passes the exists check. Failed installs clean up
  after themselves.

### Fixes

- **P3M served x86_64 binaries to Apple Silicon on R ≤ 4.5** (#102, #98,
  thanks @connormfrench, @mvuorre): CRAN builds arm64 R ≤ 4.5 against the Big
  Sur SDK and R ≥ 4.6 against Sonoma, and each P3M channel silently falls
  back to x86_64 `.tgz` outside its native range. uvr hardcoded the
  `sonoma-arm64` channel, so every R ≤ 4.5 sync got Intel binaries (caught
  by the v0.3.9 wrong-arch guard). Binary URLs now route by R version:
  `big-sur-arm64` for R ≤ 4.5, `sonoma-arm64` for R ≥ 4.6.
- CI: fixed the Rust 1.97 `useless_borrows_in_formatting` lint and the
  RUSTSEC-2026-0204 audit failure (crossbeam-epoch 0.9.20).

## v0.3.14 (2026-07-06)

Small-fix batch: two environment-sensitivity bugs on macOS/Linux and two
output-correctness issues.

### Fixes

- **`uvr r install` on macOS works with Homebrew GNU tar on PATH** (#125,
  thanks @zorbax): CRAN `.pkg` Payloads are gzip-compressed *cpio* archives.
  Apple's `/usr/bin/tar` (bsdtar) reads them; GNU tar does not — so with
  Homebrew's `gnu-tar` first on PATH, every Payload failed to extract and the
  install died with a misleading "Could not find bin/R in any Payload(s)"
  error. The extractor now pins Apple's `/usr/bin/tar` (this code path is
  macOS-only, where that binary is always bsdtar) and surfaces the underlying
  tar error in the message when extraction is the real culprit.

- **`uvr run script.R` no longer echoes the script source** (#117, thanks
  @svraka): script mode passed `--quiet`, which only suppresses the startup
  banner — R still echoed every parsed line back with a `> ` prompt.
  It now passes `--no-echo`, matching what `Rscript` does under the hood,
  so only the script's actual output reaches stdout. Interactive
  `uvr run` (no script) is unchanged.

- **Consistent User-Agent between PPM index fetch and tarball download**
  (#124): the Linux index request sent `R (4.5.0 …)` while the download sent
  `R (4.5 …)`. Both now derive from one normalizer (`X.Y` → `X.Y.0`), closing
  a latent divergence hazard with the UA-keyed download cache introduced for
  #122. First sync after upgrading re-downloads Linux binaries once (cache
  keys change form); no action needed.

- **macOS libR patch failures are no longer silent**: when redirecting a
  binary package's `R.framework` library references to uvr-managed R failed,
  the error was discarded and the package would later fail at `library()`
  with a cryptic `dyld: Library not loaded` error. The install still
  completes (the tree is intact and works where a system R is present), but
  uvr now prints a warning naming the package and the likely load-time
  symptom.

## v0.3.13 (2026-06-24)

A correctness fix for a silent wrong-R-version binary install when projects
on different R minors share the download cache.

### Fixes

- **`uvr sync` no longer installs a binary built for the wrong R version**
  (#122, thanks @svraka): the download cache keyed entries on the URL basename
  alone. On Linux, Posit Package Manager serves a *different* R-version binary
  at the *same URL* (it selects by the R version in the request's User-Agent),
  so two projects on different R minors sharing `~/.uvr/cache/` collided on one
  entry — installing a binary compiled for the other R, which then failed at
  `library()` with a cryptic `dlopen`/`LoadLibrary` "symbol not found" error.
  Cache entries are now keyed on a hash of the URL **and** User-Agent, so the
  two R versions get distinct entries. Windows/macOS were also affected in
  principle (R-minor in the URL path) and are likewise disambiguated.

  > If you already hit this, a one-time `uvr cache clean` clears the
  > already-poisoned extracted-package cache; upgrading alone doesn't repair
  > entries written before the fix.

## v0.3.12 (2026-06-18)

Follow-ups to the v0.3.11 Bioconductor work plus macOS source-build fixes —
together these get the full Bioconductor/flow-cytometry stack (S4Vectors,
flowCore, …) installing on R 4.6 / macOS arm64.

### Fixes

- **Stale Bioconductor pin in the lockfile no longer wins** (#119): a lockfile
  could record a Bioconductor release paired with a different R than the one
  now active — e.g. R 4.6 with `bioc_version = "3.21"`, written by a pre-v0.3.11
  uvr — and resolution reused it verbatim, so the v0.3.11 mapping fix never
  applied and packages kept resolving against the wrong R's API. uvr now reuses
  a locked release only when it matches the release the active R maps to;
  otherwise it re-derives the correct one and warns (the lockfile self-heals on
  that run). Explicit `bioc_version` pins in `uvr.toml` are still honored.
- **macOS source builds find their libraries again** (#121): the CRAN macOS R
  bakes its separately-shipped build-toolchain prefix (`/opt/R/<arch>`) into
  `etc/Makeconf`. uvr doesn't ship that bundle, so source builds of packages
  linking external libraries (e.g. `flowCore` → `libssl`) failed with "library
  not found" even when the library was installed via Homebrew. When the CRAN
  toolchain is absent and Homebrew is present, uvr now redirects those Makeconf
  paths to the Homebrew prefix at install time.
- **`Rscript` is on PATH during source builds** (#121): packages that shell out
  to `Rscript`/`R` from their build (e.g. cytolib/RProtoBufLib for `flowCore`)
  failed with "Rscript: No such file or directory" on macOS/Linux. uvr now adds
  the managed R's `bin` to `PATH` for `R CMD INSTALL` (previously Windows-only).

> Note: the Makeconf fix applies at `uvr r install` time. R versions installed
> before v0.3.12 need a reinstall (`uvr r install <ver>`) to pick it up.

## v0.3.11 (2026-06-17)

Bioconductor and macOS-install reliability: fixes a real breakage for R 4.6
users installing Bioconductor packages, makes wrong-channel `uvr add` errors
actionable, and hardens the source-build and macOS code-signing paths.

### Fixes

- **R 4.6 + Bioconductor now resolves the right release** (#119): uvr's
  R→Bioconductor mapping had no entry for R 4.6 and fell back to the R 4.5
  release (3.21). On R 4.6 that pulled package sources written against the
  R 4.5 C API — e.g. S4Vectors 0.46.0 — which fail to compile against R 4.6
  headers (`PRENV`/`Rf_findVar` errors under the current macOS toolchain).
  R 4.6 now maps to Bioconductor 3.23 (which ships compatible versions and
  prebuilt binaries), and the fallback for unrecognized R versions is the
  newest known release rather than a stale one.
- **`uvr add` gives channel-aware "not found" guidance** (#118): when a
  package isn't found, uvr now checks Bioconductor and, if it's there,
  suggests `--bioc`; a `--bioc` add that isn't in the current Bioconductor
  release is explained as such instead of showing the CRAN-archive hint that
  doesn't apply. Works even offline (never shows the wrong-channel hint).
- **Source builds: kill the whole build process group on timeout** (#113):
  a timed-out build now terminates R *and* its `make`/`cc`/`Rscript`
  grandchildren, which otherwise survived and held the output pipe open —
  a follow-on to the v0.3.10 hang fix (#52). Ctrl+C does the same.
- **macOS install surfaces patch-pipeline failures** (#112, #114): a failed
  `install_name_tool`/`otool` step is now reported instead of silently
  swallowed, and `uvr r install` reports every dylib it couldn't re-sign
  rather than aborting on the first — avoiding installs that look healthy
  but crash on first load.

### Features

- **`uvr add --install-system-deps`** (#115, thanks @pat-s): the
  auto-install of detected system libraries, previously only on `uvr sync`,
  is now available on `uvr add` too (same `UVR_INSTALL_SYSREQS` mechanism).

## v0.3.10 (2026-06-12)

Fixes a long-standing source-build hang on Linux plus a batch of
correctness and robustness cleanups across the install, download, and
git-dependency paths. The headline fix resolves #52 (s2/Matrix/StanHeaders
builds hanging indefinitely); the rest harden code paths added in the
recent Forgejo and macOS-Tahoe work.

### Fixes

- **Source builds no longer hang on verbose packages** (#52 @B-Nilson):
  `uvr add`/`uvr sync` captured a compiling package's stdout and stderr
  but only drained stderr while the build ran. On a verbose build (s2,
  Matrix, StanHeaders) the unread stdout pipe filled its ~64KB kernel
  buffer and the compiler blocked on `write()` forever, deadlocked against
  uvr's own wait — the process sat at 0% CPU and never finished. uvr now
  drains stdout on a dedicated thread concurrently with the progress
  output, so a build of any output volume completes.

- **macOS install aborts loudly on a code-signing failure** (#111): the
  `uvr r install` patch pipeline (`patch_r_executables`, `patch_r_dylibs`,
  and the shared ad-hoc resign) previously swallowed `codesign` failures
  as log lines, producing an install that crashed on first dyld load
  (the #99 family) instead of failing visibly. A signing failure now
  aborts the install with an actionable error.

- **git lockfile resolution is deterministic across sources** (#107):
  when the same package was reachable through two different git specs,
  the winning version depended on dependency-walk order. uvr now keys
  deduplication on the resolved package name and reports an explicit
  error naming both candidate sources when they genuinely diverge,
  rather than silently picking one.

### Internal / hardening

- Consolidated three independent `forgejo::host/owner/repo[@ref]` spec
  parsers (CLI `add`, manifest `Remotes:`, registry resolver) into one
  validated parser, so all three accept and reject identical inputs and
  validate host and owner/repo segments uniformly (#108).
- A host-scoped auth token can no longer be forwarded onto the CRAN
  Archive fallback URL on a download retry (#105).
- The renv-export Forgejo host is now derived from the API path anchor
  so it can't drift if the URL prefix changes, with an empty-host guard
  (#106).
- The macOS entitlements plist is written `0o600` via an exclusive
  create so no other local process can read or replace it before
  `codesign` consumes it (#110).
- Release CI now fails if the Windows binary's embedded manifest declares
  a side-by-side `<dependency>`, guarding against a regression of the
  #74 `ERROR_BAD_EXE_FORMAT` breakage (#104).

## v0.3.9 (2026-06-07)

Three user-reported macOS/Linux install issues fixed: a Tahoe SIGSEGV in
the methods package, opaque "No such file or directory" errors on Linux
.deb installs, and Posit Package Manager serving wrong-arch binaries on
Apple Silicon.

### Fixes

- **macOS Tahoe 26.x: R 4.6.0 no longer segfaults in `methods` init**
  (#99 @mvuorre): Tahoe tightened library-validation enforcement on
  ad-hoc signed binaries; R's `methods` package crashed during
  `initMethodDispatch` the first time it dyld-loaded an S4 method table.
  The ad-hoc resign step in `uvr r install` now embeds an entitlements
  plist with `disable-library-validation`,
  `allow-unsigned-executable-memory`, and
  `allow-dyld-environment-variables`. Verified on Tahoe 26.5.1 arm64:
  fresh install of R 4.6.0 + `library(stats)` loads without segfault.
  Existing uvr-installed R versions on Tahoe need a reinstall (`rm -rf
  ~/.uvr/r-versions/<ver> && uvr r install <ver>`) to pick up the new
  signature.

- **Linux `uvr r install` now reports the failing path** (#54
  @joelostblom): the .deb install pipeline propagated bare
  `std::io::Error` through `?` in several spots, surfacing the opaque
  message `"I/O error: No such file or directory (os error 2)"` with no
  hint at which path was missing. Each io operation in
  `install_r_linux`, `patch_text_files`, `patch_makeconf_libr`, and
  `write_renviron_site` now reports the specific file or directory
  involved. Also added a `mkdir -p etc/` guard since some partial
  extracts leave the `etc/` subtree missing.

- **macOS arm64: wrong-arch packages from P3M are detected and
  rejected** (#102 @connormfrench): Posit Package Manager has been
  observed serving x86_64 tarballs from its `sonoma-arm64` binary
  channel. Previously `uvr sync` installed them blindly and the
  packages failed to load at runtime with arch errors. uvr now reads
  the Mach-O header of every `.so` in `<pkg>/libs/` post-extraction
  and rejects the install if the cpu_type doesn't match the host
  (handles thin 64-bit and universal/fat Mach-O). On rejection the
  partial extract is rolled back and the error names the workaround
  (set `UVR_REPOS` to prefer CRAN). Linux distro channels are
  unaffected.

## v0.3.8 (2026-06-03)

Two-headline release: `uvr import` now scaffolds a complete project layout
(closing the half-migrated-from-renv gap), and uvr learns to resolve
dependencies from Forgejo/Codeberg/codefloe via a new `forgejo::` prefix
(PR #101, @pat-s).

### Features

- **Forgejo remote support** (#101 by @pat-s): a new `forgejo::` prefix in
  `Remotes` and `uvr add` resolves package dependencies hosted on any
  Forgejo instance (Codeberg, codefloe, self-hosted). Resolves via Forgejo's
  list-commits API, walks transitive cross-host `Remotes` (forgejo →
  github → CRAN), and ships a renv-compatible `RemoteType: git2r` export
  path so existing renv tooling can re-import the lockfile. Private
  repositories: set `UVR_FORGEJO_TOKEN_<HOST>` (e.g.
  `UVR_FORGEJO_TOKEN_CODEBERG_ORG`) and the token is host-scoped through
  both API calls and tarball downloads (fallback URLs explicitly drop the
  token so it never leaks to other hosts).

### Fixes

- **`uvr import` now scaffolds a complete uvr project**: previously
  `uvr import renv.lock` wrote `uvr.toml` and `.uvr/library/` but
  skipped everything else `uvr init` does — no `.Rprofile` block,
  no `.gitignore` entry, no Positron config, no companion package.
  R startup would still pick up `renv/activate.R` (if present) and
  reset `.libPaths()` to `renv/library/`, completely bypassing
  uvr's library. Workaround pre-fix was `rm uvr.toml && uvr init &&
  uvr import renv.lock`. Now `uvr import` calls the same idempotent
  scaffolding `uvr init` does.

- **`uvr import` detects leftover renv plumbing and warns**: after
  import, if `renv/` exists or `.Rprofile` still sources
  `renv/activate.R`, uvr prints a warning block listing what's
  left and how to remove it. Pass `--clean-renv` to strip the
  hook line from `.Rprofile` and delete `renv/` automatically.
  Without this, the migration appears to succeed but R startup
  still loads renv and competes with uvr for the library path.

## v0.3.7 (2026-05-24)

Version-string hotfix. v0.3.5 and v0.3.6 binaries were built from a
workspace `Cargo.toml` that still pinned `version = "0.3.4"`, so they
identify themselves as v0.3.4 in `uvr --version` and the welcome screen.
That also traps users in an `uvr upgrade` loop, since the embedded version
keeps comparing as older than the latest GitHub tag.

The code shipped under v0.3.5 / v0.3.6 is functionally correct; only the
reported version was wrong. v0.3.7 contains no other changes — it just
bumps the workspace version so the binary self-identifies correctly and
breaks the upgrade loop.

If you're stuck on a binary that says v0.3.4 but you ran `uvr upgrade`
recently, run `uvr upgrade` again once v0.3.7 is published and it will
settle.

## v0.3.6 (2026-05-24)

Follow-up release to v0.3.5. Lands @pat-s's PR #88 (alpine binaries +
custom binary sources + `extract_tgz` rewrite + `UVR_REPOS`), the #99
broken-install recovery path, and a few small UX fixes.

### Features

- **`UVR_REPOS` env var (pat-s, #31 follow-up)**: inject `[[sources]]`
  entries from the environment at **sync time only**, so the lockfile
  stays reproducible across environments (lock time only sees
  `uvr.toml`'s `[[sources]]`). Comma-separated URLs; source names
  auto-derived from the URL host. Useful for CI workflows that want to
  swap binary mirrors at install time without committing to project
  config:

  ```sh
  UVR_REPOS=https://cran.rpkgs.com/arm64/alpine323/latest uvr sync
  ```

- **Custom binary sources via `[[sources]]` (pat-s)**: any CRAN-like
  repo declared in `[[sources]]` can now supply binaries to uvr.
  Auto-detection: an entry's `Built:` field is matched against the
  running host's triple + R minor. If at least one source has
  host-matching `Built:` entries, P3M is suppressed and custom
  sources are queried in declaration order. Source-only custom repos
  (r-multiverse, r-universe) keep their existing behavior — they
  coexist with P3M as today. The `Path:` field is honored for
  non-default tarball locations, with traversal hardening.

  Example for alpine:

  ```toml
  [[sources]]
  name = "rpkgs"
  url  = "https://cran.rpkgs.com"
  ```

  uvr's User-Agent now matches what real R sends via
  `getOption("HTTPUserAgent")`: `R (<ver> <triple> <arch> <os>-<abi>)`.
  This satisfies PPM's existing gating and gives cran.rpkgs.com the
  triple substring (`linux-musl` vs `linux-gnu`) it needs to route
  requests to the right binary.

### Fixes

- **`uvr r install` detects and replaces broken installs (#99)**: the
  short-circuit on "directory exists" now validates that the binary
  actually responds to `R --version`. If it doesn't (e.g. a
  half-patched macOS install on macOS 26.x left behind), the dir is
  removed and a fresh install proceeds. The "pinned but not installed
  (installed: 4.6.0, 4.6.0)" warning now distinguishes
  broken-from-missing — when the dir exists at the pinned version,
  the message reads "appears installed at X but is broken (no version
  response)" and points at the recovery path.

- **Install summary: 'binary' covers everything that didn't compile (pat-s)**:
  uvr's tarball inspector internally distinguishes truly-binary tarballs
  (host-matching `Built:` line, extracted via the pure-Rust fast path)
  from pure-R packages (`NeedsCompilation: no`, installed via `R CMD
  INSTALL` with no C compilation). User-facing, both are reported as
  "binary" because neither invokes a compiler. Only packages that
  actually fired the C/C++/Fortran compiler are reported as "from
  source". For a typical rcmdcheck install on cran.rpkgs.com, the
  summary reads `79 binary, 4 from source`.

- **Pre-install and "no binary repo" messages reflect actual classification (pat-s)**:
  uvr now runs Task 13's tarball-sniff for every downloaded package
  before printing the pre-install summary. Both the upfront
  "Installing N package(s): X binary, Y from source" line and the
  "No binary repo for X on R Y" hint now use runtime classification
  instead of the lock-time pre-estimate. For cran.rpkgs.com (binaries
  served behind a source-style PACKAGES), the upfront message now
  correctly says "binary" for entries with a host-matching `Built:`
  inside their tarball DESCRIPTION. The "no binary repo" hint only
  fires when no package was reclassified.

- **extract_tgz uses manual file extraction (pat-s)**: replaced
  `tar::Entry::unpack` (which performs metadata preservation, symlink
  validation, and a remove-then-recreate dance) with explicit
  `fs::create_dir_all` + `fs::File::create` + `io::copy`. R packages
  need none of the syscalls tar-rs's unpack tries; sidestepping them
  fixes opaque first-entry extraction failures on Drone CI / overlayfs
  / FUSE filesystems. Error messages now include `io::Error.kind()`
  for future debuggability.

- **Alpine binary install (pat-s)**: `detect_posit_distro_slug()`
  no longer rewrites alpine to `ubuntu-2204`. On alpine, uvr now produces
  the slug `alpine-X.Y` which `ppm_linux_codename` cannot translate, so
  `P3MBinaryIndex` returns empty. Sync falls through to source compile
  (slow but correct) instead of silently downloading wrong-libc binaries
  from P3M's Jammy index. Other unknown distros keep the legacy fallback.

- **Welcome screen surfaces `uvr upgrade`**: the Tooling section now
  includes `uvr upgrade` between `doctor` and `help`. Users on a stale
  build no longer need to dig through `uvr help` to find the
  self-update command.

- **Benchmark Dockerfile bumps Rust 1.83 → 1.86**: transitive deps
  (notably `time` 0.3.47+) require Cargo's stabilised `edition2024`
  feature (Cargo 1.85+). The bench image had been silently failing for
  ~3 weeks before this. Bench-only change; doesn't affect release
  builds.

## v0.3.5 (2026-05-15)

Largest batched release since v0.3.0. Two new commands, one new
contributor PR merged, one critical bug fix for Apple Silicon, plus
roughly a dozen smaller fixes and developer-experience improvements.

### Features

- **`uvr scan` (#82)** — new subcommand that walks `.R`, `.Rmd`, and
  `.Qmd` files in the project (honouring `.gitignore` and a new
  `.uvrignore`) and reports packages used via `library()`, `require()`,
  `requireNamespace()`, `loadNamespace()`, `pkg::fn`, `pkg:::fn`, and
  roxygen2 `@import` / `@importFrom` tags that aren't declared in
  `uvr.toml`. `--all` reports every reference with `(declared)` /
  `(missing)` markers; default mode reports only the missing set with a
  copy-paste `uvr add ...` hint. Base R packages are filtered out.

- **`uvr sync --install-system-deps` / `UVR_INSTALL_SYSREQS=1` (#30)**
  — opt-in flag that runs the platform's package manager
  (`apk add` / `sudo apt-get install` / `sudo dnf install`) to install
  missing system libraries instead of just printing the hint. Effective
  UID checked via `geteuid()`; sudo applied uniformly when not root
  (including on Alpine). Falls back gracefully when sudo is needed but
  missing on PATH (no hard-bail in minimal containers). Interactive
  TTY gets a `[y/N]` confirm with N default; non-TTY runs (CI)
  proceed since the user opted in.

- **`uvr sync --ignore-cache` / `UVR_IGNORE_CACHE=1` (#93)** — force
  re-download instead of cache lookup. Useful for troubleshooting a
  single corrupted cached package without wiping the entire cache.
  Cache is still written on successful install, so subsequent syncs
  benefit again.

- **Environment-variable customisation of paths (#79, contributed by
  @bsirak)** — `UVR_CACHE_DIR`, `UVR_R_INSTALL_DIR`, and
  `UVR_INSTALL_DIR` now override the default `~/.uvr/{cache,r-versions}`
  and standalone-installer target directories. Joins existing
  `UVR_LIBRARY`, `UVR_EXTRA_LIBS`, `UVR_INSTALL_TIMEOUT`,
  `UVR_PROGRESS`. All reads centralised through a new
  `crates/uvr-core/src/env_vars.rs` module with consistent
  whitespace / empty-string handling. `uvr doctor` now reports the
  effective values, with green / red glyphs that validate path
  existence so misconfigured paths flag visibly.

- **`GITHUB_PAT` / `GITHUB_TOKEN` honoured by github API calls (#95)** —
  authenticated rate limit (5000 req/hr) replaces the unauthenticated
  60 req/hr default. Eliminates sporadic 403s on CI runners importing
  `renv.lock` files with several github deps. Reads `GITHUB_PAT` first
  (renv / devtools convention), falls back to `GITHUB_TOKEN` (Actions
  / generic CI). Attached to commit-SHA lookup, DESCRIPTION raw fetch
  in the resolver, and the cheap-path DESCRIPTION fetch in `uvr add`'s
  package-name resolution.

### Fixes

- **Apple Silicon binary architecture mismatch (#72 / #53)** — P3M's
  `macosx/big-sur-arm64` URL was serving x86_64 binaries despite the
  path. Verified by downloading `rlang.tgz` from that URL and running
  `file rlang/libs/rlang.so` → `Mach-O 64-bit … x86_64`. The actual
  arm64 binaries live at `macosx/sonoma-arm64`. `Platform::MacOsArm64`
  now points there. Every Apple Silicon user installing R packages
  via uvr was affected; they would have hit
  "incompatible architecture (have 'x86_64', need 'arm64')" on
  `library()`.

- **Resolver walks transitive `Remotes:` chains (#84)** — when a
  github-sourced package's DESCRIPTION declares another github dep via
  `Remotes:` (e.g. `B-Nilson/airquality` → `B-Nilson/handyr`), the
  resolver previously fell through to CRAN for the sub-dep and bailed
  with "Package not found". `resolve_github_deps` now BFS-walks the
  `Remotes:` field. Manifest-direct specs hard-error on fetch failure;
  transitive specs warn and fall back to the registry chain so a typo
  in a third-party DESCRIPTION doesn't brick the lock. The resolver's
  `pre_resolved` branch now also validates the parent's `Imports:`
  constraint — was bypassed before, allowing silent wrong-version
  installs.

- **Windows binary compatibility (#74)** — v0.3.4 embedded a Win32
  manifest but `embed_manifest::new_manifest()` includes a default
  `<dependency>` on `Microsoft.Windows.Common-Controls v6` (a SxS
  assembly for visual styles in GUI apps). On machines where SxS
  activation fails for any reason — corrupt SxS cache, AppLocker /
  WDAC policy, AV interference — Windows refused to load the binary
  with `ERROR_BAD_EXE_FORMAT`. Strip the dep so the manifest only
  carries `supportedOS` GUIDs, `asInvoker`, long-path-aware, UTF-8
  codepage, and DPI awareness.

- **Release-workflow smoke tests** — `release.yml` now runs
  `./uvr --version` and `./uvr --help` on every native target
  (Windows, Linux x86_64, macOS arm64) after `cargo build` and before
  publishing. Catches regressions where the binary loads but won't
  run.

- **`.Rprofile` r-version mismatch warning (#70 follow-up)** — the
  uvr-managed block now reads `.r-version` at session startup and
  warns if the active R minor doesn't match the pin. CLI already
  refused destructive sync on minor mismatch since v0.2.19; this
  surfaces the same signal to users who open R against the project
  without running sync.

- **`.Rprofile` preserves the user's site library (#17)** — switched
  from `.libPaths(lib)` (which dropped the user's site lib) to
  `.libPaths(unique(c(lib, .libPaths())))`. Project library still
  wins resolution, user's existing paths stay accessible.

- **Wipe-confirm prompt before destructive library rebuild (#85)** —
  on R-version-change detected mismatch, sync now prompts
  "Wipe project library at .uvr/library (N package(s))? [y/N]" with N
  default. CI / non-TTY proceeds without prompting. Avoids silently
  nuking hand-built or pinned packages on a misdetected R version
  change. Combined with the calling-R-session guard from #70 phase 1
  so users always see one clear story instead of two competing
  guards.

- **Sysreqs warning gated on actual SystemRequirements (#30)** —
  binaries-only installs on unsupported distros no longer fire the
  loud "System dependency check skipped" warning. Fires only when at
  least one package in the install set actually declares sysreqs.

- **`uvr-r#9` Windows `r_list(all = TRUE)` archive scraping** — CRAN
  dropped trailing slashes on `/bin/windows/base/old/` index entries.
  Replaced the split-on-slash scraper with a regex
  (`href="<version>"?/?"`) robust to either format. Previously only
  the current release surfaced.

- **`.Rprofile` block now noticeably less verbose (#90)** — design-
  rationale comments stripped from the user-facing snippet. Net −22
  LOC per project's `.Rprofile`.

### Smaller fixes shipped earlier in the v0.3.5 window

- **B-Nilson batch — #75 (init doesn't require an R pin), #76 (`uvr
  add --no-lock` / `--no-install`), #77 (`uvr import --name`), #81
  (`uvr run --quiet` to suppress R session banner), uvr-r#8 (`uvr add
  user/repo` now uses DESCRIPTION's Package: field as manifest key),
  uvr-r#9 (`r_list` no longer surfaces `..` parent-dir as a version).**

### Contributors

@bsirak (env-var customisation, PR #79). Thank you.

### Upgrading

No migration steps. Apple Silicon users running R 4.6+ should
`uvr cache clean` once to drop any cached x86_64 binaries from the
pre-v0.3.5 P3M routing bug.

## v0.3.4 (2026-05-03)

Hotfix for #74: Windows 11 users running the v0.3.3 release artifact saw
"This version of … uvr.exe is not compatible with the version of Windows
you're running." Building from source via cargo worked; the released
artifact didn't.

Diagnosis: PE-header inspection of the v0.3.3 artifact showed no
embedded resource directory and no Win32 application manifest at all.
Recent Windows 11 builds reject naked MSVC binaries — without a
manifest declaring `supportedOS` GUIDs, the OS treats the exe as
"compatibility unknown" and refuses to run it. The default Cargo build
on `windows-latest` (now Win Server 2025 + VS 17.14, linker 14.44)
doesn't embed a manifest by itself.

### Fixes
- **Windows binary now embeds a Win32 application manifest (#74)** via
  the `embed-manifest` build-dep. Manifest declares `supportedOS` GUIDs
  for Windows 7 through Windows 11 and `asInvoker` execution level.
  Build-dep is gated under `[target.'cfg(windows)'.build-dependencies]`
  so non-Windows targets are unaffected.

Per the batched-cadence rule (#69), this tag is allowed under the
install-blocking-bug exception — every Windows user trying to install
v0.3.3 currently has a broken binary.

## v0.3.3 (2026-04-30)

Hotfix for v0.3.2's Linux PPM binaries — the feature was end-to-end
broken in two ways found by post-release code review. Both produced
silent install corruption (no error, just unloadable packages at
`library()` time). Allowed under the cadence rule's install-blocking
exception.

### Fixes
- **Linux PPM package downloads now carry the R-shaped User-Agent.**
  v0.3.2 set the UA on the index fetch (which correctly returned binary
  URLs) but not on the per-package tarball downloads. PPM serves source
  vs. binary at the same URL based on UA — without it on the download,
  every "binary" package was actually a source tarball that
  `install_binary_package` extracted as if it had pre-compiled `.so`
  files. Threaded a `user_agent` field through `DownloadSpec` and
  `download_one`; sync sets it for any URL containing `/__linux__/`.
- **Bioconductor packages on Linux no longer enter the binary path.**
  v0.3.2 fetched `bioconductor.org/packages/<release>/bioc/src/contrib/PACKAGES.gz`
  on Linux and registered those URLs in the binary index — but
  Bioconductor doesn't serve Linux binaries, so the tarballs at those
  URLs are sources. They got installed as binaries → unloadable. Now
  guarded with `!info.is_linux` so Bioc on Linux falls through to
  source install (same path as before #55).
- **Linux UA uses the project's actual R minor instead of a hardcoded
  `4.5.3`.** Future-proof against PPM tightening its UA sniffing.

### Tests
- New `ppm_codename_rhel_centos_naming_discontinuity` pins the
  rhel-7/8 → centos7/8 / rhel-9 → rhel9 mapping (the asymmetry was an
  unsigned cliff in the table).
- New `linux_user_agent_uses_r_minor_not_hardcoded` asserts both arch
  variants (x86_64, aarch64) get the right UA from the wired-through
  `r_minor`.

## v0.3.2 (2026-04-30)

Linux gets pre-built binary packages. The platform-support table no
longer shows "-" for the Linux rows.

### Features
- **Linux PPM binary packages (#55)**: `uvr sync` now fetches pre-compiled
  binary packages from Posit Package Manager on supported Linux distros
  (Ubuntu 20.04 / 22.04 / 24.04, Debian 11 / 12, RHEL/CentOS 7 / 8 / 9,
  openSUSE 15.4 / 15.5). Same `__linux__/<codename>/latest` URL space the
  R `install.packages()` setup wizard recommends; uvr injects the User-Agent
  PPM uses to route binary vs. source. Falls back to source for distros
  not on PPM (Alpine, Arch, NixOS, etc.). `uvr doctor` now reports the
  detected codename instead of the prior "source-only" line, and the
  README platform-support table reflects the new coverage.

## v0.3.1 (2026-04-30)

Hotfix-eligible release: closes the macOS R 4.6 byte-compile bug that
v0.3.0 shipped as a known issue, plus three smaller items that landed
together. Per the batched-cadence rule (#69), this tag is allowed under
the install-blocking-bug exception (R 4.6 source-package installs were
fully broken on macOS in v0.3.0).

### Features
- **`uvr r install --distribution <SLUG>`** — manual override for the
  Posit CDN distro slug (e.g. `ubuntu-2204`, `debian-12`, `rhel-9`).
  Useful on Ubuntu / Debian derivatives that aren't matched by
  `/etc/os-release` autodetection like PopOS or Manjaro (#54).
- **`uvr import -i / --input <FILE>`** — alternative spelling of the
  positional path argument, for symmetry with `uvr export -o <FILE>` (#71).

### Fixes
- **Positron-SSH spinner (#48)**: `UVR_PROGRESS=always` now actually
  shows the spinner on terminals that report not-a-TTY. Indicatif's
  default `ProgressDrawTarget::stderr()` runs its own `is_terminal()`
  check and silently drops draws even when our env-var path approved
  drawing — fix uses `ProgressDrawTarget::term()` to write through
  `console::Term` unconditionally when force-on is requested.
- **macOS R 4.6 source-package installs**: previously the v0.3.0 known-issue
  ("missing value where TRUE/FALSE needed" in `tools::makeLazyLoading`).
  Root cause: CRAN's R 4.6 build records two different framework prefixes
  in `bin/R` — Versions-prefixed for `R_HOME_DIR` and the bare
  `/Library/Frameworks/R.framework/Resources` path for `R_SHARE_DIR` /
  `R_INCLUDE_DIR` / `R_DOC_DIR`. Our `patch_text_files` pass only
  rewrote the prefix it extracted from `R_HOME_DIR`, so `R.home("share")`
  resolved to `/Library/Frameworks/R.framework/Resources/share` (which
  doesn't exist in our copy). `nspackloader.R` lookup returned NA file
  size, the comparison evaluated to NA, and `if (NA) ... else ...`
  bombed. Fix: also patch the bare `/Library/Frameworks/R.framework/Resources`
  prefix when it differs from the extracted `R_HOME_DIR`. Closes the
  v0.3.0 known issue.

## v0.3.0 (2026-04-30)

First batched-cadence release per #69. Wraps everything since v0.2.20: the
B-Nilson UX papercut sweep, the `uvr upgrade` command from v0.2.20, the
Ubuntu / Linux install fixes, and the alpine / Remotes / IDE-config /
companion 0.1.2 batch from this iteration. Companion R package bumps to
**0.1.2**.

### Features
- **`uvr init <name>`** creates a new directory and initializes inside it,
  matching `uv init`'s behavior (#56). `uvr init --here [<name>]` keeps the
  old in-place behavior with optional name override.
- **R companion package 0.1.2** — adds `update_pkgs()` (uvr-r #2), a thin
  wrapper around `lock(upgrade = TRUE)` followed by `sync()`.
- **`.vscode/settings.json` covers more keys** (#62, #50): `positron.r.customRootFolders`
  exposes every uvr-managed R install to Positron's picker; `r.rterm.<os>`
  and `r.rpath.<os>` are written for the vanilla VSCode R extension. When
  there's no `.r-version` pin, settings still bind to whatever R uvr would
  use system-wide instead of leaving the file unwritten.

### Fixes
- **Linux sysreqs (#30, pat-s)**: rule lookup now matches a host's full
  `VERSION_ID` *and* a `major.minor` truncation. Alpine 3.23.4 reports
  `3.23.4` in `/etc/os-release` but rules key on `3.23` — without the
  truncation a 3.23.4 host got zero rule hits despite the rules covering
  3.23.
- **DESCRIPTION Remotes parser (#68, B-Nilson)**: `Remotes: nbafrank/uvr-r`
  paired with a `Suggests: uvr` entry now binds to the `uvr` dev-dep
  instead of inserting a new `uvr-r` runtime dep. URL-derived names are
  tried first; on no match, common R companion suffixes (`-r`, `_r`, `.r`
  and uppercase) are stripped before giving up.
- **Ubuntu / Linux**: `R CMD INSTALL` (used for the companion package and
  source-built dependencies) now skips the user/project `.Rprofile` via
  `R_PROFILE_USER=/dev/null`. Previously a leftover `source("renv/activate.R")`
  in a project's `.Rprofile` would abort R startup and the companion would
  fail to install with a confusing "cannot open the connection" error.
- **Ubuntu / Linux**: `uvr r install` pre-flights `ar` (binutils) and `tar`
  on PATH. Missing tools now produce an actionable "install binutils"
  message instead of the opaque "I/O error: No such file or directory".
- **Ubuntu / Linux**: `uvr r list --all` now returns the current-major
  R 4.x release list. Was scraping `/src/base/` (which lists R-1/R-2/R-3/R-4
  subdirs, not tarballs) and returning an empty list.
- **#70 follow-up**: cross-R-minor wipe guard moved out of the wipe
- **Ubuntu / Linux**: `R CMD INSTALL` (used for the companion package and
  source-built dependencies) now skips the user/project `.Rprofile` via
  `R_PROFILE_USER=/dev/null`. Previously a leftover `source("renv/activate.R")`
  in a project's `.Rprofile` would abort R startup and the companion would
  fail to install with a confusing "cannot open the connection" error.
- **Ubuntu / Linux**: `uvr r install` pre-flights `ar` (binutils) and `tar`
  on PATH. Missing tools now produce an actionable "install binutils"
  message instead of the opaque "I/O error: No such file or directory".
- **Ubuntu / Linux**: `uvr r list --all` now returns the current-major
  R 4.x release list. Was scraping `/src/base/` (which lists R-1/R-2/R-3/R-4
  subdirs, not tarballs) and returning an empty list.
- **#70 follow-up**: cross-R-minor wipe guard moved out of the wipe
  conditional. A library already at the resolved R minor (no wipe) but
  invoked from a calling R session on a different minor was still
  installing packages the calling session couldn't load. Now bails
  unconditionally when calling R minor differs from resolved R minor.
- **#65**: `.gitignore` and `.Rbuildignore` writers compare line-by-line
  (leading-slash insensitive for gitignore) so existing entries don't get
  duplicated.
- **#61**: ASCII fallback for the bullet separator is now `-` instead of
  `.`, so "74 cached - 61 binary" reads as a separator list, not three
  sentences.
- **#60**: drop the "uvr R companion package installed" line from the
  user-facing output (demoted to debug — visible under `-v`).
- **#59**: `.Rprofile` now reports "0 of N package(s) installed, run
  uvr::sync()" when the project library hasn't been created yet but the
  lockfile exists.
- `find_r_binary` validates pin and constraint paths via `query_r_version`
  (not just the no-pin fallback). A broken pinned R install now surfaces
  "install at <path> is broken — reinstall" instead of a cryptic
  downstream error.
- macOS install patch: warn when `otool` or `codesign` are missing instead
  of silently no-op'ing — silent failure on a container with no Xcode CLT
  was leaving users chasing a SIGKILL.
- `find_r_binary`'s broken-install fallback now also kicks in for the
  `[project] r_version` constraint path, in version-descending order.

### Internal
- Document `COMPANION_HASH`'s provenance in `sync.rs` — it's the SHA-256
  of the `/repos/<owner>/<repo>/tarball/<sha>` API output, not the
  `/archive/<sha>.tar.gz` archive.

### Known issues
- **macOS R 4.6 + source package installs**: byte-compile / lazy-load step
  in `R CMD INSTALL` errors with "missing value where TRUE/FALSE needed"
  on the patched 4.6 install. R 4.5.x is unaffected. Likely interaction
  between `patch_r_executables`'s ad-hoc resign and R 4.6's lazy-loader.
  Workaround: pin `.r-version` to 4.5.3 for now, or wait for a follow-up
  release that diagnoses the patch interaction.

## v0.2.10 – v0.2.20 (2026-04-23 → 2026-04-29)

A two-week cluster of releases driven by issue triage. Going forward,
fixes will batch into weekly releases instead of shipping per-commit
(per #69 / @pat-s feedback). This section is the consolidated wrap-up
of that cluster — read this instead of clicking through 11 tags.

### Features
- **`uvr upgrade`** (alias `uvr self-update`) — checks GitHub releases and
  installs the latest binary in place; `--check` reports availability
  without downloading.
- **`uvr.toml` config**: `--timeout <DURATION>` flag on `uvr add` / `uvr sync`
  (also `UVR_INSTALL_TIMEOUT` env), default 30 minutes per package.
- **`UVR_PROGRESS=always`** escape hatch for environments where TTY detection
  is wrong (e.g. Positron-SSH).
- **R companion package 0.1.1** — `update_uvr()` updates both the R package
  and the CLI binary in one call.
- **Clap colored help** — `--help` output now picks up the same palette as
  the rest of uvr's output.

### Fixes — install + R version management
- **macOS Sonoma URL (#51)**: `uvr r install` now uses `sonoma-arm64` /
  `sonoma-x86_64` on Darwin ≥14, with a `big-sur-*` fallback for older R
  versions not yet rebuilt for Sonoma. R 4.6 is sonoma-only; this unblocks
  it for Sonoma users.
- **macOS R 4.6 SIGKILL on startup**: CRAN ships R 4.6 with hardened-runtime
  signing, which silently strips `DYLD_LIBRARY_PATH` and made our managed
  installs unloadable. The installer now patches `bin/exec/R`'s framework
  load commands to point at our `lib/libR.dylib` and re-signs ad-hoc
  (clearing the runtime flag). Existing broken installs auto-repair on the
  next `uvr sync`.
- **Windows registry pollution (#49)**: `uvr r install` passes
  `/MERGETASKS=!recordversion` to the R Inno Setup installer so it no
  longer clobbers RStudio's default-R selection.
- **R-version pin enforcement (#63, #64, #70)**: every library-affecting
  command warns loudly when the active R doesn't match the pin —
  including the case where uvr is invoked from inside an R session
  (`R_HOME` env) whose minor differs from the pin. Sync's wipe-and-rebuild
  refuses to proceed when the calling R can't load the result.
- **R version sentinel in the project library (#66)**: `.uvr/library/.uvr-r-version`
  records the R minor that populated the library. Mismatch on subsequent
  sync triggers a wipe-and-reinstall, catching the "lockfile already
  reflects the new R but library doesn't" case the previous wipe condition
  missed.

### Fixes — install reliability + cancellation
- **Stale `00LOCK-<pkg>/` cleanup (#52)**: every install failure path
  (timeout, non-zero exit, parse error, Ctrl+C) removes the lockdir before
  returning. Wedged installs no longer block subsequent syncs.
- **Per-package install timeout (#52)**: defaults to 30 minutes; override
  via `--timeout <DURATION>` or `UVR_INSTALL_TIMEOUT`. On expiry uvr SIGTERMs
  the `R CMD INSTALL` child and surfaces a clear error.
- **Ctrl+C interrupt (#58)**: SIGINT (or Ctrl+C on Windows) now cleanly
  kills the in-flight `R CMD INSTALL`, removes its 00LOCK dir, and exits
  130. Process-tree-aware on Windows via `taskkill /F /T`.
- **`find_r_binary` validates candidates**: a broken managed R install no
  longer captures every uvr command. We probe via `query_r_version` in
  version-descending order and skip ones that don't respond.
- **Companion package install was unloadable (#43)**: was extracting the
  GitHub tarball directly into the library, producing a source-package
  layout R couldn't load. Now uses `R CMD INSTALL` and verifies
  `Meta/package.rds` exists post-install.
- **Positron R interpreter discovery (#50)**: `init` writes
  `positron.r.customBinaries` so Positron actually finds the uvr-managed
  R, not just `interpreters.default` which silently no-op'd.

### Fixes — error messages / UX
- **R install error on 404 (#51)**: instead of bare 404, scrape the CRAN
  directory listing and tell the user "Latest available for your platform:
  4.5.3."
- **Older package URL fallback (#46)**: `download_one` retries via the CRAN
  Archive when `src/contrib` 404s for an older package version.
- **Softer P3M binary-fallback message**: "no P3M binary for this R minor,
  compiling from source" at info level (was a scary `WARN` for an
  expected-and-fine condition).
- **Quickstart order in README (#42)**: `uvr init` then `cd`, not the
  reverse.

### Internal
- **Sonoma vs Big Sur mirror dispatching** is encapsulated in `macos_arm64_dir()`
  / `macos_x86_64_dir()` with a cached `macos_major_version()` to avoid
  repeated `sw_vers` calls.
- **Process-global SIGINT registry** (`uvr_core::signal`) tracks active
  installs so the Ctrl+C handler can reach into running children even
  across the async/sync boundary.
- **R-pin warning** moved into `commands::util::warn_r_pin_mismatch` and
  dispatched from a single match in `main.rs`.
