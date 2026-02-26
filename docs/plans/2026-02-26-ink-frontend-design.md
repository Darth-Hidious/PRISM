# Ink Frontend Architecture Design (v2.5.0)

**Date**: 2026-02-26
**Status**: Approved
**Scope**: TypeScript Ink (React) TUI frontend + Protocol-Driven UI architecture

## Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Framework | Ink (React for terminals) | Same as Claude Code, mature, large ecosystem |
| Communication | JSON-RPC over stdio (+ HTTP+SSE future) | Same pattern as MCP/LSP, no ports/networking |
| Distribution | Bun-compiled binary in platform-specific pip wheels | Zero runtime deps, single pip install |
| Migration | Parallel modes: `prism` (Ink) / `prism --classic` (Rich) | Both maintained, no hard cutover |
| Scope | Full parity with Rich REPL | All features in both frontends |

## Critical Rule: Dual Frontend Maintenance

**Every UI change must be made in BOTH frontends.** Both consume the same UIEmitter (`app/backend/ui_emitter.py`). All logic lives in UIEmitter — frontends are dumb renderers. Zero business logic duplication.

```
Backend (Python)           Protocol (JSON-RPC)         Frontend (TS or Rich)
─────────────────          ─────────────────           ─────────────────────
AgentCore logic      →     Typed UI events        →    Render event → pixels
Card type detection  →     {type: "card.tool",    →    TS: <ToolCard .../>
Cost calculation     →      data: {...}}           →    Rich: render_tool_result()
Slash command logic  →     {type: "prompt.ask",   →    Both: show prompt, return answer
Plan detection       →      choices: [...]}        →
```

---

## 1. Communication Protocol

JSON-RPC 2.0 over stdio. Python backend is the server, frontends are clients.

```
┌─────────────────┐     stdin/stdout      ┌─────────────────┐
│  TS Frontend     │ ◄──── JSON-RPC ────► │  Python Backend  │
│  (Ink)           │                       │  (app.backend)   │
└─────────────────┘                       └─────────────────┘

┌─────────────────┐     direct import     ┌─────────────────┐
│  Rich Frontend   │ ◄──── same events ──►│  Python Backend  │
│  (--classic)     │                       │  (app.backend)   │
└─────────────────┘                       └─────────────────┘
```

### Backend → Frontend Events

| Method | Params | Purpose |
|---|---|---|
| `ui.text.delta` | `{text}` | Streaming text chunk |
| `ui.text.flush` | `{text}` | Freeze accumulated text permanently |
| `ui.tool.start` | `{tool_name, call_id, verb}` | Tool execution started |
| `ui.card` | `{card_type, tool_name, elapsed_ms, content, data}` | Tool result card (type: tool/error/metrics/calphad/validation/results/plot/plan/info) |
| `ui.cost` | `{input_tokens, output_tokens, turn_cost, session_cost}` | Cost line |
| `ui.prompt` | `{prompt_type, message, choices, tool_name?, tool_args?}` | Interactive prompt (approval/plan_confirm/save_on_exit) |
| `ui.welcome` | `{version, provider, capabilities, tool_count, skill_count, auto_approve}` | Welcome banner data |
| `ui.status` | `{auto_approve, message_count, has_plan}` | Status line update |
| `ui.turn.complete` | `{}` | End of turn |
| `ui.session.list` | `{sessions: [...]}` | Session list response |

### Frontend → Backend Messages

| Method | Params | Purpose |
|---|---|---|
| `init` | `{provider?, auto_approve?, resume?}` | Initialize backend |
| `input.message` | `{text}` | User input |
| `input.command` | `{command}` | Slash command |
| `input.prompt_response` | `{prompt_type, response}` | Answer to ui.prompt |
| `input.load_session` | `{session_id}` | Load saved session |

---

## 2. Project Structure

