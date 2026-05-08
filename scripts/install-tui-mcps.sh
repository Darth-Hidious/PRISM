#!/usr/bin/env bash
#
# install-tui-mcps.sh — One-command install of the TUI testing MCPs that
# can't ship via npm/npx auto-pull (because they're not on npm).
#
# After running this, restart Claude Code so the new MCPs are picked up
# from .mcp.json. The npm-distributed ones (pty-debug, terminalcp,
# iterm-tmux) are already wired and don't need this script.
#
# Run this from the PRISM project root:
#   ./scripts/install-tui-mcps.sh
#
# Idempotent — safe to re-run.
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$(pwd)"

c_grn='\033[0;32m'
c_yel='\033[0;33m'
c_red='\033[0;31m'
c_off='\033[0m'

log()  { printf "${c_grn}[install-tui-mcps]${c_off} %s\n" "$*"; }
warn() { printf "${c_yel}[install-tui-mcps]${c_off} %s\n" "$*"; }
err()  { printf "${c_red}[install-tui-mcps]${c_off} %s\n" "$*"; }

# ── 1. mcp-tui-driver ─────────────────────────────────────────────────
# Rust binary. Adds a `tui_screenshot` tool that returns a PNG of the
# terminal — visual verification that text-only pty-debug can't give.
# Repo: https://github.com/michaellee8/mcp-tui-driver
log "Installing mcp-tui-driver (Rust, screenshot-capable)..."
if command -v mcp-tui-driver >/dev/null 2>&1; then
    log "  already installed → $(command -v mcp-tui-driver)"
else
    if ! command -v cargo >/dev/null 2>&1; then
        err "  cargo not found — install Rust first: https://rustup.rs"
        exit 1
    fi
    cargo install --git https://github.com/michaellee8/mcp-tui-driver --locked
    log "  installed → $(command -v mcp-tui-driver)"
fi

# ── 2. mcp-tui-test ───────────────────────────────────────────────────
# Python MCP server — Playwright-style buffer-mode TUI testing with
# position assertions and snapshot diffing.
# Repo: https://github.com/GeorgePearse/mcp-tui-test
log "Installing mcp-tui-test (Python, Playwright-style)..."
TUI_TEST_DIR="$HOME/.prism/mcps/mcp-tui-test"
if [[ -d "$TUI_TEST_DIR/.git" ]]; then
    log "  already cloned → $TUI_TEST_DIR (pulling latest)"
    git -C "$TUI_TEST_DIR" pull --quiet || warn "  pull failed; using local copy"
else
    mkdir -p "$(dirname "$TUI_TEST_DIR")"
    git clone --depth 1 https://github.com/GeorgePearse/mcp-tui-test "$TUI_TEST_DIR"
fi

if ! command -v uv >/dev/null 2>&1; then
    warn "  uv not found — falling back to pip. Install uv for faster setup: https://docs.astral.sh/uv/"
    python3 -m venv "$TUI_TEST_DIR/.venv"
    "$TUI_TEST_DIR/.venv/bin/pip" install -q -e "$TUI_TEST_DIR"
else
    (cd "$TUI_TEST_DIR" && uv venv --quiet && uv pip install -q -e .)
fi
log "  installed in $TUI_TEST_DIR/.venv"

# ── 3. @microsoft/tui-test (CI-grade end-to-end TUI testing) ──────────
# Not an MCP — it's a Playwright-style test FRAMEWORK we add to PRISM's
# CI matrix to catch TUI regressions before they ship.
# Repo: https://github.com/microsoft/tui-test
log "Adding @microsoft/tui-test as a dev dependency for PRISM CI..."
if [[ -f "$ROOT/package.json" ]]; then
    npm install --save-dev --silent @microsoft/tui-test
    log "  installed in $ROOT/node_modules"
else
    warn "  no package.json at project root — skipping. Add it manually with:"
    warn "    npm i -D @microsoft/tui-test"
fi

# ── 4. Update .mcp.json with the cargo+python entries ─────────────────
# We add these to .mcp.json AFTER install confirms they exist, so an
# unfinished install doesn't leave a broken MCP entry.
log "Wiring installed MCPs into .mcp.json..."
python3 <<PY
import json
import sys
from pathlib import Path

cfg_path = Path(".mcp.json")
cfg = json.loads(cfg_path.read_text())
servers = cfg.setdefault("mcpServers", {})

# mcp-tui-driver: visual screenshot (PNG) capability
servers["tui-driver"] = {
    "command": "mcp-tui-driver",
    "args": [],
    "description": (
        "VISUAL TUI MCP — adds tui_screenshot (PNG) for verifying "
        "color, alignment, kerning, banner geometry. Use when "
        "text-only snapshots can't tell you what the human will see."
    ),
}

# mcp-tui-test: Playwright-style buffer-mode testing
import os
home = os.environ.get("HOME", "")
tui_test_python = f"{home}/.prism/mcps/mcp-tui-test/.venv/bin/python"
tui_test_module = f"{home}/.prism/mcps/mcp-tui-test/server.py"
servers["tui-test"] = {
    "command": tui_test_python,
    "args": [tui_test_module],
    "description": (
        "PLAYWRIGHT-style TUI testing — buffer mode with position "
        "assertions, snapshot diffing, wait_for_text, cursor tracking. "
        "Use for writing reusable TUI regression tests."
    ),
}

cfg_path.write_text(json.dumps(cfg, indent=2) + "\n")
print("  .mcp.json updated with tui-driver + tui-test entries")
PY

log "All TUI MCPs installed and wired."
log ""
log "Next: restart Claude Code so the new MCPs are picked up."
log "Quick smoke check (after restart):"
log "  in any conversation, ask Claude to call \\\`mcp-tui-driver --help\\\`"
log "  or \\\`mcp-tui-test\\\` — both should be visible."
