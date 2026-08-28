# uvr <img src="r-package/man/figures/logo.png" align="right" height="139" alt="uvr hex logo" />

[![CI](https://github.com/nbafrank/uvr/actions/workflows/ci.yml/badge.svg)](https://github.com/nbafrank/uvr/actions)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Built with Rust](https://img.shields.io/badge/built%20with-Rust-orange.svg)](https://www.rust-lang.org/)

A fast R package and project manager, written in Rust.

---

`uvr` brings uv-style project management to R: a `uvr.toml` manifest, a reproducible `uvr.lock` lockfile, and a per-project isolated library. Packages install from pre-built [P3M](https://packagemanager.posit.co/) binaries by default — no compilation, no waiting — with automatic fallback to CRAN source. R versions are managed per-project with no `sudo` required.

1.  Linux / MacOS

    ``` sh
    curl -fsSL https://raw.githubusercontent.com/nbafrank/uvr/main/install.sh | sh
    ```

2.  Windows

    ``` powershell
    irm https://raw.githubusercontent.com/nbafrank/uvr/main/install.ps1 | iex
    ```

Here's a following demo of `uvr`: 

``` sh
$ uvr init my-analysis
$ uvr add ggplot2 dplyr tidymodels
$ uvr sync          # installs from lockfile, idempotent
$ uvr run analysis.R
```

(Checksum-verified install to `~/.local/bin`; Windows and other options under [Installation](#installation).)

### R companion package

Prefer working from the R console? The [`uvr` R package](https://github.com/nbafrank/uvr-r) wraps the CLI for use from R/RStudio/Positron — no terminal needed:

```r
pak::pak("nbafrank/uvr-r")

library(uvr)
init()                         # uvr init
add("ggplot2")                 # uvr add ggplot2
sync()                         # uvr sync
run("analysis.R")              # uvr run analysis.R
```

---

## Rationale

R has several package management tools — `renv`, `pak`, `rv`, `rig` — each solving a different slice of the problem. After 10+ years of R development, the workflow I kept wanting was the one `uv` brought to Python: **a single tool that handles the full lifecycle**, from installing R itself to adding packages to reproducible installs in CI, with no configuration sprawl.

Here is how existing tools compare and where the gaps are:

- **renv** — the de-facto standard for reproducibility. It snapshots an existing library into a lockfile, but it does not pin R versions ("renv tracks, but doesn't help with, the version of R used") and it works library-first: the lockfile records what your library already has rather than driving what gets installed. Install speed is a property of your mirror, not of renv — pointed at a binary repo like P3M it is fast (see the benchmarks below).
- **pak** — fast parallel installs and good system dependency detection. It does have lockfiles (`pak::lockfile_create()` / `pak::lockfile_install()`, aimed at CI), but no R version management, and it is an installer rather than a project workflow — in practice paired with renv, not a replacement for it.
- **rv** — the closest prior art: Rust-based, declarative, fast, with P3M binaries, `rv run`, `rv sysdeps`, and `rv sync --locked` for CI. It selects among the R versions already installed on the machine — including ones `rig` put there — but does not install R itself, which is the gap `uvr` closes.
- **rig** — excellent R version manager. No package management or lockfile. Per its own FAQ it cannot install R without admin permissions.
- **pixi** — conda-based multi-language environment manager. Supports R via conda-forge, but packages come from conda-forge rather than CRAN/Bioconductor/P3M natively. Language-agnostic by design; not R-first.
- **rix** — Nix-based, with extreme reproducibility including system-level dependencies. Right tool if you need bit-for-bit reproducibility across machines. Requires Nix; a different philosophy than a fast pragmatic workflow.

`uvr` is the combination of all of the above in one tool, with a single config file (`uvr.toml`) and a single lockfile (`uvr.lock`). The design goals are:

1. **One tool, one config** — no juggling renv + rig + pak. `uvr.toml` declares both the R version and package dependencies.
2. **Lockfile-first** — `uvr.lock` is the source of truth. `uvr sync` is always reproducible and idempotent.
3. **Fast by default** — P3M pre-built binaries on macOS, Windows, and Linux; source fallback only when needed.
4. **R version management built in** — `uvr r install`, `uvr r use`, `uvr r pin` work the same way `uv python` does, because needing a separate tool for this is friction.
5. **CI-native** — `uvr sync --frozen` is a first-class command, not an afterthought.

If you are happy with renv + rig, that is a perfectly good setup. `uvr` is for people who want the `uv` experience in R.

### Feature matrix

|                                | uvr | renv | pak | rv  | rig | pixi |
|--------------------------------|-----|------|-----|-----|-----|------|
| Declarative manifest           | Y   | Y†   | Y†  | Y   | -   | Y    |
| Lockfile                       | Y   | Y    | Y   | Y   | -   | Y    |
| R version selection / pinning  | Y   | -    | -   | Y   | Y   | Y    |
| Installs R itself              | Y   | -    | -   | -   | Y   | Y    |
| Run scripts in isolated env    | Y   | Y    | -   | Y   | -   | Y    |
| CRAN packages                  | Y   | Y    | Y   | Y   | -   | Y*   |
| Bioconductor packages          | Y   | Y    | Y   | Y   | -   | Y*   |
| GitHub packages                | Y   | Y    | Y   | Y   | -   | -    |
| Pre-built binaries (P3M)       | Y   | -    | Y   | Y   | -   | -    |
| System dep detection (Linux)   | Y   | -    | Y   | Y‡  | -   | Y    |
| CI mode (fail on stale lock)   | Y   | Y    | -   | Y   | -   | Y    |
| No admin rights required       | Y   | Y    | Y   | Y   | -** | Y    |
| Standalone CLI (no R required) | Y   | -    | -   | Y   | Y   | Y    |
| Windows support                | Y   | Y    | Y   | Y   | Y   | Y    |

\* pixi installs R packages from conda-forge, not CRAN/Bioconductor directly.
\** Per rig's own FAQ, rig cannot install R without admin permissions.
† Via DESCRIPTION-based workflow, not a dedicated manifest format.
‡ Per `rv sysdeps`' own help, coverage is currently Ubuntu/Debian.

---

## Benchmarks

<!-- BENCH:START - auto-updated by benchmarks/update-readme.sh -->
Install wall time (empty library, index caches warm). All tools use P3M as CRAN mirror. Median of 5 runs on Apple Silicon (arm64), R 4.6.0, uvr 0.4.6. pak was not installed on the bench machine for this run; container numbers including pak are on the [website](https://nbafrank.github.io/uvr/).

| Scenario | Packages | uvr sync | renv | install.packages |
|----------|----------|----------|------|------------------|
| jsonlite  | 1        | **0.52s**  | 0.56s  | 2.95s              |
| ggplot2   | 17       | **0.51s**  | 0.61s  | 5.1s               |
| tidyverse | 100      | **0.57s**  | 0.81s  | 14.92s             |
<!-- BENCH:END -->

> uvr pre-resolves dependencies into a lockfile (`uvr lock`); only `uvr sync` (install) is timed. The other tools resolve dependencies inline. renv uses its default global cache (symlinks).
>
> **Reproduce on your own machine:** `bash benchmarks/bench.sh`.
> **Reproduce in a clean container:** `bash benchmarks/run-in-docker.sh` builds [`benchmarks/Dockerfile`](benchmarks/Dockerfile) and runs the bench inside it. The Dockerfile pins **R version, debian base, Rust toolchain, and the CRAN-mirror PPM snapshot** — so numbers from a CI run today are directly comparable to a CI run a month from now and to a local docker-build by anyone who wants to verify the published numbers (per [#40](https://github.com/nbafrank/uvr/issues/40)). The same image runs on every tag push via [`.github/workflows/benchmark.yml`](.github/workflows/benchmark.yml); the workflow uploads `bench-results.json` as an artifact and surfaces the meta block in the GH Actions step summary.

---

## Highlights

- **Fast** — parallel downloads, native binary extraction, no R process overhead
- **Reproducible** — `uvr.lock` is the source of truth; `uvr sync` is always idempotent
- **Project-isolated** — every project gets its own `.uvr/library/`, never touching system R
- **Full R version management** — `uvr r install 4.4.2`, `uvr r use >=4.3`, `uvr r pin 4.4.2`
- **CRAN + Bioconductor + GitHub** — `uvr add DESeq2 --bioc`, `uvr add user/repo@main`
- **Standalone scripts** — declare dependencies in a `# /// script` header and `uvr run script.R` anywhere, no project needed
- **CI-ready** — `uvr sync --frozen` fails fast if the lockfile is stale; respects `NO_COLOR`
- **Cross-platform** — macOS, Linux, and Windows with pre-built binaries for all three
- **Written in Rust** — single static binary, no R or Python required to install

---

## Installation

### Quick install (recommended)

```sh
curl -fsSL https://raw.githubusercontent.com/nbafrank/uvr/main/install.sh | sh
```

This auto-detects your platform, downloads the binary, verifies the SHA256 checksum, and installs to `~/.local/bin`. Override the install directory with `UVR_INSTALL_DIR`:

```sh
curl -fsSL https://raw.githubusercontent.com/nbafrank/uvr/main/install.sh | UVR_INSTALL_DIR=/usr/local/bin sh
```

You can quick install on Windows as well with the following Powershell command:

``` bash
irm https://raw.githubusercontent.com/nbafrank/uvr/main/install.ps1 | iex
```

### Manual download

Download the latest release for your platform from [GitHub Releases](https://github.com/nbafrank/uvr/releases/latest):

```sh
# macOS (Apple Silicon)
curl -fsSL https://github.com/nbafrank/uvr/releases/latest/download/uvr-aarch64-apple-darwin.tar.gz | tar xz
sudo mv uvr /usr/local/bin/

# macOS (Intel)
curl -fsSL https://github.com/nbafrank/uvr/releases/latest/download/uvr-x86_64-apple-darwin.tar.gz | tar xz
sudo mv uvr /usr/local/bin/

# Linux (x86-64)
curl -fsSL https://github.com/nbafrank/uvr/releases/latest/download/uvr-x86_64-unknown-linux-gnu.tar.gz | tar xz
sudo mv uvr /usr/local/bin/

# Linux (ARM64)
curl -fsSL https://github.com/nbafrank/uvr/releases/latest/download/uvr-aarch64-unknown-linux-gnu.tar.gz | tar xz
sudo mv uvr /usr/local/bin/
```

On Windows, download `uvr-x86_64-pc-windows-msvc.zip` from the releases page and add `uvr.exe` to your PATH. 

### From R

The companion R package can install the binary for you:

```r
# Install the R package from GitHub (uvr-r is not on CRAN yet)
pak::pak("nbafrank/uvr-r")
# or: remotes::install_github("nbafrank/uvr-r")

# Download and install the uvr binary
uvr::install_uvr()
```

### Arch Linux (AUR)

```sh
# Pre-built binary
yay -S uvr-bin

# Or build from source
yay -S uvr
```

Packages maintained by [@novica](https://github.com/novica). See [uvr](https://aur.archlinux.org/packages/uvr) and [uvr-bin](https://aur.archlinux.org/packages/uvr-bin) on the AUR.

### From source (requires [Rust](https://rustup.rs))

```sh
cargo install --git https://github.com/nbafrank/uvr
```

---

## Quick start

```sh
# Create a new project
mkdir my-project && cd my-project
uvr init --r-version ">=4.3.0"

# Add packages (CRAN, Bioconductor, GitHub)
uvr add ggplot2 dplyr
uvr add DESeq2 --bioc
uvr add tidymodels@>=1.0.0
uvr add user/repo@main
uvr add 'user/monorepo@main#subdirectory=packages/nestedPkg'

# Install everything from the lockfile
uvr sync

# Run a script in the isolated environment
uvr run analysis.R -- --input data.csv

# See what you have
uvr tree
```

GitHub package directories also propagate through supported DESCRIPTION
`Remotes:` entries, including `owner/repo/subdir@ref` and
`owner/repo:subdir@ref`. This traversal follows the source chain: only a
package already selected from a manifest Git source can introduce another
remote source; ordinary registry packages cannot inject remote URLs. Bound
aliases and subdirectory targets fail rather than falling back to a registry
or to the repository root.

---

## Commands

| Command | Description |
|---------|-------------|
| `uvr init [name]` | Create `uvr.toml` and `.uvr/library/` in the current directory |
| `uvr add <pkg...>` | Add packages, update manifest + lockfile, install |
| `uvr remove <pkg...>` | Remove packages from manifest and re-lock |
| `uvr sync` | Install all packages from the lockfile |
| `uvr sync -v` | Show the resolved install plan first — each package's source and whether it installs from binary or source |
| `uvr sync --frozen` | Like `sync`, but fail if the lockfile is stale (CI mode) |
| `uvr sync --no-binary` | Build everything from source, ignoring pre-built binaries |
| `uvr sysdeps` | Report missing system dependencies without installing anything |
| `uvr sysdeps --all` | List every system dependency, installed or not |
| `uvr update [pkg...]` | Upgrade packages to latest allowed versions |
| `uvr update --dry-run` | Show what would change without installing |
| `uvr lock` | Re-resolve all deps and update `uvr.lock` without installing |
| `uvr lock --upgrade` | Upgrade all packages to their latest allowed versions |
| `uvr tree` | Show the dependency tree |
| `uvr tree --depth 1` | Show only direct dependencies |
| `uvr run [script.R]` | Run a script (or interactive R) with the project library active |
| `uvr run --with pkg` | Run with extra packages available (not added to manifest) |
| `uvr run script.R` | Run a standalone script from its inline `# /// script` dependency header — outside any project |
| `uvr activate` | Print how to activate the project in your shell (`source .uvr/activate`) |
| `uvr r install <ver>` | Download and install a specific R version to `~/.uvr/r-versions/` (override the location with `--install-dir`) |
| `uvr r install devel` | Install a rolling channel — `devel` or `next`, rebuilt continuously and marked `[unstable]` (not reproducible; don't pin one) |
| `uvr r list` | Show installed R versions |
| `uvr r list --all` | Show all available R versions (fetched from the portable build index) |
| `uvr r use <ver>` | Set R version constraint in `uvr.toml` |
| `uvr r pin <ver>` | Write exact version to `.r-version` |
| `uvr export` | Export lockfile to renv.lock format |
| `uvr export -o renv.lock` | Export to a file |
| `uvr import` | Import packages from an renv.lock file |
| `uvr import --lock` | Import and immediately resolve + install |
| `uvr upgrade` | Update uvr itself to the latest GitHub release (alias: `uvr self-update`) |
| `uvr doctor` | Diagnose environment issues (R, build tools, project status) |
| `uvr completions <shell>` | Generate shell completions (bash, zsh, fish, powershell) |
| `uvr cache clean` | Remove all cached package downloads |
| `uvr cache clean --package <name>` | Remove cache entries for specific packages (repeatable, comma-separated) |
| `uvr cache clean --r-version <minor>` | Remove extracted-package entries built for an R minor version (e.g. `4.5`) |

---

## Standalone scripts

A script can declare its own dependencies in a header comment and run
anywhere — no project, no `uvr.toml`, no lockfile:

```r
# /// script
# dependencies = [
#   "jsonlite",
#   "praise",
# ]
# ///

cat(praise::praise(), "\n")
```

```console
$ cd /anywhere && uvr run analysis.R
> Installing 2 package(s): 2 binary
v Installed 2 package(s) in 1.75s
You are epic!
```

The dependencies install into a cached environment keyed by the dependency
set, so the second run of that script — or any other script wanting the same
packages — starts immediately. Nothing is written next to the script.

The header is the R analogue of Python's [PEP 723](https://peps.python.org/pep-0723/)
inline script metadata, which `uv run` uses. It must start at column zero,
may follow a shebang or banner comment, and takes plain package names today
(version constraints, Bioconductor and git sources are planned). A malformed
or duplicated header is an error naming the file and the problem, never
silently ignored.

Scripts run isolated from any project you happen to be standing in: the
project library, its `.r-version` pin, and its `.Rprofile` are all bypassed,
so a script behaves the same wherever it is invoked from.

A script can also carry a shebang and run as a plain executable:

```r
#!/usr/bin/env -S uvr run
# /// script
# dependencies = ["praise"]
# ///

cat(praise::praise(), "\n")
```

```console
$ chmod +x hooray
$ ./hooray
You are wondrous!
```

The `-S` flag needs GNU coreutils 8.30+ on Linux; macOS and the BSDs have
supported it for years.

---

## Shell activation

Prefer working in a plain R console over prefixing everything with `uvr run`?
Activate the project and a bare `R` or `Rscript` uses it:

```sh
source .uvr/activate      # bash, zsh, sh
source .uvr/activate.fish # fish
. .uvr/activate.ps1       # PowerShell

R                         # uses the project's R and .uvr/library/
deactivate                # restore your shell
```

`uvr init` writes these files (`uvr activate --write-shim` recreates them).
They contain **no paths**: each one asks uvr to recompute the environment as
it is sourced, so changing the project's R version with `uvr r use` or
`uvr r pin` never leaves a stale activation behind.

To show the project name in your prompt while activated — off by default:

```toml
# uvr.toml
[activate]
prompt = true
```

or per-shell, which overrides the manifest either way:

```sh
export UVR_ACTIVATE_PROMPT=1   # or 0 to opt out of a project that opts in
```

---

## Shell completions

Generate and install completions for your shell:

```sh
# Zsh
uvr completions zsh > ~/.zfunc/_uvr

# Bash
uvr completions bash > /etc/bash_completion.d/uvr

# Fish
uvr completions fish > ~/.config/fish/completions/uvr.fish

# PowerShell
uvr completions powershell > $HOME\Documents\PowerShell\Completions\uvr.ps1
```

---

## R version management

`uvr` can install and manage multiple R versions without `sudo` or admin rights:

```sh
# Install R 4.4.2
uvr r install 4.4.2

# See what's available
uvr r list --all

# Set project constraint (writes to uvr.toml)
uvr r use ">=4.3.0"

# Pin exact version (writes .r-version file)
uvr r pin 4.4.2
```

R versions are installed to `~/.uvr/r-versions/` and managed independently of any system R installation. uvr downloads **portable, relocatable R builds** from the [rstudio/r-builds](https://github.com/rstudio/r-builds) project ([`cdn.posit.co/r`](https://cdn.posit.co/r/versions.json)): each is a self-contained archive that detects its own location at runtime — no system-wide install, no admin/`sudo`, and no post-install patching. This makes it ideal for corporate and university environments where users cannot install software system-wide.

---

## CI usage

```yaml
# GitHub Actions example
- name: Install uvr
  run: |
    curl -fsSL https://github.com/nbafrank/uvr/releases/latest/download/uvr-x86_64-unknown-linux-gnu.tar.gz | tar xz
    sudo mv uvr /usr/local/bin/

- name: Install R
  run: uvr r install 4.4.2

- name: Install packages (frozen = fail if lockfile is stale)
  run: uvr sync --frozen

- name: Run tests
  run: uvr run tests/run_tests.R
```

---

## Project layout

```
my-project/
├── uvr.toml          # manifest (commit this)
├── uvr.lock          # lockfile (commit this)
├── .r-version        # optional exact R pin (commit this)
└── .uvr/
    └── library/      # isolated package library (.gitignore this)
```

### `uvr.toml`

```toml
[project]
name = "my-project"
r_version = ">=4.3.0"

[dependencies]
ggplot2 = ">=3.0.0"
dplyr = "*"
DESeq2 = { bioc = true }
myPkg = { git = "user/repo", rev = "main" }
nestedPkg = { git = "user/monorepo", rev = "main", subdirectory = "packages/nestedPkg" }

[dev-dependencies]
testthat = "*"
```

Generated or imported git entries may also carry `exact = true`, which preserves an explicit DESCRIPTION `PackageName=` alias and requires the fetched DESCRIPTION `Package:` field to match the manifest dependency name.

---

## System dependencies (Linux)

On Linux, `uvr sync` automatically checks for missing system libraries and
prints the install command for your distro's package manager (`apt-get`,
`dnf`, `zypper`, or `apk`):

```
! Missing system dependencies for 2 package(s):
  textshaping requires: libharfbuzz-dev, libfribidi-dev
  ragg requires: libfreetype6-dev, libpng-dev

  Install with: sudo apt-get install -y libharfbuzz-dev libfribidi-dev libfreetype6-dev libpng-dev
```

To ask the question without syncing — writing a Dockerfile, adding a CI
setup step, or just checking before you commit to an install:

```sh
uvr sysdeps        # what is missing here; exits 1 when anything is
uvr sysdeps --all  # everything the project needs, installed or not; exits 0
```

`uvr sysdeps` reads `uvr.lock`, so run `uvr lock` first. Use `--all` when
building an image: the image has nothing installed yet, so "what is missing
on this host" is the wrong question.

Pass `--install-system-deps` (or set `UVR_INSTALL_SYSREQS=1`) and uvr runs
the commands itself, showing each one and where it came from before
anything executes as root. Requirements are resolved from the
[r-system-requirements](https://github.com/rstudio/r-system-requirements)
rules vendored into uvr, cross-checked against Posit's sysreqs API when
reachable.

---

## Environment diagnostics

Run `uvr doctor` to check your setup:

```
> uvr doctor

Platform
  v OS / architecture            macos/aarch64
  v P3M binary packages          available

R installations
  v R 4.5.3                      ~/.uvr/r-versions/4.5.3/bin/R - managed
  v R 4.4.2                      ~/.uvr/r-versions/4.4.2/bin/R - managed
  -> active                      4.5.3 ~/.uvr/r-versions/4.5.3/bin/R

Build tools
  v cargo (Rust toolchain)       found
  v Xcode command line tools     found
  v Homebrew                     found

Project
  v Manifest                     uvr.toml
  v Lockfile                     42 package(s), R 4.5.3

Cache
  - 166 file(s), 204.6 MB

v No issues found
```

---

## Platform support

| Platform | Binary packages | Source install | R version management |
|----------|----------------|----------------|----------------------|
| macOS ARM64 (Apple Silicon) | P3M | Y | Y (R 4.1.0+) |
| macOS x86-64 | P3M | Y | Y (R 4.1.0+) |
| Linux x86-64 (glibc ≥ 2.34) | P3M (Ubuntu, Debian, RHEL, openSUSE) | Y | Y |
| Linux ARM64 (glibc ≥ 2.34) | P3M (Ubuntu, Debian, RHEL, openSUSE) | Y | Y |
| Linux (musl / Alpine) | source | Y | Y |
| Windows x86-64 | P3M | Y (with Rtools) | Y (R 4.1.0+, no admin required) |

P3M binary packages are sourced from [Posit Package Manager](https://packagemanager.posit.co/). R itself is installed from the **portable, relocatable builds** published by [rstudio/r-builds](https://github.com/rstudio/r-builds) on [Posit CDN](https://cdn.posit.co/r/versions.json) — `manylinux_2_34` tarballs on glibc Linux (requires **glibc ≥ 2.34**; excludes Ubuntu 20.04, RHEL 8, Debian 11), `musllinux_1_2` on Alpine, ad-hoc-signed `.tar.gz` on macOS (R 4.1.0+), and `.zip` on Windows (R 4.1.0+). The portable Linux builds bundle their own libraries but expect `ca-certificates` and `fontconfig` (plus `ttf-dejavu` on Alpine) to be present on the host.

---

## Acknowledgments

uvr is shaped by the people who use it and report back. Special thanks to:

- [@B-Nilson](https://github.com/B-Nilson) — a steady stream of field
  reports and requests that became core behavior: cache-preserving R
  switches (#85), filtered cache cleaning (#92), and more.
- [@bsirak](https://github.com/bsirak) — the trampoline and symlink
  integration RFC (#109).
- [@gdevenyi](https://github.com/gdevenyi) — a systematic 46-issue audit of
  the entire codebase (#127–#172), with file-and-line precision, that drove
  the v0.4.1 and v0.4.2 fix batches (and a code contribution on top).
- [@pat-s](https://github.com/pat-s) — the Alpine/musl system-requirements
  groundwork, and candid feedback that improved how this project is run.
- [@hongyuanjia](https://github.com/hongyuanjia) — suggested building on
  Posit's r-builds (#96), which became the foundation of the current R
  install backend.
- [@zorbax](https://github.com/zorbax) — the precise diagnosis of the macOS
  GNU-tar install failure (#125).

And to everyone who has filed an issue, tested a fix, or suggested a
direction — thank you; the last several releases were built from your
reports.

---

## Support

uvr is free and MIT-licensed. If it saves you time, you can support its
development on [Ko-fi](https://ko-fi.com/nbafrank).

---

## License

MIT — see [LICENSE](LICENSE).
