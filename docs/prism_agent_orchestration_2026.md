# PRISM as a Fully Agent-Orchestrated App — Feasibility Memo

**Status:** Research / feasibility draft v0.1 — no code changes proposed yet.
**Question from the user:** *"PRISM runs locally as a CLI tool. Can we give access to it and make it 100% agent-orchestrated — a real app, like Zed?"*
**Scope:** What "like Zed" concretely means, how much of it PRISM already has, the gap, and a recommended path. Written to inform a decision, not to lock one in.

---

## TL;DR

1. **PRISM's *backend* is already ~85% "agent-orchestrated."** There is a real Think-Act-Observe-Repeat (TAOR) agent loop (`crates/agent/src/agent_loop.rs::run_turn`), tool retrieval, sub-agents, an MCP client, OPA policy gating, and — critically — a **transport-agnostic JSON-RPC-over-stdio protocol** (`crates/agent/src/protocol.rs`, documented in `docs/FRONTEND_PROTOCOL.md`) that already lets *any* frontend drive the agent. The "CLI tool" framing undersells what's under the hood.

2. **"Like Zed" decomposes into three separable things.** Zed is (a) a **native desktop shell** (Rust/GPUI, not web), (b) an **agent-first UX** (the agent panel, inline edits, approvals), and (c) the **Agent Client Protocol (ACP)** — a JSON-RPC-over-stdio contract that lets an editor host *any* external agent, and lets an agent plug into *any* ACP editor. You can adopt these independently.

3. **The single highest-leverage finding:** PRISM's existing frontend protocol is *architecturally the same shape as ACP* — JSON-RPC 2.0 over stdio, one side sends user turns, the other streams typed UI events + approval prompts. **Making PRISM speak ACP is a protocol-mapping exercise, not a rewrite.** Once it does, PRISM's materials-science agent runs *inside Zed itself* — you get a native, Zed-quality GUI "for free" without building one.

4. **Recommended path (phased):**
   - **Phase 0 (spike, ~days):** Wrap the existing backend in an ACP adapter → run PRISM as an agent inside Zed. This is the literal "runs like Zed" unlock, with almost no new UI code.
   - **Phase 1 (weeks):** Close the "100% agent-orchestrated" gap — route the command paths that currently *bypass* the LLM (`input.command`) through the agent loop, so the agent is the single orchestrator.
   - **Phase 2 (later, opt-in):** If you want a *branded* PRISM app rather than "PRISM-inside-Zed," build a thin native shell (GPUI or Tauri) that speaks the *same* ACP/JSON-RPC backend. This reconciles with, and supersedes, the earlier `prism_ide_design_2026.md` canvas/Tauri direction.

5. **This is cheap because the hard part is already built.** The backend, tools, agent loop, and protocol exist. What's missing is a protocol adapter and a decision about the shell.

---

## 1. What "like Zed" actually means

Zed is worth copying deliberately, not by vibes. It is three distinct bets, and PRISM can take any subset:

| Layer | What Zed does | Is it the thing you want? |
|---|---|---|
| **Native shell** | Rust + GPUI, GPU-accelerated, native desktop app — *not* Electron/web. This is why it "feels like an app." | Your "not web, it's the best app" steer points here. |
| **Agent-first UX** | Agent panel, inline diffs, tool-call cards, approval flow, threads, "follow the agent" view. The agent is a first-class pane, not a chatbox bolted on. | Yes — this is "100% agent-orchestrated" as a *product*. |
| **Agent Client Protocol (ACP)** | An open JSON-RPC-over-stdio protocol that **decouples the editor from the agent.** Zed hosts external agents (Gemini CLI, Claude Code, custom) as ACP subprocesses; conversely an agent that speaks ACP shows up in Zed's agent panel. | This is the interop layer — and PRISM is one adapter away from it. |

**Key consequence:** because of (c), you do **not** have to choose between "PRISM plugs into Zed" and "PRISM is its own app." ACP is a two-sided contract:
- Speak ACP as a **server/agent** → PRISM runs *inside* Zed (and Neovim, Emacs, any ACP host).
- Speak ACP as a **client/host** → a future PRISM shell can run *other* agents the way Zed does.

