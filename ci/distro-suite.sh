#!/bin/sh
# Exercise a *released* uvr binary in one Linux distribution.
#
# Invoked by .github/workflows/distro-suite.yml, once per image in
# crates/uvr-core/tests/distro_matrix.json, via:
#
#   docker run --rm -v "$PWD:/work" -w /work <image> sh ci/distro-suite.sh
#
# `docker run` rather than a `container:` job on purpose: node-based actions
# need a glibc the minimal and musl images don't have, so the checkout happens
# on the host and the tree is mounted in. One shape for every image.
#
# This deliberately does not build uvr here. Users don't cargo build it —
# install.sh drops a release binary onto their machine — so the artifact under
# test is the one that ships, selected by the same libc rule install.sh uses.
# Building from source in each container would paper over exactly the failures
# that matter: a gnu binary whose glibc floor is above what the distro ships,
# or a musl binary picked on a glibc host.
#
# Env:
#   DIST                 directory of release builds (default: ./dist)
#   R_VERSIONS           space-separated R versions to install (default: 4.5.1)
#   BINARY_PKG           package for the binary stage (default: jsonlite)
#   SYSREQS_PKG          package with an external system library (default: xml2)
#   SKIP_STAGES          space-separated stage names to skip (default: none)
#   EXPECT_FAIL_STAGE    stage the matrix records as unsupported here
#   EXPECT_FAIL_MESSAGE  substring uvr must print when it refuses
set -eu

DIST="${DIST:-./dist}"
R_VERSIONS="${R_VERSIONS:-4.5.1}"
# Two packages, because the two stages ask different questions and only one of
# them is about system libraries.
#
# The binary stage asks "did uvr pick a repo whose binaries run here", so it
# needs a package P3M builds for every repo. The sysreqs stage asks "did uvr
# resolve this distro's *devel* package", so it needs one with an external
# system library whose headers the image deliberately lacks.
#
# Sharing one package made the binary stage depend on Posit's per-package build
# queue, which is not a property of the distro and not something uvr controls:
# P3M's Linux `PACKAGES` index lists every package whether or not a binary
# exists, and the binary-vs-source choice is made at download time from the R
# User-Agent. A recently released version can therefore be indexed everywhere
# and built only for the busiest repos — xml2 1.6.0 (2026-06-22) has a jammy
# binary and serves *source* on opensuse156, bookworm, bullseye, focal and
# centos7. jsonlite is what ci.yml already uses for this: small, compiled
# component, and built for every repo in the matrix.
BINARY_PKG="${BINARY_PKG:-jsonlite}"
SYSREQS_PKG="${SYSREQS_PKG:-xml2}"
SKIP_STAGES="${SKIP_STAGES:-}"
EXPECT_FAIL_STAGE="${EXPECT_FAIL_STAGE:-}"
EXPECT_FAIL_MESSAGE="${EXPECT_FAIL_MESSAGE:-}"
R_LATEST="${R_VERSIONS##* }"
# Where the tree is mounted; stages cd into scratch projects and back.
ROOT="$(pwd)"

