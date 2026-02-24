# PRISM Textual TUI Design

**Date:** 2026-02-24
**Status:** Approved
**Replaces:** Current Rich + prompt_toolkit REPL (`app/agent/repl.py`)

## Summary

Full rewrite of the PRISM REPL as a Textual app. Persistent screen zones, card-based output stream, modal overlays, proper key bindings, and a hex crystal mascot. The agent core, event system, and all tools remain untouched — only the rendering layer changes.

## Decisions

| Decision | Choice |
|----------|--------|
| Framework | Textual (replaces Rich console + prompt_toolkit) |
| Mascot | C4 glowing hex crystal (4 lines) |
| Layout | Header → Stream → AgentStatus/Tasks → InputBar |
| Output model | Single chronological stream with typed cards |
| Truncation | Configurable, default 6 lines |
| Expansion | Modal overlay (Ctrl+O), Escape to dismiss |
| Key bindings | Moderate set, separate keymap file |
| Theme | Dark-only |
| Box style | Curved corners (Textual rounded border) |
| Input behavior | Pinned at bottom; on Enter moves to stream as InputCard |
| Error handling | ErrorRetryCard with [r]etry / [s]kip / [Ctrl+O] details |

## File Structure

```
app/
├── tui/
│   ├── __init__.py
│   ├── app.py              # PrismApp(App) — main entry, compose(), CSS
│   ├── widgets/
│   │   ├── __init__.py
│   │   ├── header.py       # C4 hex crystal mascot + command hints + caps
│   │   ├── stream.py       # ScrollableContainer for card stream
│   │   ├── cards.py        # All card widgets (see Card Types below)
│   │   ├── status_bar.py   # AgentStatus (spinner+step) + TaskTracker
│   │   └── input_bar.py    # Text input widget, pinned bottom
│   ├── screens/
│   │   ├── __init__.py
│   │   └── overlay.py      # FullContentScreen — modal for expanded view
│   ├── keymap.py            # Key binding definitions + command map
│   ├── theme.py             # Colors, styles, dark-only constants
│   └── config.py            # Truncation lines, configurable settings
```

### Files NOT touched

- `app/agent/core.py` — AgentCore, TAOR loop, tool execution
- `app/agent/events.py` — Event types (TextDelta, ToolCallStart, etc.)
- `app/agent/autonomous.py` — Non-interactive mode
- `app/agent/memory.py` — Session persistence
- `app/agent/scratchpad.py` — Execution log
- `app/agent/spinner.py` — Verb map reused, threading removed (Textual async)
- `app/tools/` — All tools unchanged
- `app/skills/` — All skills unchanged
- `app/ml/` — ML pipeline unchanged
- `app/simulation/` — CALPHAD bridge unchanged
- `app/validation/` — Rules unchanged

### Backward compatibility

- `app/agent/repl.py` stays as-is (legacy fallback)
- `cli.py` gets `--tui` flag (default on) and `--classic` for old REPL

## Screen Layout

```
┌──────────────────────────────────────────────────────────────┐
│  HeaderWidget (dock: top, height: 4)                         │
│    ⬡ ⬡ ⬡                                                    │
│    ⬡ ⬢ ⬢ ⬢ ⬡  ━━━━━━━━━━━━━━  /help /tools /skills         │
│    ⬡ ⬢ ⬢ ⬢ ⬡  ━━━━━━━━━━━━━━  /scratchpad /status           │
│    ⬡ ⬡ ⬡           MARC27 • Claude • ML ● CALPHAD ○         │
├──────────────────────────────────────────────────────────────┤
│  StreamView (flex: 1, scrollable)                            │
│                                                              │
│  ╭─ input ────────────────────────────────────────────╮      │
│  │ Find W-Rh alloys that are stable                   │      │
│  ╰────────────────────────────────────────────────────╯      │
│                                                              │
│  ╭─ output ───────────────────────────────────────────╮      │
│  │ I'll search for tungsten-rhenium alloys across     │      │
│  │ OPTIMADE and Materials Project...                  │      │
│  │ ...                                     [Ctrl+O]  │      │
│  ╰────────────────────────────────────────────────────╯      │
│                                                              │
│  ╭─ tool ── search_optimade ────────────── 17.0s ─────╮      │
│  │ ✔ 49 results from MP, OQMD, COD, JARVIS           │      │
│  │ ⚠ 2 providers failed     [r]etry       [Ctrl+O]  │      │
│  ╰────────────────────────────────────────────────────╯      │
│                                                              │
├──────────────────────────────────────────────────────────────┤
│  AgentStatus (dock: bottom, height: auto, max: 12)           │
│                                                              │
│  ⠋ Thinking...                                               │
│     └── Searching OPTIMADE databases        [Ctrl+O]         │
│     └── Next: Parse results                 [dim/grey]       │
│                                                              │
│  ◉ Tasks [2/5]                              [Ctrl+Q]         │
│    ✔ Search Materials Project                                │
│    ▸ Running CALPHAD phase calc...                           │
│    ○ Predict mechanical properties                           │
│    ○ Validate candidates                                     │
│    ○ Generate report                                         │
│                                                              │
├──────────────────────────────────────────────────────────────┤
│  InputBar (dock: bottom, height: 3)                          │
│  ❯ _                                                         │
╰──────────────────────────────────────────────────────────────╯
```

