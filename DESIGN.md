# PRISM app — the design

Owner-specified. This is the target the desktop app is built against. It does not get
reinterpreted; when a brief conflicts with this file, this file wins.

Source: photograph supplied by the owner 2026-08-08, plus his written direction quoted below.

---

## The one-sentence rule

**The right side is the WORK, rendered. Not a panel about the agent.**

---

## Layout — three columns

### Left rail — thin, collapsed
Session / workspace list. Icons plus short labels. It is not the subject of the screen and never
competes for attention.

### Centre — the transcript
Prose interleaved with tool cards. Each card shows the **real code** (`python`, `bash`) and the
**real output** inline, collapsible. It reads like a lab notebook: a sentence of reasoning, then the
code that tested it, then what came back, then the next sentence.

### Right — the artifact canvas
A single large surface, roughly 40% of the width, full height. It renders **the thing the agent is
working on right now**.

In the reference photograph this is a 3D protein structure — six chains, ball-and-stick, drawn big.
Not a chart *about* the work. The work.

The canvas is driven by what the agent is doing, **not by a tab the user picks**. When the agent
opens a file, the file is there. When it fetches a page, the page is there. When it produces a
structure or a plot, that is there.

### The caption card
One small floating card over the bottom-right of the canvas:

> **Deriving the amino-acid sequence from the atomic coordinates**
> `DGRVKIGHY…AAP`
> 6 chains (A–F)
> – read out residue by residue

Title = what is being derived. Then the result. Then one method line. Nothing else. Populated from
the agent's current step; if there is no current step, no card.

### Bottom — a single hairline strip
Version, token count. Tiny. **That is all the instrumentation in the entire window.**

---

## What is NOT in the design

No cost panel. No manifest. No "tools available". No "witnessed leaves". No "Sign & seal" button
occupying prime real estate.

The owner's words, verbatim:

> *"The right side should not be doing whatever you're doing. Why do I need to see what tools you
> have live right now?"*

And earlier, on the same surface:

> *"On the right side we very prominently show how much money is being spent on a live run. I was
> hoping that it's just one of the tabs. When you have a VS Code extension installed, you have a
> left side and a right side — the sidebars should work like that. It should be the screen where
> you can also see your code, scroll the code. The agent should open the code there. Or show live
> simulations, or whatever it is doing. Even going online and fetching stuff should be shown in a
> canonical manner. **Please don't make it dumb.**"*

Cost and session data are permitted **as tabs among the artifacts, never as the lead**.

---

## Artifact kinds

```rust
enum Artifact {
    Code      { path, text, language, highlight: Option<Range<usize>> },
    Image     { bytes, mime },
    Web       { url, title, text },
    Structure { .. },   // molecular / crystal — CIF, VASP POSCAR/CONTCAR
    Plot      { .. },   // charts, simulation output
    Table     { .. },
}
```

Dispatched through a `ViewerRegistry` so a new kind slots in without touching the panel.

**Never fake a renderer.** If the artifact is a kind that cannot be drawn yet, the canvas says so
with the real facts it does have — *"3D structure viewer not built yet — 6 chains, 1,247 atoms"*.
A dead surface that admits it beats a fake one. This is not a style preference; a materials person
reads a render as data.

---

## Empty state

Quiet. Not a manifest, not a placeholder grid.

> *"Please don't make it dumb."*

---

## Implementation status (2026-08-08)

Built and merged on `feat/remedy-hardening` (150 tests, gated):

- `src/artifact.rs` — `Artifact` / `ArtifactKind` / `ViewerRegistry`, dispatch by kind
- `src/screens/agent.rs` — right panel where **artifacts are the headline; Cost and Session are
  tabs among them, never the lead**; auto-follows the newest artifact until the user selects one
- `src/structure.rs` — CIF and VASP POSCAR/CONTCAR parsing, with honest parse errors rather than an
  empty box

Not built:

- The 3D structure **renderer** — the parser exists, the drawing does not
- `Plot` — simulation and chart output

Both must state what they cannot draw rather than approximate it.
