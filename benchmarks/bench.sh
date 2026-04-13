#!/usr/bin/env bash
#
# uvr benchmark suite
#
# Measures cold-install wall time for uvr vs install.packages() vs pak::pkg_install().
# Each tool installs from a clean library. Download caches are cleared between
# scenarios to prevent cross-scenario bleed.
#
# Usage:
#   bash benchmarks/bench.sh              # default: 3 runs per tool
#   BENCH_RUNS=5 bash benchmarks/bench.sh # override run count
#
# Requirements:
#   - uvr on PATH (or CARGO_HOME/bin)
#   - R managed by uvr (uvr r install <version>)
#   - Optional: pak (auto-detected; skipped if missing)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
RUNS="${BENCH_RUNS:-3}"
P3M_REPO="https://p3m.dev/cran/latest"

# ─── helpers ────────────────────────────────────────────────────────────────

UVR="${UVR:-uvr}"
if ! command -v "$UVR" &>/dev/null; then
    UVR="$HOME/.cargo/bin/uvr"
fi
if ! command -v "$UVR" &>/dev/null; then
    echo "error: uvr not found on PATH or in ~/.cargo/bin" >&2
    exit 1
fi

# Return wall-clock seconds (float) for a command.
time_cmd() {
    local start end
    start=$(python3 -c 'import time; print(f"{time.time():.3f}")')
    "$@" >/dev/null 2>&1 || true
    end=$(python3 -c 'import time; print(f"{time.time():.3f}")')
    python3 -c "print(f'{${end} - ${start}:.1f}')"
}

# Return the median of a space-separated list of numbers.
median() {
    echo "$@" | tr ' ' '\n' | sort -n | awk '{a[NR]=$1} END{print a[int((NR+1)/2)]}'
}

# Clear download caches for all tools to prevent cross-scenario bleed.
clear_all_caches() {
    # uvr download cache
    rm -rf ~/.uvr/cache/
    # pak / pkgcache
    rm -rf ~/.cache/R/pkgcache/
    rm -rf ~/Library/Caches/org.R-project.R/R/pkgcache/
    # renv global cache (sandbox contains read-only symlinks — chmod first)
    chmod -R u+w ~/.cache/R/renv/ 2>/dev/null || true
    rm -rf ~/.cache/R/renv/
    chmod -R u+w ~/Library/Caches/org.R-project.R/R/renv/ 2>/dev/null || true
    rm -rf ~/Library/Caches/org.R-project.R/R/renv/
    # R default download cache
    rm -rf /tmp/Rtmp*/downloaded_packages 2>/dev/null || true
}

# ─── detect tools ───────────────────────────────────────────────────────────

echo "=== uvr benchmark suite ==="
echo "uvr:  $($UVR --version 2>&1 || echo 'unknown')"
echo "runs: $RUNS per tool"
echo ""

# Check if pak / renv are available
HAS_PAK=false
HAS_RENV=false
TMPCHECK="$(mktemp -d)"
cp "$SCRIPT_DIR/uvr-ggplot2.toml" "$TMPCHECK/uvr.toml"
mkdir -p "$TMPCHECK/.uvr/library"
cat > "$TMPCHECK/check_tools.R" <<'REOF'
if (requireNamespace("pak", quietly=TRUE)) cat("pak:yes\n") else cat("pak:no\n")
if (requireNamespace("renv", quietly=TRUE)) cat("renv:yes\n") else cat("renv:no\n")
REOF
TOOL_CHECK=$(cd "$TMPCHECK" && "$UVR" run check_tools.R 2>/dev/null)
if echo "$TOOL_CHECK" | grep -q "pak:yes"; then HAS_PAK=true; fi
if echo "$TOOL_CHECK" | grep -q "renv:yes"; then HAS_RENV=true; fi
rm -rf "$TMPCHECK"

echo "tools: uvr, install.packages"
if $HAS_PAK; then echo "       pak (detected)"; else echo "       pak (not found — skipping)"; fi
if $HAS_RENV; then echo "       renv (detected)"; else echo "       renv (not found — skipping)"; fi
echo ""

# ─── storage (flat arrays — bash 3 compatible) ─────────────────────────────

