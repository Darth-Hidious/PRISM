# PRISM REPL Redesign — Design Document

## Goal

Redesign the PRISM REPL to match the visual quality of modern CLI coding agents (Claude Code, Codex, Gemini CLI, OpenCode/Crush) while establishing PRISM's own brand identity through a VIBGYOR prism motif.

## Architecture

Inline REPL (not fullscreen TUI). Keep the current stack: Rich for rendering + prompt_toolkit for input. No new dependencies. All changes are within `app/agent/repl.py` plus a new `app/agent/spinner.py` module.

## Visual Identity

### Banner

VIBGYOR block-letter "PRISM" rendered with Rich `Text` per-character coloring:
- P = red (`#ff0000`)
- R = orange (`#ff7700`)
- I = yellow (`#ffdd00`)
- S = green (`#00cc44`)
- M = blue (`#0066ff`)

Below the letters: a prism triangle (Unicode box-drawing) with a continuous rainbow gradient bar. Version, provider, and capabilities displayed to the right of the triangle.

### Color Palette

GitHub Dark-inspired:
- Background: terminal default (assume dark)
- Foreground: `#c9d1d9` (Rich default)
- Accent: purple — tool names, prompt character, provider badge
- Approval: amber/gold — tools requiring consent, approval prompts
- Success: green — completed tool checks, ready status
- Dim: gray — timestamps, secondary info, hints

### Prompt Character

`❯` (Unicode heavy right-pointing angle) in purple/accent color, inside a bordered input area rendered via prompt_toolkit's `bottom_toolbar` or a Rich-rendered box before each input cycle.

## Components

### 1. VIBGYOR Banner (`_show_welcome`)

Block letters using a hardcoded 5-line ASCII art string. Each column of characters colored by letter. Prism triangle below with rainbow bar (Rich `Text` with gradient). Info block to the right.

Shown once on startup. Hidden on `/clear`.

### 2. Braille Spinner (`app/agent/spinner.py`)

New module. A background `threading.Thread` that cycles through braille dot characters (`⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`) at ~80ms intervals. Displays context-aware verb based on the active tool:

| Tool | Verb |
|---|---|
| `search_optimade` | "Searching OPTIMADE databases..." |
| `query_materials_project` | "Querying Materials Project..." |
| `predict_property` | "Training ML model..." |
| `calculate_phase_diagram` | "Computing phase diagram..." |
| `literature_search` | "Searching literature..." |
| `run_simulation` | "Running simulation..." |
| default | "Thinking..." |

Spinner color: accent purple. Verb color: dim gray italic.

Interface:
```python
class Spinner:
    def start(self, verb: str = "Thinking..."): ...
    def update(self, verb: str): ...
    def stop(self, result_line: str = ""): ...
```

Uses `rich.live.Live` for flicker-free updates. Stops and clears when tool completes.

### 3. Tool Call Cards

Replace flat `tool_name done (1.2s)` with bordered cards using Rich box-drawing:

```
┌ search_optimade ─────────────────── 1.2s ┐
│ ✓ 14 structures from mp, oqmd, cod       │
└───────────────────────────────────────────┘
```

- Header: tool name (accent purple) + timing (dim)
- Body: check mark (green) + result summary (gray)
- Border: `dim` style (thin)

For approval-required tools, border changes to amber:
```
┌ predict_property ──────────── approval ┐
│ Train random_forest → predict band_gap │
├────────────────────────────────────────┤
│ ? Allow?  [y] yes  [n] no  [a] always │
└────────────────────────────────────────┘
```

The `[a] always` option sets auto-approve for that specific tool for the session.

### 4. Markdown Response Rendering

Replace `sys.stdout.write(event.text)` with buffered Rich `Markdown` rendering. Accumulate text until a natural break (paragraph end, tool call start, turn complete), then render the buffered chunk via `console.print(Markdown(buffer))`.

This gives us: bold, headers, code blocks with syntax highlighting, lists, links — all for free from Rich's existing Markdown support.

### 5. Plan Panel

Keep the existing `<plan>` detection but render with a styled Rich Panel:
- Header: "Plan" with clipboard icon
- Body: numbered steps rendered as Markdown
- Footer: `Execute? [y] yes [n] cancel [e] edit`
- Border: dim, rounded corners

### 6. /help and /status

Keep current implementations but adjust styling:
- `/help`: two-column with bold command names, dim descriptions
- `/status`: green/gray status dots, clean alignment
- `/tools`: approval-required tools highlighted in amber with `★`

## Files to Modify

| File | Changes |
|---|---|
| `app/agent/repl.py` | Banner, spinner integration, tool cards, markdown rendering, input prompt, approval UX |
| `app/agent/spinner.py` | **New file** — `Spinner` class with braille animation |
| `tests/test_repl.py` | Update tests for new rendering |
| `tests/test_repl_skills.py` | Minor adjustments if command output format changes |

## What We're NOT Doing

- No fullscreen TUI (no Textual, no alternate screen buffer)
- No new dependencies beyond what's already in pyproject.toml
- No React/Ink port
- No theme system (single dark theme, matches terminal)
- No mouse support
- No tab title changes
- No file explorer sidebar

## Testing

- Spinner: test start/stop/update without actual threading (mock `Live`)
- Banner: test that `_show_welcome` produces output containing "PRISM" and version
- Tool cards: test that `ToolCallResult` events render with timing
- Approval: test that approval-required tools show the `[y/n/a]` prompt
- Markdown: test that accumulated text is rendered via `Markdown` not raw stdout

## Concept Mockup

See `docs/assets/repl-concept.html` — open in browser for the full visual reference.
