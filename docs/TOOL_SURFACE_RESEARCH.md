# Tool-Surface & Research-Agent — State of the Art Digest

> Status: **Phase 1 research deliverable** for the PRISM tool-surface production-grade effort.
> Every finding is tagged **[EVIDENCE-BACKED]** (cited to a primary source: paper, spec, or
> vendor engineering post) or **[INFERRED]** (our synthesis / reasoned extrapolation). A
> `→ PRISM:` line maps each finding to a concrete recommendation in this codebase.
> Compiled 2026-07-10.

---

## How to read this

Four areas, each ending in recommendations:

1. **Tool retrieval at scale** — how to select the right tool from 100+ without stuffing the
   prompt. (PRISM already has neural retrieval; this section grades maturity and names the
   next refinements.)
2. **Reasoning / acting loops** — ReAct vs. Plan-and-Execute and what they imply for a
   *long-running* task loop (directly informs the Task object in `run_turn`).
3. **Tool DESIGN** — what makes a tool's description + schema drive *correct* model selection
   and argument-filling (the core of the SPEC).
4. **Context engineering for long-running agents** — memory/trajectory/artifact handling and
   resumability (the core of the context-passing contract).

The digest closes with a **prioritized recommendation list** that flows directly into
`TOOL_SURFACE_SPEC.md` (Phase 3) and `TOOL_SURFACE_AUDIT.md` (Phase 2).

---

## 1. Tool retrieval at scale

### 1.1 Stuffing all tools fails predictably; retrieval fixes it [EVIDENCE-BACKED]

**RAG-MCP** (arXiv:2505.03275, 2025) is the canonical result. On a large MCP toolset, naive
"put every tool schema in the prompt" selection scores **13.62%** tool-selection accuracy;
retrieving a top-K relevant subset first lifts that to **43.13%** — roughly a 3× improvement —
*while cutting prompt tokens by >50%* (up to ~11K tokens saved). Open-source reproductions
report **74.8% token reduction** and **62.1% faster** responses at minimal accuracy cost.

- → PRISM: **Neural retrieval is already shipped and ON by default**
  (`crates/agent/src/capability.rs:11-16`; `agent_loop.rs:721`). This was the right call. The
  RAG-MCP win is largely banked. The remaining work is *maturity*, not adoption.

### 1.2 At 527 tools, ~50% of agent failures are retrieval errors [EVIDENCE-BACKED]

