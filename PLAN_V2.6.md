# PRISM v2.6 — Agent Architecture & UX Plan

**Date:** 2026-04-02
**Status:** Planning
**Goal:** Close the UX and architecture gaps to make PRISM feel production-grade

---

## Tier 1 — Must Have (blocks user experience)

These are the things that make someone download PRISM and think "this doesn't work properly."

### 1.1 Session Persistence & Resume
**Why:** Users lose their conversation when they close the terminal. Every serious CLI agent persists sessions.
**What:**
- JSONL session files at `~/.prism/sessions/`
- `prism` resumes last session by default
- `/session list` — show saved sessions
- `/session resume <id>` — load a previous session
- `/session fork` — branch current session
- Session metadata: created_at, turn_count, model, compaction history
- File rotation at 256KB (max 3 backups)
**Where:** New `app/agent/session_store.py`, wire into `core.py`
**Effort:** 4-6 hours

### 1.2 Intelligent Compaction
**Why:** Our current compaction is flat text. When context gets long, the agent forgets what it was doing. The compaction summary needs to capture pending work, key files, and timeline.
**What:**
- Structured summary: scope, tools used, recent requests, pending work, key files, timeline
- Pending work inference (scan for "todo", "next", "remaining")
- Key file extraction (paths mentioned in conversation)
- Re-compaction: merge old + new summary, don't repeat
- Continuation instruction: "Resume directly, do not acknowledge the summary"
- `/compact` slash command
**Where:** Rewrite `app/agent/transcript.py` compact method
**Effort:** 3-4 hours

### 1.3 Slash Commands (Core 7)
**Why:** Users need to control the session without leaving the REPL.
**What:**
- `/help` — list all commands
- `/status` — turns, model, permissions, usage, cost
- `/compact` — trigger manual compaction
- `/model [name]` — show or switch model
- `/clear` — reset conversation (require confirmation)
- `/cost` — show token usage and estimated cost
- `/session` — list/resume/fork sessions
**Where:** `app/cli/slash/handlers.py` (some exist, need completion)
**Effort:** 2-3 hours

### 1.4 Permission Modes (3-tier)
**Why:** "auto-approve everything" or "ask for everything" isn't enough. Users need read-only mode for browsing, workspace-write for editing, and danger mode for destructive ops.
**What:**
- `ReadOnly` — search, read, query tools only
- `WorkspaceWrite` — file editing, data export, code execution
- `DangerFullAccess` — delete, drop, system commands
- Each tool tagged with minimum required permission
- `/permissions [mode]` — show or switch
- `--permission-mode` CLI flag
**Where:** Extend `app/agent/permissions.py`, tag tools in `app/tools/*.py`
**Effort:** 3-4 hours

---

## Tier 2 — Should Have (quality of life)

These make PRISM feel polished. Not blocking but noticeable when missing.

### 2.1 Streaming Markdown Buffer
**Why:** Code blocks and tables break mid-render when LLM is still streaming.
**What:**
- Buffer streaming text until safe boundary (blank line, closed code fence)
- Don't flush mid-code-block
- State machine: normal → in_code_fence → normal
**Where:** `app/backend/ui_emitter.py` streaming path
**Effort:** 2-3 hours

### 2.2 Tool Input Validation
**Why:** LLM sometimes sends malformed tool args. We should catch before execution, not crash during.
**What:**
- Validate tool_args against tool's input_schema (JSON Schema)
- Return descriptive error to LLM if validation fails
- Simple validation: required fields, type checks
**Where:** `app/agent/core.py` `_execute_tool_with_hooks`
**Effort:** 2 hours

### 2.3 Hook Shell Execution
**Why:** Our hooks are Python callbacks. Real extensibility means users write shell scripts that run as hooks.
**What:**
- `.prism/hooks/pre_tool_use.sh` — runs before tool calls
- `.prism/hooks/post_tool_use.sh` — runs after
- JSON payload on stdin, JSON response on stdout
- Exit code 0 = allow, 2 = deny
- Environment vars: HOOK_TOOL_NAME, HOOK_TOOL_INPUT
**Where:** Extend `app/agent/hooks.py` with `ShellHookRunner`
**Effort:** 3-4 hours

### 2.4 Multiline Input
**Why:** Users can't write multi-line prompts or paste code blocks.
**What:**
- Ctrl+J or Shift+Enter inserts newline without submitting
- Enter submits (same as now)
**Where:** `frontend/src/components/Prompt.tsx`
**Effort:** 1-2 hours

---

## Tier 3 — Nice to Have (polish)

### 3.1 Model Alias Resolution
- `opus` → `claude-opus-4-6`, `sonnet` → `claude-sonnet-4-20250514`
- `fast` → cheapest model, `best` → most capable
**Effort:** 1 hour

### 3.2 Output Format Toggle
- `--output-format json` for piped/scripted usage
- NDJSON for streaming
**Effort:** 2 hours

### 3.3 Command Palette (Ctrl+K)
- Fuzzy-searchable list of all commands and tools
**Effort:** 3-4 hours

### 3.4 Session Export
- `prism session export <id> --format md` → markdown transcript
- `prism session export <id> --format json` → structured data
**Effort:** 2 hours

---

## Execution Order

```
Week 1: Tier 1 (sessions, compaction, slash commands, permissions)
Week 2: Tier 2 (streaming buffer, validation, shell hooks, multiline)
Week 3: Tier 3 (aliases, output format, command palette, export)
```

Each tier gets its own tag:
- v2.6.0-alpha after Tier 1
- v2.6.0-beta after Tier 2
- v2.6.0 final after Tier 3

---

## Definition of Done

For EACH item:
1. Code written
2. Smoke tested locally (manual verification)
3. cargo check/clippy/fmt pass
4. TypeScript compiles (if frontend changed)
5. Python imports work (if agent changed)
6. Committed with descriptive message
7. CI passes

For release:
1. All Tier 1 items done
2. Full smoke test (19 CLI + 7 Python + 406 Rust)
3. install.sh tested on fresh environment
4. README updated
5. Tag + GitHub release
