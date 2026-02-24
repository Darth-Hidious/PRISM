#!/bin/sh
# PRISM One-Command Installer
# Usage: curl -fsSL https://prism.marc27.com/install.sh | bash
set -e

REPO="https://github.com/Darth-Hidious/PRISM.git"
PACKAGE="prism-platform"
GIT_PACKAGE="$PACKAGE @ git+$REPO"
MIN_PYTHON="3.11"

# ── Helpers ──────────────────────────────────────────────────────────
info()  { printf '  \033[1;34m%s\033[0m %s\n' "$1" "$2"; }
ok()    { printf '  \033[1;32m%s\033[0m %s\n' "$1" "$2"; }
warn()  { printf '  \033[1;33m%s\033[0m %s\n' "$1" "$2"; }
err()   { printf '  \033[1;31m%s\033[0m %s\n' "ERROR:" "$1" >&2; exit 1; }

printf '\n\033[1;36mPRISM Installer\033[0m\n'
printf '  Platform for Research in Intelligent Synthesis of Materials\n\n'

# ── Ensure ~/.local/bin is on PATH (used by pip --user, pipx, uv) ──
case ":$PATH:" in
    *":$HOME/.local/bin:"*) ;;
    *) export PATH="$HOME/.local/bin:$PATH" ;;
esac

# ── Detect OS ────────────────────────────────────────────────────────
OS="$(uname -s)"
ARCH="$(uname -m)"
case "$OS" in
    Darwin) info "OS:" "macOS ($ARCH)" ;;
    Linux)  info "OS:" "Linux ($ARCH)" ;;
    *)      err "Unsupported OS: $OS. PRISM supports macOS and Linux." ;;
esac

# ── Find Python >= 3.11 ─────────────────────────────────────────────
find_python() {
    for cmd in python3.13 python3.12 python3.11 python3 python; do
        if command -v "$cmd" >/dev/null 2>&1; then
            ver=$("$cmd" -c "import sys; print(f'{sys.version_info.major}.{sys.version_info.minor}')" 2>/dev/null || true)
            if [ -n "$ver" ]; then
                major=$(echo "$ver" | cut -d. -f1)
                minor=$(echo "$ver" | cut -d. -f2)
                if [ "$major" -ge 3 ] && [ "$minor" -ge 11 ]; then
                    echo "$cmd"
                    return 0
                fi
            fi
        fi
    done
    return 1
}

PYTHON=""
PYTHON=$(find_python) || true

# ── Find or install an installer (uv > pipx) ────────────────────────
INSTALLER=""

# Step 1: Check for uv already installed
if command -v uv >/dev/null 2>&1; then
    INSTALLER="uv"
    ok "Found:" "uv ($(uv --version 2>/dev/null || echo 'installed'))"

# Step 2: Check for pipx already installed
elif command -v pipx >/dev/null 2>&1; then
    INSTALLER="pipx"
    ok "Found:" "pipx ($(pipx --version 2>/dev/null || echo 'installed'))"

