#!/usr/bin/env bash
#
# Convenience wrapper for the reproducible Docker bench (#40).
# Builds benchmarks/Dockerfile and runs it, mounting benchmarks/out/
# for results.
#
# Usage:
#   bash benchmarks/run-in-docker.sh                # default 4.5.3 / 2025-04-01
#   R_VERSION=4.6.0 bash benchmarks/run-in-docker.sh
#   PPM_SNAPSHOT=2025-05-01 BENCH_RUNS=3 bash benchmarks/run-in-docker.sh
#
# The CI workflow at .github/workflows/benchmark.yml runs the same image
# on a clean ubuntu-latest runner — this script reproduces those numbers
# locally.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

R_VERSION="${R_VERSION:-4.5.3}"
PPM_SNAPSHOT="${PPM_SNAPSHOT:-2025-04-01}"
BENCH_RUNS="${BENCH_RUNS:-5}"

if ! command -v docker >/dev/null; then
    echo "error: docker not on PATH. Install Docker Desktop or the docker CLI." >&2
    exit 1
fi

mkdir -p benchmarks/out

echo "Building uvr-bench image (R ${R_VERSION}, PPM ${PPM_SNAPSHOT})..."
docker build \
    --build-arg "R_VERSION=${R_VERSION}" \
    --build-arg "PPM_SNAPSHOT=${PPM_SNAPSHOT}" \
    -t "uvr-bench:r${R_VERSION}-${PPM_SNAPSHOT}" \
    -f benchmarks/Dockerfile \
    .

echo ""
echo "Running benchmark (BENCH_RUNS=${BENCH_RUNS})..."
docker run --rm \
    -e "BENCH_RUNS=${BENCH_RUNS}" \
    -v "${REPO_ROOT}/benchmarks/out:/out" \
    "uvr-bench:r${R_VERSION}-${PPM_SNAPSHOT}"

echo ""
echo "Results:"
echo "  benchmarks/out/bench-results.json"
echo "  benchmarks/out/bench.log"
