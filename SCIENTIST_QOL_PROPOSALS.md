# Quality-of-life proposals for the working scientist

Produced 2026-08-26 by a second model asked to advise on PRISM for a practising
materials scientist (LPBF, CALPHAD, DFT, microstructure) rather than a developer.
Grounded in the real crate layout, ranked by leverage. Two of its top four —
hover-explain on everything coloured, and risk-tiered approvals — were shipped
the same day, independently, which is a useful signal that the ranking is sound.

Quick grounding pass first (I kept it to the TUI crate): the notebook is indeed one `Textbox` + a cell log (`notebook.rs`), approvals are a per-call popup (`approval_pending` in `app.rs`), and — importantly for costing — you already have `provenance`, `ontology`, `python-bridge`, `campaign`, `ingest` and `hit_map` crates. Several proposals below are cheaper than they look because of that.

Framing: the user is a scientist who runs LPBF builds, Scheil/CALPHAD sweeps and DFT jobs, tracks samples in Excel, reads papers in a PDF manager, and must be able to defend every number to a supervisor or reviewer. She does not care how it works. She cares that it behaves like a lab instrument: predictable, interruptible, and it writes down what it did.

---

## Ranked proposals

### 1. A real notebook that can attach to *her* kernel
**Situation:** She's iterating a Scheil solidification sweep over 6 alloy compositions in `pycalphad`. The agent ran something; now she wants to tweak one line, re-run only that, and continue from the live variables.
**What it does:** Cell types (code / markdown / parameter-table), up/down navigation, edit and re-run any cell, out-of-order execution with a visible execution counter, and `kernel attach <url|name>` to connect to any running Jupyter kernel — her own env with Thermo-Calc TC-Python, pymatgen, ASE already installed. Agent and human share one kernel namespace: the agent says "phase fractions are in `df_phases`, next cell is yours" and she manipulates them directly.
**Why it beats today:** Jupyter gives her cells but has no agent; today's PRISM notebook is an append-only transcript she can't edit, which means every iteration is a new chat prompt — slower than Jupyter, so she'll abandon it. Shared kernel is the part Jupyter can't do.
**Cost:** large (but `python-bridge` already exists, so it's cell model + editor UX, not plumbing from zero).

### 2. Marks: draw on anything on screen, and the agent sees it
**Situation:** The agent rendered a computed phase diagram. She knows the region around 1150 °C / 18 wt% Nb looks wrong. Describing that in words ("the little lobe left of the Laves field") is lossy and exhausting.
**What it does:** A mark mode (`m`) that works over any rendered content — plot, table row/column, PDF page, structure view, chat answer. Box/arrow/row-select produces a named reference (`M3`), drawn visibly, addressable in chat: "what stabilizes the phase in M3?", "re-run with cooling rate 10×", "extract these points into the sample sheet". Marks are objects: they carry the underlying data coordinates, not pixels, so the agent can actually compute with them.
**Why it beats today:** Today: screenshot → paste into chat → hope the model guesses the region. This turns pointing into a first-class operation, which is how scientists actually communicate ("this peak", "that grain").
**Cost:** large — but `hit_map.rs` is already there, and this is the spine that makes #7 and #9 worth having.

### 3. Hover-explain on everything coloured
**Situation:** The chat shows a colour-coded tool result ("Laves fraction 4.2%") and a sidebar badge. She must be able to put this in a paper and in front of her supervisor. "The AI said so" ends conversations badly.
**What it does:** Every coloured element has a provenance overlay on hover/focus: what produced it, why it was called, the calling chain (agent turn → tool → inputs), data sources with links, the ontology node it belongs to, and confidence/version (database release, tool version). One key (`?` over anything) opens the full record.
**Why it beats today:** Nothing she uses today traces a number back to its origin; she reconstructs it manually from scripts and memory. This is also the trust mechanism that decides whether she delegates compute to the agent at all.
**Cost:** medium — `provenance` and `ontology` crates exist; the work is attaching those records to render spans and drawing the overlay.

### 4. Risk-tiered approvals instead of a popup per tool call
**Situation:** She asks for a composition sweep. The agent reads a file, then a database, then plots — three popups in 20 seconds. By day two she presses `y` reflexively, which is *less* safe than no approval.
**What it does:** Tiers. Read-only and sandboxed compute: run silently, still logged and hover-explainable (#3). Reversible writes: one-line toast, undoable. Genuinely consequential actions (submit a paid/queued HPC job, spend money, delete data, send externally): full approval with the actual command shown. Plus "approve this tool for this project" memory.
**Why it beats today:** Per-call approval is a developer's model of risk (any code execution is scary). A scientist's model is: reading is free, compute is cheap, cluster time and deletions are precious. Match hers.
**Cost:** small–medium — `policy` crate exists; mostly classification of existing tools + UI states.

### 5. The sample sheet: one living table for compositions, builds and results
**Situation:** Her LPBF campaign is a matrix — 8 alloys × power × scan speed × hatch spacing, then heat treatments, then density/hardness measurements. It lives in `final_FINAL_v3.xlsx`, and the agent can't see it.
**What it does:** A sortable, filterable grid inside PRISM: rows are samples/runs, columns are composition, process parameters, treatments, measured properties. She edits cells directly; the agent can read it, append results, derive columns (at% conversion, energy density `P/(v·h·t)`), and plot from it. Backed by a plain CSV she can also open in Excel — PRISM is not a hostage situation.
**Why it beats today:** Excel has the table but no agent; the chat has the agent but no table. This is the object the whole session refers to ("run Scheil on rows 4–9", "plot hardness vs energy density, colour by alloy").
**Cost:** medium for a genuinely useful v1 (grid + CSV + agent read/append); large only if you chase full Excel parity — don't.

### 6. Units and composition layer
**Situation:** A paper reports composition in at%, her CALPHAD input wants wt%, the DFT energy is in eV/atom, the handbook says ksi. Conversions are done by hand, in the head, at 6pm. Errors here are silent and embarrassing.
**What it does:** PRISM parses and emits quantities with units everywhere: type `Al-18at%Nb` anywhere and it's understood; "show in wt%", "per mole", "ksi → MPa" are one keystroke; the sample sheet and notebook normalise to declared units; mismatched unit arithmetic warns ("adding J/mol to eV/atom"). Composition renormalization (excluding O/N, normalizing to 100%) is a built-in operation, because that's a real daily decision.
**Why it beats today:** Today this is mental math and a conversion cell in Excel that everyone distrusts. Cheap to build, removes the single most common correctness failure in alloy work.
**Cost:** small–medium. Highest value-per-effort on this list; do it early because #5 and #1 want it underneath them.

### 7. Plots that respond: cursor readout, zoom, overlay, export
**Situation:** The agent produced a hardness-vs-energy-density plot. She needs the value at the kink, wants to overlay yesterday's batch, and needs a 300-dpi figure for the SI — right now each of those means "regenerate with matplotlib, please, no, xlim was wrong, again".
**What it does:** Every rendered plot is live: pan/zoom, crosshair with x/y readout in real units, point-selection (reusing marks, #2), overlay another series from the sample sheet or a previous run, and `e` to export PNG/SVG with the exact data + code that produced it recorded in the file metadata.
**Why it beats today:** Chat-generated plots are currently screenshots — read-only dead ends. Scientists *read* plots interactively; a plot you can't interrogate gets re-made in Excel within the hour.
**Cost:** medium — depends on image-protocol rendering (kitty/sixel) + the existing hit-map work.

### 8. Long-running jobs lane with completion callbacks
**Situation:** She submits a DFT relaxation and a 40-point process-window simulation to the cluster. For the next six hours her actual workflow is `ssh` / `squeue` / `tail -f`, scattered across terminals.
**What it does:** A jobs strip showing queued/running jobs with state and ETA; on completion PRISM notifies in-session and the agent parses the result into the conversation ("OUTCAR converged, direct gap 2.1 eV, POSCAR relaxed — here's the diff"). Submission still goes through the consequential-action approval (#4).
**Why it beats today:** Turns "watch a terminal" into "get pinged, get analysis", and keeps the results inside the session instead of in a scratch terminal she'll lose.
**Cost:** medium — `campaign`/`compute` crates suggest most of the lifecycle exists; the work is the TUI lane and the on-complete agent hook.

### 9. Paper desk: PDFs inline with citation pins
**Situation:** She's comparing her Scheil result against the 2019 paper on the same alloy. Today: alt-tab to Zotero, find PDF, find figure, screenshot, paste, describe.
**What it does:** Render papers/PDF pages inside PRISM next to the chat. Marking works on them (#2): box a figure region, ask "reproduce this boundary with my thermodynamic database", and the agent gets the region plus the paper's metadata. When the agent makes a claim from a paper, the claim carries a pin — hover (#3) shows paper, page, quoted snippet.
**Why it beats today:** Collapses the read–compute–cite loop that currently spans three apps and a clipboard. The pin-to-source also feeds her writing: pulling citations for a draft becomes free.
**Cost:** medium once #2 exists (PDF→image rendering is the hard part; no text-reflow ambition needed).

### 10. Provenance receipts — one key from result to defensible record
**Situation:** Reviewer 2 asks how the Laves fraction was computed. She needs: input composition, database + version, tool, parameters, the plot, and when. Today: archaeology.
**What it does:** `x` on any result (plot, table, number) produces a receipt: full provenance chain from #3, formatted as a lab-notebook entry, exportable as Markdown/PDF, appendable to a project log. The transcript of every session is already captured — this is the exporter with teeth.
**Why it beats today:** No tool she owns does this; it's the difference between "agent-assisted" and "agent-assisted and publishable". For a working scientist this is not a nice-to-have, it's the permission slip to use the tool at all.
**Cost:** small–medium — mostly formatting over data you already record.

### 11. Recall: "what exactly did I run for Alloy-7?"
**Situation:** Three weeks later she can't remember whether the Scheil run used cooling rate 0.1 or 1 K/s, and whether she excluded the oxygen analysis. The answer is in some transcript somewhere.
**What it does:** Semantic search over her own session history — runs, parameters, results, decisions — from one query, returning the cell/result with its receipt (#10). The agent answers from her history the same way it answers from literature.
**Why it beats today:** Her current history is Jupyter file dates and Excel tabs named `final2`. This makes past sessions a searchable asset.
**Cost:** small–medium — index an existing transcript store; retrieval infra already in `retrieval` crate.

### 12. Session resume
**Situation:** She closes her laptop at 7pm mid-analysis, opens it the next morning, and wants the same notebook, the same open paper, the sample sheet, and the last chat context — not a blank screen that says "how can I help?".
**What it does:** Persist and restore workspace layout, notebook state (unsaved edits included), open artifacts, and a three-line "where you were" summary. Experiments run in weeks; the tool must be continuous across them.
**Why it beats today:** Jupyter at least keeps her cells; a chat that forgets itself daily forces re-explaining the whole project every morning. That friction alone decides daily-use adoption.
**Cost:** small.

### 13. Recipes for the workflows she runs every week
**Situation:** For every new alloy batch she does the same things: normalize compositions, Scheil across the set, energy-density map for the LPBF parameters, hardness-vs-porosity plot. Each time she re-describes it to the agent.
**What it does:** Named, parameterized templates — "Scheil screen", "process-window grid", "convergence sweep" — selectable from the sidebar, asking only for inputs (composition set, ranges), generating real editable notebook cells (#1), not hidden magic. Her own saved runs can become recipes.
**Why it beats today:** A Jupyter template is a stale script she maintains; here the template produces inspectable cells in a shared kernel, so it stays transparent.
**Cost:** small once #1 exists.

### 14. Instrument ingest: drop the machine's CSV, get clean columns
**Situation:** The hardness tester / DSC / XRD exports a CSV with 14 header rows, mixed units, and timestamp formats from 1998. She hand-cleans it before anything else can touch it.
**What it does:** Drop a file (or point at an inbox folder): agent sniffs the layout, proposes header/units/column mapping, she confirms, and the clean rows land in the sample sheet (#5) with the raw file preserved and referenced. Repeatable: new export of the same instrument reuses the mapping.
**Why it beats today:** Removes the most tedious recurring manual step in experimental data flow and makes instrument output agent-visible immediately.
**Cost:** medium — `ingest` crate exists; the work is the confirm-mapping UI and sample-sheet hookup.

---

## Deliberately not proposed

- Git/version-control UI, refactoring, linting, plugin APIs, theming: developer concerns; the scientist never asks for them.
- A richer text editor for writing papers: she writes in Word/LaTeX; PRISM should export receipts and citations *to* those, not replace them.

Sequencing note: #1 and #4 gate daily usability; #2 is the platform move that #7 and #9 build on; #3 and #10 are the same provenance data viewed two ways, so build them together; #6 is cheap and everything above it wants it.
