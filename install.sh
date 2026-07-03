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
#
# Extract to a staging dir inside TMPDIR first, then move ONLY the
# expected binary names to INSTALL_DIR. This stops a malicious or
# malformed archive from:
#   1. Writing outside INSTALL_DIR via `../` paths or absolute paths
#      (BSD tar on older macOS doesn't reject these by default).
#   2. Dropping arbitrary files into INSTALL_DIR alongside the
#      expected binaries (e.g. an extra .sh that gets sourced by
#      a careless user later).
echo "Extracting to ${INSTALL_DIR}..."
mkdir -p "$INSTALL_DIR"
STAGE="${TMPDIR}/stage"
mkdir -p "$STAGE"

if [ "$PLATFORM" = "windows" ]; then
    unzip -o "${TMPDIR}/${ARCHIVE}" -d "$STAGE"
else
    tar -xzf "${TMPDIR}/${ARCHIVE}" -C "$STAGE"
fi

# Move ONLY the expected binaries — anything else in the archive is
# silently dropped. Add to this list if a future release legitimately
# ships more files.
EXTRACTED_ANY=0
for bin in prism prism-node; do
    if [ -f "${STAGE}/${bin}" ]; then
        mv "${STAGE}/${bin}" "${INSTALL_DIR}/${bin}"
        EXTRACTED_ANY=1
    elif [ "$PLATFORM" = "windows" ] && [ -f "${STAGE}/${bin}.exe" ]; then
        mv "${STAGE}/${bin}.exe" "${INSTALL_DIR}/${bin}.exe"
        EXTRACTED_ANY=1
    fi
done

if [ $EXTRACTED_ANY -eq 0 ]; then
    echo "Error: archive did not contain expected binary (prism)." >&2
    echo "Listing what we got:" >&2
    ls -la "$STAGE" >&2 || true
    exit 1
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

# --- Setup Python venv + install the PRISM tool platform ---
# The binary is self-sufficient for chat; the Python venv provides the
# local tool server. A venv that exists but has no pip (Debian/Ubuntu
# without python3-venv) or no `app` package is BROKEN, not "done" —
# heal it or remove it so a re-run can start clean. Never fail the
# binary install over Python; degrade with honest, actionable messages.
VENV_DIR="$HOME/.prism/venv"
setup_python_tools() {
    PYTHON=""
    for py in python3.14 python3.13 python3.12 python3.11 python3; do
        if command -v "$py" >/dev/null 2>&1; then
            PYTHON="$py"
            break
        fi
    done
    if [ -z "$PYTHON" ]; then
        echo "  Warning: No Python 3 found — local tools disabled (chat still works)."
        echo "  Install python3 + python3-venv, then re-run this installer."
        return 0
    fi

    if [ ! -d "$VENV_DIR" ]; then
        echo "Setting up Python environment..."
        "$PYTHON" -m venv "$VENV_DIR" 2>/dev/null || true
    fi
    # Stock Debian/Ubuntu ships python3 without ensurepip (python3-venv
    # package). `venv` then half-creates: interpreter present, no pip.
    if [ ! -x "$VENV_DIR/bin/python3" ]; then
        "$PYTHON" -m venv --without-pip "$VENV_DIR" 2>/dev/null || true
    fi
    # Heal a pipless venv: ensurepip first, then pypa's get-pip bootstrap
    # (works without python3-venv and without sudo).
    if [ ! -x "$VENV_DIR/bin/pip" ] && [ -x "$VENV_DIR/bin/python3" ]; then
        "$VENV_DIR/bin/python3" -m ensurepip --upgrade >/dev/null 2>&1 || true
    fi
    if [ ! -x "$VENV_DIR/bin/pip" ] && [ -x "$VENV_DIR/bin/python3" ]; then
        curl -fsSL https://bootstrap.pypa.io/get-pip.py \
            | "$VENV_DIR/bin/python3" - --quiet >/dev/null 2>&1 || true
    fi
    if [ ! -x "$VENV_DIR/bin/pip" ]; then
        rm -rf "$VENV_DIR"
        echo "  Warning: could not create a working Python venv (pip unavailable)."
        echo "  On Debian/Ubuntu:  sudo apt-get install -y python3-venv"
        echo "  then re-run:       curl -fsSL https://prism.marc27.com/install.sh | bash"
        return 0
    fi

    # Install the tool platform, pinned to this release. Wheel asset first
    # (no git required); tagged-tree sdist as fallback for older releases.
    if ! "$VENV_DIR/bin/python3" -I -c "import app" >/dev/null 2>&1; then
        echo "Installing PRISM tools (Python) — this can take a few minutes..."
        "$VENV_DIR/bin/pip" install -q --upgrade pip 2>/dev/null || true
        WHEEL_URL="https://github.com/${REPO}/releases/download/${VERSION}/prism_platform-${VERSION#v}-py3-none-any.whl"
        if ! "$VENV_DIR/bin/pip" install -q "prism-platform @ ${WHEEL_URL}"; then
            if ! "$VENV_DIR/bin/pip" install -q "prism-platform @ https://github.com/${REPO}/archive/refs/tags/${VERSION}.tar.gz"; then
                echo "  Warning: Python tools install failed — chat works, local tools disabled."
                echo "  Retry later:  $VENV_DIR/bin/pip install \"prism-platform @ ${WHEEL_URL}\""
                return 0
            fi
        fi
        echo "  PRISM tools installed."
    fi
}
setup_python_tools

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
echo "  prism            Launch the interactive chat"
echo "  prism login      Authenticate with MARC27"
echo "  prism setup      First-time setup"
echo "  prism doctor     Diagnose local + platform health"
echo "  prism --help     See all commands"
echo ""

# Verify install
if command -v prism >/dev/null 2>&1; then
    echo "Verified: $(prism --version 2>/dev/null || echo 'installed')"
else
    echo "Note: Run 'source ${RC_FILE}' or open a new terminal to use prism."
fi