# Results stored as "scenario:value" entries in flat arrays.
RESULT_UVR_ENTRIES=""
RESULT_IP_ENTRIES=""
RESULT_PAK_ENTRIES=""
RESULT_RENV_ENTRIES=""
RESULT_NPKG_ENTRIES=""

set_result() { eval "${1}=\"\${${1}} ${2}:${3}\""; }
get_result() {
    local entries val
    eval "entries=\"\${${1}}\""
    for entry in $entries; do
        case "$entry" in "${2}:"*) val="${entry#*:}"; echo "$val"; return ;; esac
    done
    echo "n/a"
}

# ─── scenarios ──────────────────────────────────────────────────────────────

SCENARIOS="ggplot2 tidyverse"

for scenario in $SCENARIOS; do
    MANIFEST="$SCRIPT_DIR/uvr-${scenario}.toml"
    echo "--- scenario: $scenario ---"

    # ── uvr sync ────────────────────────────────────────────────────────────

    echo -n "  uvr sync:            "
    UVR_TIMES=""

    # Pre-resolve lockfile once (resolution is not part of install benchmark)
    BENCHDIR="$(mktemp -d)"
    cp "$MANIFEST" "$BENCHDIR/uvr.toml"
    mkdir -p "$BENCHDIR/.uvr/library"
    (cd "$BENCHDIR" && "$UVR" lock 2>/dev/null) || true

    # Count packages in lockfile
    NPKG=$(grep -c '^\[\[package\]\]' "$BENCHDIR/uvr.lock" 2>/dev/null || echo "?")
    set_result RESULT_NPKG_ENTRIES "$scenario" "$NPKG"

    # Run sync once to seed companion package, then preserve it across wipes
    (cd "$BENCHDIR" && "$UVR" sync >/dev/null 2>&1) || true
    COMPANION_DIR="$BENCHDIR/.uvr/library/uvr"

    for i in $(seq 1 "$RUNS"); do
        # Clear all caches before every run for true cold-cache measurement
        clear_all_caches
        # Wipe library but preserve companion package to avoid re-install overhead
        COMPANION_BAK=""
        if [ -d "$COMPANION_DIR" ]; then
            COMPANION_BAK="$(mktemp -d)"
            mv "$COMPANION_DIR" "$COMPANION_BAK/uvr"
        fi
        rm -rf "$BENCHDIR/.uvr/library"
        mkdir -p "$BENCHDIR/.uvr/library"
        if [ -n "$COMPANION_BAK" ]; then
            mv "$COMPANION_BAK/uvr" "$COMPANION_DIR"
            rm -rf "$COMPANION_BAK"
        fi
        t=$(time_cmd sh -c "cd '$BENCHDIR' && '$UVR' sync")
        UVR_TIMES="$UVR_TIMES $t"
        echo -n "${t}s "
    done
    rm -rf "$BENCHDIR"
    MED=$(median $UVR_TIMES)
    set_result RESULT_UVR_ENTRIES "$scenario" "$MED"
    echo "→ median ${MED}s"

    # ── install.packages ────────────────────────────────────────────────────

    echo -n "  install.packages:    "
    IP_TIMES=""
    for i in $(seq 1 "$RUNS"); do
        clear_all_caches
        BENCHDIR="$(mktemp -d)"
        cp "$MANIFEST" "$BENCHDIR/uvr.toml"
        mkdir -p "$BENCHDIR/.uvr/library" "$BENCHDIR/iplib"

        # Write the R script that does install.packages
        cat > "$BENCHDIR/bench_ip.R" <<REOF
lib <- file.path(getwd(), "iplib")
dir.create(lib, recursive = TRUE, showWarnings = FALSE)
options(repos = c(CRAN = "${P3M_REPO}"))
install.packages("${scenario}", lib = lib, quiet = TRUE, dependencies = TRUE)
REOF
        t=$(time_cmd sh -c "cd '$BENCHDIR' && '$UVR' run bench_ip.R")
        IP_TIMES="$IP_TIMES $t"
        rm -rf "$BENCHDIR"
        echo -n "${t}s "
    done
    MED=$(median $IP_TIMES)
    set_result RESULT_IP_ENTRIES "$scenario" "$MED"
    echo "→ median ${MED}s"

    # ── pak ──────────────────────────────────────────────────────────────────

    if $HAS_PAK; then
        echo -n "  pak::pkg_install:    "
        PAK_TIMES=""
        for i in $(seq 1 "$RUNS"); do
            clear_all_caches
            BENCHDIR="$(mktemp -d)"
            cp "$MANIFEST" "$BENCHDIR/uvr.toml"
            mkdir -p "$BENCHDIR/.uvr/library" "$BENCHDIR/paklib"

            cat > "$BENCHDIR/bench_pak.R" <<REOF
