# PRISM CLI UI Polish — Design Document

**Version:** 2.5.0
**Date:** 2026-02-25
**Status:** Approved
**Approach:** Surgical patches (Approach A) — modify 6 existing files, create 0 new files

---

## Goal

Polish the PRISM interactive REPL and `prism run` terminal experience to match modern AI CLI standards (Claude Code, OpenCode). No framework changes, no tool rewrites, no new abstractions. Pure functions stay pure. Cards stay cards. Just wire up what's missing.

## Reference Architectures

| | Claude Code | OpenCode | PRISM |
|---|---|---|---|
| Stack | React + Ink (compiled Bun binary) | Solid.js + @opentui (60fps custom renderer) | Rich + prompt_toolkit |
| Rendering | Component tree, Static/Live split | Reactive component tree | Pure card functions + console.print |
| Streaming | Token-by-token in Live area | Event batching (16ms windows) | Currently accumulates — needs Live |
| Tool display | Collapsible summaries | InlineTool / BlockTool (2-tier) | 11 typed card renderers |
| Truncation | 50K chars, persist to disk | Line-based (3-10 lines), click-expand | 6 lines — needs char threshold |

**Key insight:** `console.print()` is our `<Static>` (frozen once printed). `Rich Live` is our streaming area. Card functions are our components. We already have the pattern — just need to wire it up.

---

## Section 1: Live Text Streaming

**Problem:** `stream.py` accumulates all text tokens into a string, renders one output card at the end. User sees nothing until the LLM finishes.

