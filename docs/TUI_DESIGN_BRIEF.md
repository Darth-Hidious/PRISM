# PRISM TUI Design Brief

> For an AI designer to create the complete terminal UI layout and Rust/Ratatui implementation.

## What PRISM Is

PRISM is an AI-native materials discovery CLI platform. Scientists use it to search knowledge graphs (211K+ entities), run AI agents, execute compute jobs on GPUs, and orchestrate research workflows. It connects to the MARC27 platform API.

## Tech Stack

- **Framework:** Ratatui 0.26 + Crossterm 0.27 (Rust)
- **Backend:** JSON-RPC over stdio — the TUI spawns `prism backend` and communicates via JSON-RPC
- **Design language:** Dark terminal aesthetic, cyan/white primary, RGB color support
- **Boot screen:** Already built — animated prism with refraction beams (see `crates/cli/src/tui.rs`)

## What the Backend Sends (12 event types)

The TUI receives these JSON-RPC notifications from the backend. Each one needs a visual representation.

### Streaming text
```json
{"method": "ui.text.delta", "params": {"text": "chunk of text"}}
{"method": "ui.text.flush", "params": {"text": ""}}
```
Incremental LLM response. Append to chat area character by character.

### Tool execution
```json
{"method": "ui.tool.start", "params": {"tool_name": "search_materials", "call_id": "abc", "verb": "Running search_materials", "preview": "query=nickel"}}
{"method": "ui.card", "params": {"card_type": "results", "tool_name": "search_materials", "elapsed_ms": 1234, "content": "Found 47 materials", "data": {}}}
```
Show a spinner when tool starts, replace with result card when done. Cards should be compact — one line summary, expandable.

### Approval prompt
```json
{"method": "ui.prompt", "params": {"prompt_type": "approval", "message": "Allow execute_python?", "choices": ["y","n","a","b"], "tool_name": "execute_python", "tool_args": {"code": "import pandas..."}, "permission_mode": "workspace-write"}}
```
Modal overlay. Show tool name, args preview, permission level. Keys: y=allow, n=deny, a=allow-all, b=block-all.

### Tabbed views
```json
{"method": "ui.view", "params": {"view_type": "models", "title": "Hosted Models", "tabs": [{"id": "summary", "title": "Summary", "body": "523 models..."}, {"id": "anthropic", "title": "anthropic (9)", "body": "..."}], "selected_tab": "summary"}}
```
Used by slash commands: `/tools`, `/models`, `/deploy`, `/discourse`, `/status`, `/context`, `/permissions`, `/help`, `/config`, `/usage`. Each has different tab count and content. Needs tab switching (number keys + tab key) and scrolling (arrow keys).

### Session state
```json
{"method": "ui.welcome", "params": {"version": "2.6.1", "tool_count": 106, "session_id": "abc123"}}
{"method": "ui.status", "params": {"auto_approve": false, "message_count": 5, "model": "claude-sonnet-4-6", "session_mode": "chat", "has_plan": false}}
{"method": "ui.turn.complete", "params": {}}
```

### Cost tracking
```json
{"method": "ui.cost", "params": {"input_tokens": 1500, "output_tokens": 200, "turn_cost": 0.003, "session_cost": 0.018}}
```

### Permissions
```json
{"method": "ui.permissions", "params": {"mode": "chat", "auto_approved": [...], "blocked": [...], "approval_required": [...], "read_only": [...], "workspace_write": [...], "full_access": [...]}}
```

### Session list
```json
{"method": "ui.session.list", "params": {"sessions": [{"session_id": "abc", "turn_count": 5, "model": "claude-sonnet-4-6"}]}}
```

## What the TUI Sends (3 request types)

```json
{"method": "input.message", "params": {"text": "user message"}}
{"method": "input.command", "params": {"command": "/models", "silent": false}}
{"method": "approval.respond", "params": {"response": "y"}}
```

## Layout Requirements

### Primary layout — sidebar + main + status

