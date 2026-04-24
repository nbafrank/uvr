#!/usr/bin/env bash
#
# uvr benchmark suite
#
# Measures install wall time for uvr vs install.packages() vs pak vs renv.
# Two tiers: "warm" (index caches intact, library cleared) and "cold" (all
# caches cleared). The warm tier reflects typical daily use; the cold tier
# is the worst-case first-ever-run.
#
# Usage:
#   bash benchmarks/bench.sh                # default: 5 runs per tier
#   BENCH_RUNS=3 bash benchmarks/bench.sh   # fewer runs (faster)
#
# Requirements:
#   - uvr on PATH (or CARGO_HOME/bin)
#   - R managed by uvr (uvr r install <version>)
#   - Optional: pak, renv (auto-detected; skipped if missing)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
RUNS="${BENCH_RUNS:-5}"
P3M_REPO="https://p3m.dev/cran/latest"
JSON_OUT="$SCRIPT_DIR/bench-results.json"

# ─── helpers ────────────────────────────────────────────────────────────────

UVR_ORIG="${UVR:-uvr}"
if ! command -v "$UVR_ORIG" &>/dev/null; then
    UVR_ORIG="$HOME/.cargo/bin/uvr"
fi
if ! command -v "$UVR_ORIG" &>/dev/null; then
    echo "error: uvr not found on PATH or in ~/.cargo/bin" >&2
    exit 1
fi

# Snapshot the binary so a mid-run rebuild can't corrupt results.
UVR_SNAPSHOT="$(mktemp)"
cp "$(command -v "$UVR_ORIG")" "$UVR_SNAPSHOT"
chmod +x "$UVR_SNAPSHOT"
# macOS kills unsigned binaries; re-sign the copy (strip existing first).
if [ "$(uname -s)" = "Darwin" ]; then
    codesign --remove-signature "$UVR_SNAPSHOT" 2>/dev/null || true
    codesign -s - "$UVR_SNAPSHOT" 2>/dev/null || true
fi
UVR="$UVR_SNAPSHOT"
trap 'rm -f "$UVR_SNAPSHOT"' EXIT

# Return wall-clock seconds for a command using /usr/bin/time.
# Prints "FAIL" and returns 1 if the command exits non-zero.
time_cmd() {
    local timefile
    timefile=$(mktemp)
    # Separate /usr/bin/time's stderr (captured) from the command's stderr (discarded).
    # The inner sh -c silences the command; /usr/bin/time writes "real X.XX" to $timefile.
    /usr/bin/time -p sh -c '"$@" >/dev/null 2>&1' -- "$@" 2>"$timefile"
    local exit_code=$?
    local real
    real=$(awk '/^real/{print $2}' "$timefile")
    rm -f "$timefile"
    if [ "$exit_code" -ne 0 ]; then
        echo "FAIL"
        return 1
    fi
    echo "$real"
}

# Return the median of a space-separated list of numbers.
# Correct for both odd and even N.
median() {
    echo "$@" | tr ' ' '\n' | sort -n | awk '{
        a[NR]=$1
    } END {
        if (NR%2==1) print a[(NR+1)/2]
        else printf "%.2f\n", (a[NR/2]+a[NR/2+1])/2
    }'
}

# Clear ALL caches — used for cold-tier and warmup runs.
clear_all_caches() {
    # uvr download + index cache
    rm -rf ~/.uvr/cache/
    # uvr global package cache (CoW clonefile source)
    rm -rf ~/.uvr/packages/
    # pak / pkgcache
    rm -rf ~/.cache/R/pkgcache/
    rm -rf ~/Library/Caches/org.R-project.R/R/pkgcache/
    # renv global cache
    chmod -R u+w ~/.cache/R/renv/ 2>/dev/null || true
    rm -rf ~/.cache/R/renv/
    chmod -R u+w ~/Library/Caches/org.R-project.R/R/renv/ 2>/dev/null || true
    rm -rf ~/Library/Caches/org.R-project.R/R/renv/
    # R default download cache (macOS uses $TMPDIR, not /tmp)
    rm -rf "${TMPDIR:-/tmp}"/Rtmp*/downloaded_packages 2>/dev/null || true
    rm -rf /tmp/Rtmp*/downloaded_packages 2>/dev/null || true
}

# ─── detect tools ───────────────────────────────────────────────────────────

echo "=== uvr benchmark suite ==="
echo "uvr:  $($UVR --version 2>&1 || echo 'unknown')"
echo "runs: $RUNS per tier"
echo ""