**LiveMCPBench** (arXiv:2508.01780, 2025) deploys 70 MCP servers / **527 tools** and finds
that **nearly half of all agent failures stem from tool-retrieval errors** — not execution
errors. Retrieval is the primary bottleneck at scale. The CAPABILITY_REGISTRY_DESIGN doc
already cites this ("LiveMCPBench (527 tools): ~50% of all agent failures are *retrieval*
errors").

- → PRISM: Retrieval quality is a first-class reliability concern, not a nicety. Our catalog
  is >100 tools (`tool_catalog.rs:223` comment "all 99 tools"; real count now larger). The
  `find_tools` manual escape (`meta_tools.rs`) must stay as the tail-case recovery for the
  99%-paradox, and the neural index must stay warm off the turn path
  (`spawn_neural_warm`, `agent_loop.rs:620`).

### 1.3 "Less is more" — fewer tools shown can raise accuracy [EVIDENCE-BACKED]

**Less-is-More** (arXiv:2411.15399, 2024) shows a fine-tuning-free, similarity-based
*dynamic* reduction of the visible tool set improves function-calling accuracy and cost,
especially under tight budgets. A follow-up, "How Many Tools Should an LLM Agent See?"
(2025), confirms similarity-based filtering helps.

- → PRISM: Our `MAX_TOOLS_PER_REQUEST = 15` (`tool_catalog.rs:6`) plus top-K neural selection
  already implements this principle. The cap is sound; do not raise it casually.

### 1.4 Embed *example queries*, not just descriptions (Tool2Vec) [EVIDENCE-BACKED]

**Tool2Vec** (SqueezeAILab) builds *usage-driven* embeddings: instead of embedding the tool's
description, it embeds **multiple example user queries** per tool and averages them, so the
vector reflects *how the tool is actually invoked* rather than how it is *described*.
Retrieval over usage-embeddings is more accurate than over description-embeddings,
especially when descriptions are terse or use jargon that does not match user phrasing. This
is exactly our weak spot: our `retrieval_text = "{name}: {description}"`
(`capability.rs:25-28`), which the code itself flags as "a later refinement."

- → PRISM: **Add an optional `example_queries: Vec<String>` to each capability and fold them
  into `retrieval_text`** (e.g. `"{name}: {description}\nqueries: q1; q2; q3"`), then re-embed.
  The existing `CapabilityIndex` + BGE-small backend (`crates/embed`) already do the heavy
  lifting; this is a content + scoring change, not new infra. **This is the single
  highest-leverage retrieval refinement.**

### 1.5 Rerank + confirmation beat recall alone (the 99% paradox) [EVIDENCE-BACKED]

Near-perfect *recall* in retrieval can still equal near-random *selection* at scale —
getting the right tool into the top-K is not the same as the model *picking* it. The
CAPABILITY_REGISTRY_DESIGN doc cites "The 99% Success Paradox" (arXiv:2605.18857). RAG-MCP's
own ablations and the Graph-RAG-Tool-Fusion line both point to a **rerank** stage
(cross-encoder or LLM-rerank) and to keeping the model in a confirmation loop as the
mitigations.

- → PRISM: Cosine top-K (what we have) is the cheap first win. A rerank step is a *later*
  phase — spec it in `TOOL_SURFACE_SPEC.md` §retrieval-maturity but do not build it in the
  first implementation pass. Keep `find_tools` as the explicit confirmation escape.

### 1.6 Massive-API adaptation via retrieval + self-authoring [EVIDENCE-BACKED]

**Gorilla** (arXiv:2305.15334, Berkeley) fine-tunes on 1,600+ APIs and, *combined with a
document-retrieval system*, adapts to documentation changes and reduces hallucination. The
*self-authoring* analog we already ship is the Voyager-style `write_skill` loop
(`skills.rs`, `meta_tools.rs`) — generate → execute → verify → store, retrievable next turn.
This is the established pattern (3.3× prior SOTA on open-ended discovery per Voyager).

- → PRISM: Keep `write_skill` as the first-class self-authoring path; ensure authored skills
  are untrusted-by-default (they already are, `skills.rs:49`) and embed their
  `when_to_use` like any capability (already designed, `CAPABILITY_REGISTRY_DESIGN §4.3`).

---

## 2. Reasoning / acting loops

### 2.1 ReAct degrades on long horizons [EVIDENCE-BACKED]

ReAct (reason → act → observe, one step at a time) is the pattern our `run_turn` inner loop
implements (`for iteration in 0..max_iterations`, `agent_loop.rs:687`). The literature is
consistent that **pure ReAct degrades on long-horizon tasks** due to: (a) context bloat/drift
as observations accumulate, (b) no global plan (locally-good, globally-poor choices), (c)
error accumulation with no re-planning, (d) cost/latency of many large-model calls.

- → PRISM: Our `max_iterations = 20` (`types.rs:138`) bound plus the deterministic
  TRAJECTORY block (`agent_loop.rs:89`) + SESSION MEMORY block are *partial* mitigations for
  within-turn drift. But there is **no plan object and no cross-turn task state inside the
  loop** — confirmed in the audit. This is the structural gap the bridge architecture closes.

### 2.2 Plan-and-Execute beats ReAct on multi-step structured tasks [EVIDENCE-BACKED]

**Plan-and-Act** (arXiv:2503.09572, ICML 2025) separates a **Planner** (produces a high-level
plan) from an **Executor** (carries it out step by step) and adds **dynamic replanning** —
the planner updates the plan after each executor step, addressing the stochastic-drift
weakness of a single static plan. It is designed specifically to fix ReAct's long-horizon
failure mode. Practitioner guidance (LangChain "Planning Agents") agrees: Plan-and-Execute
promises "faster, cheaper, and more performant" execution on structured long tasks, and can
use a cheaper model for execution.

- → PRISM: This is the **theoretical basis for the Task/plan-position object** we add to
  `run_turn`. A research campaign = a plan (sequence of research steps) + an executor that
  drives one `run_turn` per step + the campaign checkpoint as the durable plan-position
  store. Dynamic replanning maps to "update the plan position in the checkpoint after each
  turn." The campaign engine already has checkpoint/resume/budget/approval
  (`crates/campaign`); the bridge makes `run_turn` its executor instead of the hardcoded
  propose/evaluate/rank body.

### 2.3 When to keep ReAct [INFERRED]

The same sources say ReAct is preferable for **short, dynamic, tool-heavy** turns where the
next action genuinely can't be predicted and tight environment feedback matters. Chat turns
(short, reactive) fit this; long research tasks (structured, plannable) fit Plan-and-Execute.

- → PRISM: **Keep the chat path as ReAct (task=None).** Add the Plan-and-Execute shape *only*
  for task-driven turns (task=Some). One loop, two modes, no chat regression. This is
  enforced by a "chat-path-unchanged" test.

---

## 3. Tool DESIGN — what makes a description + schema drive correct use

This is the core of the SPEC. The most authoritative primary source is Anthropic's
**"Writing effective tools for AI agents"** engineering post; it is reinforced by the
**MCP Tools specification** and by OpenAI function-calling conventions.

### 3.1 Tool descriptions are prompts [EVIDENCE-BACKED]

Anthropic: *"Tool descriptions are prompts. Every word in your tool's name, description, and
parameter documentation shapes how agents understand and use it."* Treat the description
text as first-class prompt engineering, not documentation.

- → PRISM: Adopt a **description-writing standard** (purpose / when-to-use / when-not /
  side-effects / returns) with a length floor. Enforce mechanically with the CI lint (Phase 4).

### 3.2 The biggest practical win: make tools' boundaries clear and distinct [EVIDENCE-BACKED]

Anthropic's central practical lesson: most tool-use failures are *overlap* failures — the
model can't tell which of two similar tools to call. The cure is (a) clear, minimal-overlap
descriptions and (b) description rewrites that disambiguate. They report using *agents
themselves* to rewrite tool descriptions and then A/B-testing the rewrites on eval sets, with
measurable accuracy gains.

- → PRISM: Several PRISM tool clusters have natural overlap (e.g. `query` /
  `query_local` / `query_platform` / `query_federated` / `research` /
  `research_query`; `mesh_*`; `goal_*`). The audit must flag overlap clusters, and the
  descriptions must each state *when to use THIS one vs. the sibling*. This is higher
  leverage than schema richness for selection accuracy.

### 3.3 Include examples, edge cases, and input-format requirements [EVIDENCE-BACKED]

Anthropic ("Building Effective Agents"): *"A good tool definition often includes example
usage, edge cases, input format requirements, and clear boundaries from other tools."*

- → PRISM: The SPEC should *encourage* (and the lint should *allow*) `examples` in JSON
  Schema parameter definitions and example invocations in descriptions. (Not a hard fail —
  but rewarded.) This dovetails with §1.4: example queries double as both selection signal
  (embedded) and arg-filling guidance (in-schema).

### 3.4 The MCP spec mandates name + description + inputSchema; quality is not optional [EVIDENCE-BACKED]

The **MCP Tools spec** (2025-06-18) requires three components per tool: a unique `name`, a
functional `description`, and an `inputSchema` (a JSON Schema). Community standardization work
(**SEP-1382**) pushes the spec to add *documentation best practices* — clear descriptions and
per-parameter docs — because inconsistent descriptions are a measured quality problem
(arXiv:2602.14878, "MCP Tool Descriptions Are Smelly"). The spec also supports **structured
output** (`outputSchema`) and **elicitation** for tools that need follow-up input.

- → PRISM: Our `LoadedTool` already carries `name`, `description`, `input_schema`
  (`tool_catalog.rs:17-26`), and `to_definition()` emits the OpenAI/MCP function shape. The
  gap is *quality enforcement*, not shape. Add the lint. Consider an `output_schema` /
  result-contract field in the SPEC for tools whose return shape is load-bearing for later
  steps (this is the "handle, not blob" contract — see §4).

### 3.5 Prefer fewer, composable tools; avoid over-fragmentation [EVIDENCE-BACKED]

Anthropic recommends using **fewer tools** to reduce context load and overlap confusion; use
**composability** (one tool with an `action`/`mode` discriminator and a tight enum) over
many micro-tools when the actions share an object. PRISM already does this well in places
(e.g. `file` with an `action` enum, `dataset`, `mcp_services_invoke`) — these are the
*exemplars* to standardize on.

- → PRISM: The SPEC should bless the `action`-enum composability pattern where it fits and
  warn against splitting a cohesive object into N single-action tools. (No mass merge/split
  required — this is guidance, not a refactor mandate.)

### 3.6 Parameterize honestly: the generic-args anti-pattern [INFERRED]

A tool that exposes `{"type":"object","properties":{"args":{"type":"array","items":"string"}}}`
tells the model *nothing* about what arguments to pass — it forces the model to guess CLI
flag names from the description text, which is exactly the mis-parameterization failure
RAG-MCP measures. This is strictly worse than a rich schema or even an honest empty schema
(one that declares "no parameters").

- → PRISM: **This is the real schema gap in PRISM.** The Rust `CommandToolSpec` surface uses
  `root_args_schema` (a generic `args: array<string>`, `command_tools.rs:822`) for many tools
  that *do* have distinct, knowable arguments. The audit (Phase 2) quantifies this; Batch 1
  of Phase 4 replaces the generic-args escape with typed schemas for those tools. (The Python
  side, by contrast, is mostly already healthy — only ~6 genuinely parameter-less tools use
  an empty schema there, and that is correct.)

---

## 4. Context engineering for long-running agents

### 4.1 Context is finite; active management beats larger windows [EVIDENCE-BACKED]

Anthropic ("Effective context engineering for AI agents"): context is a *critical but finite*
resource; simply expanding the window often **degrades** performance ("lost in the middle"),
so *active curation* matters more than raw capacity. LangChain distills the playbook to four
operations: **write, select, compress, isolate**.

- → PRISM: We already **compress** (transcript compaction at 75% of window,
  `transcript.rs:18`; `compact_history` `agent_loop.rs:459`), **select** (neural top-K tools),
  and **isolate** (large results spilled to provenance + `recall`). The missing op at
  task granularity is **write**: a model-facing working-memory surface the task writes
  hypotheses/next-steps to. (The scratchpad exists but is *not* model-facing — audit finding.)

### 4.2 The harness — not the model — owns compaction, checkpoints, resume [EVIDENCE-BACKED]

Anthropic ("Effective harnesses for long-running agents"): the *harness* (scaffolding around
the model) owns **compaction** (so the agent works across many context windows without
exhausting one), **checkpoints/subagent-isolation**, and the ability to **stop, restart, and
keep improving** across runs. The model should not manage its own context lifecycle.

- → PRISM: This validates the bridge architecture precisely. The **campaign engine is the
  harness** (checkpoint/resume/budget/approval already exist, `crates/campaign`);
  `run_turn` becomes the per-context-window executor. The harness owns the checkpoint and
  re-drives the executor from the checkpointed plan position. We already have every harness
  primitive except the executor wiring — that wiring is Phase 4c.

### 4.3 Return references, not blobs [EVIDENCE-BACKED]

A recurring practitioner lesson (Anthropic, LangChain, deepset/Haystack): long-running agents
must pass **references/handles to artifacts** between steps, not inline large payloads, or
the context fills in 15–20 steps. The model retrieves the full content on demand.

- → PRISM: **We already do this** for oversized results (`process_large_result`,
  `agent_loop.rs:59` spills to `result_store` + provenance; model gets a `recall(query=…)`
  pointer). Phase 4c *formalizes* it as the cross-step/cross-turn context-passing contract:
  node-fetched data and any result above N chars gets a typed `artifact_ref` into the
  provenance store; later steps pull via `recall`/`fetch_artifact`. This converts the
  turn-local optimization into a durable, task-scoped pattern.

### 4.4 Make the trajectory deterministic [EVIDENCE-BACKED]

Anthropic and the Plan-and-Act paper both stress that long horizons die when the model
forgets (or ignores) what it already did. Memory tools are *opt-in* for the model, so recall
is probabilistic; the harness must make the recent past **deterministic** (a system block
showing the last N executed steps as one-line pointers).

- → PRISM: **Already shipped** — the TRAJECTORY block (`agent_loop.rs:89-105`) and SESSION
  MEMORY block (`load_session_memory`) do exactly this. The gap is that TRAJECTORY is
  *within-turn only* and SESSION MEMORY is *read-only at turn start*. For a multi-turn task,
  the task's plan-position + completed-steps must be deterministically injected every turn —
  that is what the Task context object provides.

### 4.5 Structured error/result contracts [INFERRED]

Both the MCP `outputSchema` direction and the general agent literature point to **typed result
objects** (success/error discriminated; size + handle fields) over free-form strings. A tool
that returns a string forces the model to parse it; a tool that returns `{ok, data_ref,
summary, error}` lets the next step reason structurally.

- → PRISM: The SPEC defines a result-contract convention. Command-tools today return
  `{root, args, success, exit_code, stdout, stderr}` (`command_tools.rs:2849`) — already
  structured. Phase 4c extends this with an optional `artifact_ref` for large/derived outputs
  so downstream steps reference by handle.

---

## 5. Prioritized recommendations (→ SPEC + implementation)

Ordered by leverage × readiness (evidence strength), highest first:

| # | Recommendation | Source basis | Maps to |
|---|---|---|---|
| R1 | **Add `example_queries` to capabilities and fold into `retrieval_text`; re-embed.** Highest-leverage retrieval refinement; infra already exists. | Tool2Vec (§1.4) | SPEC §retrieval-maturity; impl |
| R2 | **Standardize tool descriptions** (purpose/when-to-use/when-not/side-effects/returns) with a length floor; enforce via lint. | Anthropic (§3.1, §3.3) | SPEC §description-standard; lint |
| R3 | **Disambiguate overlapping tool clusters** (query_*, mesh_*, goal_*) — each description states "use THIS when…, use <sibling> when…". | Anthropic overlap lesson (§3.2) | SPEC; Batch-3 description pass |
| R4 | **Replace the generic `args: array<string>` escape** on Rust command-tools that have real distinct arguments, with typed JSON Schemas. The true "empty schema" gap. | §3.6 (inferred) + audit | SPEC §schema-convention; Batch 1 |
| R5 | **Add the Task/plan-position object to `run_turn`** (Plan-and-Execute for task-driven turns; ReAct unchanged for chat). | Plan-and-Act (§2.2) | Phase 4c; SPEC §context-contract |
| R6 | **Bridge the campaign engine to drive `run_turn` turns** — one durable spine, one tool-dispatch path; honor owner's "retire redundancy" decision. | Anthropic harnesses (§4.2) | Phase 4c |
| R7 | **Formalize artifact handles** (typed `artifact_ref` into provenance; `recall`/`fetch_artifact` to pull) as the cross-step data contract. | §4.3 references-not-blobs | SPEC §context-contract; Phase 4c |
| R8 | **Make the scratchpad a model-facing working-memory surface for task-driven turns** (the missing "write" op). | LangChain 4-ops (§4.1) | Phase 4c step 5 |
| R9 | **Add a CI lint** (Python + Rust) that fails a tool lacking a real schema or a usable description; plus a permissions-map drift test. | MCP spec quality (§3.4) | Phase 4a |
| R10 | **Spec (don't yet build) a rerank stage** and a provenance-fed retrieval flywheel as later phases. | §1.5 paradox | SPEC §retrieval-maturity (later) |

---

## Sources

- RAG-MCP — *Mitigating Prompt Bloat in LLM Tool Selection via Retrieval-Augmented Generation*, arXiv:2505.03275 (2025): https://arxiv.org/abs/2505.03275
- LiveMCPBench — *Can Agents Navigate an Ocean of MCP Tools?*, arXiv:2508.01780 (2025): https://arxiv.org/abs/2508.01780
- Less-is-More — *Optimizing Function Calling for LLM Execution on Edge Devices*, arXiv:2411.15399 (2024): https://arxiv.org/abs/2411.15399
- Tool2Vec — SqueezeAILab, usage-driven tool embeddings: https://github.com/SqueezeAILab/Tool2Vec
- Gorilla — *Large Language Model Connected with Massive APIs*, arXiv:2305.15334 (Berkeley): https://arxiv.org/abs/2305.15334
- Plan-and-Act — *Improving Planning of Agents for Long-Horizon Tasks*, arXiv:2503.09572 (ICML 2025): https://arxiv.org/abs/2503.09572
- Anthropic — *Writing effective tools for AI agents—using AI agents*: https://www.anthropic.com/engineering/writing-tools-for-agents
- Anthropic — *Building Effective Agents*: https://www.anthropic.com/engineering/building-effective-agents
- Anthropic — *Effective context engineering for AI agents*: https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents
- Anthropic — *Effective harnesses for long-running agents*: https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents
- MCP Tools specification (2025-06-18): https://modelcontextprotocol.io/specification/2025-06-18/server/tools
- SEP-1382 — *Documentation Best Practices for MCP Tools*: https://github.com/modelcontextprotocol/modelcontextprotocol/issues/1382
- *MCP Tool Descriptions Are Smelly*, arXiv:2602.14878: https://arxiv.org/abs/2602.14878
- LangChain — *Context Engineering for Agents* (write/select/compress/isolate): https://www.langchain.com/blog/context-engineering-for-agents
- LangChain — *Planning Agents* (Plan-and-Execute): https://www.langchain.com/blog/planning-agents
- Red Hat — *Tool RAG: The Next Breakthrough in Scalable AI Agents*: https://next.redhat.com/2025/11/26/tool-rag-the-next-breakthrough-in-scalable-ai-agents/
- Red Hat — *A Practical Approach to Smart Tool Retrieval for Enterprise AI Agents* (Tool2Vec): https://next.redhat.com/2025/12/05/a-practical-approach-to-smart-tool-retrieval-for-enterprise-ai-agents/
- Writer Engineering — *When too many tools become too much context*: https://writer.com/engineering/rag-mcp/