```
┌──────────┬───────────────────────────────────────────┐
│          │ Title / breadcrumb                         │
│ Sidebar  ├───────────────────────────────────────────┤
│          │                                           │
│ Sessions │ Main content area                         │
│ Tools    │ (chat messages, tool cards, streaming)    │
│ Models   │                                           │
│ Compute  │                                           │
│ Mesh     │                                           │
│          │                                           │
│          ├───────────────────────────────────────────┤
│          │ Input prompt                              │
├──────────┴───────────────────────────────────────────┤
│ Status bar                                           │
└──────────────────────────────────────────────────────┘
```

### Sidebar sections (collapsible)

- **Session** — current session info, resume/fork
- **Tools** — 106 tools grouped by permission (read/write/full)
- **Models** — current model + quick switch
- **Compute** — GPU status, running jobs
- **Mesh** — connected peers
- **Workflows** — available workflows

Sidebar should be toggleable (a key to show/hide). When hidden, full width goes to main content.

### Main content area states

1. **Chat** (default) — scrollable message history with:
   - User messages (right-aligned or prefixed)
   - Assistant text (streaming, left-aligned)
   - Tool cards (inline, compact, expandable)
   - Cost line after each turn

2. **View panel** (triggered by slash commands) — tabbed panel that overlays or replaces chat:
   - Tab bar at top
   - Scrollable body
   - Footer with keybindings
   - Esc to close and return to chat

3. **Approval prompt** — modal overlay:
   - Tool name + description
   - Args preview (scrollable if long)
   - Permission level badge
   - Choice keys at bottom

4. **Workflow progress** — when a workflow is running:
   - Step list with status icons (✓ completed, ⠋ running, · pending, ✗ failed)
   - Current step highlighted
   - Elapsed time
   - Output preview

### Input area

- Single-line input with prompt prefix (`prism λ `)
- `/` prefix enters command mode (show command palette/autocomplete)
- Up/down for history
- Enter to send

### Status bar (bottom)

Always visible. Shows:
- Session mode (chat/plan)
- Message count
- Tool count
- Current model
- Session cost
- Turn status (idle/working/approval pending)

## Design Preferences

- **Dark background** — #0a0a0a or pure black
- **Primary accent** — cyan (#00ffff)
- **Secondary accent** — green (#00ff00) for success, red for errors
- **Text** — white (#e5e5e5) for primary, gray (#666) for secondary
- **Borders** — subtle (#333), single-line box drawing
- **No emoji** — use Unicode box drawing, bullets (·), checkmarks (✓), spinners (⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏)
- **Compact** — minimize vertical space, information dense
- **Responsive** — handle terminal resize, minimum 80x24

## Key Files

| File | Purpose |
|------|---------|
| `crates/cli/src/tui.rs` | Current TUI code (boot sequence + splash) |
| `crates/cli/src/main.rs` | CLI entry point, `mod tui` declared |
| `docs/FRONTEND_PROTOCOL.md` | Full JSON-RPC protocol reference |
| `docs/WORKFLOW_GUIDE.md` | Workflow engine reference |

## Ratatui Components to Use

- `Layout` with `Direction::Horizontal` for sidebar + main
- `Tabs` widget for view panels
- `Paragraph` with `Wrap` for chat messages
- `List` / `Table` for tool cards, session list
- `Block` with `Borders` for all panels
- `Gauge` or custom for workflow progress
- `Span` with `Style::fg(Color::Rgb(...))` for all coloring

## What to Generate

1. **Layout system** — the 4-zone split (sidebar, title, content, input, status) with resize handling
2. **Chat renderer** — streaming text, tool cards, cost lines
3. **View panel** — tabbed overlay with scroll
4. **Approval modal** — centered overlay
5. **Sidebar** — collapsible sections
6. **Input handler** — text input, command mode, history
7. **Status bar** — live state display
8. **Event loop** — poll backend JSON-RPC + keyboard input, dispatch to correct component

Generate complete Rust code using Ratatui 0.26 + Crossterm 0.27. The backend communication layer (`prism backend` spawning + JSON-RPC parsing) is already documented in `docs/FRONTEND_PROTOCOL.md`.
