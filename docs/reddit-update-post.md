# Reddit Post Draft — r/rstats update

**Title:** uvr update: R companion package, RStudio/Positron integration, and more based on your feedback

---

A few weeks ago I shared [uvr](https://github.com/nbafrank/uvr), a fast R package and project manager written in Rust. The response was great — 18K views, 61 upvotes, and a lot of specific, actionable feedback. I've been heads-down shipping features based on what you asked for. Here's what's new.

### R companion package — use uvr without touching the terminal

This was the #1 request (thanks u/BothSinger886). Many R users — especially scientists — live in the console, not the terminal. Now you can manage your entire project from R:

```r
# install.packages("pak")
pak::pak("nbafrank/uvr-r")

library(uvr)
init()
add("tidyverse")
add("DESeq2", bioc = TRUE)
sync()
```

Every uvr command has an R equivalent: `init()`, `add()`, `remove_pkgs()`, `sync()`, `lock()`, `run()`. If the CLI binary isn't installed, it prompts you to install it automatically. No terminal required.

### RStudio and Positron just work

`uvr init` and `uvr sync` now generate a `.Rprofile` that sets up the project library path automatically. Open your project in RStudio and it picks up the right library — no configuration needed.

For Positron, uvr also writes `.vscode/settings.json` with the project's R interpreter path, so the correct R version appears in the IDE without manual setup.

### Smarter error handling

- **Typo protection:** `uvr add tidyvese` (typo) used to write the bad name to your manifest before failing resolution. Now the manifest rolls back automatically on failure — your `uvr.toml` stays clean.
- **4-component versions:** Packages like `data.table` (version `1.18.2.1`) now resolve correctly against version constraints. This was a subtle semver edge case that broke real workflows.

### `uvr run --with` for one-off dependencies

Like `uv run --with` in Python. Need a package for a quick script without adding it to your project?

```sh
uvr run --with gt script.R
```

The package is installed to a temporary cache and available only for that run.

### What's next

- **Windows support** — compiles and runs on Windows now, full testing in progress
- **DESCRIPTION file support** — use `DESCRIPTION` as an alternative manifest alongside `uvr.toml`
- Continued benchmarking and hardening

The full feature set: R version management, CRAN + Bioconductor + GitHub packages, P3M pre-built binaries, lockfile, dependency tree, `uvr doctor`, `uvr export` (to renv.lock), `uvr import` (from renv.lock), shell completions, self-update, and more.

Install in one line:

```sh
curl -fsSL https://raw.githubusercontent.com/nbafrank/uvr/main/install.sh | sh
```

Or from R:

```r
pak::pak("nbafrank/uvr-r")
uvr::install_uvr()
```

GitHub: https://github.com/nbafrank/uvr  
R package: https://github.com/nbafrank/uvr-r

Happy to answer questions. Your feedback last time shaped all of this — keep it coming.
