#!/usr/bin/env bash
# setup-engine-tooling.sh — install the empirical-observation toolkit PRISM contributors
# use to debug the engine layer. Idempotent. Safe to re-run.
#
# Tools:
#   pty-mcp-server (MCP server, drives PRISM TUI from Claude Code via JSON-RPC)
#   tui-use        (CLI, drives PRISM TUI from any shell with friendly session names)
#
# Both speak PTY / pseudo-tty. Either can drive an interactive prism, capture the
# rendered screen, send keypresses, and watch for state changes. Wire one or both —
# `.mcp.json` already wires pty-mcp; tui-use is for shell-driven scripted walkthroughs.
#
# After running this, restart Claude Code (or kick a fresh /clear session) so the
# pty-mcp MCP server gets registered. The tui-use CLI is ready immediately.
#
# Background: see memory/project_redesign_roadmap_2026_05.md (Phase 1 — Engine).

set -euo pipefail

OK=$'\033[32m✓\033[0m'
WARN=$'\033[33m!\033[0m'
ERR=$'\033[31m✗\033[0m'

say() { printf '%s %s\n' "$1" "$2"; }

# ── Prereq check ──────────────────────────────────────────────────────────────
if ! command -v node >/dev/null 2>&1; then
  say "$ERR" "Node.js not found. Install Node 18+ (https://nodejs.org or 'brew install node' / 'apt install nodejs')."
  exit 1
fi
if ! command -v npm >/dev/null 2>&1; then
  say "$ERR" "npm not found alongside Node — your install may be broken."
  exit 1
fi
say "$OK" "Node $(node --version) / npm $(npm --version) detected"

# ── Install / upgrade tui-use ─────────────────────────────────────────────────
if command -v tui-use >/dev/null 2>&1; then
  CURRENT=$(tui-use --version 2>/dev/null || echo "unknown")
  say "$OK" "tui-use already installed (current: $CURRENT) — checking for upgrade"
  npm install -g tui-use@latest >/dev/null 2>&1 || say "$WARN" "tui-use upgrade non-fatal failure"
else
  say "$WARN" "tui-use not found — installing globally"
  npm install -g tui-use >/dev/null 2>&1
fi
say "$OK" "tui-use $(tui-use --version 2>/dev/null) ready at $(command -v tui-use)"

# ── Install / upgrade pty-mcp-server ──────────────────────────────────────────
if command -v pty-mcp-server >/dev/null 2>&1; then
  say "$OK" "pty-mcp-server already installed — checking for upgrade"
  npm install -g @so2liu/pty-mcp-server@latest >/dev/null 2>&1 || say "$WARN" "pty-mcp-server upgrade non-fatal failure"
else
  say "$WARN" "pty-mcp-server not found — installing globally"
  npm install -g @so2liu/pty-mcp-server >/dev/null 2>&1
fi
say "$OK" "pty-mcp-server ready at $(command -v pty-mcp-server)"

# ── Verify .mcp.json wiring ───────────────────────────────────────────────────
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MCP_JSON="$REPO_ROOT/.mcp.json"
if [[ -f "$MCP_JSON" ]] && grep -q 'pty-mcp-server' "$MCP_JSON"; then
  say "$OK" ".mcp.json already wires pty-debug → pty-mcp-server"
else
  say "$WARN" "Writing $MCP_JSON to wire pty-debug"
  cat > "$MCP_JSON" <<'JSON'
{
  "mcpServers": {
    "pty-debug": {
      "command": "pty-mcp-server",
      "args": [],
      "description": "PTY/TUI debug MCP — drives interactive terminal programs (PRISM TUI, llama-server, REPLs) from Claude Code. Used for engine-layer empirical observation per Phase 1 of redesign roadmap."
    }
  }
}
JSON
fi

# ── Smoke tests ───────────────────────────────────────────────────────────────
say "$WARN" "smoke-testing tui-use against bash..."
SID=$(tui-use start --label setup-smoke bash 2>/dev/null | tr -d '\n')
if [[ -n "$SID" ]]; then
  tui-use use "$SID" >/dev/null
  sleep 0.8  # let bash render its prompt
  tui-use type 'echo tui-use-smoke-ok' >/dev/null
  tui-use press enter >/dev/null
  sleep 1.2  # bash needs a beat to echo + render
  SNAP=$(tui-use snapshot 2>/dev/null || true)
  if printf '%s' "$SNAP" | grep -q 'tui-use-smoke-ok'; then
    say "$OK" "tui-use roundtrip works (session $SID)"
  else
    say "$ERR" "tui-use snapshot did not see typed text. Last snapshot:"
    printf '%s\n' "$SNAP" | sed 's/^/    /'
  fi
  tui-use kill >/dev/null
else
  say "$ERR" "tui-use start did not return a session id"
fi

say "$WARN" "smoke-testing pty-mcp-server JSON-RPC handshake..."
HANDSHAKE=$(printf '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"setup","version":"0"}}}\n' | { cat; sleep 1; } | pty-mcp-server 2>/dev/null | head -1)
if echo "$HANDSHAKE" | grep -q 'pty-debug-server'; then
  say "$OK" "pty-mcp-server stdio responds (server: pty-debug-server)"
else
  say "$ERR" "pty-mcp-server stdio did not respond to initialize — check install"
fi

# ── Done ──────────────────────────────────────────────────────────────────────
cat <<DONE

Setup complete.

  - tui-use:        drives interactive programs from shell scripts (e.g. tui-use start prism)
  - pty-mcp-server: same primitives, exposed as Claude Code MCP via .mcp.json

To use pty-mcp inside Claude Code, restart Claude Code (or kick a fresh session)
so it loads the project's .mcp.json. The first session in this directory will
prompt you to approve the pty-debug MCP server.

To drive prism right now from a shell:

  tui-use start --label run1 --cols 140 --rows 40 prism
  tui-use snapshot
  tui-use press ctrl+c
  tui-use kill

Engine-layer observation rules (per redesign roadmap Phase 1):
  - Friction list goes in memory/project_friction_list_*.md with file:line
  - TUI / strings / README issues are logged but NOT fixed in Phase 1
DONE