HAS_PAK=false
HAS_RENV=false
TMPCHECK="$(mktemp -d)"
cp "$SCRIPT_DIR/uvr-ggplot2.toml" "$TMPCHECK/uvr.toml"
mkdir -p "$TMPCHECK/.uvr/library"
cat > "$TMPCHECK/check_tools.R" <<'REOF'
if (requireNamespace("pak", quietly=TRUE)) cat("pak:yes\n") else cat("pak:no\n")
if (requireNamespace("renv", quietly=TRUE)) cat("renv:yes\n") else cat("renv:no\n")
REOF
TOOL_CHECK=$(cd "$TMPCHECK" && "$UVR" run check_tools.R 2>/dev/null) || true
if echo "$TOOL_CHECK" | grep -q "pak:yes"; then HAS_PAK=true; fi
if echo "$TOOL_CHECK" | grep -q "renv:yes"; then HAS_RENV=true; fi
rm -rf "$TMPCHECK"

echo "tools: uvr, install.packages"
if $HAS_PAK; then echo "       pak (detected)"; else echo "       pak (not found — skipping)"; fi
if $HAS_RENV; then echo "       renv (detected)"; else echo "       renv (not found — skipping)"; fi
echo ""

# ─── JSON accumulator ──────────────────────────────────────────────────────

JSON_RESULTS=""
json_add() {
    local scenario="$1" tier="$2" tool="$3" times="$4" med="$5" npkg="$6" note="${7:-}"
    local times_json
    times_json="[$(echo "$times" | sed 's/ /, /g')]"
    local entry
    entry=$(printf '    {"scenario": "%s", "packages": %s, "tier": "%s", "tool": "%s", "times": %s, "median": %s' \
        "$scenario" "$npkg" "$tier" "$tool" "$times_json" "$med")
    if [ -n "$note" ]; then
        entry="$entry, \"note\": \"$note\"}"
    else
        entry="$entry}"
    fi
    if [ -n "$JSON_RESULTS" ]; then
        JSON_RESULTS="$JSON_RESULTS,
$entry"
    else
        JSON_RESULTS="$entry"
    fi
}

# ─── storage (flat arrays — bash 3 compatible) ─────────────────────────────

RESULT_ENTRIES=""
RESULT_NPKG_ENTRIES=""

set_result() {
    # Usage: set_result <tier>:<tool>:<scenario> <value>
    RESULT_ENTRIES="$RESULT_ENTRIES $1=$2"
}
get_result() {
    local key="$1"
    for entry in $RESULT_ENTRIES; do
        case "$entry" in "${key}="*) echo "${entry#*=}"; return ;; esac
    done
    echo "n/a"
}
set_npkg() { RESULT_NPKG_ENTRIES="$RESULT_NPKG_ENTRIES $1=$2"; }
get_npkg() {
    for entry in $RESULT_NPKG_ENTRIES; do
        case "$entry" in "${1}="*) echo "${entry#*=}"; return ;; esac
    done
    echo "?"
}

# ─── per-tool benchmark functions ──────────────────────────────────────────

bench_uvr() {
    local manifest="$1" scenario="$2" tier="$3"
    local times="" failed=0

    # Pre-resolve lockfile (resolution is separate from install in uvr's model)
    local setupdir
    setupdir="$(mktemp -d)"
    cp "$manifest" "$setupdir/uvr.toml"
    mkdir -p "$setupdir/.uvr/library"
    (cd "$setupdir" && "$UVR" lock 2>/dev/null) || true

    # Count packages
    local npkg
    npkg=$(grep -c '^\[\[package\]\]' "$setupdir/uvr.lock" 2>/dev/null || echo "?")
    set_npkg "$scenario" "$npkg"

    # Warmup run (populates index caches, discarded)
    clear_all_caches
    rm -rf "$setupdir/.uvr/library"
    mkdir -p "$setupdir/.uvr/library"
    (cd "$setupdir" && "$UVR" sync >/dev/null 2>&1) || true

    echo -n "  uvr sync ($tier):     "
    for i in $(seq 1 "$RUNS"); do
        if [ "$tier" = "cold" ]; then
            clear_all_caches
        fi
        # Clear library (companion reinstalled each run — no preservation)
        rm -rf "$setupdir/.uvr/library"
        mkdir -p "$setupdir/.uvr/library"

        local t
        t=$(time_cmd sh -c "cd '$setupdir' && '$UVR' sync") || true
        if [ "$t" = "FAIL" ]; then
            echo -n "FAIL "
            failed=$((failed + 1))
            continue
        fi
        times="$times $t"
        echo -n "${t}s "
    done

    rm -rf "$setupdir"

    if [ -z "$(echo "$times" | tr -d ' ')" ]; then
        echo "→ ALL FAILED"
        return
    fi
    local med
    med=$(median $times)
    set_result "${tier}:uvr:${scenario}" "$med"
    local note=""
    if [ "$failed" -gt 0 ]; then
        note="$failed of $RUNS runs failed"
    fi
    json_add "$scenario" "$tier" "uvr" "$(echo "$times" | sed 's/^ //')" "$med" "$npkg" "$note"
    if [ "$failed" -gt 0 ]; then
        echo "→ median ${med}s ($failed failed)"
    else
        echo "→ median ${med}s"
    fi
}

