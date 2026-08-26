# What remains before people can use PRISM

Written 2026-08-26 from **driving the shipped binary**, not from reading it.
`target/release/prism`, 79MB, `prism 1.1.0`, model `glm-5.3-flash`.

Everything below was seen on screen. Nothing here is inferred from source.

---

## What already works

This is the part worth being clear about, because it is most of the product.

- **The loop is real.** Asked for the spacegroup of tungsten, PRISM reasoned,
  chose `materials_search`, read the result, noticed every record came back with
  `space_group: null`, said so, and changed strategy on its own:
  *"none carried symmetry data … let me pull the structure directly from two
  sources that do report space groups."* Then it ran `lookup_structure`, then
  `query_materials_project`. That is a genuine multi-step research trajectory.
- **146 tools** load from the Python tool server; neural selection retrieves
  over all of them and hands the model 35 within a 32k budget.
- **Policy and provenance are live** — OPA evaluated every call, four hooks
  registered (safety_guard, cost_tracker, audit_log, provenance), each tool
  timed and audited.
- **The TUI holds together** at 200x50: workspace sidebar, tab strip, trajectory
  list, approval modal, prompt box — all fitting, nothing clipped.
- **The error messages are unusually honest.** The tool-pipe failure explains
  precisely what went wrong and why it cannot be papered over. That is rarer
  than it sounds and it is worth keeping.

## What blocks shipping

### 1. One slow tool used to end the session — FIXED TODAY

The binding failure. A plain `structure` build for tungsten overran a hardcoded
60s ceiling; because the timed-out call still owed a response on the pipe, the
handle refused everything afterwards and the **next, unrelated** tool
(`prior_art_search`) died with it. Two commits today:

- `PRISM_TOOL_CALL_TIMEOUT_SECS` — the ceiling is now the operator's, default
  unchanged at 60s, malformed values fall back rather than disabling the bound.
- `ToolServerHandle::recover()` — a poisoned handle replaces its child with an
  identical one and rebinds the session. The pool already did this; the agent
  held a bare handle and never got it. Recovery preserves the
  clean-environment credential boundary, and that is enforced by a test which
  goes red if it is dropped.

Needs a live re-run to confirm end-to-end.

### 2. Results render as "untitled"

`query_materials_project` returned 8 results; all eight displayed as
`1. untitled, 2. untitled …`. The data arrives, the display mapping loses the
material identity. A user sees nothing usable.

### 3. Fast input drops characters

Pasting a long prompt delivered only `"Screen refra"`. Anyone who pastes a
research question loses most of it. Blocking for real use.

### 4. A missing API key produces the vendor's error, not ours

With `ZAI_CODING_KEY` unset, PRISM makes a doomed HTTP call and surfaces
`HTTP 401: Authentication parameter not received in Header`. It knows the
variable name — `~/.prism/config.toml` says `api_key_env = "ZAI_CODING_KEY"` —
so it should say so before the call.

### 5. The model registry does not know the shipped default

`model not in registry (~/.prism/models.toml, platform catalog cache, or seed)
— using UNKNOWN fallback`, then `context_window=Some(128000)`. The default model
is not in the registry and the context window is a guess. Budgets computed from
a guessed window are how you overflow silently.

### 6. Approval friction

Every tool call stops and waits. A four-tool research trajectory means four
interruptions. Ship needs a sensible default policy: read-only tools
(`*_read`, `query`, `papers`) auto-allowed, billable and mutating ones gated.
The permission model already distinguishes them — `ReadOnly` vs `FullAccess`,
`requires_approval` — so this is policy, not new machinery.

### 7. Smaller, but visible

- The status bar reads **"Ready"** while the state is `[APPROVAL]`.
- `[RED indeterminate]` — an internal label — is printed in user-facing tool
  result lines.
- `ERROR` log lines write to stderr while the TUI owns the screen and corrupt
  the status bar.
- `command_tools_filtered(true)` and `(false)` return the **identical 57 tools**
  — the `local_node_online` parameter filters nothing.
- `xterm-ghostty` terminfo is not installed, so `TERM` falls back and mouse
  reporting degrades. Environment, not code, but it affects hover.

## Order I would do them in

1. Verify today's recovery fix end-to-end on a real research task. (#1)
2. Default approval policy for read-only tools. (#6) — biggest usability win
3. Fix the "untitled" result mapping. (#2)
4. Fix input dropping. (#3)
5. Actionable missing-key error + register the default model. (#4, #5)
6. The cosmetic set. (#7)

None of these are architectural. The engine, the loop, the tool surface and the
provenance spine are working. What remains is the last mile between a system
that computes the right answer and one a person can sit in front of.