lib <- file.path(getwd(), "paklib")
dir.create(lib, recursive = TRUE, showWarnings = FALSE)
pak::pkg_install("${scenario}", lib = lib, ask = FALSE)
REOF
            t=$(time_cmd sh -c "cd '$BENCHDIR' && '$UVR' run bench_pak.R")
            PAK_TIMES="$PAK_TIMES $t"
            rm -rf "$BENCHDIR"
            echo -n "${t}s "
        done
        MED=$(median $PAK_TIMES)
        set_result RESULT_PAK_ENTRIES "$scenario" "$MED"
        echo "→ median ${MED}s"
    fi

    # ── renv ────────────────────────────────────────────────────────────────

    if $HAS_RENV; then
        echo -n "  renv::restore:       "
        RENV_TIMES=""
        for i in $(seq 1 "$RUNS"); do
            clear_all_caches
            BENCHDIR="$(mktemp -d)"
            cp "$MANIFEST" "$BENCHDIR/uvr.toml"
            mkdir -p "$BENCHDIR/.uvr/library" "$BENCHDIR/renvlib"

            # Generate an renv.lock from the uvr lockfile, then restore from it
            cat > "$BENCHDIR/bench_renv.R" <<REOF
lib <- file.path(getwd(), "renvlib")
dir.create(lib, recursive = TRUE, showWarnings = FALSE)
options(repos = c(CRAN = "${P3M_REPO}"))
# Bootstrap renv in this temp project
renv::init(bare = TRUE, settings = list(use.cache = FALSE))
# Install the target package + deps into the renv library
renv::install("${scenario}", prompt = FALSE)
REOF
            t=$(time_cmd sh -c "cd '$BENCHDIR' && '$UVR' run bench_renv.R")
            RENV_TIMES="$RENV_TIMES $t"
            rm -rf "$BENCHDIR"
            echo -n "${t}s "
        done
        MED=$(median $RENV_TIMES)
        set_result RESULT_RENV_ENTRIES "$scenario" "$MED"
        echo "→ median ${MED}s"
    fi

    echo ""
done

# ─── results table ──────────────────────────────────────────────────────────

echo ""
echo "## Results"
echo ""

# Build header dynamically
HEADER="| Scenario | Packages | uvr sync | install.packages"
SEPARATOR="|----------|----------|----------|------------------"
if $HAS_PAK; then HEADER="$HEADER | pak"; SEPARATOR="$SEPARATOR|-----"; fi
if $HAS_RENV; then HEADER="$HEADER | renv"; SEPARATOR="$SEPARATOR|------"; fi
echo "$HEADER |"
echo "$SEPARATOR|"

for scenario in $SCENARIOS; do
    npkg=$(get_result RESULT_NPKG_ENTRIES "$scenario")
    uvr_t="$(get_result RESULT_UVR_ENTRIES "$scenario")s"
    ip_t="$(get_result RESULT_IP_ENTRIES "$scenario")s"
    ROW="| $scenario | $npkg | **$uvr_t** | $ip_t"
    if $HAS_PAK; then ROW="$ROW | $(get_result RESULT_PAK_ENTRIES "$scenario")s"; fi
    if $HAS_RENV; then ROW="$ROW | $(get_result RESULT_RENV_ENTRIES "$scenario")s"; fi
    echo "$ROW |"
done

echo ""
R_VER=$("$UVR" run -e 'cat(R.version.string)' 2>/dev/null || echo "R (version unknown)")
echo "_Measured on $(uname -m), ${R_VER}, P3M binaries (all tools use P3M as CRAN mirror). Median of ${RUNS} cold installs._"