Same protocol, both directions. That is the architectural gift here.

---

## 2. Where PRISM is today (the parts that already exist)

This matters because it changes the estimate from "build an agentic app" to "adapt an agentic backend." Evidence, with files:

| Capability | Status | Where |
|---|---|---|
| TAOR agent loop (LLM picks tools → execute → repeat) | **Shipped** | `crates/agent/src/agent_loop.rs::run_turn` (loop at `:786`; turn-complete when no tool calls at `:1068`) |
| Transport-agnostic backend | **Shipped** | `crates/agent/src/protocol.rs` (JSON-RPC/stdio) + `crates/agent/src/service.rs` ("so ANY client — HTTP, MCP, future transports — gets the same agent") |
| Documented frontend protocol | **Shipped** | `docs/FRONTEND_PROTOCOL.md` — `init`, `input.message`, `input.command`, `approval.respond` → `ui.text.delta`, `ui.tool.start`, `ui.card`, `ui.prompt`, `ui.turn.complete` |
| Frontend/backend split (TUI is *just* a client) | **Shipped** | `crates/tui/src/backend.rs:135` spawns `prism backend` and talks JSON-RPC over stdio |
| Sub-agents (hierarchical delegation) | **Shipped** | `crates/agent/src/subagent.rs` — `spawn_subagent`, depth ≤ 2, token-budgeted, inherited gating |
| MCP **client** (consume external tools) | **Shipped** | `crates/agent/src/mcp.rs` (Rust, `rmcp`); `app/mcp_client.py` (Python) |
| MCP **server** (expose PRISM as tools) | **Shipped** | `app/mcp_server.py`; `crates/cli/src/mcp_server_native.rs` |
| Tool catalog + neural retrieval (top-K per turn) | **Shipped** | `crates/agent/src/tool_catalog.rs`; retrieval at `agent_loop.rs:807` |
| ~100+ tools (Python sidecar) + Rust command-tools | **Shipped** | `app/tools/*`; `crates/agent/src/command_tools.rs` |
| Approval gating + OPA policy | **Shipped** | `crates/agent/src/permissions.rs`; `crates/policy` |
| Provenance / durable memory | **Shipped** | `crates/provenance`; `recall`/`fetch_artifact` meta-tools |
| Autonomous campaigns (long-running loops) | **Shipped** | `crates/campaign/src/lib.rs` drives `run_turn` per iteration |
| Native desktop GUI | **Missing** | Only Ratatui TUI (`crates/tui`) + an unfinished web dashboard (`dashboard/`) + `macos/PRISMMac` |
| ACP conformance | **Missing** | Protocol is *ACP-shaped* but not ACP-compliant (method names, capability handshake, content blocks differ) |
| "Everything through the agent" | **Partial** | `input.command` slash commands **bypass the LLM** by design (`FRONTEND_PROTOCOL.md`: *"Does NOT trigger an LLM turn"*) |

**Bottom line:** the expensive, hard-to-get-right parts (agent loop, tool dispatch, approvals, provenance, a clean frontend/backend boundary) are done. What's missing is a GUI decision and a protocol adapter.

---

## 3. The gap — decomposed into two independent workstreams

The user's ask bundles two things that are actually separable. Treating them separately makes each tractable.

### Gap A — "100% agent-orchestrated" (a backend/UX property)

Today PRISM has **two control paths**:
1. **Agentic path:** `input.message` → `run_turn` → LLM decides tools. This is already agent-orchestrated.
2. **Scripted path:** `input.command` (slash commands) and many `prism <subcommand>` CLI verbs run *deterministic code* without the LLM. These are wrapped as "command-tools" (`crates/agent/src/command_tools.rs`) so the agent *can* call them, but the human can also invoke them directly, bypassing the agent.