# Resolve DIST against ROOT now, while we are still standing in it. Every stage
# below cds into a scratch project under /tmp before invoking $UVR, and a
# relative ./dist stops resolving the moment it does — the shell then reports
# the binary as "not found" (exit 127), which reads like a missing artifact
# rather than a missing directory.
case "$DIST" in
    /*) ;;
    *) DIST="$ROOT/${DIST#./}" ;;
esac

# ---------------------------------------------------------------- helpers ---

group() { printf '::group::%s\n' "$*"; }
endgroup() { printf '::endgroup::\n'; }
note() { printf '\n>>> %s\n' "$*"; }
fail() { printf '\n!!! FAIL: %s\n' "$*" >&2; exit 1; }

skipped() {
    for s in $SKIP_STAGES; do
        [ "$s" = "$1" ] && return 0
    done
    return 1
}

# A stage the matrix records as unsupported on this distro. The expectation is
# asserted rather than skipped: skipping hides a limitation, asserting keeps it
# visible *and* checked, so the lane turns red if uvr stops refusing, or starts
# refusing for a different reason. A permanently-red job nobody reads is how a
# real failure hides, and a silently-skipped one is how a fixed limitation goes
# unnoticed.
expect_fail() { [ "$EXPECT_FAIL_STAGE" = "$1" ]; }

# An expectation with no message would match *any* failure, quietly turning the
# strongest assertion in the suite into "this distro may fail for any reason".
# Refuse it at the top rather than pass a distro for the wrong reason.
if [ -n "$EXPECT_FAIL_STAGE" ] && [ -z "$EXPECT_FAIL_MESSAGE" ]; then
    fail "expect.stage=$EXPECT_FAIL_STAGE with no expect.message: an expectation
       that matches any failure is not an assertion. Add the substring uvr must
       print to this entry in distro_matrix.json."
fi

# --------------------------------------------------------- distro plumbing ---

# The one place that knows package-manager dialects. Adding a distro to the
# matrix must not mean adding a line of YAML — if its package manager is
# already known here, the image just works.
detect_pm() {
    for pm in apt-get dnf microdnf zypper apk pacman yum; do
        if command -v "$pm" >/dev/null 2>&1; then
            echo "$pm"
            return 0
        fi
    done
    fail "no supported package manager found in this image"
}

PM="$(detect_pm)"
note "package manager: $PM"

pm_install() {
    case "$PM" in
        apt-get) DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends "$@" ;;
        dnf|yum) "$PM" -y install "$@" ;;
        microdnf) microdnf -y install "$@" ;;
        zypper) zypper --non-interactive --gpg-auto-import-keys install -y "$@" ;;
        apk) apk add --no-cache "$@" ;;
        pacman) pacman -S --noconfirm --needed "$@" ;;
    esac
}

pm_refresh() {
    case "$PM" in
        apt-get)
            # Debian 11 left LTS on 2026-08-31. deb.debian.org still indexes
            # bullseye-security but serves 404 for its packages, and dropping
            # the suite does not help: the image already carries some of those
            # updates, so the base suite cannot satisfy their dependents
            # (#268). The image names the snapshot.debian.org copy of the
            # security archive it was built from; switch to that. The snapshot
            # Release file is past its Valid-Until, so every later apt run
            # (uvr's sysreqs install included) needs the check off.
            if grep -qs '^VERSION_CODENAME=bullseye' /etc/os-release \
                && grep -qs '^# deb http://snapshot.debian.org/archive/debian-security/' /etc/apt/sources.list; then
                sed -i \
                    -e 's|^deb http://deb.debian.org/debian-security bullseye-security|# &|' \
                    -e 's|^# \(deb http://snapshot.debian.org/archive/debian-security/[0-9TZ]* bullseye-security\)|\1|' \
                    /etc/apt/sources.list
                echo 'Acquire::Check-Valid-Until "false";' > /etc/apt/apt.conf.d/99-uvr-snapshot
            fi
            apt-get update ;;
        zypper) zypper --non-interactive --gpg-auto-import-keys refresh ;;
        pacman) pacman -Sy --noconfirm ;;
        *) : ;;
    esac
}

# Runtime only — what a user has before they compile anything:
#   ca-certificates                TLS to the CDN and CRAN
#   fontconfig (+ a font on musl)  R probes these at startup
#   which                          R's `utils` calls system(which ...) in
#                                  .onLoad and fails to load without it; the
#                                  minimal RPM images ship no `which`, Debian
#                                  and busybox do
#   libxml2 runtime                load SYSREQS_PKG once it is built; only its
#                                  *headers* are withheld, which is the point
# Deliberately absent: a compiler, and libxml2's *headers*. The stages below
# run on a bare image precisely to prove the shipped binary needs neither.
runtime_prereqs() {
    case "$PM" in
        apt-get) echo "ca-certificates fontconfig libxml2" ;;
        dnf|yum|microdnf|pacman) echo "ca-certificates fontconfig libxml2 which" ;;
        zypper) echo "ca-certificates fontconfig libxml2-2 which" ;;
        apk) echo "ca-certificates fontconfig ttf-dejavu libxml2" ;;
    esac
}

# Only for the source-build stage: R compiles C for xml2.
build_prereqs() {
    case "$PM" in
        apt-get) echo "build-essential pkg-config" ;;
        dnf|yum|microdnf) echo "gcc gcc-c++ make pkgconf-pkg-config" ;;
        zypper) echo "gcc gcc-c++ make pkg-config" ;;
        # build-base does not pull pkgconf on Alpine, and R's anticonf
        # configure scripts need it to find libxml-2.0.
        apk) echo "build-base musl-dev pkgconf" ;;
        pacman) echo "gcc make pkgconf" ;;
    esac
}

# The tarball tools are requested by *command*, not by package: the RHEL images
# ship curl-minimal, and asking for `curl` there is a package conflict rather
# than a no-op. Only name a package when its command is genuinely absent.
tools() {
    for pair in "tar tar" "gzip gzip" "xz $(xz_pkg)"; do
        cmd=${pair%% *}
        pkg=${pair#* }
        command -v "$cmd" >/dev/null 2>&1 || printf '%s ' "$pkg"
    done
}

xz_pkg() {
    case "$PM" in
        apt-get) echo "xz-utils" ;;
        *) echo "xz" ;;
    esac
}

# uvr's auto-installer knows apk/dnf/apt-get (sync.rs::pick_sysreqs_installer)
# and nothing else — its last branch is an unconditional apt-get, so a host
# without one of those three gets a command that isn't there. That includes
# yum-only images: `dnf` is absent, so uvr falls through to apt-get, which is
# also absent. Those distros assert the *diagnosis* instead of the install.
sysreqs_autoinstall_supported() {
    case "$PM" in
        apt-get|dnf|apk) return 0 ;;
        *) return 1 ;;
    esac
}

# ------------------------------------------------------------- stage: deps ---

if ! skipped prereqs; then
    group "Install runtime prerequisites ($PM)"
    pm_refresh
    # shellcheck disable=SC2046  # deliberate word splitting: a package list
    pm_install $(runtime_prereqs) $(tools)
    endgroup
fi

# ------------------------------------------------- stage: pick the artifact ---

# The same rule install.sh applies, so the matrix exercises the binary a user
# on this distro would actually receive — including the choice itself.
libc=gnu
if command -v ldd >/dev/null 2>&1 && ldd --version 2>&1 | grep -qi musl; then
    libc=musl
elif [ -f /etc/alpine-release ]; then
    libc=musl
fi
arch="$(uname -m)"
[ "$arch" = arm64 ] && arch=aarch64
TARGET="${arch}-unknown-linux-${libc}"
UVR="$DIST/uvr-$TARGET/uvr"

note "install.sh would fetch: uvr-$TARGET.tar.gz"
[ -x "$UVR" ] || fail "no release build for $TARGET under $DIST"

# Whether the portable R builds can run here at all.
#
# Posit publishes them as manylinux_2_34, so a glibc host below 2.34 gets no R
# — `MANYLINUX_GLIBC_MIN` in r_version/downloader.rs, and a different number
# from both the 2.28 floor the uvr *binary* is built against (#212) and the
# 2.28 `PPM_MANYLINUX_GLIBC_MIN` that gates binary *packages*. Three floors,
# three meanings; only this one decides whether `uvr r install` can work.
#
# Derived from the host, deliberately. This used to be an `expect` block on
# each entry, which meant it was only as complete as whoever remembered to add
# one — and it was not: rocky-8, almalinux-8, oracle-8 and opensuse-155 are all
# below the floor, none of them carried the block, and all four failed the
# first nightly after the pr lane went green (#230). The floor is a property of
# the host, so the host is what gets asked.
R_GLIBC_FLOOR=2.34
# Set by below_r_glibc_floor; reported by the stage below.
hv=""

below_r_glibc_floor() {
    # musllinux builds carry no glibc floor; Alpine is judged on its own terms.
    if [ "$libc" = musl ]; then return 1; fi
    hv="$(ldd --version 2>&1 | grep -oE '[0-9]+\.[0-9]+' | head -1)"
    # An unreadable ldd must not silently excuse a distro. Expect success and
    # let the stage fail loudly if that was wrong.
    if [ -z "$hv" ]; then return 1; fi
    hmaj="${hv%%.*}"; hmin="${hv#*.}"; hmin="${hmin%%.*}"
    fmaj="${R_GLIBC_FLOOR%%.*}"; fmin="${R_GLIBC_FLOOR#*.}"
    if [ "$hmaj" -lt "$fmaj" ]; then return 0; fi
    if [ "$hmaj" -eq "$fmaj" ] && [ "$hmin" -lt "$fmin" ]; then return 0; fi
    return 1
}

# `--version` on a bare image is the whole glibc-floor question in one line: a
# gnu binary linked against a newer glibc than this distro ships dies right
# here, which is what a user hits seconds after running install.sh.
group "uvr --version ($TARGET)"
"$UVR" --version || fail "the $TARGET release binary does not run on this distro"
"$UVR" --help > /dev/null
endgroup

group "uvr doctor"
"$UVR" doctor || true
endgroup

# --------------------------------------------------------------- stage: R ---

if ! skipped r && below_r_glibc_floor; then
    # This host cannot run the portable R builds. Assert the refusal rather than
    # skipping the stage: skipping hides the limitation, and a permanently red
    # job nobody reads is how a real failure hides. Asserting keeps it visible
    # *and* checked, in both directions.
    group "Install R ($R_LATEST) — glibc $hv is below the $R_GLIBC_FLOOR floor"
    if out="$("$UVR" r install "$R_LATEST" 2>&1)"; then
        printf '%s\n' "$out"
        fail "uvr installed R on glibc $hv, below the $R_GLIBC_FLOOR floor the
       portable builds need. If Posit started publishing lower, update
       R_GLIBC_FLOOR here and MANYLINUX_GLIBC_MIN in downloader.rs together."
    fi
    printf '%s\n' "$out"
    printf '%s\n' "$out" | grep -qF "too old for portable R builds" || fail \
        "uvr refused to install R, but not for the documented reason.
       On a host below the floor it must say so in terms a user can act on;
       this is #212's message regressing into a bare linker error."
    endgroup
    note "expected failure confirmed: glibc $hv < $R_GLIBC_FLOOR"
    # Every stage below needs a working R, so there is nothing further to ask
    # this distro. Stopping here is the answer, not a truncation.
    note "distro suite complete (no portable R below glibc $R_GLIBC_FLOOR)"
    exit 0
elif ! skipped r; then
    group "Install R ($R_VERSIONS)"
    for v in $R_VERSIONS; do
        "$UVR" r install "$v"
    done
    "$UVR" r list --all
    endgroup
fi

# ---------------------------------------------------- stage: binary package ---

# #175: a binary from the wrong distro's repo installs happily and only fails
# at library() — so the assertion has to load the package, not just install it.
# There is still no compiler on this image, so a "binary" install that quietly
# falls back to a source build fails here instead of passing for the wrong
# reason.
#
# musl hosts sit this one out: P3M publishes no musl repo and the portable
# manylinux fallback needs glibc, so there is no binary to select and `uvr add`
# correctly compiles from source — which is the *next* stage's subject, on an
# image that by then has a compiler. Alpine's coverage is the sysreqs path
# (#30), not this one.
if [ "$libc" = musl ]; then
    note "skipping the binary stage: no binary repo exists for musl"
elif ! skipped binary; then
    group "Binary package install ($BINARY_PKG)"
    work=/tmp/binary-smoke
    rm -rf "$work" && mkdir -p "$work" && cd "$work"
    "$UVR" init --here binary-smoke --r-version "$R_LATEST"
    printf 'library(%s); cat("binary-smoke-ok\\n")\n' "$BINARY_PKG" > check.R
    "$UVR" add "$BINARY_PKG"
    "$UVR" run check.R
    cd "$ROOT"
    endgroup
fi

# --------------------------------------------------------- stage: sysreqs ---

# The #209 regression test, at the real end of the chain.
#
# The image has libxml2's runtime library but not its headers, so a source
# build of xml2 cannot succeed unless uvr resolves this distro's devel package
# and installs it. Nothing here greps a warning string: the build either
# compiles or it does not, and it only compiles if every link — os-release
# parse, catalog naming, package-manager probe, installer dialect — is right
# for this distro.
#
# Which is exactly what #209 broke: RHEL asked the catalog under a name it does
# not publish, got nothing back, installed nothing, and the build failed. A
# pre-fix binary fails this stage with `libxml/tree.h: No such file or
# directory` having printed no warning at all — which is why the assertion is
# "the build works" rather than "the warning is absent".
if ! skipped sysreqs; then
    group "System dependency resolution ($SYSREQS_PKG from source)"
    # shellcheck disable=SC2046  # deliberate word splitting: a package list
    pm_install $(build_prereqs)

    work=/tmp/sysreqs-smoke
    rm -rf "$work" && mkdir -p "$work" && cd "$work"
    "$UVR" init --here sysreqs-smoke --r-version "$R_LATEST"
    printf 'library(%s); cat("sysreqs-smoke-ok\\n")\n' "$SYSREQS_PKG" > check.R

    if expect_fail sysreqs; then
        # This host has a working toolchain that still cannot build R packages,
        # because R's own Makeconf asks for a C standard its compiler predates.
        # Asserting it keeps the limitation visible and checked; the sysreqs
        # half is still verified by the common check below, which is the point
        # — uvr resolves and installs the system dependency correctly here, and
        # only the compile step fails.
        if UVR_INSTALL_SYSREQS=1 "$UVR" add "$SYSREQS_PKG" --no-binary > add.log 2>&1; then
            cat add.log
            fail "the source build succeeded, but the matrix records this distro as
       unable to compile R packages. If the toolchain moved or R's recorded
       C standard changed, drop the 'expect' block from this entry."
        fi
        cat add.log
        grep -qF "$EXPECT_FAIL_MESSAGE" add.log || fail \
            "the source build failed, but not for the documented reason.
       wanted: $EXPECT_FAIL_MESSAGE
       Something else is broken here, or the known breakage has changed shape."
        note "expected failure confirmed: $EXPECT_FAIL_MESSAGE"
    elif sysreqs_autoinstall_supported; then
        UVR_INSTALL_SYSREQS=1 "$UVR" add "$SYSREQS_PKG" --no-binary 2>&1 | tee add.log
        "$UVR" run check.R
    else
        # zypper/pacman: uvr cannot run the install itself, so assert it at
        # least named the package a human would then install.
        "$UVR" add "$SYSREQS_PKG" --no-binary > add.log 2>&1 || true
        grep -Eq 'libxml2-dev(el)?' add.log \
            || fail "uvr did not name libxml2's devel package; see add.log"
    fi

    grep -q 'System dependency check skipped' add.log \
        && fail "uvr skipped the sysreqs check on this distro (#209 shape)"

    cd "$ROOT"
    endgroup
fi

note "distro suite complete"
