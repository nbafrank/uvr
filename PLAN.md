# uvr — Roadmap

Derived from community feedback on the r/rstats launch post (18K views, 61 upvotes).

---

## High Priority

### 1. Benchmarks
**Why:** Multiple commenters asked "you say it's fast, prove it." `victor2wy` asked directly for numbers vs rv, install.packages, pak. `Ok_Sell_4717` (7 upvotes) challenged the whole project as vibe-coded with no thought behind it — benchmarks are the clearest rebuttal.
**What to do:**
- Benchmark `uvr sync` vs `install.packages()`, `pak`, `renv::restore()`, `rv` on a representative set of packages (e.g. tidyverse, DESeq2)
- Add a `benchmarks/` directory with the scripts and results
- Add a summary table to the README

### 2. System dependency resolution
**Why:** `r2u` and `pak` both detect/install system libraries. `Peach_Muffin` flagged this as a gap. It's a common Linux pain point and is why many people still prefer r2u or conda/pixi for Linux workflows.
**What to do:**
- On Linux: detect missing system deps for a package and surface `apt install` suggestions
- Minimum viable: warn the user with the list of missing libs before failing
- Stretch goal: auto-install via `apt` with user confirmation (`--system-deps` flag)

### 3. DESCRIPTION file support
**Why:** `Bach4Ants` pushed back on `uvr.toml`. `countnfight` clarified: DESCRIPTION is not just for packages — R users routinely use it for non-package projects to track dependencies with renv, commitizen, etc. `Unicorn_Colombo` +1'd this. R-native users will reach for DESCRIPTION before a .toml.
**What to do:**
- Support reading `DESCRIPTION` as an alternative manifest (at least `Imports:` and `Suggests:`)
- Document clearly in the README why `uvr.toml` was chosen (explicit versioning, multi-source deps, Bioc/GitHub support)
- Consider `uvr init --description` to generate a `DESCRIPTION`-compatible layout

---

## Medium Priority

### 4. R companion package (`uvr` R package)
**Why:** `BothSinger886` (12 upvotes) made the case strongly: many R users — especially scientists for whom R is their first and only programming language — will switch off entirely when asked to use the terminal. usethis proved that wrapping CLI operations as R functions drives adoption.
**What to do:**
- Create a separate R package (e.g. on GitHub as `nbafrank/uvr-r`)
- Expose: `uvr_init()`, `uvr_add()`, `uvr_remove()`, `uvr_sync()`, `uvr_run()`
- Each function shells out to the `uvr` binary
- Auto-install the binary if not found (similar to how `gert` bundles libgit2)

### 5. Windows support
**Why:** `analytix_guru` and `lamhintai` requested it. `lamhintai` specifically flagged that rig requires admin rights on Windows — corporate/university environments often block this. This is a key differentiator opportunity vs rig.
**What to do:**
- R version management on Windows: download `.exe` installer from CRAN, extract silently to `~/.uvr/r-versions/` **without requiring admin rights** (key differentiator vs rig)
- Package install: should work via `R CMD INSTALL` once R binary is available
- CI: add `windows-latest` to the GitHub Actions matrix

### 6. Bioconductor version tracking
**Why:** `banseljaj` asked directly: "I am curious about how it works with bioconductor versions. That is a big issue when working with bioconductor." Bioc releases are tied to specific R versions; tracking Bioc version alongside R version is a real pain point for bioinformatics users.
**What to do:**
- Add `bioc_version` field to `uvr.toml`
- Lock Bioc version in `uvr.lock` alongside package versions
- Resolve Bioc packages against the correct Bioc release for the pinned R version
- Document the Bioc version ↔ R version compatibility matrix

### 7. `uvr run` polish
**Why:** `bee_advised` explicitly called out that rv is still missing `rv run` — and that `uv run` (isolated script execution without needing Python installed) is a core part of what makes uv compelling. This is a concrete, current gap in rv that uvr has already solved. Make it shine.
**What to do:**
- Test `uvr run` thoroughly on macOS and Linux
- Support `uvr run --r-version <ver> script.R` (override version for one-off runs)
- Document inline dependency declarations (like `uv run --with pandas script.py`)
- Highlight `uvr run` prominently in README and comparison docs — it's a live differentiator vs rv

