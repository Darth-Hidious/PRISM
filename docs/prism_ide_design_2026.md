# PRISM IDE — Self-Building Canvas Design

**Status:** Phase 4 design draft v0.1 — NOT FINAL. User pushback queued (see "Open questions" below).
**Author:** Claude (research session 2026-05-07)
**One-line ask from the user:** *"Like Notion, like a blank canvas just building itself."*

> **⚠ Pushback received (2026-05-07 evening):** "If you remove the VSCode platform,
> we can't use the extensions that come with VSCode — most of the job is just writing
> code. You also need to research what material scientists actually need in an app."
>
> The user is right. This v0.1 over-rotates on the canvas at the expense of:
> 1. **VSCode's extension ecosystem.** Python, Jupyter, GitLens, language servers,
>    debuggers, container/Kubernetes, dotenv, Markdown, GitHub PRs — materials
>    scientists writing simulation code or notebooks lean on these every day.
>    Walking away from that costs more than it saves.
> 2. **What materials scientists actually do.** v0.1 was designed without a real
>    user-needs study. Day-to-day in a wet lab + DFT/MD shop:
>    - Read/write Python (DFT pipelines, ASE, pymatgen), Fortran (legacy), Bash
>      (cluster scripts), YAML (workflow specs), .cif/.pwx/.in (structure files)
>    - Run Jupyter notebooks against compute clusters (SLURM/PBS submission)
>    - Inspect/diff long log files from VASP/Quantum ESPRESSO/LAMMPS runs
>    - Plot energy/property vs. composition curves
>    - Manage millions of structures in datasets (Materials Project, OQMD, JARVIS)
>    - Write papers / lab notebooks in LaTeX or Markdown
>
>    A **canvas-only** UI doesn't serve the "write the simulation script" core loop.
>    A **VSCode fork or extension** does — and Cursor/Windsurf prove the model works.
>
> **Revision direction:** keep the canvas as a *companion* surface for high-level
> orchestration (discourse, knowledge graph, comparison tables) but pair it with a
> VSCode-derived editor for the file/code/notebook work. Either:
>
> - **(A) VSCode extension** — ships fast, rides upstream, hits the broadest install
>   base. Bridge the canvas through the extension's webview API.
> - **(B) VSCodium fork** — own the brand and the boot screen, integrate the canvas
>   natively into the activity bar. More expensive but gives full Apple-feel.
>
> Need user-needs research before picking. See "Phase 4 plan v0.2 needed" below.

---

## TL;DR

Don't fork VSCodium as the primary surface. Build a **three-tier hybrid**:

1. **Tier 1 — PRISM CLI/TUI** (already shipped). Power users live here. Keep it.
2. **Tier 2 — Canvas (NEW)** — tldraw infinite canvas as the user-facing "blank page".
   Each block is a live PRISM operation (tool call, discourse, workflow, simulation, note).
   Agents spawn new blocks as their reasoning progresses. **This is the "Notion" feeling.**
3. **Tier 3 — IDE bridge (last)** — start as a VSCode extension that links the canvas to
   files. Promote to a VSCodium fork only if the extension hits hard limits.

Why this beats "fork VSCodium":
- A folder-tree IDE can't *be* the blank canvas. It's a file editor.
- tldraw + agentic blocks IS the blank canvas, by construction.
- The PRISM platform's existing capabilities (chat, tool routing, discourse, workflow, knowledge graph, compute, marketplace) map 1:1 to canvas block types.
- Cursor/Windsurf already won the "VSCode fork with AI" niche. Re-fighting that battle is sunk-cost.
- Apple-feel = blank slate that fills with intent. File tree = the opposite of that.

---

## What we learned from the field (May 2026)