```
PRISM/
├── app/
│   ├── backend/                  # NEW: JSON-RPC server + UIEmitter
│   │   ├── __init__.py
│   │   ├── __main__.py           # python -m app.backend
│   │   ├── server.py             # JSON-RPC stdio dispatcher
│   │   ├── protocol.py           # Event type definitions (source of truth)
│   │   ├── ui_emitter.py         # AgentCore events → ui.* protocol events
│   │   └── handler.py            # Routes incoming methods to logic
│   ├── cli/                      # Rich frontend (--classic)
│   │   ├── tui/                  # Cards, stream, theme (unchanged renderers)
│   │   └── slash/                # Thin wrappers (logic moved to UIEmitter)
│   ├── agent/                    # AgentCore, backends, events (unchanged)
│   └── _bin/                     # Compiled Ink binary (per platform)
│       └── prism-tui
│
├── frontend/                     # NEW: TypeScript Ink app
│   ├── package.json
│   ├── tsconfig.json
│   ├── build.ts                  # Bun compile (multi-platform)
│   ├── src/
│   │   ├── index.tsx             # Entry: spawns Python backend
│   │   ├── app.tsx               # Root <App/>, Static/Live split
│   │   ├── bridge/
│   │   │   ├── client.ts         # JSON-RPC client over stdio
│   │   │   ├── types.ts          # Generated from protocol.py
│   │   │   └── events.ts         # Event parser + dispatcher
│   │   ├── components/
│   │   │   ├── Welcome.tsx       # Crystal mascot banner
│   │   │   ├── Prompt.tsx        # User input (❯)
│   │   │   ├── StreamingText.tsx # Live streaming markdown
│   │   │   ├── ToolCard.tsx      # All card types (switch on card_type)
│   │   │   ├── CostLine.tsx      # Token + cost display
│   │   │   ├── Spinner.tsx       # Tool execution spinner
│   │   │   ├── ApprovalPrompt.tsx
│   │   │   ├── PlanCard.tsx
│   │   │   ├── StatusLine.tsx
│   │   │   └── SessionList.tsx
│   │   ├── hooks/
│   │   │   ├── useBackend.ts     # Manage backend connection
│   │   │   ├── useStreaming.ts   # Accumulate TextDelta → render
│   │   │   └── useSession.ts    # Session cost, message count
│   │   └── theme.ts             # Colors, icons (mirrors theme.py)
│   └── dist/                     # Build output (gitignored)
└── pyproject.toml
```

### Type Synchronization

`app/backend/protocol.py` is canonical. A codegen script produces TypeScript types:

```bash
python -m app.backend.protocol --emit-ts > frontend/src/bridge/types.ts
```

---

## 3. Ink Component Tree

### Static/Live Split (Claude Code pattern)

```
┌──────────────────────────────────────────────────┐
│  <Static items={history}>     ← frozen history   │
│    <Welcome />                                    │
│    <InputCard />                                  │
│    <StreamingText /> (frozen)                     │
│    <ToolCard />                                   │
│    <CostLine />                                   │
│    ...                                            │
├──────────────────────────────────────────────────┤
│  <LiveArea>                   ← active area      │
│    <Spinner /> or <StreamingText /> (live)        │
│    <StatusLine />                                 │
│    <Prompt />                                     │
└──────────────────────────────────────────────────┘
```

### Component Mapping

| Rich Function | Ink Component |
|---|---|
| `show_welcome()` | `<Welcome />` |
| `render_input_card()` | `<InputCard />` |
| `Rich Live + Markdown()` | `<StreamingText />` |
| `render_tool_result()` dispatch | `<ToolCard type={card_type} />` |
| `render_cost_line()` | `<CostLine />` |
| `Spinner` class | `<Spinner />` |
| `render_plan_card()` | `<PlanCard />` |
| `ask_approval()` | `<ApprovalPrompt />` |
| `render_status_line()` | `<StatusLine />` |
| `get_user_input()` | `<Prompt />` |

---

## 4. Python Backend Server

### UIEmitter — The Shared Brain

Consumes `AgentCore.process_stream()`, emits `ui.*` protocol events. Contains ALL presentation logic:

- Plan detection (`<plan>` tag parsing)
- Card type detection (metrics/calphad/error/etc.)
- Cost accumulation (session total)
- Tool verb mapping
- Slash command dispatch
- Large result handling (truncation)
- Approval gating

