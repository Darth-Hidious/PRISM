#!/usr/bin/env bash
set -euo pipefail

# PRISM installer — downloads the Rust CLI binary for your platform.
# Usage: curl -fsSL https://prism.marc27.com/install.sh | bash
#
# Env vars:
#   PRISM_VERSION      — version tag (default: latest)
#   PRISM_INSTALL_DIR  — install directory (default: ~/.prism/bin)

VERSION="${PRISM_VERSION:-latest}"
INSTALL_DIR="${PRISM_INSTALL_DIR:-$HOME/.prism/bin}"
REPO="Darth-Hidious/PRISM"

# --- Detect platform ---
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
    Linux)   PLATFORM="linux" ;;
    Darwin)  PLATFORM="macos" ;;
    MINGW*|MSYS*|CYGWIN*) PLATFORM="windows" ;;
    *)       echo "Error: Unsupported OS: $OS" >&2; exit 1 ;;
esac

case "$ARCH" in
    x86_64|amd64)    ARCH="x86_64" ;;
    aarch64|arm64)   ARCH="aarch64" ;;
    *)               echo "Error: Unsupported architecture: $ARCH" >&2; exit 1 ;;
esac

if [ "$PLATFORM" = "windows" ]; then
    ARCHIVE="prism-windows-${ARCH}.zip"
else
    ARCHIVE="prism-${PLATFORM}-${ARCH}.tar.gz"
fi

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

# --- Download ---
URL="https://github.com/${REPO}/releases/download/${VERSION}/${ARCHIVE}"
TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

echo "Downloading ${URL}..."
if ! curl -fSL "$URL" -o "${TMPDIR}/${ARCHIVE}"; then
    echo "Error: Download failed." >&2
    echo "Check that ${VERSION} has a release for ${PLATFORM}-${ARCH}." >&2
    echo "Available at: https://github.com/${REPO}/releases" >&2
    exit 1
fi

# --- Extract ---
echo "Extracting to ${INSTALL_DIR}..."
mkdir -p "$INSTALL_DIR"

if [ "$PLATFORM" = "windows" ]; then
    unzip -o "${TMPDIR}/${ARCHIVE}" -d "$INSTALL_DIR"
else
    tar -xzf "${TMPDIR}/${ARCHIVE}" -C "$INSTALL_DIR"
fi

chmod +x "${INSTALL_DIR}/prism" 2>/dev/null || true
chmod +x "${INSTALL_DIR}/prism-node" 2>/dev/null || true

# --- macOS: handle code signing and Gatekeeper ---
if [ "$PLATFORM" = "macos" ]; then
    echo "Configuring macOS security..."

    for bin in prism prism-node; do
        BIN_PATH="${INSTALL_DIR}/${bin}"
        [ -f "$BIN_PATH" ] || continue

        # 1. Remove quarantine attributes (browser downloads add these)
        xattr -d com.apple.quarantine "$BIN_PATH" 2>/dev/null || true
        xattr -d com.apple.provenance "$BIN_PATH" 2>/dev/null || true

        # 2. Ad-hoc sign if not already signed (GitHub releases are unsigned,
        #    cargo build linker-signs automatically on ARM64, but CI builds may not be)
        if ! codesign -v "$BIN_PATH" 2>/dev/null; then
            codesign -s - -f "$BIN_PATH" 2>/dev/null || true
        fi
    done

    echo "  Binaries signed and quarantine cleared."
fi

# --- Linux: ensure executable ---
if [ "$PLATFORM" = "linux" ]; then
    chmod +x "${INSTALL_DIR}/prism" "${INSTALL_DIR}/prism-node" 2>/dev/null || true
fi

# --- Setup Python venv for tools ---
VENV_DIR="$HOME/.prism/venv"
if [ ! -d "$VENV_DIR" ]; then
    echo "Setting up Python environment..."
    PYTHON=""
    for py in python3.14 python3.13 python3.12 python3.11 python3; do
        if command -v "$py" >/dev/null 2>&1; then
            PYTHON="$py"
            break
        fi
    done

    if [ -n "$PYTHON" ]; then
        "$PYTHON" -m venv "$VENV_DIR" 2>/dev/null || true
        if [ -f "$VENV_DIR/bin/pip" ]; then
            "$VENV_DIR/bin/pip" install -q --upgrade pip 2>/dev/null || true
        fi
    else
        echo "  Warning: No Python 3 found. Some tools will be unavailable."
    fi
fi

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
fi

# --- Create config directory ---
mkdir -p "$HOME/.prism"

# --- Done ---
echo ""
echo "PRISM ${VERSION} installed successfully!"
echo ""
echo "  prism            Launch the interactive TUI"
echo "  prism login      Authenticate with MARC27"
echo "  prism setup      First-time setup"
echo "  prism --help     See all commands"
echo ""

# Verify install
if command -v prism >/dev/null 2>&1; then
    echo "Verified: $(prism --version 2>/dev/null || echo 'installed')"
else
    echo "Note: Run 'source ${RC_FILE}' or open a new terminal to use prism."
fi
