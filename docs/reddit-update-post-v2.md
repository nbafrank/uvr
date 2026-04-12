# Reddit Post Draft — r/rstats update v2

**Title:** uvr v0.2.5 — fast R package manager now has a website, Alpine/MUSL support, renv import improvements, and security hardening

---

Hey r/rstats — I shared [uvr](https://nbafrank.github.io/uvr/) here a few weeks ago and the response was incredible (18K+ views). Since then I've been shipping features and fixes based on your feedback and real-world bug reports. Here's what's new.

### Website with benchmarks

uvr now has a proper landing page: **https://nbafrank.github.io/uvr/**

It has interactive benchmarks, feature comparison table, and install instructions all in one place. For the lazy:

| Scenario | uvr | renv | pak | install.packages |
|-----------|------|------|------|-----------------|
| ggplot2 (17 pkgs) | **0.6s** | 3.8s | 4.6s | 24.0s |
| tidyverse (99 pkgs) | **1.6s** | 12.1s | 12.1s | 14.3s |

_All tools use P3M as CRAN mirror. Median of 3 cold installs, caches cleared between scenarios._

### renv.lock import got smarter

`uvr import` now handles real-world renv.lock files much better:

- **Custom repositories** (r-universe, internal repos) are auto-detected and added as `[[sources]]` in uvr.toml
- **Merge mode** — running `uvr import` when `uvr.toml` already exists merges new packages in instead of erroring
- **GitHub SHA preservation** — commit SHAs from renv.lock are preserved, not just branch names
- RSPM and other CRAN mirror variants are correctly recognized

This was directly from issue reports — if you tried importing a polars or r-universe project before, it should work now.

### Alpine / MUSL builds

Docker users on Alpine: we now ship prebuilt MUSL binaries for both x86_64 and aarch64. The install script auto-detects your libc:

```sh
# Works in Alpine containers now
curl -fsSL https://raw.githubusercontent.com/nbafrank/uvr/main/install.sh | sh
```

### `--library` flag

`uvr sync --library /path/to/lib` installs packages to a custom location instead of `.uvr/library/`. Useful for shared environments or CI caching.

### Security hardening (3 rounds of code review)

This release went through three brutal code reviews. Highlights:

- **Atomic package extraction** — if extraction fails midway, no broken package is left behind
- **Companion package pinned to exact SHA + hash** — supply chain attack resistant
- **Path traversal guards** on all archive extraction (zip-slip prevention)
- **Download integrity** — tempfile + persist pattern eliminates race conditions
- **Cross-platform** — replaced `curl` dependency with native Rust HTTP (works on Windows without curl)
- **Version-aware install check** — `uvr sync` now verifies installed package versions match the lockfile, not just that a directory exists

### Other fixes

- 4-component versions (e.g. `data.table 1.18.2.1`) resolve correctly
- `stats4` recognized as base R package
- Bioconductor version pinning per R version
- P3M binary download falls back to source gracefully on server errors
- Diamond dependency conflicts trigger re-resolution instead of immediate failure

### Numbers

- 164 tests passing
- CI on macOS ARM64, macOS x86, Linux x86, Linux ARM64, Windows
- AUR packages maintained by @novica (`yay -S uvr-bin`)

---

Install:

```sh
curl -fsSL https://raw.githubusercontent.com/nbafrank/uvr/main/install.sh | sh
```

Or from R:

```r
pak::pak("nbafrank/uvr-r")
uvr::install_uvr()
```

Website: https://nbafrank.github.io/uvr/
GitHub: https://github.com/nbafrank/uvr
R package: https://github.com/nbafrank/uvr-r

Feedback welcome — it's been driving almost everything in this release.
