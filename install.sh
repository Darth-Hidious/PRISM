#!/bin/sh
# PRISM One-Command Installer
# Usage: curl -fsSL https://raw.githubusercontent.com/Darth-Hidious/PRISM/main/install.sh | sh
set -e

PACKAGE="prism-platform"
MIN_PYTHON="3.10"

info()  { printf '  \033[1;34m%s\033[0m %s\n' "$1" "$2"; }
ok()    { printf '  \033[1;32m%s\033[0m %s\n' "$1" "$2"; }
warn()  { printf '  \033[1;33m%s\033[0m %s\n' "$1" "$2"; }
err()   { printf '  \033[1;31m%s\033[0m %s\n' "ERROR:" "$1" >&2; exit 1; }

printf '\n\033[1;36mPRISM Installer\033[0m\n'
printf '  Platform for Research in Intelligent Synthesis of Materials\n\n'

# --- Detect OS ---
OS="$(uname -s)"
ARCH="$(uname -m)"
case "$OS" in
    Darwin) info "OS:" "macOS ($ARCH)" ;;
    Linux)  info "OS:" "Linux ($ARCH)" ;;
    *)      err "Unsupported OS: $OS. PRISM supports macOS and Linux." ;;
esac

# --- Find Python >= 3.10 ---
PYTHON=""
for cmd in python3 python; do
    if command -v "$cmd" >/dev/null 2>&1; then
        ver=$("$cmd" -c "import sys; print(f'{sys.version_info.major}.{sys.version_info.minor}')" 2>/dev/null || true)
        if [ -n "$ver" ]; then
            major=$(echo "$ver" | cut -d. -f1)
            minor=$(echo "$ver" | cut -d. -f2)
            if [ "$major" -ge 3 ] && [ "$minor" -ge 10 ]; then
                PYTHON="$cmd"
                break
            fi
        fi
    fi
done

if [ -z "$PYTHON" ]; then
    err "Python >= $MIN_PYTHON is required but not found. Install it from https://python.org"
fi
ok "Python:" "$($PYTHON --version)"

# --- Find pipx or uv ---
INSTALLER=""
if command -v uv >/dev/null 2>&1; then
    INSTALLER="uv"
    ok "Installer:" "uv ($(uv --version 2>/dev/null || echo 'installed'))"
elif command -v pipx >/dev/null 2>&1; then
    INSTALLER="pipx"
    ok "Installer:" "pipx ($(pipx --version 2>/dev/null || echo 'installed'))"
else
    info "Installing:" "pipx via pip..."
    $PYTHON -m pip install --user pipx >/dev/null 2>&1 || $PYTHON -m pip install pipx >/dev/null 2>&1
    $PYTHON -m pipx ensurepath >/dev/null 2>&1 || true
    if command -v pipx >/dev/null 2>&1; then
        INSTALLER="pipx"
        ok "Installed:" "pipx"
    else
        # pipx installed but not yet on PATH — use python -m pipx
        INSTALLER="pipx-module"
        ok "Installed:" "pipx (via module)"
    fi
fi

# --- Install PRISM ---
printf '\n'
info "Installing:" "$PACKAGE..."

case "$INSTALLER" in
    uv)
        uv tool install "$PACKAGE"
        ;;
    pipx)
        pipx install "$PACKAGE"
        ;;
    pipx-module)
        $PYTHON -m pipx install "$PACKAGE"
        ;;
esac

# --- Verify ---
printf '\n'
if command -v prism >/dev/null 2>&1; then
    ok "Success!" "PRISM is installed."
    printf '\n'
    info "Try:" "prism --help"
    info "Docs:" "https://github.com/Darth-Hidious/PRISM"
    printf '\n'
else
    warn "Installed" "but 'prism' is not on your PATH yet."
    warn "Fix:"     "Add the pipx/uv bin directory to your PATH, then restart your shell."
    printf '\n'
fi