**Solution:** Wrap the event loop in a Rich `Live` context. Tokens appear as they arrive. When a tool call starts or the turn ends, flush the Live area to a permanent `console.print()` (the "freeze" — equivalent to Claude Code's `<Static>`).

**Change to `stream.py`:**

1. Create a `Live` context around the event loop body
2. On `TextDelta`: append to buffer, `live.update(Markdown(buffer))`
3. On `ToolCallStart`: stop Live, `console.print(Markdown(buffer))` — freezes text
4. On `TurnComplete`: same flush-and-freeze
5. Plan detection (`<plan>...</plan>`) flushes Live before rendering plan card

**Card renderers (`cards.py`) stay untouched** — they already use `console.print()`.

**Visual:**
```
 ❯ input  ─────────────────────────────────
│ Find me silicon-based semiconductors     │
 ──────────────────────────────────────────

I'll search for silicon-based materials with     ← tokens stream live
semiconductor properties. Let me query the
federated database...

 ⚙ tool  search_materials  1.2s  ──────────
│ ✔ 14 materials from 6 providers          │    ← card after tool completes
 ──────────────────────────────────────────

Based on the results, here are the top           ← streaming resumes
candidates...
```

---

## Section 2: Truncation & Large Result Handling

**Problem:** Tool results have no character-based truncation. A 50K-char search result prints in full.

**Solution:** Two-tier truncation.

### Tier 1: Display truncation (existing — no change)
- Output card: `TRUNCATION_LINES = 6` for LLM text
- Results card: 3-row preview + "+N more"
- Error card: 200-char cap

### Tier 2: Character-based truncation (new)
- Add `TRUNCATION_CHARS = 50_000` to `theme.py`
- When any tool result exceeds 50K chars (serialized), persist full result to `~/.prism/cache/results/` as JSON
- Display truncated version + reference line
- Ties into existing `ResultStore + peek_result` tool — agent can page through stored results

**Visual (large result):**
```
 ⚙ tool  search_materials  3.4s  ──────────
│ ≡ results  (247 materials)               │
│  id       │ formula │ band_gap │ source  │
│  mp-149   │ Si      │ 1.11     │ MP      │
│  mp-2534  │ SiC     │ 2.36     │ NOMAD   │
│  mc-12045 │ SiO2    │ 8.90     │ COD     │
│            +244 more · /export to save   │
 ──────────────────────────────────────────
 ── 52,341 chars truncated · stored as result_a3f2 · use peek_result("result_a3f2") ──
```

**Files:** `theme.py` (add constant), `cards.py` (add char check in `render_tool_result`)

---

## Section 3: Unified `prism run` Rendering

**Problem:** `run.py` has its own inline rendering — plain yellow/green `Panel()` calls, its own `Rich Live`, its own `_flush_text()`. Looks different from the REPL.

**Solution:** `run.py` imports and uses the same card renderers + Spinner + streaming pattern as the REPL.

**What gets deleted from `run.py`** (~30 lines):
- Inline `Panel(f"[dim]Calling...[/dim]", border_style="yellow")` for ToolCallStart
- Inline `Panel(f"[green]{event.summary}[/green]", border_style="green")` for ToolCallResult
- The `_flush_text()` helper
- The manual `Live` context manager

**What replaces it** (~10 lines of imports + calls):
- Import `render_tool_result`, `Spinner` from `app.cli.tui`
- Use the shared streaming pattern from Section 1
- ToolCallStart → `spinner.start(verb)`
- ToolCallResult → `spinner.stop()` + `render_tool_result()`
- TurnComplete → flush + cost line

**Difference from REPL:** `run.py` has no PromptSession (no user input mid-run), no plan detection. It uses the renderers but not the full `handle_streaming_response()`. A small shared helper extracts the Live-stream + card-dispatch pattern.

**Result:** `prism run` output looks identical to REPL — same cards, same colors, same spinner verbs, same truncation. Only difference: no input card, no prompt between turns.

**Files:** `run.py` (rewrite event loop), `stream.py` (extract shared streaming helper)

---

## Section 4: Token & Cost Display

**Problem:** REPL shows no token or cost info. `prism run` shows basic tokens+cost but will be rewritten (Section 3).

**`TurnComplete` already carries everything:**
```python
class TurnComplete:
    usage: Optional[UsageInfo] = None        # this turn
    total_usage: Optional[UsageInfo] = None  # cumulative
    estimated_cost: Optional[float] = None   # this turn's cost
```

**Solution:** After every turn, print a dim cost line. Track session total in `AgentREPL`.

**Format:**
```
 ─ 2.1k in · 340 out · $0.0008 · total: $0.0142 ─
```

**Rules:**
- Token counts use `k` suffix above 1000 (`2.1k in` not `2,100 in`)
- Cost: 4 decimal places (`$0.0008`)
- Session total accumulates across all turns in the REPL
- If `estimated_cost` is None (backend doesn't report), show tokens only
- `DIM` styled — informational, not distracting

**Session total tracking:**
- `AgentREPL` in `app.py` gets `session_cost: float = 0.0`
- Passed into streaming handler, incremented on each `TurnComplete`
- Reset on `/clear` or new session

**Files:** `stream.py` (render cost line), `app.py` (session_cost accumulator), `cards.py` (add `render_cost_line()` helper)

---

## Section 5: Crystal Mascot Alignment Fix

**Problem:** Top/bottom rows of crystal use 3-space indent but need 4-space to center under the 5-glyph middle rows.

**Fix in `theme.py`:**
```python
# Before (misaligned):
MASCOT = [
    "   ⬡ ⬡ ⬡",          # 3 spaces — off by 1
    "  ⬡ ⬢ ⬢ ⬢ ⬡",
    "  ⬡ ⬢ ⬢ ⬢ ⬡",
    "   ⬡ ⬡ ⬡",          # 3 spaces — off by 1
]

# After (centered):
MASCOT = [
    "    ⬡ ⬡ ⬡",         # 4 spaces — centered
    "  ⬡ ⬢ ⬢ ⬢ ⬡",
    "  ⬡ ⬢ ⬢ ⬢ ⬡",
    "    ⬡ ⬡ ⬡",         # 4 spaces — centered
]
```

**Files:** `theme.py` only. One-character change on two lines.

---

## Section 6: install.sh Polish

**Problem:** Working installer, but output is plain compared to modern CLI installers.

**Changes — cosmetic only, no logic rewrites:**

### 6a. Banner tightening
- One-line header with version (not multi-line box)
- Unicode step markers: `✓` done, `→` in progress, `✗` error
- Consistent ANSI color usage

**Visual:**
```
PRISM v2.5.0 — Materials Discovery Platform

→ Detecting Python... ✓ 3.14.0
→ Finding installer... ✓ uv
→ Installing prism-platform[all]... ✓
→ Verifying... ✓ prism v2.5.0

Done. Run 'prism' to start.
```

### 6b. Upgrade output
- Say "Upgrading" not "Installing" when `--upgrade`
- Show old → new version if detectable

### 6c. Error output
- Red ANSI + `✗` marker for consistency

**No changes to:** detection logic, PATH setup, strategy selection, fallback order.

**Files:** `install.sh` only.

---

## File Change Summary

| File | Changes |
|------|---------|
| `app/cli/tui/stream.py` | Live streaming, extract shared helper, cost line rendering |
| `app/cli/tui/cards.py` | Char-based truncation check, `render_cost_line()` helper |
| `app/cli/tui/theme.py` | `TRUNCATION_CHARS = 50_000`, crystal alignment fix |
| `app/cli/tui/app.py` | `session_cost` accumulator in AgentREPL |
| `app/commands/run.py` | Replace inline rendering with card imports |
| `install.sh` | Banner, step markers, upgrade/error output |

**0 new files. 6 modified files. Approach A — surgical patches.**

---

## Constraints

- All 861 existing tests must keep passing
- No framework changes (Rich + prompt_toolkit stays)
- No tool rewrites — tool registry is frozen
- No command changes — all CLI commands are done
- Card renderers remain pure functions
- Backward compatibility: old import paths via shim files preserved