### Zone behaviors

- **HeaderWidget**: Static. Shows mascot, available commands, provider, capabilities.
- **StreamView**: Auto-scrolls to bottom on new cards. Pauses auto-scroll when user scrolls up. Resumes on scroll-to-bottom or new user input.
- **AgentStatus**: Two sub-sections:
  - *Spinner/Step*: What the agent is doing RIGHT NOW (ephemeral, changes every few seconds). Current action + next action (greyed out).
  - *Task tracker*: Higher-level plan items. Done tasks move down, upcoming shown on top. Truncates after ~5 visible, collapsed with count.
- **InputBar**: Always at the very bottom. On Enter, message becomes an InputCard in the stream, bar clears. Captures focus by default, returns focus after modal dismiss.

## Card Types

### InputCard
- **Border:** cyan, curved
- **Trigger:** User presses Enter
- **Content:** Full user message in a box

### OutputCard
- **Border:** dim white, curved
- **Trigger:** Agent text response (TextDelta → TurnComplete)
- **Content:** Markdown-rendered agent text, truncated to N lines
- **Expand:** Ctrl+O → modal with full Markdown

### ToolCard
- **Border:** green, curved
- **Title:** tool name + elapsed time
- **Content:** Success summary (1-2 lines)
- **Expand:** Ctrl+O → full result dict

### ErrorRetryCard
- **Border:** orange (partial failure) or red (total failure), curved
- **Title:** tool name + "FAILED" or "PARTIAL"
- **Content:** Summary of what failed and what succeeded
- **Actions:** `[r]` retry failed providers, `[s]` skip, `[Ctrl+O]` full error details
- **Requires:** New retry mechanism in tool execution layer

### ApprovalCard
- **Border:** orange, curved
- **Content:** Tool name, truncated args, approval reason
- **Actions:** `[y]` approve, `[n]` deny, `[a]` always approve this tool

### PlanCard
- **Border:** magenta, curved
- **Content:** Markdown-rendered plan from `<plan>` tags
- **Actions:** `[y]` execute, `[n]` reject

### MetricsCard
- **Border:** blue, curved
- **Trigger:** ML training tool returns metrics dict
- **Content:** Property name, mini table (MAE, RMSE, R², train/test split), plot path
- **Expand:** Ctrl+O → full metrics + open plot

### CalphadCard
- **Border:** blue, curved
- **Trigger:** CALPHAD bridge tool returns phase data
- **Content:** System, T/P conditions, phase fractions mini table, Gibbs energy
- **Expand:** Ctrl+O → full data points

### ValidationCard
- **Border:** severity-colored (red/yellow/blue gradient), curved
- **Trigger:** validate_dataset or review_dataset returns findings
- **Content:** Quality score in title, severity-grouped findings (truncated)
- **Expand:** Ctrl+O → all findings

### ResultsTableCard
- **Border:** dim, curved
- **Trigger:** Tool returns `results` list with >3 entries
- **Content:** First 3 rows as mini table + "N more rows"
- **Actions:** `[Ctrl+O]` full scrollable table, `[e]` export CSV

### PlotCard
- **Border:** green, curved
- **Trigger:** Visualization tool returns PNG path
- **Content:** Plot description, file path
- **Expand:** Ctrl+O → inline terminal image (textual-image/term-image if supported) or system open

## Mascot: C4 Glowing Hex Crystal

