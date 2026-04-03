#!/bin/sh
# uvr installer — https://github.com/nbafrank/uvr
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/nbafrank/uvr/main/install.sh | sh
#
# Environment variables:
#   UVR_INSTALL_DIR  — where to place the binary (default: ~/.local/bin)
#   UVR_VERSION      — specific version to install (default: latest)

set -eu

REPO="nbafrank/uvr"
INSTALL_DIR="${UVR_INSTALL_DIR:-$HOME/.local/bin}"

main() {
    detect_platform
    resolve_version
    download_and_verify
    install_binary
    print_success
}

detect_platform() {
    OS="$(uname -s)"
    ARCH="$(uname -m)"

    case "$OS" in
        Linux)
            case "$ARCH" in
                x86_64)  TARGET="x86_64-unknown-linux-gnu" ;;
                aarch64) TARGET="aarch64-unknown-linux-gnu" ;;
                arm64)   TARGET="aarch64-unknown-linux-gnu" ;;
                *)       err "Unsupported architecture: $ARCH" ;;
            esac
            ;;
        Darwin)
            case "$ARCH" in
                x86_64)  TARGET="x86_64-apple-darwin" ;;
                arm64)   TARGET="aarch64-apple-darwin" ;;
                aarch64) TARGET="aarch64-apple-darwin" ;;
                *)       err "Unsupported architecture: $ARCH" ;;
            esac
            ;;
        *)
            err "Unsupported OS: $OS. For Windows, download from https://github.com/$REPO/releases/latest"
            ;;
    esac

    ASSET="uvr-${TARGET}.tar.gz"
    log "Detected platform: ${TARGET}"
}

resolve_version() {
    if [ -n "${UVR_VERSION:-}" ]; then
        VERSION="$UVR_VERSION"
    else
        VERSION="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
            | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"//;s/".*//')"
        if [ -z "$VERSION" ]; then
            err "Failed to determine latest version"
        fi
    fi
    log "Installing uvr ${VERSION}"
}

download_and_verify() {
    BASE_URL="https://github.com/$REPO/releases/download/$VERSION"
    TMPDIR="$(mktemp -d)"
    trap 'rm -rf "$TMPDIR"' EXIT

    log "Downloading ${ASSET}..."
    curl -fsSL "${BASE_URL}/${ASSET}" -o "${TMPDIR}/${ASSET}"

    # Verify checksum if sha256sum is available
    if command -v sha256sum >/dev/null 2>&1; then
        log "Verifying checksum..."
        curl -fsSL "${BASE_URL}/sha256sums.txt" -o "${TMPDIR}/sha256sums.txt"
        EXPECTED="$(grep -F "  ${ASSET}" "${TMPDIR}/sha256sums.txt" | head -1 | awk '{print $1}')"
        if [ -n "$EXPECTED" ]; then
            ACTUAL="$(sha256sum "${TMPDIR}/${ASSET}" | awk '{print $1}')"
            if [ "$EXPECTED" != "$ACTUAL" ]; then
                err "Checksum mismatch!\n  Expected: ${EXPECTED}\n  Got:      ${ACTUAL}"
            fi
            log "Checksum verified"
        fi
    elif command -v shasum >/dev/null 2>&1; then
        log "Verifying checksum..."
        curl -fsSL "${BASE_URL}/sha256sums.txt" -o "${TMPDIR}/sha256sums.txt"
        EXPECTED="$(grep -F "  ${ASSET}" "${TMPDIR}/sha256sums.txt" | head -1 | awk '{print $1}')"
        if [ -n "$EXPECTED" ]; then
            ACTUAL="$(shasum -a 256 "${TMPDIR}/${ASSET}" | awk '{print $1}')"
            if [ "$EXPECTED" != "$ACTUAL" ]; then
                err "Checksum mismatch!\n  Expected: ${EXPECTED}\n  Got:      ${ACTUAL}"
            fi
            log "Checksum verified"
        fi
    else
        log "Warning: sha256sum/shasum not found, skipping checksum verification"
    fi

    # Extract
    tar xzf "${TMPDIR}/${ASSET}" -C "${TMPDIR}"
}

install_binary() {
    mkdir -p "$INSTALL_DIR"
    mv "${TMPDIR}/uvr" "${INSTALL_DIR}/uvr"
    chmod +x "${INSTALL_DIR}/uvr"
}

print_success() {
    log "Installed uvr to ${INSTALL_DIR}/uvr"

    # Check if INSTALL_DIR is in PATH
    case ":$PATH:" in
        *":${INSTALL_DIR}:"*) ;;
        *)
            echo ""
            log "Add uvr to your PATH by adding this to your shell profile:"
            echo ""
            echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
            echo ""
            ;;
    esac

    "${INSTALL_DIR}/uvr" --version
}

log() {
    echo "uvr-install: $*" >&2
}

err() {
    log "error: $*"
    exit 1
}

main