# Step 3: Neither found — try to install one
else
    info "Setup:" "Need pipx or uv to install PRISM..."

    # Strategy A: Try pip (from the target Python) to install pipx
    if [ -n "$PYTHON" ] && $PYTHON -m pip --version >/dev/null 2>&1; then
        info "Trying:" "pipx via $PYTHON -m pip..."
        _pip_log="$(mktemp 2>/dev/null || echo "/tmp/prism-pip.$$.log")"
        PIPX_INSTALLED=0
        if $PYTHON -m pip install --user pipx >"$_pip_log" 2>&1; then
            PIPX_INSTALLED=1
        elif $PYTHON -m pip install --break-system-packages --user pipx >"$_pip_log" 2>&1; then
            PIPX_INSTALLED=1
        elif $PYTHON -m pip install pipx >"$_pip_log" 2>&1; then
            PIPX_INSTALLED=1
        fi
        rm -f "$_pip_log"
        if [ "$PIPX_INSTALLED" -eq 1 ]; then
            $PYTHON -m pipx ensurepath >/dev/null 2>&1 || true
            if command -v pipx >/dev/null 2>&1; then
                INSTALLER="pipx"
                ok "Installed:" "pipx"
            else
                INSTALLER="pipx-module"
                ok "Installed:" "pipx (via $PYTHON -m pipx)"
            fi
        fi
    fi

    # Strategy B: Try ensurepip + pip to install pipx
    if [ -z "$INSTALLER" ] && [ -n "$PYTHON" ]; then
        if $PYTHON -m ensurepip --upgrade >/dev/null 2>&1 || $PYTHON -m ensurepip >/dev/null 2>&1; then
            info "Trying:" "pipx via ensurepip..."
            if $PYTHON -m pip install --user pipx >/dev/null 2>&1 || \
               $PYTHON -m pip install --break-system-packages --user pipx >/dev/null 2>&1; then
                $PYTHON -m pipx ensurepath >/dev/null 2>&1 || true
                if command -v pipx >/dev/null 2>&1; then
                    INSTALLER="pipx"
                    ok "Installed:" "pipx (via ensurepip)"
                else
                    INSTALLER="pipx-module"
                    ok "Installed:" "pipx (via $PYTHON -m pipx)"
                fi
            fi
        fi
    fi

    # Strategy C: Try any available pip3/pip on the system to install pipx
    if [ -z "$INSTALLER" ]; then
        for pipcmd in pip3 pip; do
            if command -v "$pipcmd" >/dev/null 2>&1; then
                info "Trying:" "pipx via $pipcmd..."
                if $pipcmd install --user pipx >/dev/null 2>&1 || \
                   $pipcmd install --break-system-packages --user pipx >/dev/null 2>&1 || \
                   $pipcmd install pipx >/dev/null 2>&1; then
                    # Run ensurepath if possible
                    pipx ensurepath >/dev/null 2>&1 || \
                      $pipcmd show pipx >/dev/null 2>&1 && \
                      python3 -m pipx ensurepath >/dev/null 2>&1 || true
                    if command -v pipx >/dev/null 2>&1; then
                        INSTALLER="pipx"
                        ok "Installed:" "pipx (via $pipcmd)"
                        break
                    fi
                fi
            fi
        done
    fi

    # Strategy D: Auto-install uv (no pip needed at all)
    if [ -z "$INSTALLER" ]; then
        info "Trying:" "auto-installing uv (no pip needed)..."
        if command -v curl >/dev/null 2>&1; then
            if curl -LsSf https://astral.sh/uv/install.sh 2>/dev/null | sh >/dev/null 2>&1; then
                # Source uv's env to update PATH
                if [ -f "$HOME/.local/bin/env" ]; then
                    . "$HOME/.local/bin/env" 2>/dev/null || true
                fi
                # Also check cargo bin in case uv installed there
                case ":$PATH:" in
                    *":$HOME/.cargo/bin:"*) ;;
                    *) export PATH="$HOME/.cargo/bin:$PATH" ;;
                esac
                if command -v uv >/dev/null 2>&1; then
                    INSTALLER="uv"
                    ok "Installed:" "uv"
                fi
            fi
        elif command -v wget >/dev/null 2>&1; then
            if wget -qO- https://astral.sh/uv/install.sh 2>/dev/null | sh >/dev/null 2>&1; then
                if [ -f "$HOME/.local/bin/env" ]; then
                    . "$HOME/.local/bin/env" 2>/dev/null || true
                fi
                case ":$PATH:" in
                    *":$HOME/.cargo/bin:"*) ;;
                    *) export PATH="$HOME/.cargo/bin:$PATH" ;;
                esac
                if command -v uv >/dev/null 2>&1; then
                    INSTALLER="uv"
                    ok "Installed:" "uv"
                fi
            fi
        fi
    fi

    # All strategies exhausted
    if [ -z "$INSTALLER" ]; then
        printf '\n'
        warn "Could not install pipx or uv automatically."
        warn ""  ""
        warn "Manual options:"
        warn "  1." "Install uv:   curl -LsSf https://astral.sh/uv/install.sh | sh"
        warn "  2." "Install pip:  sudo apt install python3-pip  (Debian/Ubuntu)"
        warn "    " "              sudo dnf install python3-pip  (Fedora)"
        warn "    " "              sudo pacman -S python-pip     (Arch)"
        warn "  3." "Install pipx: https://pipx.pypa.io/stable/installation/"
        printf '\n'
        err "No installer available. See suggestions above, then re-run this script."
    fi
fi

# ── If using uv, it can provide Python — check/upgrade our Python ───
if [ "$INSTALLER" = "uv" ] && [ -z "$PYTHON" ]; then
    info "Python:" "No Python >= $MIN_PYTHON found, using uv to get one..."
    if uv python install 3.12 >/dev/null 2>&1; then
        # After uv installs Python, find it
        PYTHON=$(find_python) || true
        if [ -n "$PYTHON" ]; then
            ok "Python:" "$($PYTHON --version) (installed by uv)"
        else
            # uv will handle Python selection internally
            ok "Python:" "uv will manage Python version"
        fi
    fi
