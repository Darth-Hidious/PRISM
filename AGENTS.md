# AGENTS.md — PRISM TUI Agent Instructions

## Project Summary

PRISM is an AI-native autonomous materials discovery platform. The TUI is a Ratatui terminal interface built with the Elm Architecture (TEA): `App` (model), `handle_key`/`apply_agent_msg` (update), `render::draw` (pure view). The backend is either a real `prism backend` subprocess or a deterministic fake backend for testing.

## Language Policy

All output must be in English. Do not write Chinese, Japanese, Korean, or other CJK text in reports, code comments, test names, documentation, commit messages, or summaries unless quoting an existing source that is already in that language.

If you accidentally produce non-English text, rewrite it in English and note "language drift corrected" in your report.

Run the language drift checker after agent work:
```
python scripts/check_no_cjk_in_agent_artifacts.py
```

## Safety Policy

During TUI verification, do NOT:
- Call real LLM providers (OpenAI, Anthropic, Google, Z.ai)
- Call real materials APIs (Materials Project, OPTIMADE)
- Submit real compute jobs
- Install marketplace tools
- Use real MARC27 auth tokens
- Access Turso, Qdrant, FalkorDB, or other databases
- Make network calls of any kind

Use fake backend scenarios exclusively:
```
prism tui --fake-backend --scenario <name>
```

## TUI Architecture Summary

- **Model**: `App` struct in `crates/tui/src/app.rs` — holds messages, input, scroll, status, approval state, metrics
- **Update**: `handle_key` (keyboard), `apply_agent_msg` (backend events)
- **View**: `render::draw` in `crates/tui/src/render.rs` — pure, no I/O, no network
- **Events**: `AgentMsg` enum in `crates/tui/src/msg.rs`, parsed by `parse_notification`
- **Sanitizer**: `sanitize_for_render` in `crates/tui/src/sanitize.rs` — strips ANSI/control chars at state ingress
- **Backend**: `BackendHandle` enum (Real/Fake) in `crates/tui/src/backend.rs`
- **Scenarios**: `FakeScenario` enum with 9 deterministic scenarios

## Fake Backend Commands

```
prism tui --fake-backend --scenario basic_chat
prism tui --fake-backend --scenario streaming_answer
prism tui --fake-backend --scenario thinking_stream
prism tui --fake-backend --scenario tool_success
prism tui --fake-backend --scenario tool_error
prism tui --fake-backend --scenario approval_required
prism tui --fake-backend --scenario cost_metrics
prism tui --fake-backend --scenario backend_warning_error
prism tui --fake-backend --scenario ansi_injection
```

## Required Verification Command

After any TUI patch, run:
```
bash scripts/verify-tui.sh
```

This runs fmt, build, tests, clippy, PTY e2e, and the CJK language drift checker.

## Patch Discipline

- Work in small, reviewable patches.
- Add tests with every patch.
- Run `scripts/verify-tui.sh` after each patch.
- Stop after each patch and report: files changed, tests added, commands run, failures, fixes, remaining risk.
- Do not proceed to the next patch without approval.

## Roles

- **GLM / OpenCode (implementer)**: writes code, tests, fixes. Small patches, stops after each report.
- **Claude / TUI-MCP (verifier/reviewer)**: visually verifies TUI via MCP, writes verification report, does not edit files unless explicitly approved.

## Guardrails

- Do not weaken approval gating.
- Do not bypass the sanitizer.
- Do not add network-dependent tests.
- Do not make the render path impure (no I/O in `render::draw`).
- Do not remove `Unknown(Value)` fallback in `parse_notification`.
- Do not change the approval state model without explicit approval.
- Do not change the tool-card model without explicit approval.