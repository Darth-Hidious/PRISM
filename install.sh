#!/bin/sh
# PRISM One-Command Installer
# Usage: curl -fsSL https://prism.marc27.com/install.sh | sh
set -e

REPO="https://github.com/Darth-Hidious/PRISM.git"
PACKAGE="prism-platform"
GIT_PACKAGE="$PACKAGE @ git+$REPO"
MIN_PYTHON="3.11"

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

# --- Find Python >= 3.11 ---
PYTHON=""
for cmd in python3.13 python3.12 python3.11 python3 python; do
    if command -v "$cmd" >/dev/null 2>&1; then
        ver=$("$cmd" -c "import sys; print(f'{sys.version_info.major}.{sys.version_info.minor}')" 2>/dev/null || true)
        if [ -n "$ver" ]; then
            major=$(echo "$ver" | cut -d. -f1)
            minor=$(echo "$ver" | cut -d. -f2)
            if [ "$major" -ge 3 ] && [ "$minor" -ge 11 ]; then
                PYTHON="$cmd"
                break
            fi
        fi
    fi
done

if [ -z "$PYTHON" ]; then
    err "Python >= $MIN_PYTHON is required but not found.\n  Install Python 3.11+ from https://python.org\n  On Debian/Ubuntu: sudo apt install python3.12\n  On macOS: brew install python@3.12"
fi
PYTHON_PATH="$(command -v "$PYTHON")"
ok "Python:" "$($PYTHON --version) ($PYTHON_PATH)"

# --- Find pipx or uv ---
INSTALLER=""
if command -v uv >/dev/null 2>&1; then
    INSTALLER="uv"
    ok "Installer:" "uv ($(uv --version 2>/dev/null || echo 'installed'))"
elif command -v pipx >/dev/null 2>&1; then
    INSTALLER="pipx"
    ok "Installer:" "pipx ($(pipx --version 2>/dev/null || echo 'installed'))"
else
    # Ensure ~/.local/bin is on PATH for --user installs
    case ":$PATH:" in
        *":$HOME/.local/bin:"*) ;;
        *) export PATH="$HOME/.local/bin:$PATH" ;;
    esac

    info "Installing:" "pipx via pip..."

    # Check if pip is available for this Python
    if ! $PYTHON -m pip --version >/dev/null 2>&1; then
        printf '\n'
        warn "Note:" "pip is not available for $($PYTHON --version)."
        case "$OS" in
            Linux)
                warn "Install pip:" "sudo apt install python3-pip  (Debian/Ubuntu)"
                warn "         or:" "sudo dnf install python3-pip  (Fedora)"
                warn "         or:" "sudo pacman -S python-pip     (Arch)" ;;
            Darwin)
                warn "Install pip:" "$PYTHON -m ensurepip" ;;
        esac
        warn "Alternative:" "Install uv instead (no pip needed):"
        warn "            " "curl -LsSf https://astral.sh/uv/install.sh | sh"
        printf '\n'
        err "pip is required to install pipx. See suggestions above."
    fi

    # Install pipx — show output so errors are visible
    _pip_log="$(mktemp 2>/dev/null || echo "/tmp/prism-pip.$$.log")"
    PIPX_INSTALLED=0
    if $PYTHON -m pip install --user pipx >"$_pip_log" 2>&1; then
        PIPX_INSTALLED=1
    elif $PYTHON -m pip install --break-system-packages --user pipx >"$_pip_log" 2>&1; then
        PIPX_INSTALLED=1
    elif $PYTHON -m pip install pipx >"$_pip_log" 2>&1; then
        PIPX_INSTALLED=1
    fi

    if [ "$PIPX_INSTALLED" -eq 0 ]; then
        printf '\n'
        warn "pip output:" ""
        cat "$_pip_log" >&2
        rm -f "$_pip_log"
        printf '\n'
        err "Failed to install pipx. Install pipx or uv manually:\n  pipx: https://pipx.pypa.io/stable/installation/\n  uv:   curl -LsSf https://astral.sh/uv/install.sh | sh"
    fi
    rm -f "$_pip_log"

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
info "Installing:" "PRISM from GitHub..."

INSTALL_OK=0
case "$INSTALLER" in
    uv)
        if uv tool install --python "$PYTHON_PATH" "$GIT_PACKAGE"; then
            INSTALL_OK=1
        fi
        ;;
    pipx)
        if pipx install --python "$PYTHON_PATH" "$GIT_PACKAGE"; then
            INSTALL_OK=1
        fi
        ;;
    pipx-module)
        if $PYTHON -m pipx install --python "$PYTHON_PATH" "$GIT_PACKAGE"; then
            INSTALL_OK=1
        fi
        ;;
esac

if [ "$INSTALL_OK" -eq 0 ]; then
    printf '\n'
    err "PRISM installation failed. You can try installing manually:\n  pip install \"$GIT_PACKAGE\"\n  Or see: https://github.com/Darth-Hidious/PRISM#installation"
fi

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
    warn "Run:"     "pipx ensurepath   (or: uv tool update-shell)"
    printf '\n'
fi
