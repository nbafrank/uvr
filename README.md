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

## Highlights

- **Blazing fast** — installs from pre-built P3M binaries; compiles from source only when needed
- **Reproducible** — `uvr.lock` is the source of truth; `uvr sync` is always idempotent
- **Project-isolated** — every project gets its own `.uvr/library/`, never touching system R
- **Full R version management** — `uvr r install 4.4.2`, `uvr r use >=4.3`, `uvr r pin 4.4.2`
- **CRAN + Bioconductor + GitHub** — `uvr add DESeq2 --bioc`, `uvr add user/repo@main`
- **CI-ready** — `uvr sync --frozen` fails fast if the lockfile is stale; respects `NO_COLOR`
- **Written in Rust** — single static binary, no R or Python required to install

---

## Installation

### Standalone (requires [Rust](https://rustup.rs))

```sh
cargo install --git https://github.com/nbafrank/uvr
```

### Build from source

```sh
git clone https://github.com/nbafrank/uvr
cd uvr
cargo install --path crates/uvr
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
| `uvr lock` | Re-resolve all deps and update `uvr.lock` without installing |
| `uvr lock --upgrade` | Upgrade all packages to their latest allowed versions |
| `uvr run [script.R]` | Run a script (or interactive R) with the project library active |
| `uvr r install <ver>` | Download and install a specific R version to `~/.uvr/r-versions/` |
| `uvr r list` | Show installed R versions |
| `uvr r list --all` | Show all available R versions (fetched from CRAN) |
| `uvr r use <ver>` | Set R version constraint in `uvr.toml` |
| `uvr r pin <ver>` | Write exact version to `.r-version` |
| `uvr cache clean` | Remove all cached package downloads |

---

## Project layout

```
my-project/
├── uvr.toml          # manifest (commit this)
├── uvr.lock          # lockfile (commit this)
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
```

---

## Platform support

| Platform | Binary packages | Source install | R version management |
|----------|----------------|----------------|----------------------|
| macOS ARM64 (Apple Silicon) | ✓ P3M | ✓ | ✓ |
| macOS x86-64 | ✓ P3M | ✓ | ✓ |
| Linux x86-64 | — | ✓ | ✓ |
| Linux ARM64 | — | ✓ | ✓ |
| Windows | — | — | — |

P3M binary packages are sourced from [Posit Package Manager](https://packagemanager.posit.co/).

---

## License

MIT — see [LICENSE](LICENSE).
