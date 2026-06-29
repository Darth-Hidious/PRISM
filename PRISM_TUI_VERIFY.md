# PRISM TUI Verification Contract

## Purpose

This document defines the verification contract for PRISM's terminal UI. Any agent (Claude, GLM, OpenCode) that verifies the TUI must follow this contract.

## Fake Backend Scenario List

| Scenario | Tests |
|---------|-------|
| `basic_chat` | Startup, welcome, message submit, streamed response, turn complete |
| `streaming_answer` | Multiple text deltas, streaming pipeline |
| `thinking_stream` | Thinking deltas then answer deltas, Ctrl-T toggle |
| `tool_success` | Tool start + successful tool card result |
| `tool_error` | Tool start + error tool card |
| `approval_required` | Approval prompt with rich fields, y/n/a responses |
| `cost_metrics` | Cost with token counts (input/output/cache) |
| `backend_warning_error` | Backend warning + structured error |
| `ansi_injection` | ANSI/control sequences in text/tool/card — sanitizer must strip them |

## Required Launch Commands

```
cargo run -p prism-cli -- tui --fake-backend --scenario basic_chat
cargo run -p prism-cli -- tui --fake-backend --scenario approval_required
cargo run -p prism-cli -- tui --fake-backend --scenario ansi_injection
```

## Required Terminal Sizes

Verify the TUI renders correctly at these sizes:
- 40x12 (tiny terminal)
- 100x30 (normal terminal)
- 200x60 (wide terminal)

## Required Key Checks

| Key | Expected behavior |
|-----|-------------------|
| Ctrl-C | TUI exits, terminal restored |
| Ctrl-L | Chat cleared, system message appears |
| Ctrl-T | Thinking visibility toggled |
| Ctrl-M | Metrics display toggled |
| Ctrl-$ | Cost display toggled |
| Tab | Focus cycles between chat and input |
| Esc | Blurs from input to chat |
| Enter | Submits message (if input non-empty) |
| y | Approves pending approval |
| n | Denies pending approval |
| a | Allows all for session |

## Required Visible Assertions

For each scenario, verify:
1. Chat panel is visible
2. Input area is visible
3. Status bar is visible
4. Fake backend welcome text is visible ("PRISM v2.7.1-fake — 99 tools available")
5. Streamed response is visible (after sending a message)
6. Thinking can be toggled (Ctrl-T) in thinking_stream scenario
7. Approval popup appears in approval_required scenario
8. Approval y/n/a produce deterministic responses (card for y, status for n, permissions+card for a)
9. Tool success/error lines are visible in tool_success/tool_error scenarios
10. Backend warning/error visible in backend_warning_error scenario
11. ANSI injection scenario shows no unsafe visible controls (no ESC/BEL/BS/CR/DEL in rendered text)
12. Ctrl-C exits and terminal is restored (no broken shell)

## Required Report Format

Use the template at `docs/templates/tui_mcp_verification_report.md`.

The report must include:
- Summary: PASS or FAIL
- Environment: OS, terminal sizes, PRISM command, TUI MCP server, commit hash, scenarios tested
- Commands run: list all commands and results
- MCP interactions: per-scenario details (launch, keys sent, expected vs actual)
- Screenshots/snapshots: saved artifact paths
- Failures: per-failure details (scenario, expected, actual, likely source, suggested fix, severity)
- Missing coverage: scenarios or states not yet verified
- Language drift: whether any non-English text was detected
- Final recommendation: merge, request changes, or block

## Missing Coverage

If a scenario or UI state is not verified, list it under Missing Coverage in the report. Do not silently skip.

## Language Drift

All output must be English. If CJK characters appear in any agent artifact (docs, prompts, tests, reports), run:
```
python scripts/check_no_cjk_in_agent_artifacts.py
```
Correct any findings and note "language drift corrected" in the report.

## Snapshot Review Policy

Snapshot tests are in `crates/tui/tests/render_snapshots.rs`. Snapshots are stored as `.snap` files under `crates/tui/tests/snapshots/`.

- Snapshot failures must be inspected, not blindly accepted.
- Use `cargo insta review` if available to inspect diffs interactively.
- Use `INSTA_UPDATE=always cargo test -p prism-tui --test render_snapshots` only when intentionally generating or updating snapshots.
- Never set `INSTA_UPDATE=always` in CI or in `scripts/verify-tui.sh`.
- Snapshot diffs are visual contract changes — review them like code changes.