# PRISM TUI Requirements — from user session 2026-04-08

## Critical UX Requirements

### 1. Command autocomplete
- When user types `/`, show a dropdown/popup of all available commands
- As they type more (e.g. `/m`), filter to matching commands
- Commands include: slash commands, workflows (top-level), plugins, MCP servers
- Tab or Enter to select, Esc to dismiss

### 2. Message ordering + coloring
- Messages are chronologically ordered (user → agent → user → agent)
- User messages: distinct color (e.g. cyan prefix or right-aligned)
- Agent messages: white/default
- Each message clearly separated — not a wall of text

### 3. Streaming text
- Agent responses stream character by character (from `ui.text.delta` events)
- Visible typing indicator while streaming
- Text appears incrementally, not all at once after completion

### 4. Tool call visibility
- When agent calls a tool, show it inline in the chat:
  - Spinner + tool name while running
  - Result card when done (expandable)
  - Error card if failed (red)
- Show what data is being fetched, from where
- Status: "searching knowledge graph...", "calling search_materials...", "executing python..."

### 5. Status indicators
- Status bar should show:
  - Connection state (connected/disconnected)
  - Agent state (idle/thinking/tool-calling/streaming)
  - Current model
  - Token count / cost
  - Turn count
- Spinner/animation while agent is working

### 6. Table rendering
- Agent output often contains markdown tables
- These must render as proper terminal tables (box drawing chars)
- Not raw `| col1 | col2 |` text

### 7. Activity bar / Sidebar
- VS Code style: icon strip on far left, panel next to it
- 9 workspaces: Chat, Explorer, Models, Compute, Mesh, Workflows, Marketplace, Data, Settings
- Each workspace shows contextual actions and live data
- Number keys (1-9) to switch workspaces

### 8. Workspace content separation
- Chat canvas ONLY shows conversation (messages, tool cards, streaming)
- Models, settings, mesh etc. open in their own workspace — NOT as overlays on chat
- Only approval prompts are modal overlays (they block input)

### 9. MCP servers
- Local MCP servers should appear in the sidebar
- Show which MCP tools are available
- MCP tool calls show in chat same as regular tool calls

### 10. Workflows as top-level commands
- Workflow names appear in the autocomplete alongside slash commands
- e.g. typing `/explore` shows the explore workflow
- Workflow execution shows step-by-step progress in the chat

## Technical Notes

- All data comes from the backend via JSON-RPC (`prism backend`)
- Backend events: `ui.text.delta` (streaming), `ui.tool.start` (tool running), `ui.card` (tool result), `ui.prompt` (approval), `ui.view` (panels), `ui.cost` (tokens), `ui.status` (state)
- Frontend sends: `input.message` (chat), `input.command` (slash), `approval.respond` (y/n/a/b)
- See `docs/FRONTEND_PROTOCOL.md` for full spec
