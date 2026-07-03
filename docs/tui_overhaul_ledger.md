# PRISM TUI Overhaul — Session Ledger

> Last updated: 2026-07-02
> This file tracks what's done and what remains from the TUI overhaul session.

## Completed

### Phase 1 — Discoverability & Feel
- ✅ **Command palette** (Ctrl-P) — fuzzy command launcher, 25+ commands
- ✅ **Which-key panel** (`?`) — scrollable keymap reference, grouped
- ✅ **Theming system** — 6 themes (prism/midnight/forest/gruvbox/mono/my-eyes-hurt), theme picker, full recolor
- ✅ **Toast notifications** — transient, auto-dismissing, severity-colored

### Layout Overhaul
- ✅ **opencode-style sections** — painted background, header bar (back pill + session title + model + hints), centered transcript, bordered prompt box, footer with live hints, panel sidebar (backgroundPanel)

### Backend Wiring (each with palette entry + real backend)
- ✅ **Models** — `/models list` → `ui.model.list` → 646-model fuzzy picker, provider-grouped, scrollable; `/model <id>` switch + `ui.status` header update
- ✅ **Sessions** — `/sessions` → `ui.session.list` → session picker (list/resume); `/resume <id>`
- ✅ **Tools** — `ui.tools.catalog` at startup → sidebar Tools tab shows live catalog; bespoke Tools window (approval-grouped, filter, scroll)
- ✅ **GitHub** — `/gh issues|prs|status|bug` → `ui.gh.data` → GitHub panel (tabs, fuzzy filter, file bugs)
- ✅ **Account/Login** — device-flow login/logout; reads `~/.prism/credentials.json` for status
- ✅ **Status** — bespoke dashboard from live App state (model/mode/session/counts/cost/tokens/goal)
- ✅ **Config** — bespoke file viewer (prism.toml, .mcp.json, config.toml, credentials redacted)
- ✅ **Command palette coverage** — 12 `slash.*` commands wired (context, files, tasks, memory, permissions, usage, doctor, config, diff, compact, etc.)

### Quality
- ✅ **Stress tests** — huge data (1000 models/sessions, 500 tools), malformed messages, extreme terminal sizes (1×1 to 500×200), rapid key events — no panics
- ✅ **Fuzzy matcher** — two-tier (contiguous substring + gap-penalized subsequence), fixed ranking bug
- ✅ **Cursor hide** — terminal cursor hidden during TUI, restored on exit
- ✅ **Mouse scroll** — routes to active scrollable surface (which-key panel or transcript)

### Notebook Orchestration
- ✅ **Research** — Colab architecture, Jupyter Server API, SSH/mesh tunneling, IDE connectivity
- ✅ **Design doc** — `docs/notebook_orchestration_design.md`
- ✅ **Slice 1** — `prism notebook start/list/stop` CLI (local Jupyter launch, PID tracking, auto-port, token gen)

### Infrastructure
- ✅ **Persistent tmux server** — socket `prism`, session `tui`, real backend + local gemma
- ✅ **opencode repo cloned** — `~/Downloads/opencode` for reference

---

## Remaining

### Bespoke Windows (replace generic View panel)
- [ ] **Diff viewer** — colored patch/hunk UI for `/diff`
- [ ] **Context** — bespoke window for `/context`
- [ ] **Files** — bespoke window for `/files`
- [ ] **Tasks** — bespoke window for `/tasks`
- [ ] **Memory** — bespoke window for `/memory`
- [ ] **Permissions** — bespoke window for `/permissions`
- [ ] **Usage** — bespoke window for `/usage`
- [ ] **Doctor** — bespoke window for `/doctor`

### Bug Fixes
- [x] **Session picker scroll** — scroll_window applied, follows selection
- [x] **Model picker scroll** — fixed earlier, provider-grouped

### Diff viewer
- [x] **Content-aware View panel** — detects diff syntax (`+` green, `-` red, `@@` accent, file headers bold) automatically; any `/diff` output is now a colored patch viewer

### Remaining

### Features Not Started
- [ ] **API key mechanism** — Anthropic/OpenAI key entry + storage + provider config
- [ ] **Notebook TUI panel** — `notebook.show` palette entry, session list, open/stop/tunnel actions
- [ ] **Notebook Slice 2** — port forwarding (`prism notebook forward`)
- [ ] **Notebook Slice 3** — remote launch (`prism notebook start --remote` on MARC27 compute)
- [ ] **Notebook Slice 4** — IDE connectivity (`prism notebook connect --vscode`, kernel-spec registration)
- [ ] **Palette 1:1 with opencode** — remaining applicable commands (variant cycle, agent cycle, stash, file-context toggle, etc.)

### Text-content commands (context/files/tasks/memory/permissions/usage/doctor)
These use the content-aware View panel — each opens as its own titled, scrollable
window with its own palette entry. They share a renderer (like opencode shares
DialogSelect for its pickers). Backend output is text; no structure to specialise
on without backend changes to emit JSON.

### Known Issues
- [ ] Real backend startup is slow (~10-15s for node init) — not a TUI bug
- [ ] tmux session needs restart after rebuilding to pick up new binary
- [ ] Some snapshot tests need regenerating when palette entries change (expected, not a bug)
