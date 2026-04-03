# uvr

[![CI](https://github.com/nbafrank/uvr/actions/workflows/ci.yml/badge.svg)](https://github.com/nbafrank/uvr/actions)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Built with Rust](https://img.shields.io/badge/built%20with-Rust-orange.svg)](https://www.rust-lang.org/)

An extremely fast R package and project manager, written in Rust.

---

`uvr` brings uv-style project management to R: a `uvr.toml` manifest, a reproducible `uvr.lock` lockfile, and a per-project isolated library. Packages install from pre-built [P3M](https://packagemanager.posit.co/) binaries by default — no compilation, no waiting — with automatic fallback to CRAN source. R versions are managed per-project with no `sudo` required.

```sh
$ uvr init my-analysis
$ uvr add ggplot2 dplyr tidymodels
$ uvr sync          # installs from lockfile, idempotent
$ uvr run analysis.R
```

---

## Rationale

R has several package management tools — `renv`, `pak`, `rv`, `rig` — each solving a different slice of the problem. After 10+ years of R development, the workflow I kept wanting was the one `uv` brought to Python: **a single tool that handles the full lifecycle**, from installing R itself to adding packages to reproducible installs in CI, with no configuration sprawl.

Here is how existing tools compare and where the gaps are:

- **renv** — the de-facto standard for reproducibility. It snapshots an existing library into a lockfile, but it does not manage R versions ("renv tracks, but doesn't help with, the version of R used") and relies on `install.packages()` under the hood, which is slow and requires compilation on Linux.
- **pak** — fast parallel installs and good system dependency detection, but no lockfile and no R version management. A great complement to renv, not a replacement.
- **rv** — the closest prior art: Rust-based, declarative, fast. It focuses on package resolution. It does not manage R installations, and `rv run` is not yet available.
- **rig** — excellent R version manager. No package management or lockfile. Requires admin rights on Windows.
- **pixi** — conda-based multi-language environment manager. Supports R via conda-forge, but packages come from conda-forge rather than CRAN/Bioconductor/P3M natively. Language-agnostic by design; not R-first.
- **rix** — Nix-based, with extreme reproducibility including system-level dependencies. Right tool if you need bit-for-bit reproducibility across machines. Requires Nix; a different philosophy than a fast pragmatic workflow.

`uvr` is the combination of all of the above in one tool, with a single config file (`uvr.toml`) and a single lockfile (`uvr.lock`). The design goals are:

1. **One tool, one config** — no juggling renv + rig + pak. `uvr.toml` declares both the R version and package dependencies.
2. **Lockfile-first** — `uvr.lock` is the source of truth. `uvr sync` is always reproducible and idempotent.
3. **Fast by default** — P3M pre-built binaries on macOS and Windows; source fallback only when needed.
4. **R version management built in** — `uvr r install`, `uvr r use`, `uvr r pin` work the same way `uv python` does, because needing a separate tool for this is friction.
5. **CI-native** — `uvr sync --frozen` is a first-class command, not an afterthought.

If you are happy with renv + rig, that is a perfectly good setup. `uvr` is for people who want the `uv` experience in R.

### Feature matrix

|                              | uvr | renv | pak | rv  | rig | pixi |
|------------------------------|-----|------|-----|-----|-----|------|
| Declarative manifest         | Y   | -    | -   | Y   | -   | Y    |
| Lockfile                     | Y   | Y    | -   | Y   | -   | Y    |
| R version management         | Y   | -    | -   | -   | Y   | Y    |
| Run scripts in isolated env  | Y   | -    | -   | -   | -   | Y    |
| CRAN packages                | Y   | Y    | Y   | Y   | -   | Y*   |
| Bioconductor packages        | Y   | Y    | Y   | Y   | -   | Y*   |
| GitHub packages              | Y   | Y    | Y   | Y   | -   | -    |
| Pre-built binaries (P3M)     | Y   | -    | Y   | -   | -   | -    |
| System dep detection (Linux) | Y   | -    | Y   | -   | -   | Y    |
| Single config file           | Y   | -    | -   | Y   | -   | Y    |
| CI mode (`--frozen`)         | Y   | Y    | -   | -   | -   | Y    |
| No admin rights required     | Y   | Y    | Y   | Y   | -** | Y    |
| Single static binary         | Y   | -    | -   | Y   | Y   | -    |
| Windows support              | Y   | Y    | Y   | Y   | Y   | Y    |

\* pixi installs R packages from conda-forge, not CRAN/Bioconductor directly.
\** rig requires admin rights on Windows.

---

## Benchmarks

Cold-install wall time (empty library -> all packages installed). P3M binaries for all tools. Median of 3 runs on Apple Silicon.

| Scenario | Packages | uvr sync | install.packages | Speedup |
|----------|----------|----------|------------------|---------|
| ggplot2  | 17       | **0.5s** | 13.9s            | ~28x    |
| tidyverse| 99       | **1.6s** | 6.8s             | ~4x     |

> Run `bash benchmarks/bench.sh` to reproduce. Results vary by machine and network.

---

## Highlights

- **Blazing fast** — installs from pre-built P3M binaries; compiles from source only when needed
- **Reproducible** — `uvr.lock` is the source of truth; `uvr sync` is always idempotent
- **Project-isolated** — every project gets its own `.uvr/library/`, never touching system R
- **Full R version management** — `uvr r install 4.4.2`, `uvr r use >=4.3`, `uvr r pin 4.4.2`
- **CRAN + Bioconductor + GitHub** — `uvr add DESeq2 --bioc`, `uvr add user/repo@main`
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
# Install the R package
install.packages("uvr", repos = NULL, type = "source",
                  INSTALL_opts = "--no-multiarch")
# Or from the repo:
# install.packages("path/to/r-package", repos = NULL, type = "source")

# Download and install the uvr binary
uvr::install_uvr()
```

### From source (requires [Rust](https://rustup.rs))

```sh
cargo install --git https://github.com/nbafrank/uvr
```

---

## Quick start

```sh
# Create a new project
uvr init my-project --r-version ">=4.3.0"
cd my-project

# Add packages (CRAN, Bioconductor, GitHub)
uvr add ggplot2 dplyr
uvr add DESeq2 --bioc
uvr add tidymodels@>=1.0.0
uvr add user/repo@main

# Install everything from the lockfile
uvr sync

# Run a script in the isolated environment
uvr run analysis.R -- --input data.csv

# See what you have
uvr tree
```

---

## Commands

| Command | Description |
|---------|-------------|
| `uvr init [name]` | Create `uvr.toml` and `.uvr/library/` in the current directory |
| `uvr add <pkg...>` | Add packages, update manifest + lockfile, install |
| `uvr remove <pkg...>` | Remove packages from manifest and re-lock |
| `uvr sync` | Install all packages from the lockfile |
| `uvr sync --frozen` | Like `sync`, but fail if the lockfile is stale (CI mode) |
| `uvr update [pkg...]` | Upgrade packages to latest allowed versions |
| `uvr update --dry-run` | Show what would change without installing |
| `uvr lock` | Re-resolve all deps and update `uvr.lock` without installing |
| `uvr lock --upgrade` | Upgrade all packages to their latest allowed versions |
| `uvr tree` | Show the dependency tree |
| `uvr tree --depth 1` | Show only direct dependencies |
| `uvr run [script.R]` | Run a script (or interactive R) with the project library active |
| `uvr run --with pkg` | Run with extra packages available (not added to manifest) |
| `uvr r install <ver>` | Download and install a specific R version to `~/.uvr/r-versions/` |
| `uvr r list` | Show installed R versions |
| `uvr r list --all` | Show all available R versions (fetched from CRAN) |
| `uvr r use <ver>` | Set R version constraint in `uvr.toml` |
| `uvr r pin <ver>` | Write exact version to `.r-version` |
| `uvr export` | Export lockfile to renv.lock format |
| `uvr export -o renv.lock` | Export to a file |
| `uvr import` | Import packages from an renv.lock file |
| `uvr import --lock` | Import and immediately resolve + install |
| `uvr self-update` | Update uvr itself to the latest GitHub release |
| `uvr doctor` | Diagnose environment issues (R, build tools, project status) |
| `uvr completions <shell>` | Generate shell completions (bash, zsh, fish, powershell) |
| `uvr cache clean` | Remove all cached package downloads |

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

R versions are installed to `~/.uvr/r-versions/` and managed independently of any system R installation. On Windows, the CRAN installer runs with `/CURRENTUSER` — no admin elevation required, making it ideal for corporate and university environments where users cannot install software system-wide.

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

[dev-dependencies]
testthat = "*"
```

---

## System dependencies (Linux)

On Linux, `uvr sync` automatically checks for missing system libraries using the [r-hub sysreqs API](https://sysreqs.r-hub.io/) and prints the `apt-get install` command needed:

```
! Missing system dependencies for 2 package(s):
  textshaping requires: libharfbuzz-dev, libfribidi-dev
  ragg requires: libfreetype6-dev, libpng-dev

  Install with: sudo apt-get install -y libharfbuzz-dev libfribidi-dev libfreetype6-dev libpng-dev
```

---

## Environment diagnostics

Run `uvr doctor` to check your setup:

```
uvr doctor

  • Platform: macos/aarch64
  • P3M binary packages: available

R installations
  ✓ R 4.5.3 at ~/.uvr/r-versions/4.5.3/bin/R (managed)
  ✓ R 4.4.2 at ~/.uvr/r-versions/4.4.2/bin/R (managed)
  → Active R: 4.5.3

Build tools
  ✓ cargo (Rust toolchain): found
  ✓ Xcode command line tools: found
  ✓ Homebrew: found

Project
  ✓ Manifest: uvr.toml
  ✓ Lockfile: 42 package(s), R 4.5.3

Cache
  • 166 file(s), 204.6 MB

✓ No issues found
```

---

## Platform support

| Platform | Binary packages | Source install | R version management |
|----------|----------------|----------------|----------------------|
| macOS ARM64 (Apple Silicon) | P3M | Y | Y |
| macOS x86-64 | P3M | Y | Y |
| Linux x86-64 | - | Y | Y (Ubuntu 22.04+) |
| Linux ARM64 | - | Y | Y (Ubuntu 22.04+) |
| Windows x86-64 | P3M | Y (with Rtools) | Y (no admin required) |

P3M binary packages are sourced from [Posit Package Manager](https://packagemanager.posit.co/). Linux R binaries are sourced from [Posit CDN](https://cdn.posit.co/) (Ubuntu 22.04+ only); macOS R binaries from CRAN; Windows R from the CRAN Inno Setup installer.

---

## R companion package

The `r-package/` directory contains an R package that wraps the `uvr` CLI for use from R/RStudio:

```r
uvr::init()                    # uvr init
uvr::add("ggplot2")            # uvr add ggplot2
uvr::sync()                    # uvr sync
uvr::run("analysis.R")         # uvr run analysis.R
uvr::install_uvr()             # download + install the uvr binary
```

See `r-package/` for installation instructions.

---

## License

MIT — see [LICENSE](LICENSE).