bench_install_packages() {
    local manifest="$1" scenario="$2" tier="$3"
    local times="" failed=0
    local npkg
    npkg=$(get_npkg "$scenario")

    # Warmup run
    clear_all_caches
    local warmdir
    warmdir="$(mktemp -d)"
    cp "$manifest" "$warmdir/uvr.toml"
    mkdir -p "$warmdir/.uvr/library" "$warmdir/iplib"
    cat > "$warmdir/bench_ip.R" <<REOF
lib <- file.path(getwd(), "iplib")
dir.create(lib, recursive = TRUE, showWarnings = FALSE)
.libPaths(lib)
options(repos = c(CRAN = "${P3M_REPO}"))
install.packages("${scenario}", lib = lib, quiet = TRUE, dependencies = NA)
REOF
    (cd "$warmdir" && R_LIBS= R_LIBS_USER= R_LIBS_SITE= "$UVR" run bench_ip.R >/dev/null 2>&1) || true
    rm -rf "$warmdir"

    echo -n "  install.packages ($tier): "
    for i in $(seq 1 "$RUNS"); do
        if [ "$tier" = "cold" ]; then
            clear_all_caches
        fi
        local benchdir
        benchdir="$(mktemp -d)"
        cp "$manifest" "$benchdir/uvr.toml"
        mkdir -p "$benchdir/.uvr/library" "$benchdir/iplib"

        cat > "$benchdir/bench_ip.R" <<REOF
lib <- file.path(getwd(), "iplib")
dir.create(lib, recursive = TRUE, showWarnings = FALSE)
.libPaths(lib)
options(repos = c(CRAN = "${P3M_REPO}"))
install.packages("${scenario}", lib = lib, quiet = TRUE, dependencies = NA)
REOF
        local t
        t=$(time_cmd sh -c "cd '$benchdir' && R_LIBS= R_LIBS_USER= R_LIBS_SITE= '$UVR' run bench_ip.R") || true
        if [ "$t" = "FAIL" ]; then
            echo -n "FAIL "
            failed=$((failed + 1))
        else
            times="$times $t"
            echo -n "${t}s "
        fi
        rm -rf "$benchdir"
    done

    if [ -z "$(echo "$times" | tr -d ' ')" ]; then
        echo "→ ALL FAILED"
        return
    fi
    local med
    med=$(median $times)
    set_result "${tier}:ip:${scenario}" "$med"
    local note=""
    if [ "$failed" -gt 0 ]; then
        note="$failed of $RUNS runs failed"
    fi
    json_add "$scenario" "$tier" "install.packages" "$(echo "$times" | sed 's/^ //')" "$med" "$npkg" "$note"
    if [ "$failed" -gt 0 ]; then
        echo "→ median ${med}s ($failed failed)"
    else
        echo "→ median ${med}s"
    fi
}

