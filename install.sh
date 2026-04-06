#!/usr/bin/env bash
set -euo pipefail

# PRISM installer — downloads the Rust CLI binary.
# Usage: curl -fsSL https://prism.marc27.com/install.sh | bash

VERSION="${PRISM_VERSION:-latest}"
INSTALL_DIR="${PRISM_INSTALL_DIR:-$HOME/.prism/bin}"
REPO="Darth-Hidious/PRISM"

# --- Detect platform ---
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

case "$OS" in
    linux)  PLATFORM="linux" ;;
    darwin) PLATFORM="macos" ;;
    *)      echo "Error: Unsupported OS: $OS" >&2; exit 1 ;;
esac

case "$ARCH" in
    x86_64|amd64)   ARCH="x86_64" ;;
    aarch64|arm64)   ARCH="aarch64" ;;
    *)               echo "Error: Unsupported architecture: $ARCH" >&2; exit 1 ;;
esac

ARCHIVE="prism-${PLATFORM}-${ARCH}.tar.gz"

# --- Resolve version ---
if [ "$VERSION" = "latest" ]; then
    echo "Fetching latest release..."
    VERSION=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
        | grep '"tag_name"' | head -1 | cut -d'"' -f4)
    if [ -z "$VERSION" ]; then
        echo "Error: Failed to fetch latest version from GitHub" >&2
        exit 1
    fi
fi

echo "Installing PRISM ${VERSION} for ${PLATFORM}-${ARCH}..."

# --- Download and extract ---
URL="https://github.com/${REPO}/releases/download/${VERSION}/${ARCHIVE}"
TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

echo "Downloading ${URL}..."
if ! curl -fSL "$URL" -o "${TMPDIR}/${ARCHIVE}"; then
    echo "Error: Download failed. Check that ${VERSION} has a release for ${PLATFORM}-${ARCH}." >&2
    exit 1
fi

echo "Extracting to ${INSTALL_DIR}..."
mkdir -p "$INSTALL_DIR"
tar -xzf "${TMPDIR}/${ARCHIVE}" -C "$INSTALL_DIR"
chmod +x "${INSTALL_DIR}/prism" 2>/dev/null || true
chmod +x "${INSTALL_DIR}/prism-node" 2>/dev/null || true
chmod +x "${INSTALL_DIR}/prism-tui" 2>/dev/null || true

# --- Add to PATH ---
SHELL_NAME="$(basename "${SHELL:-bash}")"
case "$SHELL_NAME" in
    zsh)  RC_FILE="$HOME/.zshrc" ;;
    fish) RC_FILE="$HOME/.config/fish/config.fish" ;;
    *)    RC_FILE="$HOME/.bashrc" ;;
esac

if ! echo "$PATH" | tr ':' '\n' | grep -qx "$INSTALL_DIR"; then
    if [ "$SHELL_NAME" = "fish" ]; then
        echo "fish_add_path $INSTALL_DIR" >> "$RC_FILE"
    else
        echo "export PATH=\"$INSTALL_DIR:\$PATH\"" >> "$RC_FILE"
    fi
    echo "Added ${INSTALL_DIR} to PATH in ${RC_FILE}"
    echo "Run: source ${RC_FILE}  (or open a new terminal)"
fi

echo ""
echo "PRISM ${VERSION} installed successfully!"
echo ""
echo "  prism login     Authenticate with MARC27"
echo "  prism --help    See all commands"
echo ""