```
    ⬡ ⬡ ⬡
  ⬡ ⬢ ⬢ ⬢ ⬡  ━━━━━━━━━━━━━━
  ⬡ ⬢ ⬢ ⬢ ⬡  ━━━━━━━━━━━━━━
    ⬡ ⬡ ⬡
```

- Outer hexagons: dim purple (#555577 → #7777aa), fading glow effect
- Inner hexagons: bright white (#ccccff → #ffffff), crystal core
- Rainbow rays: 10-color VIBGYOR gradient (bold)

## Key Bindings

Stored in `app/tui/keymap.py` as a separate map.

| Key | Action | Context |
|-----|--------|---------|
| `Enter` | Submit input | InputBar focused |
| `Ctrl+O` | Open modal overlay (full content) | Any card focused / global |
| `Ctrl+Q` | View task queue detail | Global |
| `Ctrl+S` | Save session | Global |
| `Ctrl+L` | Clear stream | Global |
| `Ctrl+P` | Toggle plan mode | Global |
| `Ctrl+T` | List tools | Global |
| `Ctrl+C` | Cancel current operation | Global |
| `Ctrl+D` | Exit | Global |
| `Escape` | Dismiss modal / cancel | Modal open |
| `Up/Down` | Input history | InputBar focused |
| `r` | Retry failed tool | ErrorRetryCard focused |
| `s` | Skip failed tool | ErrorRetryCard focused |
| `y/n/a` | Approve/deny/always | ApprovalCard focused |
| `e` | Export to CSV | ResultsTableCard focused |

## Theme

Dark-only. Constants in `app/tui/theme.py`.

| Element | Color |
|---------|-------|
| Surface background | #1a1a2e |
| Card background | #16213e |
| Primary text | #e0e0e0 |
| Dim text | #888888 |
| Success | #00cc44 |
| Warning/approval | #d29922 |
| Error | #cc555a |
| Info | #0088ff |
| Magenta/primary accent | #bb86fc |
| Cyan (input) | #00cccc |
| Crystal outer | #555577 → #7777aa |
| Crystal inner | #ccccff → #ffffff |
| Rainbow | #ff0000 → #8b00ff (10 stops) |

## Configuration

`app/tui/config.py` — user-overridable via `~/.prism/config.toml`.

```python
DEFAULTS = {
    "truncation_lines": 6,        # Lines before card truncates
    "max_status_tasks": 5,        # Visible tasks before collapse
    "auto_scroll": True,          # Auto-scroll stream on new cards
    "image_preview": "system",    # "inline" | "system" | "none"
}
```

## Known Issues / Follow-ups

1. **Context bloat** — Large result sets (49+ dicts) are currently dumped into LLM context. Need a summarization/pagination strategy so the agent gets a digest, not raw data. Separate design needed.
2. **OPTIMADE progress bars** — The pyoptimade library prints its own Rich progress bars to stdout. Need to capture/redirect these into the StatusBar area or suppress them.
3. **No retry logic** — ErrorRetryCard UI is designed but the tool execution layer needs a retry mechanism. Tools currently return `{"error": ...}` with no callback to retry.
4. **Image inline display** — Terminal image rendering is spotty. Default to system open, inline as opt-in.
5. **Textual dependency** — New required dependency. Add to `pyproject.toml` extras or make it required.

## Integration with Agent Core

The Textual app consumes the same event stream as the current REPL:

```
AgentCore.process_stream(message)
  → yields TextDelta, ToolCallStart, ToolCallResult, ToolApprovalRequest, TurnComplete
```

`PrismApp` runs the stream in a Textual worker thread, maps events to card widgets:

| Event | Card |
|-------|------|
| TextDelta (accumulated) → TurnComplete | OutputCard |
| ToolCallStart | Spinner update in AgentStatus |
| ToolCallResult (success) | ToolCard, MetricsCard, CalphadCard, ValidationCard, ResultsTableCard, or PlotCard (based on result shape) |
| ToolCallResult (error) | ErrorRetryCard |
| ToolApprovalRequest | ApprovalCard |
| `<plan>` tags in text | PlanCard |

Card type selection uses result shape detection:
- Has `metrics` + `algorithm` → MetricsCard
- Has `phases_present` + `gibbs_energy` → CalphadCard
- Has `findings` + `quality_score` → ValidationCard
- Has `results` list with >3 items → ResultsTableCard
- Has `filename` ending in `.png` → PlotCard
- Has `error` key → ErrorRetryCard
- Default → ToolCard