bench_pak() {
    local manifest="$1" scenario="$2" tier="$3"
    local times="" failed=0
    local npkg
    npkg=$(get_npkg "$scenario")

    # Warmup run
    clear_all_caches
    local warmdir
    warmdir="$(mktemp -d)"
    cp "$manifest" "$warmdir/uvr.toml"
    mkdir -p "$warmdir/.uvr/library" "$warmdir/paklib"
    cat > "$warmdir/bench_pak.R" <<REOF
lib <- file.path(getwd(), "paklib")
dir.create(lib, recursive = TRUE, showWarnings = FALSE)
# Load pak before isolating .libPaths (pak lives in the system library)
library(pak)
.libPaths(lib)
options(repos = c(CRAN = "${P3M_REPO}"))
pak::pkg_install("${scenario}", lib = lib, ask = FALSE, upgrade = FALSE)
REOF
    (cd "$warmdir" && R_LIBS= R_LIBS_USER= R_LIBS_SITE= "$UVR" run bench_pak.R >/dev/null 2>&1) || true
    rm -rf "$warmdir"

    echo -n "  pak ($tier):          "
    for i in $(seq 1 "$RUNS"); do
        if [ "$tier" = "cold" ]; then
            clear_all_caches
        fi
        local benchdir
        benchdir="$(mktemp -d)"
        cp "$manifest" "$benchdir/uvr.toml"
        mkdir -p "$benchdir/.uvr/library" "$benchdir/paklib"

        cat > "$benchdir/bench_pak.R" <<REOF
lib <- file.path(getwd(), "paklib")
dir.create(lib, recursive = TRUE, showWarnings = FALSE)
library(pak)
.libPaths(lib)
options(repos = c(CRAN = "${P3M_REPO}"))
pak::pkg_install("${scenario}", lib = lib, ask = FALSE, upgrade = FALSE)
REOF
        local t
        t=$(time_cmd sh -c "cd '$benchdir' && R_LIBS= R_LIBS_USER= R_LIBS_SITE= '$UVR' run bench_pak.R") || true
        if [ "$t" = "FAIL" ]; then
            echo -n "FAIL "
            failed=$((failed + 1))
        else
            times="$times $t"
            echo -n "${t}s "
        fi
        rm -rf "$benchdir"
    done

    if [ -z "$(echo "$times" | tr -d ' ')" ]; then
        echo "→ ALL FAILED"
        return
    fi
    local med
    med=$(median $times)
    set_result "${tier}:pak:${scenario}" "$med"
    local note=""
    if [ "$failed" -gt 0 ]; then
        note="$failed of $RUNS runs failed"
    fi
    json_add "$scenario" "$tier" "pak" "$(echo "$times" | sed 's/^ //')" "$med" "$npkg" "$note"
    if [ "$failed" -gt 0 ]; then
        echo "→ median ${med}s ($failed failed)"
    else
        echo "→ median ${med}s"
    fi
}

bench_pak_lockfile() {
    # Apples-to-apples with uvr sync: pre-resolved lockfile, install only.
    local manifest="$1" scenario="$2" tier="$3"
    local times="" failed=0
    local npkg
    npkg=$(get_npkg "$scenario")

    # Pre-generate pak lockfile (resolution excluded from timing)
    local lockdir
    lockdir="$(mktemp -d)"
    cp "$manifest" "$lockdir/uvr.toml"
    mkdir -p "$lockdir/.uvr/library"
    cat > "$lockdir/bench_pak_lock.R" <<REOF
library(pak)
options(repos = c(CRAN = "${P3M_REPO}"))
pak::lockfile_create("${scenario}", lockfile = file.path(getwd(), "pkg.lock"), lib = tempfile(), dependencies = TRUE)
REOF
    (cd "$lockdir" && R_LIBS= R_LIBS_USER= R_LIBS_SITE= "$UVR" run bench_pak_lock.R >/dev/null 2>&1) || {
        echo "  pak_lockfile ($tier):  SKIPPED (lockfile_create failed)"
        rm -rf "$lockdir"
        return
    }
    local lockfile_path="$lockdir/pkg.lock"
    if [ ! -f "$lockfile_path" ]; then
        echo "  pak_lockfile ($tier):  SKIPPED (no lockfile generated)"
        rm -rf "$lockdir"
        return
    fi

    # Warmup
    clear_all_caches
    local warmdir
    warmdir="$(mktemp -d)"
    cp "$lockfile_path" "$warmdir/pkg.lock"
    mkdir -p "$warmdir/paklib"
    cat > "$warmdir/bench_pak_li.R" <<REOF
library(pak)
lib <- file.path(getwd(), "paklib")
dir.create(lib, recursive = TRUE, showWarnings = FALSE)
.libPaths(lib)
pak::lockfile_install(lockfile = file.path(getwd(), "pkg.lock"), lib = lib, update = FALSE)
REOF
    (cd "$warmdir" && R_LIBS= R_LIBS_USER= R_LIBS_SITE= "$UVR" run bench_pak_li.R >/dev/null 2>&1) || true
    rm -rf "$warmdir"

    echo -n "  pak_lockfile ($tier): "
    for i in $(seq 1 "$RUNS"); do
        if [ "$tier" = "cold" ]; then
            clear_all_caches
        fi
        local benchdir
        benchdir="$(mktemp -d)"
        cp "$lockfile_path" "$benchdir/pkg.lock"
        mkdir -p "$benchdir/paklib"
        cat > "$benchdir/bench_pak_li.R" <<REOF
library(pak)
lib <- file.path(getwd(), "paklib")
dir.create(lib, recursive = TRUE, showWarnings = FALSE)
.libPaths(lib)
pak::lockfile_install(lockfile = file.path(getwd(), "pkg.lock"), lib = lib, update = FALSE)
REOF
        local t
        t=$(time_cmd sh -c "cd '$benchdir' && R_LIBS= R_LIBS_USER= R_LIBS_SITE= '$UVR' run bench_pak_li.R") || true
        if [ "$t" = "FAIL" ]; then
            echo -n "FAIL "
            failed=$((failed + 1))
        else
            times="$times $t"
            echo -n "${t}s "
        fi
        rm -rf "$benchdir"
    done

    rm -rf "$lockdir"

    if [ -z "$(echo "$times" | tr -d ' ')" ]; then
        echo "→ ALL FAILED"
        return
    fi
    local med
    med=$(median $times)
    set_result "${tier}:pak_lockfile:${scenario}" "$med"
    local note=""
    if [ "$failed" -gt 0 ]; then
        note="$failed of $RUNS runs failed"
    fi
    json_add "$scenario" "$tier" "pak_lockfile" "$(echo "$times" | sed 's/^ //')" "$med" "$npkg" "$note"
    if [ "$failed" -gt 0 ]; then
        echo "→ median ${med}s ($failed failed)"
    else
        echo "→ median ${med}s"
    fi
}

