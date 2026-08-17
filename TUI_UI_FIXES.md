# TUI_UI_FIXES.md — PRISM TUI layout repair

Branch `feat/annotate-not-refuse`. Layout-only changes: same keybindings, same
flow, same content. No new dependencies. No release rebuild (the sidecar was
pointed at `target/debug/prism` via `PRISM_BIN`). No commits made.

Files changed:

- `crates/tui/src/render.rs` — the layout fixes
- `crates/tui/tests/render_snapshots.rs` — two tests encoded the old layout math
- `crates/tui/tests/snapshots/*.snap` — 52 snapshots re-blessed (justified below)

Verification method: every fix was reproduced and confirmed on the live tmux
sidecar (`./scripts/prism-sidecar.sh start/peek`), not by reasoning about code.

---

## Defect 1 — Workspace sidebar clipped to a stripe

**Symptom (120x36, streaming_answer):** right column showed only the word
`Workspace` on row 0 and a lone vertical rule near the prompt; 36 rows of
nothing else.

**Root cause:** two compounding problems.

1. `crates/tui/src/render.rs:103` (pre-fix): the home overlay was given the
   FULL screen: `let home_bounds = Rect::new(area.x, chunks[1].y, area.width,
   chunks[1].height);`. `draw_home` then does `f.render_widget(Clear, bounds)`
   plus a full background fill (`render.rs:2192-2196` pre-fix) — which wiped
   the entire workspace sidebar column for every row of the transcript area.
   Only row 0 (header row, above the overlay) and the rows below the
   transcript survived, hence one stray "Workspace" word and border fragments.
2. Even when visible, the sidebar had no collapse rule: `render.rs:39`
   allocated `(width / 3).clamp(24, 42)` at EVERY width — at 40 columns it
   stole 24 of 40, at 80 it took 26 (see `tiny_terminal_basic_chat_40x12.snap`
   before: content was a 16-column sliver cutting words mid-letter).

**Fix:**
- `home_bounds` now spans only the content column:
  `Rect::new(cols[0].x, chunks[1].y, cols[0].width, chunks[1].height)`
  (`render.rs`, `draw` overlay branch). The sidebar is never painted over.
- Sidebar hidden below 100 columns (`SIDEBAR_MIN_WIDTH` in `draw`). 100 was
  chosen because `scripts/prism-sidecar.sh` itself documents "the workspace
  sidebar collapses below ~100 columns", and it keeps the sidebar present at
  the 100x30 snapshot size where a dozen `workspace_*` snapshot tests guard
  its rendering. Degrade deliberately, don't clip.

**Confirmed on screen:** at 120x36 and 200x50 the sidebar now shows its title,
tab strip (`[Act] Too Fil Obj Str Art`), stats line and activity feed; at
80x24 it is absent by design and the content column gets the full width.

## Defect 2 — Prompt box misaligned with the main panel

**Symptom:** main panel inset ~12 columns and ~96 wide; prompt box at column
0, ~80 wide. Stacked elements disagreeing about origin AND width.

**Root cause:** the "main panel" is the Mission Control home overlay.
`draw_home` (`render.rs:2183-2192` pre-fix) built its panel with
`centered_rect(80, 100, panel_bounds)` over the FULL-SCREEN bounds — an
80%-wide box centered on the whole terminal — while the prompt box and footer
are laid out inside the left content column (`chunks[2]`, `chunks[3]` of
`cols[0]`). Two different coordinate regimes for one visual stack.