| Tool | Pattern | Take for PRISM |
|---|---|---|
| **tldraw computer** ([computer.tldraw.com](https://computer.tldraw.com/)) | Infinite canvas, blocks of text/images/instructions, output of one block becomes input of the next, agents build workflows from a high-level prompt | This IS the "Notion blank canvas" the user wants. Use the [tldraw SDK](https://tldraw.dev/) (MIT-ish, React, mature). |
| **tldraw agent starter kit** ([tldraw.dev/starter-kits/agent](https://tldraw.dev/starter-kits/agent)) | LLM gets read+write access to the canvas, sees both screenshots AND structured shape data | Drop-in pattern; we just point the agent at PRISM's tool catalog. |
| **AG-UI / A2UI / Open-JSON-UI / MCP Apps** | Three patterns for generative UI: controlled, declarative, open-ended | Use **A2UI** for our block schemas (JSON-driven, agent-renderable), **MCP Apps** for marketplace-installed custom blocks. |
| **VSCode 1.109–1.110** (Feb 2026) | `/init` workspace priming, subagent architecture, agent customization files via `/create-*` | If we go IDE route, ride the new APIs — don't build them from scratch. |
| **Cursor / Windsurf** | VSCode fork → editor internals access for AI features | Forking is *expensive*. Cursor has 100+ engineers on it. We'd rather ship the canvas and use VSCode-extension for files. |
| **Roo Code / Cline** | Open-source agentic VSCode extensions | Existence proof: agentic file editing fits in an extension. |

Sources at the bottom.

---

## Architecture proposal

```
┌──────────────────────────────────────────────────────────────────┐
│ PRISM Canvas (browser, Tauri-wrapped for desktop)                │
│ ┌───────────────────────────────────────────────────────────┐   │
│ │ tldraw infinite canvas                                     │   │
│ │  ┌────────┐    ┌─────────┐    ┌──────────┐                │   │
│ │  │ Note   │───→│ Tool    │───→│ Discourse│  ←── agents    │   │
│ │  │ block  │    │ block   │    │ block    │      spawn     │   │
│ │  └────────┘    └─────────┘    └──────────┘      blocks    │   │
│ └───────────────────────────────────────────────────────────┘   │
│              │                        │                          │
│              │ block events           │ agent observes canvas    │
│              ▼                        │                          │
│         OpenAI-compatible API ◄───────┘                          │
└────────────────────┬─────────────────────────────────────────────┘
                     │ same wire format PRISM CLI uses today
                     ▼
┌──────────────────────────────────────────────────────────────────┐
│ PRISM platform_bridge (already shipped)                          │
│  - Stage 2.1 semantic top-K retrieval (EmbeddingGemma)           │
│  - Tool catalog: 125 tools across 22 API surfaces                │
│  - Forwards to MARC27 platform                                   │
└────────────────────┬─────────────────────────────────────────────┘
                     ▼
┌──────────────────────────────────────────────────────────────────┐
│ MARC27 platform (existing)                                       │
│  /discourse  /workflows  /knowledge  /compute  /jobs ...         │
└──────────────────────────────────────────────────────────────────┘
```

The **agent loop on the canvas is the same forge agent loop PRISM CLI uses today.** Same tools, same retrieval, same chat LLM. The canvas is a different *renderer* sitting on top — not a re-implementation.

### Block types (the "tools" of the canvas)

Each block maps to an existing PRISM/MARC27 capability:

| Block | Backed by | What the user sees |
|---|---|---|
| `Note` | local | Markdown text, drawings |
| `Question` | `/projects/{id}/llm` | Free-form natural language → chat response in-block |
| `Knowledge` | `/knowledge` | Graph queries with ontology hints |
| `Tool` | platform_bridge top-K + tool dispatch | One specific MCP tool with a form generated from its schema |
| `Discourse` | `/discourse` | Multi-agent debate (already wired in CLI as `prism discourse run`) |
| `Workflow` | `/workflows` | YAML pipeline run |
| `Simulation` | `/compute` + `/jobs` | GPU job submission, live progress |
| `Marketplace` | `/marketplace` | Install community blocks/tools |
| `Custom (MCP App)` | community | User-installed blocks via MCP Apps protocol |

**The "self-building" part:** when an agent runs (e.g. user types into a Question block), the agent can `tldraw.create_block(...)` to spawn additional Tool/Knowledge/Discourse blocks. The canvas grows in response to intent. No file tree, no manual layout — Apple-feel.

### Why a canvas (not a file tree) is the right primary surface

Materials research is **non-linear and graph-shaped**:
- "What alloys resist creep?" → 3 candidate alloys → for each, run knowledge query → discourse on tradeoffs → simulation on best 2.
- That's a graph, not a flat file. Trying to express it in a folder tree is shoehorning.
- Existing tools (Notion, tldraw, Miro) won this exact debate for general knowledge work.

### Why VSCode (extension) for Tier 3, not a fork

| Thing | VSCode extension | VSCodium fork |
|---|---|---|
| Time to MVP | 1–2 weeks | 3–6 months |
| Maintenance | Ride upstream | Forever rebase pain |
| Reach | Anyone with VSCode/Cursor/Windsurf | Whoever installs PRISM IDE |
| Editor internals | Limited | Full |
| Apple-feel branding | Compromised (still says VSCode) | Full white-label |

**Recommend: extension first.** Reach for the fork only if a feature genuinely needs editor internals (e.g. inline materials-science syntax highlighting that VSCode's tokenizer can't express). Even then, ship the extension first to validate the demand.

---

## Concrete build plan (phase 4)

**Week 1 — Canvas skeleton**
- Scaffold a Vite + React + tldraw app at `crates/canvas/` (or a new `prism-canvas` repo if scope demands).
- Wire it to PRISM CLI's existing local proxy (the 127.0.0.1:NNNNN OpenAI-compat endpoint that forge points at).
- One block type: `Question` (just sends to chat LLM).
- Goal: type into a block, hit Enter, see response stream into the same block.

**Week 2 — Discourse + Tool blocks**
- Add `Discourse` block type that calls `/discourse/run/{spec_id}`.
- Add `Tool` block type with a JSON-schema-generated form (use [`react-jsonschema-form`](https://rjsf-team.github.io/react-jsonschema-form/)).
- Auto-suggest blocks: agent reads current canvas state and suggests "you might want to add a Knowledge block here" with a click-to-confirm.

**Week 3 — Self-building**
- Agent gets `tldraw.create_block` and `tldraw.connect_blocks` as tools.
- Test: "Compare Inconel 718 and CMSX-4 on operating temperature" → agent autonomously spawns 2 Knowledge blocks, 1 Discourse block, 1 final Note block summarizing the result.
- This is the "blank canvas building itself" demo.

**Week 4 — Tauri + ship**
- Wrap in Tauri (Rust) so it ships as a native macOS/Windows/Linux app.
- Connect to the PRISM CLI's local proxy by default; optionally direct to MARC27 with API key.
- Brand as **PRISM Canvas** (Tier 2). Tier 3 IDE bridge stays a separate project.

**Out-of-scope for phase 4:** the VSCode extension/fork. Build it once Canvas validates that the canvas-as-primary-surface bet is right.

---

## Risks + mitigations

| Risk | Mitigation |
|---|---|
| Agents create too many blocks, canvas becomes noise | Hard cap (e.g. 12 spawned blocks per turn), confirmation UI for spawning |
| Canvas state model gets gnarly | Use [tldraw's record store](https://tldraw.dev/docs/persistence) — proven at scale |
| Browser-only excludes terminal users | Canvas is **Tier 2**. Tier 1 (CLI/TUI) stays. Power users keep terminal. |
| Materials-specific blocks bloat the schema | Block schema is JSON-driven via A2UI/MCP Apps — community can ship custom blocks via marketplace, no PR to PRISM needed |
| Latency of chat-LLM-per-block feels slow | Same as today's CLI chat. Canvas surfaces it differently (per-block spinner) but doesn't make it slower. |

---

## What I'm NOT recommending

- **Don't write a custom canvas engine.** tldraw is mature, MIT-licensed (kind of — read their license), and exactly fits.
- ~~**Don't fork VSCodium first.** Build the canvas, validate the bet, then decide.~~
  → **Revisit per pushback above.** Materials scientists write code daily; a
  canvas-only product fights the core workflow.
- **Don't replace the CLI.** Power users want it; ML-pipeline scripting needs it; SSH-only environments need it.
- **Don't build a generic agentic IDE.** The product is materials-science-shaped. Lean into that.

---

## Phase 4 plan v0.2 needed

Before locking the IDE architecture, do this research first:

1. **User-needs interviews / shadowing** — 5-10 materials scientists across sub-fields
   (DFT, MD, experimental, ML for materials). What's their day look like? Where do they
   spend hours? What's painful? Which VSCode extensions do they live in?
2. **Workflow audit** — pick 3 representative end-to-end tasks (e.g. "predict creep
   resistance of a new alloy", "screen 10K candidates from Materials Project for
   stability") and map the tool surface they touch.
3. **Decision** — given (1) and (2), pick: VSCode extension only / VSCodium fork /
   canvas-only / extension+canvas hybrid. Don't lock in before this.

The v0.1 architecture above (CLI / Canvas / extension) is one plausible answer.
After research, the answer might be (CLI / VSCodium fork with embedded canvas panel)
which is what Cursor effectively is for general coding — but materials-science-shaped.

---

## Sources

Researched via WebSearch (no EXA — paid tier, skipped per user direction):

- [tldraw infinite canvas SDK](https://tldraw.dev/)
- [tldraw computer — Visual computing on a canvas](https://computer.tldraw.com/)
- [tldraw agent starter kit](https://tldraw.dev/starter-kits/agent)
- [Gemini Powers tldraw's Natural Language Computing — Google AI](https://ai.google.dev/showcase/tldraw)
- [VSCode February 2026 release (1.110)](https://code.visualstudio.com/updates/v1_110)
- [Top Agentic AI Tools for VS Code — Visual Studio Magazine](https://visualstudiomagazine.com/articles/2025/10/07/top-agentic-ai-tools-for-vs-code-according-to-installs.aspx)
- [Coding Agents Showdown: VSCode Forks vs IDE Extensions vs CLI Agents — ForgeCode](https://forgecode.dev/blog/coding-agents-showdown/)
- [The Accidental AI Canvas — Latent Space podcast with Steve Ruiz of tldraw](https://www.latent.space/p/tldraw)
- [The 13 Best Agentic IDEs in 2026 — DataCamp](https://www.datacamp.com/blog/best-agentic-ide)
- [Building the Agentic UI Stack: AG-UI, A2UI, State Sync — DevJournal](https://earezki.com/ai-news/2026-05-01-a-coding-deep-dive-into-agentic-ui-generative-ui-state-synchronization-and-interrupt-driven-approval-flows/)