bench_renv() {
    local manifest="$1" scenario="$2" tier="$3"
    local times="" failed=0
    local npkg
    npkg=$(get_npkg "$scenario")

    # Warmup run
    clear_all_caches
    local warmdir
    warmdir="$(mktemp -d)"
    cp "$manifest" "$warmdir/uvr.toml"
    mkdir -p "$warmdir/.uvr/library"
    cat > "$warmdir/bench_renv.R" <<REOF
library(renv)
options(repos = c(CRAN = "${P3M_REPO}"))
renv::init(bare = TRUE)
renv::install("${scenario}", prompt = FALSE)
REOF
    (cd "$warmdir" && R_LIBS= R_LIBS_USER= R_LIBS_SITE= "$UVR" run bench_renv.R >/dev/null 2>&1) || true
    rm -rf "$warmdir"

    echo -n "  renv ($tier):         "
    for i in $(seq 1 "$RUNS"); do
        if [ "$tier" = "cold" ]; then
            clear_all_caches
        fi
        local benchdir
        benchdir="$(mktemp -d)"
        cp "$manifest" "$benchdir/uvr.toml"
        mkdir -p "$benchdir/.uvr/library"

        cat > "$benchdir/bench_renv.R" <<REOF
library(renv)
options(repos = c(CRAN = "${P3M_REPO}"))
renv::init(bare = TRUE)
renv::install("${scenario}", prompt = FALSE)
REOF
        local t
        t=$(time_cmd sh -c "cd '$benchdir' && R_LIBS= R_LIBS_USER= R_LIBS_SITE= '$UVR' run bench_renv.R") || true
        if [ "$t" = "FAIL" ]; then
            echo -n "FAIL "
            failed=$((failed + 1))
        else
            times="$times $t"
            echo -n "${t}s "
        fi
        rm -rf "$benchdir"
    done

    if [ -z "$(echo "$times" | tr -d ' ')" ]; then
        echo "→ ALL FAILED"
        return
    fi
    local med
    med=$(median $times)
    set_result "${tier}:renv:${scenario}" "$med"
    local note=""
    if [ "$failed" -gt 0 ]; then
        note="$failed of $RUNS runs failed"
    fi
    json_add "$scenario" "$tier" "renv" "$(echo "$times" | sed 's/^ //')" "$med" "$npkg" "$note"
    if [ "$failed" -gt 0 ]; then
        echo "→ median ${med}s ($failed failed)"
    else
        echo "→ median ${med}s"
    fi
}

# ─── main loop ─────────────────────────────────────────────────────────────

SCENARIOS="jsonlite ggplot2 tidyverse"

