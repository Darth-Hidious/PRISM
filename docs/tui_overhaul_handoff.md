# PRISM TUI Overhaul — Handoff for Continuation Agent

## What was done (this session)

A full opencode-parity TUI overhaul of PRISM was implemented across ~30+ patches.
All work is committed. The code builds clean, passes `verify-tui.sh` (6/6:
fmt/build/tests/clippy/PTY-e2e/CJK), and was verified live against the real
`prism backend` + local llama.cpp (gemma-4-12B on :8080).

### Phase 1 — Discoverability & Feel
- **Command palette** (Ctrl-P): fuzzy launcher over a declarative command
  registry (`command.rs`). 25+ entries. Two-tier fuzzy matcher (contiguous
  substring + gap-penalized subsequence).
- **Which-key panel** (`?`): scrollable keymap reference from a data-driven
  `keymap.rs` registry, grouped by category.
- **Theme system** (`theme.rs`): 6 themes (prism/midnight/forest/gruvbox/mono/
  my-eyes-hurt). Default = "prism" (ported from opencode.json, brighter orange
  accent #ff9e40). Theme picker in the palette. Full recolor via `Copy` Theme
  struct threaded through all render functions.
- **Toast notifications** (`toast.rs`): transient, auto-dismissing (4s/6s for
  errors), severity-colored, non-blocking.

### Layout Overhaul (opencode sections)
- Painted full-screen background, header bar (back pill + session title + model
  + hints), centered transcript, bordered prompt box, footer with live hints,
  panel sidebar (opencode `backgroundPanel`).
- Terminal cursor hidden during TUI, restored on exit.
- Mouse scroll routes to active scrollable surface (which-key or transcript).

### Backend Wiring (each: protocol + handler + TUI + fake + tests)
- **Models**: `/models list` → `ui.model.list` → 646-model fuzzy picker,
  provider-grouped, scrollable. `/model <id>` switch + `ui.status` header update.
  Context-passing fix: `&llm_config.model` threaded into `handle_models_slash_command`.
- **Sessions**: `/sessions` → `ui.session.list` → session picker (list/resume).
  Scroll-fixed (scroll_window).
- **Tools**: `ui.tools.catalog` emitted at startup → sidebar Tools tab shows
  live 98-tool catalog (approval-marked). Bespoke Tools window (approval-grouped,
  filter, scroll).
- **GitHub**: `/gh issues|prs|status|bug` → `ui.gh.data` → GitHub panel (tabs,
  fuzzy filter, file bugs via `gh issue create`). Repo auto-detected from
  `git remote origin`.
- **Account/Login**: device-flow login/logout; reads `~/.prism/credentials.json`
  for status.
- **Status**: bespoke dashboard from live App state (model/mode/session/counts/
  cost/tokens/goal) — no backend round-trip.
- **Config**: file viewer (`prism.toml`, `.mcp.json`, `~/.prism/config.toml`,
  credentials redacted) with file switching + scroll.
- **API Keys**: provider key entry (Anthropic/OpenAI/Google/Mistral/Cohere),
  masked input, saves to `~/.prism/api_keys.json` (0600).
- **12 slash.* commands**: context/files/tasks/memory/permissions/usage/doctor/
  config/diff/compact — all palette-reachable.
- **Diff**: content-aware coloring in the View panel (auto-detects `+`/`-`/`@@`
  syntax, colors green/red/accent).

### Stress Tests
- `stress_renders_without_panic`: 1000 models + 1000 sessions + 500 tools,
  rendered at sizes 1×1 to 500×200.
- `stress_malformed_messages_never_panic`: empty objects, wrong types, nulls,
  control chars, unknown methods, raw scalars.
- `stress_rapid_key_events`: long typing + 50× open/close + 100× scroll.

### Notebook Orchestration
- Research: Colab architecture, Jupyter Server API, SSH/mesh tunneling, IDE
  connectivity.
- Design: `docs/notebook_orchestration_design.md` (4 slices).
- Slice 1: `prism notebook start/list/stop` CLI (`crates/cli/src/notebook.rs`).

### PyIron
- CLI module created (`crates/cli/src/pyiron_cmd.rs`): status/install/update.
- PyIron NOT yet installed in venv. PRISM already has Python tool wrappers
  (`app/tools/sim_tools.py`, `app/tools/simulation/bridge.py`).
- User wants: PyIron always present, auto-provisioned, never-fail, updatable.
- The `prism pyiron` CLI subcommand enum + match arm are NOT yet wired into
  `main.rs` (the module exists but isn't registered or dispatched).

### Files
- `docs/tui_overhaul_ledger.md` — full status of done/remaining.
- `docs/notebook_orchestration_design.md` — notebook system design.

## What remains (in priority order)

### 1. PyIron integration (in-progress, partially started)
- **Wire `prism pyiron` CLI subcommand**: add `PyironCommands` enum +
  `Commands::Pyiron` variant + match arm in `main.rs`. The module
  (`pyiron_cmd.rs`) already has `status()/install()/update()` functions.
- **Auto-provision**: on backend startup, check if PyIron is importable; if
  not, transparently `pip install pyiron pyiron-atomistics` in `~/.prism/venv`.
- **Never-fail bridge**: update `app/tools/simulation/bridge.py` so
  `check_pyiron_available()` triggers auto-provision instead of just returning
  False. If install fails (no network), simulation tools return a helpful
  error but **never crash the TUI or backend**.
- **Version pinning**: pin to known-good versions (`pyiron>=0.4,<0.5`).
- **TUI panel**: a PyIron status panel showing version, health, available
  simulators (LAMMPS/VASP/GPaw), with update action.

### 2. API key mechanism — backend integration (TUI done, backend not)
- The TUI API key window writes to `~/.prism/api_keys.json`.
- The **backend does NOT yet read this file**. It needs to: when constructing
  the LLM client, if the env var isn't set, fall back to reading
  `~/.prism/api_keys.json` and set the env var before spawning the client.
- Also: after saving a key, the user should be able to switch the chat target
  via `prism use provider <provider> --model <model>` without restarting.

### 3. Notebook orchestration (Slices 2–4)
- **Slice 2**: Port forwarding (`prism notebook forward <port>` — SSH tunnel).
- **Slice 3**: Remote launch (`prism notebook start --remote` — MARC27 compute
  + mesh tunnel back to localhost).
- **Slice 4**: IDE connectivity (`prism notebook connect --vscode` — register
  kernel spec so VS Code/JupyterLab see the PRISM notebook as a selectable
  kernel).
- **TUI panel**: `notebook.show` palette entry — list sessions, open/stop/
  tunnel actions.

### 4. Palette 1:1 with opencode (remaining commands)
- opencode has ~150 keybind commands. PRISM has ~25 palette entries.
- Remaining APPLICABLE commands to map: variant cycle, agent cycle, prompt
  stash, file-context toggle, session timeline, session fork, session compact,
  scrollbar toggle, animations toggle.
- EXCLUDED (no PRISM backend): session share/unshare, provider-connect,
  console-org-switch, plugin marketplace install.

### 5. Text-content bespoke windows (design decision needed)
- `context/files/tasks/memory/permissions/usage/doctor` currently use the
  content-aware View panel (each opens its own titled window). The user
  objected to the generic panel. To make each truly bespoke, the **backend
  would need to emit structured JSON** instead of text for these commands.
- Decision: either (a) modify each backend `emit_*_screen` to emit a
  structured `ui.<name>.data` notification, or (b) accept the content-aware
  View panel as adequate for text output (it IS per-command titled and
  scrollable, with diff coloring).

## Key architecture decisions made
1. **Command registry** (`command.rs`) — declarative commands with metadata
   (id/title/description/category/keybind/suggested). Palette + slash.* dispatch
   from one source.
2. **Theme as `Copy` struct** — threaded by value through render, no globals.
3. **Backend owns data** — TUI sends `input.command`, backend pushes
   structured `ui.*` notifications. TUI is pure (no shelling in render path).
   Exception: Status window reads App state directly (no backend call needed).
4. **Fuzzy matcher** — two-tier: contiguous substring (strongly preferred) +
   gap-penalized subsequence. Fixed a ranking bug where "theme" matched
   "metrics.toggle" above "Switch theme".
5. **Scroll windows** — `scroll_window(sel, total, viewport)` keeps selection
   centered and visible. Applied to model picker, session picker, tools window.
6. **Fake backend** — simulates all new notifications (`ui.model.list`,
   `ui.tools.catalog`, `ui.gh.data`, `ui.session.list`) for testability.
7. **Stress tests** — huge data + malformed messages + extreme sizes + rapid
   keys. No panics. This is the "nothing breaks under stress" guarantee.

## How to run
```bash
# Build
cd ~/Downloads/PRISM && cargo build -p prism-cli

# Real TUI (local gemma on :8080)
./target/debug/prism tui

# Fake backend (deterministic, for testing)
./target/debug/prism tui --fake-backend --scenario basic_chat

# Verify
bash scripts/verify-tui.sh

# tmux (persistent server)
tmux -L prism attach -t tui

# Notebook
./target/debug/prism notebook list
./target/debug/prism notebook start [--port 8888]
./target/debug/prism notebook stop <pid|all>
```

## Key files modified
- `crates/tui/src/app.rs` — App state (all windows/pickers), key handling, dispatch
- `crates/tui/src/render.rs` — all renderers (header, prompt, footer, sidebar, panels)
- `crates/tui/src/command.rs` — command registry + fuzzy matcher
- `crates/tui/src/keymap.rs` — keymap registry
- `crates/tui/src/theme.rs` — theme system
- `crates/tui/src/toast.rs` — toast notifications
- `crates/tui/src/gh.rs` — GitHub panel state
- `crates/tui/src/msg.rs` — AgentMsg variants (ModelList, ToolsCatalog, GhData, etc.)
- `crates/tui/src/backend.rs` — fake backend simulations
- `crates/agent/src/protocol.rs` — backend handlers (/gh, /models list, /model, /tools catalog)
- `crates/cli/src/main.rs` — Notebook + Pyiron subcommands (partial)
- `crates/cli/src/notebook.rs` — notebook manager
- `crates/cli/src/pyiron_cmd.rs` — PyIron install/update/status (NOT yet wired)
- `crates/tui/tests/render_snapshots.rs` — snapshots + stress tests
- `crates/tui/tests/unit.rs` — unit tests (updated for new behavior)