"100% agent-orchestrated" means: **the agent is the single orchestrator; humans express intent, the agent chooses the path.** Concretely:
- Keep command-tools as *tools the agent calls*, but make the human-facing entry point default to the agent (natural language), with slash commands as fast-path shortcuts the agent is *aware of* (so it can chain them).
- Fold the remaining bespoke CLI subcommands (`query`, `research`, `workflow`, `ingest`, `mesh`, `compute`…) into the tool catalog uniformly (most already are via `COMMAND_TOOLS`), so there is one dispatch surface.
- The workflow engine (`crates/workflows`, YAML DAGs) becomes *a tool the agent can author and run*, not a parallel universe.

This is mostly **consolidation of paths that already exist**, not new capability. Risk is low; the main cost is UX design (what stays a shortcut vs. what becomes agent-mediated) and not regressing power-user ergonomics.

### Gap B — "a real app, like Zed" (a frontend/protocol property)

Two sub-parts:
- **B1 — Protocol:** make the backend speak **ACP** so it can be hosted by real editors. PRISM's protocol already has the same primitives (turns in, streamed UI events out, approval round-trips); the work is mapping names/shapes to the ACP spec and implementing its capability handshake and content-block model.
- **B2 — Shell:** decide the surface. Options in §4.

---

## 4. Architecture options for the shell (B2)

Three viable surfaces. They are **not mutually exclusive** — all three can sit on the *same* ACP/JSON-RPC backend, which is the whole point.

### Option A — PRISM-as-ACP-agent, hosted by Zed *(fastest "runs like Zed")*
Ship an ACP adapter; users install Zed and add PRISM as an agent. PRISM's materials-science brain appears in Zed's native agent panel.

