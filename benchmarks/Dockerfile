# Reproducible benchmark environment for uvr.
#
# Pinned R version + pinned Linux base + pinned R-package versions
# (pak / renv installed from a fixed PPM snapshot) + uvr built from
# source at a known commit. CI runs and local re-builds against the same
# Dockerfile produce comparable numbers.
#
# Addresses #40 (BjarkeHautop): "renv looks unreasonably fast vs
# install.packages() and pak — I want a Dockerfile so I can debug it."
#
# Build:
#   docker build -t uvr-bench -f benchmarks/Dockerfile .
# Run:
#   docker run --rm -v "$PWD/benchmarks/out:/out" uvr-bench
# Output:
#   benchmarks/out/bench-results.json — same shape as bench.sh's local output
#
# Override pinned versions:
#   docker build --build-arg R_VERSION=4.6.0 --build-arg PPM_SNAPSHOT=2025-05-01 ...

ARG R_VERSION=4.5.3

# ─── Stage 1: build the uvr binary ──────────────────────────────────────────
# Pinned to an exact patch (1.83.0) so the binary is byte-reproducible
# across rebuilds on the same uvr commit. The `1.83-slim-bookworm` tag is
# a moving alias for "latest 1.83.x" — using `1.83.0` instead pins the
# patch as well as the minor.
# Builds against musl so the resulting binary is portable across Linux
# distros and matches what we ship as
# `uvr-x86_64-unknown-linux-musl.tar.gz` in release artifacts.
FROM rust:1.83.0-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config musl-tools \
 && rustup target add x86_64-unknown-linux-musl \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /src
# Copy the workspace lazily to keep cargo's incremental cache useful across
# rebuilds when only bench scripts change.
COPY Cargo.toml Cargo.lock ./
COPY crates/ ./crates/
RUN cargo build --release --target x86_64-unknown-linux-musl -p uvr

# ─── Stage 2: bench environment ────────────────────────────────────────────
# rocker/r-ver pins R to a specific patch release (e.g. 4.5.3) on
# debian:bookworm. Tagged images are immutable, so the same Dockerfile on
# the same uvr commit produces the same R version reproducibly.
FROM rocker/r-ver:${R_VERSION}

ENV DEBIAN_FRONTEND=noninteractive

# System tools the bench needs:
#   - `time` for /usr/bin/time -p (bench.sh hard-codes this path)
#   - `bash` (rocker has /bin/sh as dash; bench needs bash arrays)
#   - compile toolchain so the source-install fallback works for renv /
#     install.packages when PPM doesn't have a binary
RUN apt-get update && apt-get install -y --no-install-recommends \
    bash time curl ca-certificates git \
    build-essential gfortran \
    libcurl4-openssl-dev libssl-dev libxml2-dev libgit2-dev \
    libfontconfig1-dev libfreetype6-dev libpng-dev libtiff5-dev libjpeg-dev \
    libharfbuzz-dev libfribidi-dev \
 && rm -rf /var/lib/apt/lists/*

# Pin the PPM snapshot used as the CRAN mirror so the version of every R
# package the bench resolves stays stable across runs. Bumping
# PPM_SNAPSHOT is an explicit "we're updating the comparison baseline"
# act, not something that drifts under us.
ARG PPM_SNAPSHOT=2025-04-01
# Snapshot retention note: PPM keeps dated snapshots for an extended (but
# not contractually committed) window. If the snapshot date is ever GC'd,
# this layer's `install.packages` returns HTTP 404 and the image build
# fails LOUDLY — preferred over silent version drift. Bumping the
# PPM_SNAPSHOT arg is an explicit "we're updating the comparison
# baseline" act, not something that drifts under us.
RUN printf 'options(repos = c(CRAN = "https://packagemanager.posit.co/cran/__linux__/bookworm/%s"))\n' \
        "${PPM_SNAPSHOT}" >> /usr/local/lib/R/etc/Rprofile.site \
 && R -e "install.packages(c('pak','renv'), Ncpus = parallel::detectCores())"

# Make the system library reachable from `uvr run` (#40). uvr's run
# isolation sets `R_LIBS_SITE=""` so the project library can't bleed in
# from system-wide installs — but that also hides pak and renv (which
# we *need* for the bench's tool-detection block). UVR_EXTRA_LIBS gets
# appended to R_LIBS_USER inside `uvr run`, restoring visibility for
# this controlled environment without un-isolating in general.
ENV UVR_EXTRA_LIBS=/usr/local/lib/R/site-library

# Pull the freshly-built uvr binary from the builder stage. The bench
# tests THIS binary — exactly what a tagged release would produce for
# the linux-x86_64-musl target.
COPY --from=builder /src/target/x86_64-unknown-linux-musl/release/uvr /usr/local/bin/uvr
RUN chmod +x /usr/local/bin/uvr && uvr --version

# Bench inputs. Copy late so iterating on the bench script doesn't
# invalidate the heavy R-deps layer. /bench is made world-writable so
# the container can be safely run via `--user $(id -u):$(id -g)` from
# run-in-docker.sh — bench.sh writes bench-results.json into the
# WORKDIR before the entrypoint copies it to /out.
WORKDIR /bench
COPY benchmarks/bench.sh benchmarks/uvr-*.toml ./
RUN chmod +x bench.sh && chmod 0777 /bench

# Mount /out from the host to capture results.
VOLUME ["/out"]
ENV BENCH_RUNS=5

# Tee bench output to /out/bench.log too so a CI failure has both the
# JSON (if reached) and the raw run log to debug from. Files written
# from the container as root would otherwise be unreadable by the host
# user — chmod 666 the outputs so a subsequent local `run-in-docker.sh`
# can overwrite them without sudo.
ENTRYPOINT ["/bin/bash", "-c", "\
    bash bench.sh 2>&1 | tee /out/bench.log; \
    cp -f bench-results.json /out/bench-results.json 2>/dev/null || true; \
    chmod -f 666 /out/bench-results.json /out/bench.log 2>/dev/null || true \
"]