fi

# ── Final Python check ──────────────────────────────────────────────
if [ -z "$PYTHON" ] && [ "$INSTALLER" != "uv" ]; then
    printf '\n'
    warn "Python >= $MIN_PYTHON is required but not found."
    warn "Install:" "sudo apt install python3.12  (Debian/Ubuntu)"
    warn "     or:" "brew install python@3.12     (macOS)"
    warn "     or:" "Install uv, which can fetch Python automatically:"
    warn "        " "curl -LsSf https://astral.sh/uv/install.sh | sh"
    printf '\n'
    err "Python >= $MIN_PYTHON is required. See suggestions above."
fi

if [ -n "$PYTHON" ]; then
    PYTHON_PATH="$(command -v "$PYTHON")"
    ok "Python:" "$($PYTHON --version) ($PYTHON_PATH)"
fi

# ── Install PRISM ────────────────────────────────────────────────────
printf '\n'
info "Installing:" "PRISM from GitHub..."

INSTALL_OK=0
case "$INSTALLER" in
    uv)
        UV_ARGS=""
        if [ -n "$PYTHON" ]; then
            PYTHON_PATH="$(command -v "$PYTHON")"
            UV_ARGS="--python $PYTHON_PATH"
        fi
        if uv tool install $UV_ARGS "$GIT_PACKAGE"; then
            INSTALL_OK=1
        fi
        ;;
    pipx)
        PYTHON_PATH="$(command -v "$PYTHON")"
        if pipx install --python "$PYTHON_PATH" "$GIT_PACKAGE"; then
            INSTALL_OK=1
        fi
        ;;
    pipx-module)
        PYTHON_PATH="$(command -v "$PYTHON")"
        if $PYTHON -m pipx install --python "$PYTHON_PATH" "$GIT_PACKAGE"; then
            INSTALL_OK=1
        fi
        ;;
esac

if [ "$INSTALL_OK" -eq 0 ]; then
    printf '\n'
    err "PRISM installation failed. Try manually:\n  pip install \"$GIT_PACKAGE\"\n  Or see: https://github.com/Darth-Hidious/PRISM#quick-start"
fi

# ── Verify ───────────────────────────────────────────────────────────
printf '\n'

# Re-check PATH one more time
case ":$PATH:" in
    *":$HOME/.local/bin:"*) ;;
    *) export PATH="$HOME/.local/bin:$PATH" ;;
esac

if command -v prism >/dev/null 2>&1; then
    ok "Success!" "PRISM is installed."
    printf '\n'
    info "Run:" "prism"
    info "Help:" "prism --help"
    info "Docs:" "https://github.com/Darth-Hidious/PRISM"
    printf '\n'

    # Warn if user's login shell won't see the binary
    SHELL_RC=""
    case "${SHELL:-}" in
        */bash) SHELL_RC="~/.bashrc" ;;
        */zsh)  SHELL_RC="~/.zshrc" ;;
        */fish) SHELL_RC="~/.config/fish/config.fish" ;;
    esac
    # Check if ~/.local/bin is already in login shell PATH
    if [ -n "$SHELL_RC" ] && ! grep -q '\.local/bin' "$HOME/$(basename "$SHELL_RC" | sed 's/^~//')" 2>/dev/null; then
        warn "Note:" "You may need to restart your shell or run:"
        warn "     " "  export PATH=\"\$HOME/.local/bin:\$PATH\""
    fi
else
    warn "Installed" "but 'prism' is not on your PATH yet."
    printf '\n'
    warn "Fix:" "Add ~/.local/bin to your PATH, then restart your shell:"
    warn "    " "  echo 'export PATH=\"\$HOME/.local/bin:\$PATH\"' >> ~/.bashrc"
    warn "    " "  source ~/.bashrc"
    printf '\n'
    warn "Or run directly:"
    if [ "$INSTALLER" = "uv" ]; then
        warn "    " "  uv tool run prism"
    elif [ "$INSTALLER" = "pipx-module" ]; then
        warn "    " "  $PYTHON -m pipx run $PACKAGE"
    else
        warn "    " "  pipx run $PACKAGE"
    fi
    printf '\n'
fi