- ✅ Native, GPU-accelerated, polished GUI on day one — **you don't build or maintain a GUI.**
- ✅ Rides Zed's roadmap (threads, inline edits, file tree, LSP, debuggers) — directly answers the `prism_ide_design_2026.md` pushback that *"materials scientists write code daily and need the editor ecosystem."*
- ✅ Also lands you in Neovim/Emacs/other ACP hosts for free.
- ❌ Not *branded* PRISM — it says "Zed." No control over boot screen / identity.
- ❌ Bounded by what ACP exposes (you can't add arbitrary PRISM-specific panes inside Zed).

### Option B — Native PRISM desktop shell speaking ACP *(branded end-state)*
A thin PRISM-branded native app (GPUI, the same toolkit Zed uses, or egui/iced) that is an ACP **host** embedding the PRISM agent, plus PRISM-specific panes (knowledge graph, canvas, compute dashboard).

- ✅ Full brand/identity control; "the best app," native feel.
- ✅ Can *also* host other ACP agents (Claude Code, Gemini) — PRISM becomes a materials-science IDE that runs any agent.
- ✅ Reuses the exact same backend as Option A — Option A is the stepping stone, not throwaway work.
- ❌ Real engineering cost (GPUI is powerful but low-level; this is a multi-month effort to reach polish).
- ❌ You inherit the "build an editor" maintenance burden Cursor/Zed have 50–100+ engineers on.

### Option C — Tauri/web canvas *(the earlier `prism_ide_design_2026.md` direction)*
Vite+React (tldraw canvas) wrapped in Tauri.

- ✅ Fast to prototype; rich generative-UI/canvas story; good for the "self-building canvas" vision.
- ❌ **Conflicts with the "not web, it's the best app / like Zed" steer** — Tauri is a web view in a native window; it will not feel like Zed.
- ❌ The earlier doc already caught the flaw: canvas-only doesn't serve the "write the simulation script" core loop.

**Recommendation:** **A now, B as the productized destination, C demoted to an optional companion pane inside B.** Because all three ride the same backend, Option A is a genuine down payment on Option B — nothing is wasted. Start by *literally running like Zed* (A), validate the agent-in-editor UX with real materials scientists, then decide whether the brand/identity payoff justifies building the native shell (B).

---

## 5. Recommended phased roadmap

| Phase | Goal | Work | Rough effort |
|---|---|---|---|
| **0. ACP spike** | PRISM agent runs inside Zed | Build `prism acp` subcommand: an adapter translating ACP ⇄ existing `protocol.rs` events. Map `init`→ACP `initialize`/capability handshake, `input.message`→`session/prompt`, `ui.*`→ACP session updates, `approval.respond`→ACP permission requests. Register PRISM as a Zed agent. | **Days** (adapter over an existing protocol) |
| **1. Agent-first consolidation** | "100% agent-orchestrated" | Make the agent the default entry point; ensure every CLI verb is a tool in the catalog; let the agent author/run workflows; keep slash commands as agent-aware shortcuts. Mostly path consolidation. | **1–2 weeks** |
| **2. Materials-science UX in-editor** | Make it *useful* in Zed, not just present | ACP content blocks + rich tool cards for structures/plots/knowledge-graph results; wire approval UX; provenance surfacing. | **2–4 weeks** |
| **3. (Decision gate)** | Branded native shell? | Only if A validates the bet: start GPUI/Tauri host (Option B) on the same backend. Fold the canvas (Option C) in as one pane. | **Months** (opt-in) |

Phases 0–2 deliver "PRISM, fully agent-orchestrated, running like Zed." Phase 3 is the "make it *our* app" investment, deferred until the cheaper path proves the direction.

---

## 6. Risks & open questions

**Risks**
- **ACP is young and moving.** The spec evolves; an adapter insulates the core (good) but needs upkeep. Pin a version, keep the adapter thin.
- **Security surface grows.** Running inside a third-party editor, and/or hosting external agents, widens the trust boundary. PRISM already has OPA gating + approvals (`crates/policy`, `permissions.rs`) — keep those authoritative *inside* the agent, never delegate them to the host UI.
- **"100% agentic" can hurt power users.** Forcing every action through the LLM adds latency/cost to things that were instant CLI calls. Mitigation: keep deterministic shortcuts, but make them agent-*visible* rather than agent-*bypassing*.
- **Two GUIs to reason about.** The Ratatui TUI stays valuable (SSH/headless). Don't delete it; it's a client of the same backend, so it's cheap to keep.

**Open questions for you (would sharpen a v0.2 and any implementation plan)**
1. **"Like Zed" — which slice?** Literally *inside Zed* (Option A), or a *native PRISM-branded app that behaves like Zed* (Option B)? Your "the best app" phrasing leans B; your "runs like Zed" phrasing is satisfied cheapest by A. They share a backend, so A-then-B is coherent — but the target picture changes what we optimize.
2. **Coding editor or orchestration surface?** Zed is fundamentally a *code editor*. Do materials scientists using PRISM primarily (a) write/run simulation code (→ editor matters, Option A/B strong), or (b) orchestrate discovery at a high level (→ canvas/Option C matters)? The earlier IDE doc flagged this needs real user research.
3. **"Give access to prism"** — did you mean expose the agent *to* external editors (ACP server, Option A), or have PRISM *host* other agents (ACP client, Option B)? ACP gives both; which is the priority?
4. **Brand vs. speed** — is shipping *inside Zed* in days acceptable as v1, or is a PRISM-branded native shell a hard requirement from the start?

---

## 7. How this reconciles with prior design docs

- `docs/prism_ide_design_2026.md` (canvas/Tauri, "self-building canvas") — the pushback recorded there ("materials scientists write code daily; a canvas-only product fights the core workflow") is **decisively resolved by ACP + Zed**: you get the full editor ecosystem *and* the agent, without forking VSCodium or fighting Cursor/Windsurf. The canvas survives as an optional companion pane, not the primary surface.
- `docs/FRONTEND_PROTOCOL.md` — this is the asset that makes the whole plan cheap. ACP is "FRONTEND_PROTOCOL with a standardized vocabulary." The adapter is the bridge between them.
- The existing `prism backend` / `crates/ipc` / `crates/agent/src/service.rs` boundary is exactly the seam an ACP adapter plugs into. No re-architecture required.

---

*Draft v0.1 — grounded in the current `Darth-Hidious/PRISM` tree. No code changed. Next step on request: a v0.2 that answers the four open questions and/or a Phase-0 ACP spike.*
