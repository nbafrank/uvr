# Changelog

User-facing notes, latest first. For per-commit detail see `git log` or the
release page on GitHub. Issue numbers reference https://github.com/nbafrank/uvr/issues/.

## Unreleased

Pure tracking section — fixes and small features land here between tags.

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