Both frontends consume UIEmitter:
- **TS frontend**: via stdio JSON-RPC (server.py wraps UIEmitter)
- **Rich frontend**: via direct Python import (stream.py calls UIEmitter)

### stdio Server

```python
# app/backend/server.py — thin JSON-RPC loop
class StdioServer:
    def run(self):
        for line in sys.stdin:
            msg = json.loads(line)
            # Route to UIEmitter methods
            # Emit results as JSON on stdout
```

---

## 5. Build & Packaging

### Platform Wheels

```
dist/
├── prism_platform-2.5.0b1-py3-none-macosx_11_0_arm64.whl   (~35MB)
├── prism_platform-2.5.0b1-py3-none-macosx_11_0_x86_64.whl  (~35MB)
├── prism_platform-2.5.0b1-py3-none-manylinux_2_17_x86_64.whl (~40MB)
├── prism_platform-2.5.0b1-py3-none-manylinux_2_17_aarch64.whl (~40MB)
└── prism_platform-2.5.0b1-py3-none-any.whl                  (~2MB, no binary)
```

Pure-Python fallback wheel auto-falls back to `--classic` if binary missing.

### CI Pipeline

1. `bun build --compile --target=<platform>` per matrix entry
2. Copy binary to `app/_bin/prism-tui`
3. `python -m build` → retag wheel with platform tag
4. Upload all wheels to GitHub release / PyPI

### Entry Point

```python
# app/cli/main.py
def cli(classic, ...):
    if classic or not has_tui_binary():
        _run_classic_repl(...)     # Existing Rich REPL
    else:
        _run_ink_repl(binary, ...) # os.execvp compiled binary
```

---

## 6. HTTP+SSE Layer (Future)

Not in v2.5.0 scope. Architecture slot designed for future web UI:

```
              ┌────────────────────┐
              │     UIEmitter      │
              │  (shared logic)    │
              └──────┬─────────────┘
                     │
        ┌────────────┼────────────────┐
        │            │                │
   stdio JSON-RPC  direct import   HTTP+SSE
        │            │                │
   ┌────▼──────┐ ┌──▼─────────┐ ┌────▼──────────┐
   │ Ink TUI   │ │ Rich REPL  │ │ Web UI        │
   │ `prism`   │ │ `--classic`│ │ `serve --ui`  │
   └───────────┘ └────────────┘ └───────────────┘
```

Three renderers, zero duplicated logic.

---

## 7. Migration Path

### Phase 1: Extract UIEmitter (Python only)
Move presentation logic from `stream.py` → `ui_emitter.py`. Rich REPL keeps working identically. All 873 tests must pass.

**Changed**: `app/backend/` (new), `app/cli/tui/stream.py` (simplified), `app/cli/tui/app.py` (use emitter)
**Unchanged**: `cards.py`, `theme.py`, `prompt.py`, `welcome.py`, `agent/core.py`, `agent/events.py`

### Phase 2: Add stdio server
Wire UIEmitter to stdin/stdout JSON-RPC. Testable without TS:
```bash
echo '{"jsonrpc":"2.0","method":"init","params":{},"id":1}' | python -m app.backend
```

### Phase 3: Build Ink components (3a→3l)
Incremental, one component at a time:
3a) bridge/client.ts, 3b) Prompt, 3c) StreamingText, 3d) ToolCard,
3e) Spinner, 3f) CostLine, 3g) Welcome, 3h) ApprovalPrompt,
3i) PlanCard, 3j) StatusLine, 3k) slash commands, 3l) sessions

### Phase 4: Build & package
CI pipeline, platform wheels, binary discovery, `--classic` flag.

### Phase 5: HTTP+SSE (future, post-v2.5.0)

---

## Adding New Features Checklist

1. Add event type in `app/backend/protocol.py`
2. Add emission logic in `app/backend/ui_emitter.py`
3. Add Ink renderer in `frontend/src/components/`
4. Add Rich renderer in `app/cli/tui/cards.py`
5. Run `python -m app.backend.protocol --emit-ts` to regenerate types
6. Test both modes: `prism` and `prism --classic`