**Fix:** `draw_home` now renders the panel exactly into the bounds it is
given (the content column's transcript area): the `centered_rect` call and the
28-row floating-panel math are gone; the bordered paragraph renders into
`bounds`. Home panel, prompt box and footer now share x=0 and the same width
at every terminal size.

## Defect 3 — Stray `│` at the right edge of prompt/status rows

**Symptom:** `┐│` after the prompt box corner; a lone `│` after the status
line.

**Root cause:** not a border at the wrong x — it is the sidebar's
`Borders::LEFT` divider (`draw_workspace`, `render.rs:589` area), but the
content column had zero gap: the prompt box's right border sat in the column
immediately left of the divider, so two vertical rules touched and read as a
double-drawn stray glyph (worst in the "before" state, where the rest of the
sidebar had been wiped and the divider had no context).

**Fix:** `Layout::spacing(1)` between the content column and the sidebar in
`draw`. One column of daylight now separates every content box from the
divider; the divider reads as a column separator, not a stray border.

## Defect 4 — Main panel does not use the width it has

**Root cause:** same as defect 2 — `centered_rect(80, 100, ..)` left 10%
gutter on each side of the full screen, so the panel was inset left of the
content it should anchor to and ended short over the sidebar column.

**Fix:** same as defect 2. The panel now spans its whole column; nothing is
inset by accident.

## Defect 5 — "~300-char lines in a 120-column pane" — NOT REPRODUCIBLE

Measured on screen: no row exceeds the pane width at any of the three sizes.
The ~300 "chars" are BYTES: a box border of 96 `─` glyphs is 288 bytes in
UTF-8 plus ~12 spaces ≈ 300, while its display width is 108 columns. Ratatui
clips to the viewport, and the tmux capture confirms nothing wraps or escapes
the right edge. No fix needed; recorded here so nobody hunts it.

---

## Additional deliberate-degradation work

- `draw_home` on a short pane (e.g. 80x24): the panel previously capped at 28
  rows and floated centered — on an 80x24 pane the bottom content (including
  the `⏎ talk to the agent …` key hints) fell off the panel and clipped. Now
  the panel fills its bounds; when the content does not fit vertically, the
  blank spacer rows are dropped first (`lines.retain(|l| l.width() > 0)`), so
  all real content stays visible. Confirmed at 80x24: every line present.
- `draw_home` paragraph now wraps (`Wrap { trim: false }`) instead of
  truncating. Between 100 and ~112 columns the content column is 64-72 wide
  and the 72-column key-hint line wraps rather than losing "? keys".
  Visible and honest beats clipped.

## Things I did NOT change (and one observation)

- Placeholder strings ("no live run list wired yet", "not wired in-app yet",
  "location … not reported", "credits not reported") untouched — all still
  accurate against the code; none became wrong.
- Approval popup and other modal overlays still center on the whole screen
  (they float over the sidebar by design, `Clear` + redraw). Not a listed
  defect; behaviour preserved.
- Observation (dead-code, NOT removed per patch discipline): with the 100-col
  sidebar threshold, the two-letter MIN tab labels in `workspace_tabs_line`
  are unreachable (the visible sidebar inner width is always ≥ 32, and the
  SHORT strip is 26). Harmless defensive ladder; say the word and I remove it.

---

## Gate output (exact)

```
$ cargo fmt -p prism-tui
(no output — clean)

$ cargo test -p prism-tui
     Running unittests src/lib.rs (target/debug/deps/prism_tui-…)
test result: ok. 200 passed; 0 failed; 0 ignored
     Running tests/render_snapshots.rs (target/debug/deps/render_snapshots-…)
test result: ok. 59 passed; 0 failed; 0 ignored
     Running tests/unit.rs (target/debug/deps/unit-…)
test result: ok. 198 passed; 0 failed; 0 ignored
test result: ok. 1 passed; 0 failed; 0 ignored   (doc-tests)

$ cargo clippy -p prism-tui --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.07s

$ bash scripts/verify-tui.sh   (with PYTHON=python3 — this machine has no `python`)
  [PASS] fmt clean / build clean / unit tests passed / clippy clean
  [PASS] PTY tests passed / no CJK language drift detected — All checks passed.

$ cargo test --workspace --no-fail-fast
passed=3067 failed=2   (total 3069 = baseline total)
```

**The 2 workspace failures are pre-existing and NOT caused by this patch.**
Both are in `crates/agent/tests/jspace_benchmark.rs`
(`candidate_fixture_locks_ordered_full_definitions`,
`retriever_fixtures_do_not_contain_evaluator_labels`) — fixture-digest tests
in a crate I never touched. Proof: I stashed my TUI changes
(`git stash push -- crates/tui/…`), re-ran that test target, and it fails
identically on the untouched tree, then restored my patch with `git stash
pop`. The working tree carries many other uncommitted non-TUI modifications
(e.g. `crates/agent/src/agent_loop.rs`) which predate this work.

---

## BEFORE / AFTER peek captures (scenario `streaming_answer`, launch screen)

### 120x36

BEFORE (debug build of pre-patch source; byte-identical layout to the
reference capture in `~/.claude/jobs/bd21bca9/tmp/ui/streaming_answer.txt`):

```
 ‹ back  New session   ◆ fake-backend    99 tools    Ctrl-P · ?                 │ Workspace
            ┌ PRISM · materials research workspace ────────────────────────────────────────────────────────┐
            │                                                                                              │
            │  WORKFLOWS                                                                                   │
            │     no live run list wired yet — talk to the agent to start one                              │
            │                                                                                              │
            │  TOOLS                                                                                       │
            │   ▣ 4 tools · 2 need approval · 2 auto   t open                                              │
            │     location (cloud/local/remote) not reported — pending tool tags                           │
            │                                                                                              │
            │  NOTEBOOKS                                                                                   │
            │     not wired in-app yet — will be agent-watched + editable                                  │
            │                                                                                              │
            │  SYSTEMS                                                                                     │
            │   ● model  fake-backend   s status                                                           │
            │   ● session cost  $0.0000   as of last checkpoint                                            │
            │     credits  not reported (unauthed / not fetched)                                           │
            │     compute · nodes · knowledge · ingestion — open via ⌘K                                    │
            │                                                                                              │
            │  ⏎ talk to the agent    t tools    s systems    ⌘K commands    ? keys                        │
            │                                                                                              │
            │                                                                                              │
            │                                                                                              │
            │                                                                                              │
            │                                                                                              │
            │                                                                                              │
            │                                                                                              │
            │                                                                                              │
            └──────────────────────────────────────────────────────────────────────────────────────────────┘

┌ Prompt ──────────────────────────────────────────────────────────────────────┐│
│ Type a message... (Enter=send, /help, Ctrl-C=quit)                           ││
│                                                                              ││
│                                                                              ││
└──────────────────────────────────────────────────────────────────────────────┘│
  Ready  model: fake-backend   [INPUT]    Ctrl-C quit                           │
```

AFTER:

```
 ‹ back  New session   ◆ fake-backend    99 tools    Ctrl-P · ?                 │ Workspace
┌ PRISM · materials research workspace ───────────────────────────────────────┐ │ [Act] Too Fil Obj Str Art
│                                                                             │ │
│  WORKFLOWS                                                                  │ │  (no activity yet)
│     no live run list wired yet — talk to the agent to start one             │ │
│                                                                             │ │
│  TOOLS                                                                      │ │
│   ▣ 4 tools · 2 need approval · 2 auto   t open                             │ │
│     location (cloud/local/remote) not reported — pending tool tags          │ │
│                                                                             │ │
│  NOTEBOOKS                                                                  │ │
│     not wired in-app yet — will be agent-watched + editable                 │ │
│                                                                             │ │
│  SYSTEMS                                                                    │ │
│   ● model  fake-backend   s status                                          │ │
│   ● session cost  $0.0000   as of last checkpoint                           │ │
│     credits  not reported (unauthed / not fetched)                          │ │
│     compute · nodes · knowledge · ingestion — open via ⌘K                   │ │
│                                                                             │ │
│  ⏎ talk to the agent    t tools    s systems    ⌘K commands    ? keys       │ │
│                                                                             │ │
│                                                                             │ │
│                                                                             │ │
│                                                                             │ │
│                                                                             │ │
│                                                                             │ │
│                                                                             │ │
│                                                                             │ │
│                                                                             │ │
└─────────────────────────────────────────────────────────────────────────────┘ │
┌ Prompt ─────────────────────────────────────────────────────────────────────┐ │
│ Type a message... (Enter=send, /help, Ctrl-C=quit)                          │ │
│                                                                             │ │
│                                                                             │ │
└─────────────────────────────────────────────────────────────────────────────┘ │
  Ready  model: fake-backend   [INPUT]    Ctrl-C quit                           │
```

### 80x24

BEFORE (existing `target/release/prism`, run-only, no rebuild):

```
 ‹ back  New session   ◆ fake-backend    99 tools    C│ Workspace
        ┌ PRISM · materials research workspace ────────────────────────┐
        │                                                              │
        │  WORKFLOWS                                                   │
        │     no live run list wired yet — talk to the agent to start o│
        │                                                              │
        │  TOOLS                                                       │
        │   ▣ 4 tools · 2 need approval · 2 auto   t open              │
        │     location (cloud/local/remote) not reported — pending tool│
        │                                                              │
        │  NOTEBOOKS                                                   │
        │     not wired in-app yet — will be agent-watched + editable  │
        │                                                              │
        │  SYSTEMS                                                     │
        │   ● model  fake-backend   s status                           │
        │   ● session cost  $0.0000   as of last checkpoint            │
        │     credits  not reported (unauthed / not fetched)           │
        └──────────────────────────────────────────────────────────────┘
┌ Prompt ────────────────────────────────────────────┐│
│ Type a message... (Enter=send, /help, Ctrl-C=quit) ││
│                                                    ││
│                                                    ││
└────────────────────────────────────────────────────┘│
  Ready  model: fake-backend   [INPUT]    Ctrl-C quit │
```

AFTER:

```
 ‹ back  New session   ◆ fake-backend    99 tools    Ctrl-P · ? 
┌ PRISM · materials research workspace ────────────────────────────────────────┐
│  WORKFLOWS                                                                   │
│     no live run list wired yet — talk to the agent to start one              │
│  TOOLS                                                                       │
│   ▣ 4 tools · 2 need approval · 2 auto   t open                              │
│     location (cloud/local/remote) not reported — pending tool tags           │
│  NOTEBOOKS                                                                   │
│     not wired in-app yet — will be agent-watched + editable                  │
│  SYSTEMS                                                                     │
│   ● model  fake-backend   s status                                           │
│   ● session cost  $0.0000   as of last checkpoint                            │
│     credits  not reported (unauthed / not fetched)                           │
│     compute · nodes · knowledge · ingestion — open via ⌘K                    │
│  ⏎ talk to the agent    t tools    s systems    ⌘K commands    ? keys        │
│                                                                              │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘
┌ Prompt ──────────────────────────────────────────────────────────────────────┐
│ Type a message... (Enter=send, /help, Ctrl-C=quit)                           │
│                                                                              │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘
  Ready  model: fake-backend   [INPUT]    Ctrl-C quit
```

### 200x50

BEFORE (existing `target/release/prism`):

```
 ‹ back  New session   ◆ fake-backend    99 tools    Ctrl-P · ?                                                                                               │ Workspace







                    ┌ PRISM · materials research workspace ────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
                    │                                                                                                                                                              │
                    │  WORKFLOWS                                                                                                                                                   │
                    │     no live run list wired yet — talk to the agent to start one                                                                                              │
                    │                                                                                                                                                              │
                    │  TOOLS                                                                                                                                                       │
                    │   ▣ 4 tools · 2 need approval · 2 auto   t open                                                                                                              │
                    │     location (cloud/local/remote) not reported — pending tool tags                                                                                           │
                    │                                                                                                                                                              │
                    │  NOTEBOOKS                                                                                                                                                   │
                    │     not wired in-app yet — will be agent-watched + editable                                                                                                  │
                    │                                                                                                                                                              │
                    │  SYSTEMS                                                                                                                                                     │
                    │   ● model  fake-backend   s status                                                                                                                           │
                    │   ● session cost  $0.0000   as of last checkpoint                                                                                                            │
                    │     credits  not reported (unauthed / not fetched)                                                                                                           │
                    │     compute · nodes · knowledge · ingestion — open via ⌘K                                                                                                    │
                    │                                                                                                                                                              │
                    │  ⏎ talk to the agent    t tools    s systems    ⌘K commands    ? keys                                                                                        │
                    │                                                                                                                                                              │
                    │                                                                                                                                                              │
                    │                                                                                                                                                              │
                    │                                                                                                                                                              │
                    │                                                                                                                                                              │
                    │                                                                                                                                                              │
                    │                                                                                                                                                              │
                    │                                                                                                                                                              │
                    └──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘








┌ Prompt ────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐│
│ Type a message... (Enter=send, /help, Ctrl-C=quit)                                                                                                         ││
│                                                                                                                                                            ││
│                                                                                                                                                            ││
└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘│
  Ready  model: fake-backend   [INPUT]    Ctrl-C quit                                                                                                         │
```

AFTER:

```
 ‹ back  New session   ◆ fake-backend    99 tools    Ctrl-P · ?                                                                                               │ Workspace
┌ PRISM · materials research workspace ─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐ │ [Act] Too Fil Obj Str Art
│                                                                                                                                                           │ │
│  WORKFLOWS                                                                                                                                                │ │  (no activity yet)
│     no live run list wired yet — talk to the agent to start one                                                                                           │ │
│                                                                                                                                                           │ │
│  TOOLS                                                                                                                                                    │ │
│   ▣ 4 tools · 2 need approval · 2 auto   t open                                                                                                           │ │
│     location (cloud/local/remote) not reported — pending tool tags                                                                                        │ │
│                                                                                                                                                           │ │
│  NOTEBOOKS                                                                                                                                                │ │
│     not wired in-app yet — will be agent-watched + editable                                                                                               │ │
│                                                                                                                                                           │ │
│  SYSTEMS                                                                                                                                                  │ │
│   ● model  fake-backend   s status                                                                                                                        │ │
│   ● session cost  $0.0000   as of last checkpoint                                                                                                         │ │
│     credits  not reported (unauthed / not fetched)                                                                                                        │ │
│     compute · nodes · knowledge · ingestion — open via ⌘K                                                                                                 │ │
│                                                                                                                                                           │ │
│  ⏎ talk to the agent    t tools    s systems    ⌘K commands    ? keys                                                                                     │ │
│                                                                                                                                                           │ │
│                                                                                                                                                           │ │
│                                                                                                                                                           │ │
│                                                                                                                                                           │ │
│                                                                                                                                                           │ │
│                                                                                                                                                           │ │
│                                                                                                                                                           │ │
│                                                                                                                                                           │ │
│                                                                                                                                                           │ │
│                                                                                                                                                           │ │
│                                                                                                                                                           │ │
│                                                                                                                                                           │ │
│                                                                                                                                                           │ │
│                                                                                                                                                           │ │
│                                                                                                                                                           │ │
│                                                                                                                                                           │ │
│                                                                                                                                                           │ │
│                                                                                                                                                           │ │
│                                                                                                                                                           │ │
│                                                                                                                                                           │ │
│                                                                                                                                                           │ │
│                                                                                                                                                           │ │
│                                                                                                                                                           │ │
└───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘ │
┌ Prompt ───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐ │
│ Type a message... (Enter=send, /help, Ctrl-C=quit)                                                                                                        │ │
│                                                                                                                                                           │ │
│                                                                                                                                                           │ │
└───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘ │
  Ready  model: fake-backend   [INPUT]    Ctrl-C quit                                                                                                         │
```

---

## Snapshots changed (52) and why

Re-blessed via the project's documented mechanism
(`INSTA_UPDATE=always cargo test -p prism-tui --test render_snapshots`) after
inspecting every diff. None was accepted blindly; each diff is one of the
three layout improvements below, visible on screen:

1. **`home_launch_100x30`** — the big one. Before: home floated centered over
   the full screen, erased the sidebar, and the prompt box below sat at a
   different x and width (the `┐│` double rule). After: home, prompt and
   footer share origin and width in the content column, and the sidebar
   renders its tab strip and activity empty-state beside them. Same
   improvement visible live at 120x36.
2. **`tiny_terminal_basic_chat_40x12`** — before: the sidebar took 24 of 40
   columns; the transcript, header and prompt were 16-column slivers cutting
   words mid-letter ("New ses", "deterministi"). After: sidebar dropped below
   the threshold, content gets the full 40 columns, real words render. This
   is the deliberate-degradation requirement, pixel-proven.
3. **The other 50 (all 100x30 and `wide_terminal_basic_chat_200x60`)** —
   mechanical consequence of defect 3's fix: the content column is one
   narrower and a one-column gap replaces the doubled vertical at the prompt
   box's right edge (every `┐│`/`┘│` pair becomes `┐ │`/`┘ │`). The sidebar
   starts at the same x as before (67 at 100 cols, 158 at 200), so its
   content is pixel-identical; the only other movement is long-line wrap
   points shifting one column (e.g. `basic_chat_after_response`,
   `long_unbroken_line`). Overlays centered on the full screen (palettes,
   approval popup, toasts) are unchanged.

Test-code changes encoding the new layout rule (mirrors of `render::draw`,
both reviewed above):

- `first_screen_has_no_debug_text_stale_state_or_panel_overlap`
  (`crates/tui/tests/render_snapshots.rs`): prompt-corner coordinates now use
  the threshold + spacing rule; at 40 cols it now asserts the full-width
  prompt corner of the sidebar-less layout.
- `workspace_tab_strip_never_wraps_at_any_width`: below 100 columns it now
  asserts the sidebar is ABSENT (degrade, don't wrap); the no-wrap invariant
  still holds at 100 and 200 columns.

## Could not fix

Nothing among the confirmed defects. Defect 5 required no fix (measurement
artefact, documented above).