for scenario in $SCENARIOS; do
    MANIFEST="$SCRIPT_DIR/uvr-${scenario}.toml"
    if [ ! -f "$MANIFEST" ]; then
        echo "warning: $MANIFEST not found, skipping $scenario" >&2
        continue
    fi

    for tier in warm cold; do
        echo "--- $scenario ($tier) ---"

        # Tool order matters: each tool's warmup calls clear_all_caches, which
        # destroys prior tools' caches. Current order is safe because each tool
        # finishes all its timed runs before the next tool's warmup clears caches.
        bench_uvr "$MANIFEST" "$scenario" "$tier"
        bench_install_packages "$MANIFEST" "$scenario" "$tier"
        if $HAS_PAK; then
            bench_pak "$MANIFEST" "$scenario" "$tier"
            bench_pak_lockfile "$MANIFEST" "$scenario" "$tier"
        fi
        if $HAS_RENV; then bench_renv "$MANIFEST" "$scenario" "$tier"; fi

        echo ""
    done
done

# ─── results tables ────────────────────────────────────────────────────────

print_table() {
    local tier="$1" label="$2"

    echo "$label"
    echo ""

    # Build header
    local header="| Scenario | Packages | uvr sync | install.packages"
    local sep="|----------|----------|----------|------------------"
    if $HAS_PAK; then header="$header | pak | pak lockfile"; sep="$sep|-----|--------------"; fi
    if $HAS_RENV; then header="$header | renv"; sep="$sep|------"; fi
    echo "$header |"
    echo "$sep|"

    for scenario in $SCENARIOS; do
        local npkg
        npkg=$(get_npkg "$scenario")
        local uvr_t ip_t
        uvr_t="$(get_result "${tier}:uvr:${scenario}")s"
        ip_t="$(get_result "${tier}:ip:${scenario}")s"
        local row="| $scenario | $npkg | **$uvr_t** | $ip_t"
        if $HAS_PAK; then
            row="$row | $(get_result "${tier}:pak:${scenario}")s"
            row="$row | $(get_result "${tier}:pak_lockfile:${scenario}")s"
        fi
        if $HAS_RENV; then row="$row | $(get_result "${tier}:renv:${scenario}")s"; fi
        echo "$row |"
    done
    echo ""
}

echo ""
echo "## Results"
echo ""
print_table "warm" "### Typical use (index caches warm, library empty)"
print_table "cold" "### First run (all caches cleared)"

# ─── methodology ───────────────────────────────────────────────────────────

R_VER=$(R --vanilla --slave -e 'cat(R.version.string)' 2>/dev/null || echo "R (version unknown)")
echo "_Measured on $(uname -m), ${R_VER}, P3M binaries. Median of ${RUNS} runs._"
echo ""
echo "Methodology:"
echo "- \"warm\": 1 warmup run (discarded), then index caches left intact; target library cleared between runs"
echo "- \"cold\": all caches (index, download, metadata) cleared between runs"
echo "- uvr: lockfile pre-resolved (\`uvr lock\`); only \`uvr sync\` (install) is timed"
echo "- install.packages/pak/renv: resolution + install timed together (first-install scenario)"
echo "- pak_lockfile: apples-to-apples with uvr sync — pak::lockfile_install() from a pre-built pkg.lock (install only)"
echo "- All tools use P3M (p3m.dev/cran/latest) as CRAN mirror"
echo "- .libPaths fully isolated for install.packages and pak (system library hidden)"
echo "- renv uses its default global cache (matching real-world usage)"
echo "- Companion package (uvr's R helper) reinstalled each run (not preserved)"
echo "- Warm tier: uvr, pak, and renv benefit from persistent caches; install.packages has no persistent cache"

# ─── JSON output ───────────────────────────────────────────────────────────

UVR_VER=$("$UVR" --version 2>&1 | head -1 || echo "unknown")
cat > "$JSON_OUT" <<JEOF
{
  "meta": {
    "date": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
    "arch": "$(uname -m)",
    "r_version": "$(echo "$R_VER" | sed 's/R version //')",
    "uvr_version": "$UVR_VER",
    "runs_per_tier": $RUNS,
    "mirror": "$P3M_REPO"
  },
  "results": [
$JSON_RESULTS
  ]
}
JEOF

echo ""
echo "JSON results written to $JSON_OUT"

# Auto-update README.md if the update script exists
UPDATE_SCRIPT="$SCRIPT_DIR/update-readme.sh"
if [ -x "$UPDATE_SCRIPT" ]; then
    bash "$UPDATE_SCRIPT"
fi
