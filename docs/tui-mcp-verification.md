# TUI MCP Verification Guide

This document explains how to verify PRISM's terminal UI visually using a TUI MCP server.

## Prerequisites

- PRISM built: `cargo build -p prism-cli` (or `cargo build --release -p prism-cli`)
- Fake backend scenarios available (no external services needed)
- A TUI MCP server installed and configured

## TUI MCP Options

### A. tui-mcp (recommended)

tui-mcp maintains a persistent PTY session and can operate TUI apps like a human. Its tools include launch, resize, screenshot, snapshot, read_region, send_keys, send_text, wait_for_text, and wait_for_idle.

Install for Claude Code:
```
claude mcp add --scope user tui-mcp -- npx tui-mcp
```

Install for OpenCode: see `docs/examples/opencode.tui-mcp.jsonc` for config.

### B. mcp-tui-driver (Rust-native alternative)

mcp-tui-driver is described as "Playwright MCP, but for TUI apps."

Install:
```
cargo install --git https://github.com/michaellee8/mcp-tui-driver
```

## Verification Workflow

1. Read `PRISM_TUI_VERIFY.md` for the verification contract.
2. Launch a fake backend scenario:
   ```
   prism tui --fake-backend --scenario basic_chat
   ```
   (If launching via MCP, use the TUI MCP's launch tool with this command.)
3. Wait for the welcome text to appear.
4. Send keyboard input (type text, press Enter, press Ctrl-T, etc.).
5. Capture a screenshot or text snapshot.
6. Verify expected visible output matches the contract.
7. Send Ctrl-C (or SIGINT) to quit.
8. Verify the process exited and the terminal is restored.
9. Repeat for each scenario.
10. Write the report using `docs/templates/tui_mcp_verification_report.md`.

## Terminal Sizes

Test at minimum:
- 40x12 (tiny)
- 100x30 (normal)
- 200x60 (wide)

## Key Sequences to Test

See `PRISM_TUI_VERIFY.md` → Required Key Checks.

## OpenCode Configuration

For OpenCode, local MCP servers go under the `mcp` block in `opencode.json` with `"type": "local"` and a `"command"` array. See:

- Example config: `docs/examples/opencode.tui-mcp.jsonc`

Keep the TUI MCP disabled by default. Enable it only for the verifier agent to avoid unexpected tool availability for every task.

## Claude Code Configuration

Add to `~/.claude/settings.json`:
```json
{
  "mcpServers": {
    "tui-mcp": {
      "command": "npx",
      "args": ["-y", "tui-mcp"]
    }
  }
}
```

## Safety

- Use fake backend scenarios only (`--fake-backend --scenario <name>`)
- Do NOT call real LLM providers, materials APIs, compute, or marketplace
- Do NOT use real auth tokens
- The fake backend is fully deterministic — no network needed