# Tool-Surface Specification — Standard & Definition-of-Ready

> Status: **Phase 3 deliverable** for the PRISM tool-surface production-grade effort.
> This is the standard every PRISM tool must meet, backed by the
> [research digest](./TOOL_SURFACE_RESEARCH.md) and measured against the
> [audit](./TOOL_SURFACE_AUDIT.md). The [definition-of-ready](#6-definition-of-ready) is
> enforced mechanically by the Phase-4a CI lint.
> Compiled 2026-07-10.

---

## 0. Scope and intent

This SPEC governs **both sides** of the tool surface:

- the **Python** `Tool` definitions (`app/tools/*.py`, served by `app/tool_server.py`), and
- the **Rust** `CommandToolSpec` array (`crates/agent/src/command_tools.rs`).

A tool is any capability the LLM can select and invoke through `run_turn`'s tool-dispatch
path — Python tool-server tools, Rust command-tools, and the reserved meta-tools
(`recall`, `find_tools`, `write_skill`, `run_skill`, `list_skills`).

**North star:** every decision serves *long-running, task-driven research*. A tool that is
fine for a single chat turn but fails across a 50-step research task (because its result is
an inlined blob, or its description doesn't disambiguate it from a sibling) does **not** meet
this SPEC.

---

## 1. Schema convention

### 1.1 Every tool has a real JSON Schema

`input_schema` MUST be a JSON Schema object (`{"type":"object", ...}`). Three acceptable
shapes, in order of preference:

1. **Typed schema** (preferred). One `properties` entry per real argument, each property
   carrying a `type`, a `description`, and (where applicable) `enum` / `minimum` / `maximum`
   / `pattern` / `examples`. A non-empty `required` array listing mandatory arguments.
   `additionalProperties: false` for closed shapes.

2. **Honest empty schema** (for genuinely parameter-less tools). Exactly:
   ```json
   { "type": "object", "properties": {}, "additionalProperties": false }
   ```
   The description MUST state the tool takes no arguments and explain what it returns.

3. **Umbrella/subcommand schema** (only for the Rust `RootArgs` escape). A single
   `subcommand` property with an `enum` of the real CLI verbs that root accepts, plus an
   optional `args` for verb-specific tokens. The description MUST point to typed siblings
   where they exist ("prefer `billing_balance` over `billing` with `subcommand=balance`").
   The generic `{"args":{"type":"array","items":"string"}}` with **no** subcommand enum is
   **forbidden** going forward (this is the audit's §2.2 gap).

### 1.2 Per-property requirements

Every property in a typed schema MUST have:
- `type` (string / integer / number / boolean / array / object),
- `description` (what to pass, including format/units — e.g. *"Integer atom counts keyed by
  element symbol (Al, Fe, …)"*, not *"the composition"*).

and SHOULD have, where applicable:
- `enum` (for a closed set of values — the `action`-enum composability pattern),
- `minimum` / `maximum` / `pattern` (constraints the model can rely on),
- `examples` (one example value — doubles as arg-filling guidance and, when lifted into the
  capability's `example_queries`, as retrieval signal; see §4),
- `default` (for optional args with a sensible default).

### 1.3 Composability pattern (blessed)

When several actions share one object, use **one tool with an `action`/`mode` enum** rather
than N single-action tools. Exemplars in the current codebase: `file` (action dispatch table),
`dataset`, `mcp_services_invoke`, `execute_bash` (with `run_in_background`). New tools that
fit this shape SHOULD follow it; do not split a cohesive object into micro-tools, and do not
merge tools that have genuinely distinct purposes (that creates the overlap problem of §3).

### 1.4 Examples in schema

`examples` are encouraged (not hard-required) at both the property level and the
description level. They are the cheapest way to improve argument-filling accuracy and they
feed the retrieval signal (§4). The lint allows them; reviewers should favor them.

---

## 2. Description-writing standard

The description is a **prompt**, not documentation (research §3.1). Every tool description
MUST contain these four signals, in this order:

1. **Purpose** — one or two sentences: what the tool does, in terms of the task.
2. **When to use** — the trigger condition(s): for what kind of question or step. Where a
   sibling tool exists, state **when to use THIS one vs. the sibling** (research §3.2 —
   overlap is the #1 selection failure). Example:
   > *Use `billing_balance` for the current credit balance. Use `billing_usage` for spend
   > history. Do NOT use the `billing` umbrella for these — it exists only for verbs without
   > a typed tool.*
3. **Side effects / cost / latency** — what changes in the world; whether it spends money or
   compute; whether it is read-only; typical latency (e.g. "seconds" vs "minutes; detaches").
4. **Returns** — the shape of the result and whether it is inlined data or a **handle** into
   durable memory (see §5). State the size cliff (e.g. "≤30 KB inlined; larger results are
   spilled to durable memory — call `recall(query=…)` to retrieve").

### 2.1 Length and signal floor

- Minimum **60 characters** (the lint floor). A description shorter than this almost
  certainly lacks the when-to-use and returns signals.
- MUST contain at least one **when-to-use signal** — a word/phrase indicating the trigger
  (e.g. "use for", "when you need", "best for", "call this to"). The lint checks for a
  signal-phrase presence; reviewers judge quality.

### 2.2 Anti-patterns (lint rejects or reviewers flag)

- Descriptions that are just a name restatement (`"list_models"` → *"List models."*).
- Descriptions with no returns signal for tools that return data.
- Descriptions that don't disambiguate from an obvious sibling.
- The generic-args invitation (a description that says "pass args" with a schema that says
  `args: array<string>` and nothing else).

---

## 3. Overlap and disambiguation

Because overlap is the dominant selection-failure mode (research §3.2, LiveMCPBench §1.2),
tools that occupy the same namespace MUST disambiguate in their descriptions. The known
overlap clusters (from the audit):

- `query` / `query_local` / `query_platform` / `query_federated` / `research` / `research_query`
- `billing` / `billing_balance` / `billing_usage` / `billing_history` / `billing_prices`
- `marketplace` / `marketplace_find` / `marketplace_info` / `marketplace_install` / `marketplace_search`
- `workflow` / `workflow_list` / `workflow_show` / `workflow_run`
- `mesh` / `mesh_discover` / `mesh_health` / `mesh_peers` / `mesh_subscriptions` / `mesh_publish` / `mesh_subscribe` / `mesh_unsubscribe`
- `node` / `node_probe` / `node_status` / `node_logs`
- `deploy` / `deploy_list` / `deploy_status` / `deploy_health` / `deploy_create` / `deploy_stop`
- `discourse` / `discourse_*`

For each cluster, the typed sibling descriptions say *when to use them*; the umbrella
description says *it is the escape hatch for verbs without a typed tool* and points to the
siblings. (Implementation: Phase 4b Batch 3.)

---

## 4. Retrieval maturity (the capability index)

The capability index (`crates/agent/src/capability.rs`) retrieves tools by neural embedding.
Today `retrieval_text = "{name}: {description}"`. This SPEC defines the maturity ladder;
rungs are implemented incrementally.

### Rung 0 — description-only embedding (SHIPPED, ON by default)

`retrieval_text = "{name}: {description}"`, BGE-small-en-v1.5, cosine top-K = `MAX_TOOLS_PER_REQUEST` (15). This banks the RAG-MCP win (research §1.1).

### Rung 1 — example-query embedding (Tool2Vec) [NEXT]

Extend `Capability` with an optional `example_queries: Vec<String>` and fold them into the
retrieval text:

```
retrieval_text = "{name}: {description}\nqueries: q1; q2; q3"
```

Rationale: usage-driven embeddings beat description-only embeddings, especially when
descriptions use jargon that doesn't match how users phrase requests (research §1.4 — the
single highest-leverage refinement). Source of example queries: hand-authored at first (3-5
per tool for the overlap clusters and the science tools), later mined from provenance
(research §1.5 flywheel, a later phase).

**Definition:** every tool in an overlap cluster (§3) and every science/compute tool SHOULD
carry ≥3 example queries. The lint reports (does not fail on) tools missing them in Rung 1;
a later rung promotes this to a fail.

### Rung 2 — rerank (LATER, spec only)

A cross-encoder or LLM-rerank stage over the cosine top-K, to address the 99%-success
paradox (near-perfect recall ≠ correct selection). Not built in the first pass; the SPEC
records it as the next retrieval maturity step. `find_tools` stays as the manual
confirmation escape in the meantime.

### Rung 3 — provenance-fed flywheel (LATER, spec only)

Log `(query, capability_selected, succeeded)` from provenance and periodically adjust
retrieval/ranking (research §1.5, `CAPABILITY_REGISTRY_DESIGN §4.5`). Later phase.

---

## 5. Context-passing contract (for long-running tasks)

This is the contract for Phase 4c. It defines how state flows across the steps of a
research task — the thing the chat-turn-shaped `run_turn` does not yet have.

### 5.1 What every tool receives (task context)

When `run_turn` is driven by a task (`task: Some(&ResearchTaskContext)`), every iteration's
system prompt carries a deterministic **TASK CONTEXT** block:

```
TASK — {goal description}
Plan position: step {n} of {m}: {current step description}
Prior artifacts (reference, not inlined): prov:<id1> (summary…), prov:<id2> (summary…)
Working notes: {the task's running hypotheses/dead-ends/next steps}
```

This mirrors the existing TRAJECTORY / SESSION MEMORY blocks but is **task-scoped,
writable, and cross-turn**. Tools do not parse it; the model does. It is the "write" +
"select" ops from the context-engineering playbook (research §4.1).

### 5.2 How results flow back — handles, not blobs

- **Inlined (default):** results ≤ `MAX_TOOL_RESULT_CHARS` (30 KB) are placed in history as
  today. Command-tools already return structured JSON
  (`{root, args, success, exit_code, stdout, stderr}`, `command_tools.rs:2849`).
- **Handle (oversized / derived):** results > 30 KB are spilled to the provenance store and
  the model receives a typed pointer — formalized as:
  ```json
  { "ok": true, "artifact_ref": "prov:<id>", "summary": "<≤2KB distinguishing preview>",
    "bytes": 123456, "recall_query": "<keywords to pull the full content>" }
  ```
  The model pulls full content via `recall(query=…)` (shipped) or `fetch_artifact`
  (Python memory tool). This converts the turn-local `process_large_result` optimization
  (`agent_loop.rs:59`) into a durable, task-scoped pattern (research §4.3 — references, not
  blobs).

### 5.3 Node-fetched data is referenced, not inlined

Mesh/node query results (`query_federated`, etc.) that exceed the inline budget are spilled
to provenance and referenced by handle, exactly as above. This keeps a 50-step federated
research task from filling the context in 15 steps (research §4.3).

### 5.4 Long-running state persists and resumes

> **STATUS: NOT YET WIRED.** The behavior below describes the target design. Today only
> the types, pure translation functions, and unit tests exist — no code path drives
> `run_turn(Some(task))` or persists `ResearchTaskContext` to a checkpoint. See
> [`TOOL_SURFACE_AUDIT.md`](./TOOL_SURFACE_AUDIT.md).

The durable spine is the **campaign checkpoint** (`~/.prism/campaigns/{id}.json`), extended
for research goals. It holds: goal, plan (steps), plan position, artifact refs, budget /
iteration caps, approval-gate state. On resume, `run_turn` is re-driven from the
checkpointed plan position with the checkpointed artifact handles in the TASK CONTEXT block.
This reuses the existing `--detach` worker + poll-via-checkpoint contract (`crates/cli`,
`LONG_RESEARCH_PLAN §detach semantics`).

### 5.5 Result/error contract convention

Tools SHOULD return structured results (`{ok, data|error, ...}`) over free-form strings,
so downstream steps reason structurally. Command-tools already do. Python tools follow the
`Tool.execute` uniform contract (`{error: …}` on failure, `base.py:36-77`).

---

## 6. Definition-of-ready

A tool is **ready** (admissible to the catalog) iff ALL of:

| # | Criterion | Enforced by |
|---|---|---|
| D1 | Has a non-empty `name` and `description` (present strings). | Rust parse (`tool_catalog.rs:64,68`); Python `Tool` dataclass. |
| D2 | `description` ≥ 60 chars AND contains a when-to-use signal. | **Lint (Phase 4a).** |
| D3 | `input_schema` is a JSON object: typed (D3a), honest-empty (D3b), or umbrella-with-subcommand-enum (D3c). The bare `args:array<string>`-with-no-enum form is rejected. | **Lint (Phase 4a).** |
| D4 | If typed: every property has `type` + `description`; `required` lists mandatory args; `additionalProperties` is set (false for closed shapes). | **Lint (Phase 4a).** |
| D5 | If in an overlap cluster (§3): description disambiguates from siblings. | Reviewer + lint heuristics. |
| D6 | `permission_mode` is explicitly mapped in `tool_permissions()` (Rust command-tools) — no silent default-to-`WorkspaceWrite`. | **Drift test (Phase 4a).** |
| D7 | Output contract documented in the description (data shape + handle/blob + size cliff). | Reviewer; lint flags data-returning tools with no "return" signal. |
| D8 | Provenance-recorded (the existing `provenance` post-hook, `hooks.rs:292`, covers this for all tools). | Already enforced. |

Tools not meeting D1-D4 and D6 are **blocked by CI**. D5, D7 are reviewer-judged with lint
heuristics. D8 is already satisfied by the harness.

---

## 7. Migration policy

- **Do not break v1 / chat.** The chat path (`prism`, `task=None`) is preserved at every
  commit; a test asserts byte-for-byte parity with prior behavior.
- **Extend, don't fork.** Per the owner's 2026-07-04 "retire redundancy" decision, the tool
  surface is unified onto the existing `ToolCatalog` / `CapabilityIndex`; no parallel
  registries are introduced.
- **Lint before mass upgrade.** Phase 4a ships the lint first (it passes on the current
  catalog except for the documented `RootArgs`/empty gaps), then Phase 4b upgrades tools
  batch-by-batch. The lint must not retroactively fail the whole catalog on day one — it
  gates *new and changed* tools and carries an explicit allowlist for the pre-existing gaps
  that Batch 1 closes.
- **Small, reviewable commits.** Each batch: `cargo fmt && cargo clippy --workspace
  --all-targets -- -D warnings && cargo test --workspace` (Rust) and the lint + `pytest`
  on touched files (Python).

---

## 8. Relationship to prior design docs

- This SPEC implements the tool-quality and context-passing portions of
  `CAPABILITY_REGISTRY_DESIGN.md` (which is tool-*retrieval*-focused) and the research-loop
  portion of `LONG_RESEARCH_PLAN.md` (gap #4: generalize goal types). It does not supersede
  those docs; it operationalizes them with measurable, lint-enforced criteria.
- The bridge architecture (Phase 4c) is consistent with `CAPABILITY_REGISTRY_DESIGN §5a`
  ("full collapse, retire redundancy") — one durable spine (campaign), one tool-dispatch
  path (`run_turn`).