---

## Documentation / Positioning

### 8. Pixi comparison + "why uvr?" narrative
**Why:** `PadisarahTerminal` challenged directly (with upvotes): "Just what is uvr trying to do that others don't? You need to defend this." `I_just_made` asked about pixi. The rationale section covers renv/pak/rv/rig but not pixi or rix, and the overall "why uvr over everything" answer needs to be sharper.
**What to do:**
- Add pixi to the rationale section: pixi is conda-based (conda-forge packages), not CRAN/Bioc-native; pixi is language-agnostic while uvr is R-first and knows about Bioc, P3M, R version semantics
- Write a one-paragraph answer to "why uvr and not rv + rig?": the integration story — one config, one lock, one install path, no multi-tool setup
- The answer to `PadisarahTerminal` is: uvr is the only tool that handles R version management + package management + lockfile + `run` in one binary with one config file

### 9. rv comparison
**Why:** Most common question in the thread. Several commenters suggested merging or contributing to rv instead of building a new tool (`mostlikelylost` 12 upvotes, `novica`). The answer needs to be written down, not just said in comments.
**What to do:**
- Add a detailed feature matrix (rv vs uvr vs renv vs rig vs pixi) to README or `docs/comparison.md`
- Key uvr advantages: `uvr run` (rv doesn't have it yet), integrated R version management, P3M binaries, Bioc first-class, single-binary no-rig-dependency
- Key rv advantages: more mature, larger community, A2-AI backing — be honest

### 10. Fix Linux R source attribution
**Why:** In the Reddit thread, told `novica` that R builds come from CRAN. They actually come from Posit CDN (`cdn.posit.co`). The README platform table only mentions P3M as a source but does not clarify where Linux R binaries come from.
**What to do:**
- Update README platform table note: "Linux R binaries sourced from [Posit CDN](https://cdn.posit.co/) (Ubuntu 22.04+); macOS from CRAN"
- Be explicit that this means Ubuntu 22.04+ only for now

### 11. Address vibe-coding / credibility concern
**Why:** `Ok_Sell_4717` (7 upvotes) said: "You just asked Claude to create an R version of uv and were done in 5 commits... there is simply nothing to suggest it would be good or worth using." Benchmarks and the rationale section help, but sustained public development is the real answer.
**What to do:**
- Open GitHub issues for each roadmap item so the community can see the plan and follow along
- Post updates in r/rstats as milestones land (benchmarks, Windows, R companion package)
- Engage directly with rv/A2-AI community — being a good-faith open source actor is the best credibility signal

---

## Longer Term / Exploratory

### 12. Collaboration with rv
**Why:** Community suggested it from multiple angles (`novica`, `bee_advised`, `mostlikelylost`). Both projects are Rust-based with similar goals. `novica` specifically argued R tooling is already too fragmented and wants one robust tool. `bee_advised` said "my gut reaction is that the two projects may benefit from merging together."
**What to do:**
- Open a GitHub Discussion: "Collaboration with rv?"
- Reach out to A2-AI team directly
- Identify what each project does better and whether a merge, plugin model, or coordination makes sense

### 13. rix / Nix awareness
**Why:** `therealtiddlydump` (vocal advocate, multiple upvotes) uses rix for full Nix-based reproducibility. It solves a different problem (full system reproducibility incl. system deps) and has a dedicated audience. Not a competitor — different philosophy.
**What to do:**
- Add rix to the rationale section as a different philosophy: Nix-based full system reproducibility vs. fast pragmatic R-native installs
- Acknowledge rix is the right tool if you need bit-for-bit reproducibility or system dep pinning via Nix

---

## Quick wins (can do immediately)

- [x] Add rationale section comparing uvr to renv, pak, rv, rig (done in b6a0b7a)
- [ ] Add pixi and rix to rationale section (still missing)
- [ ] Fix Linux R source in README: Posit CDN, not CRAN (novica exposed this in thread)
- [ ] Open GitHub issues for each roadmap item so community can track progress
- [ ] Submit to R Weekly (rweekly.org) — open a PR adding uvr to `draft.md` under New Packages
