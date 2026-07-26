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

# --- Check the Python prerequisite ---
#
# We deliberately do NOT create the venv here. `prism` provisions
# ~/.prism/venv itself on every launch (crates/python-bridge/src/venv.rs
# ensure_venv, called from crates/cli/src/main.rs) and that implementation
# is strictly better than a shell copy of it: it self-heals a pipless venv,
# falls back to `uv python find`, installs the version-matched wheel with a
# git fallback, and re-verifies `import app` rather than trusting that a
# directory exists. Duplicating it here only created a second thing to keep
# in sync. The installer's job is to make sure the PREREQUISITE is present
# and to say so plainly if it is not.
#
# The floor is Python 3.11 (pyproject requires-python = ">=3.11"). This is
# not optional: with anything older, `prism` exits immediately with
# "No Python 3.11+ found" — so a silent pass here would be a lying check.
PYTHON_OK=""
for py in python3.14 python3.13 python3.12 python3.11 python3; do
    command -v "$py" >/dev/null 2>&1 || continue
    if "$py" -c 'import sys; sys.exit(0 if sys.version_info >= (3, 11) else 1)' 2>/dev/null; then
        PYTHON_OK="$py"
        break
    fi
done

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

# --- Stage 1 done: the app itself is installed and runnable ---
echo ""
echo "[1/2] PRISM ${VERSION} installed — $("${INSTALL_DIR}/prism" --version 2>/dev/null || echo 'binary in place')"

# --- Stage 2: the Python tool platform ---
#
# Provisioning is the binary's job (ensure_venv runs on every `prism`
# invocation), so we trigger it once here with `prism doctor`. That both
# builds ~/.prism/venv while the user is still watching AND leaves them
# looking at the component status tree, which is the surface they should
# come back to. Re-running this installer resumes: ensure_venv's fast path
# returns immediately once the venv has a working `import app`.
if [ -z "$PYTHON_OK" ]; then
    echo ""
    echo "[2/2] SKIPPED — no Python 3.11+ on this machine."
    echo ""
    echo "  PRISM will not start without it: the agent runs its tools in a"
    echo "  Python worker and exits with 'No Python 3.11+ found' otherwise."
    echo ""
    case "$PLATFORM" in
        macos) echo "    brew install python@3.12" ;;
        *)     echo "    sudo apt-get install -y python3.12 python3.12-venv    # Debian/Ubuntu"
               echo "    sudo dnf install -y python3.12                        # Fedora/RHEL" ;;
    esac
    echo ""
    echo "  Then re-run:  curl -fsSL https://prism.marc27.com/install.sh | bash"
elif [ "${PRISM_SKIP_TOOLS:-0}" = "1" ]; then
    echo "[2/2] SKIPPED — PRISM_SKIP_TOOLS=1. Tools install on your first \`prism\` run."
else
    echo "[2/2] Setting up the Python tool platform (first run takes a few minutes)..."
    echo ""
    # Never fail the install over this — the binary retries on every launch.
    "${INSTALL_DIR}/prism" doctor 2>&1 || {
        echo ""
        echo "  Note: setup did not finish cleanly. It retries automatically on your"
        echo "  next \`prism\` run; \`prism doctor\` shows what is still missing."
    }
fi

# --- Done ---
echo ""
echo "  prism            Launch the interactive chat"
echo "  prism login      Authenticate with MARC27"
echo "  prism doctor     Re-check local + platform health"
echo "  prism --help     See all commands"
echo ""

if ! command -v prism >/dev/null 2>&1; then
    echo "Run 'source ${RC_FILE}' or open a new terminal to use prism."
fi
