# Tool-Surface Audit — Per-Tool Inventory & Structural Gaps

> Status: **Phase 2 audit deliverable** for the PRISM tool-surface production-grade effort.
> All counts are **measured**, not estimated. Reproduce with the commands in §5.
> Compiled 2026-07-10.

---

## Executive summary (the honest picture)

The mission brief claims *"most input schemas are empty (`{"type":"object"}` with no
properties/descriptions/examples)"*. **The data does not support that claim as stated.** The
measured reality, split by where tools live:

| Surface | Total | Empty schema | Generic-schema | Rich (typed) |
|---|---|---|---|---|
| **Python** (`app/tools/*.py`) | 51 (core) + 16 (conditional) = 67 | **6** (all genuinely parameter-less) | 0 | **61** |
| **Rust** (`CommandToolSpec`) | 78 | **10** (mostly genuinely parameter-less) | **18** (`RootArgs` → `args: array<string>`) | **50** |
| **Combined runtime catalog** | ~145 | ~16 | **18** | ~111 |

The real "empty/schema-less" problem is **not** the Python side — it is the **18 Rust
`RootArgs` umbrella tools** that expose a generic `args: array<string>` and thereby give the
model **zero type information** about what arguments to pass. This is strictly worse than an
honest empty schema (one that declares "no parameters"), because it *invites* arguments while
*describing none*. Per the research digest (§3.6), this is the exact mis-parameterization
failure mode RAG-MCP measures.

**Where the leverage is (revised from the mission's assumption):**
1. The 18 `RootArgs` Rust tools (Phase 4b, Batch 1) — the genuine schema gap.
2. Tool **overlap** between the `RootArgs` umbrellas and their typed siblings (e.g. `billing`
   vs `billing_balance`/`billing_usage`/…) — a selection-accuracy problem, not a schema
   problem (research §3.2).
3. A **CI lint** that prevents regression (Phase 4a) — there is no schema/description-quality
   gate anywhere today.
4. The **agent loop shape** — chat-turn-shaped, no task object (§3 below).

The Python side is **mostly healthy** and needs only small touch-ups (Batch 2-3): 5 empty
schemas that should add `additionalProperties:false` where missing, and a handful of
descriptions that lack a when-to-use signal.

---

## 1. Python tool surface (67 definitions)

### 1.1 Core registry (loaded in a standard venv): 51 tools

Schema metrics measured by iterating `build_full_registry()` (command in §5).

| Metric | Count |
|---|---|
| Total core tools | 51 |
| Empty schema `{"type":"object","properties":{}}` | **5** |
| Rich schema (≥1 typed property) | 46 |
| Rich but missing `required` | 7 |
| Rich but missing `additionalProperties` | 18 |
| All properties have a `description` (of rich tools) | **46 / 46** ✅ |
| Description < 60 chars | 1 (`cancel_background_research`, 41 chars) |

**The 5 empty-schema Python tools** (all genuinely parameter-less read/list ops — the empty
schema is defensible, but should be normalized to `{type:object,properties:{},additionalProperties:false}`):

| Tool | Description first-line | Genuinely parameter-less? |
|---|---|---|
| `agent_capabilities` | Ask the MARC27 platform to describe itself | ✅ yes |
| `check_hpc_queue` | Inspect the HPC queue (SLURM/PBS/SGE)… | ✅ yes |
| `list_background_research` | List recent background research runs with status | ✅ yes |
| `list_models` | ML property-prediction models — trained composition + GNN… | ✅ yes |
| `show_scratchpad` | Print the agent's execution log for this chat session… | ✅ yes |

**Quality exemplars (these define the standard the SPEC codifies):**

| Tool | Props | Why it's good |
|---|---|---|
| `materials_search` | 14 | All props described; discriminated provider set; keyless OPTIMADE federation |
| `knowledge_write` | 12 | All props described; approval-gated; clear write-side semantics |
| `dataset` | 8 | Action-enum composability; all props described; `required` set |
| `file` | 6 | `action` enum over a dispatch table; all props described |
| `execute_bash` | 4 | ~10-line description covering background tasks, blocks, side effects |

**Minor gaps (Batch 2):**

| Tool | Issue |
|---|---|
| `materials_search`, `session_context`, `query_materials_project`, `structure_import`, `list_potentials`, `billing_balance`, `usage_status` | Rich schema but no `required` — at least one argument is effectively mandatory |
| 18 rich tools | No `additionalProperties` (the SPEC will require `additionalProperties:false` for closed shapes) |
| `cancel_background_research` | Description 41 chars — no when-to-use / returns signal |

### 1.2 Conditionally-registered tools (need optional deps): 16

Absent in a minimal venv but defined in Python; register when their dependency imports succeed.

| Group | Tools | Schema status |
|---|---|---|
| Memory (`app/tools/memory/tool.py`) | `search_artifacts`, `fetch_artifact`, `list_artifacts` | rich |
| MACE (`app/tools/mace.py`) | `mace_relax_structure`, `mace_md_equilibrate`, `mace_phonon_harmonic`, `mace_compute_elastic`, `mace_compute_dilute_solute`, `mace_estimate_cost`, `mace_get_job`, `mace_list_jobs`, `mace_cancel_job`, `mace_get_cached_structure` | rich |
| Spark (`app/tools/spark.py`) | `spark_submit_job`, `spark_status`, `spark_batch_transform` | rich (`spark_status` is empty — genuinely parameter-less) |

---

## 2. Rust command-tool surface (78 specs)

Source: `crates/agent/src/command_tools.rs` — the static `COMMAND_TOOLS` array + `schema_for_spec`.

### 2.1 Schema-class breakdown (measured)

| Schema class | Count | What it means |
|---|---|---|
| **Rich typed** (per-kind schema fn) | **50** | e.g. `query_local_schema`, `compute_submit_schema`, `mesh_publish_schema` |
| **`RootArgs`** (`args: array<string>`) | **18** | **The real gap — no type info on real arguments** |
| **`empty_schema()`** | **10** | Genuinely parameter-less in *most* cases |

### 2.2 The 18 `RootArgs` umbrella tools (Batch 1 target)

Each exposes only `{"args":{"type":"array","items":"string"}}` — the model must guess CLI
flags from the description. Many **overlap** with typed siblings, creating the selection
ambiguity the research flags as the #1 tool-use failure mode (§3.2).

| Umbrella (`RootArgs`) | Typed siblings that exist | Overlap risk |
|---|---|---|
| `query` | `query_local`, `query_platform`, `query_federated` | **high** — model must choose umbrella vs typed |
| `research` | `research_query` | **high** |
| `billing` | `billing_balance`, `billing_usage`, `billing_history`, `billing_prices` | **high** (desc already says "prefer siblings") |
| `marketplace` | `marketplace_find`, `marketplace_info`, `marketplace_install`, `marketplace_search` | **high** |
| `workflow` | `workflow_list`, `workflow_show`, `workflow_run` | **high** (desc shows `args=["list"]` etc.) |
| `mesh` | `mesh_discover`, `mesh_health`, `mesh_peers`, `mesh_subscriptions`, `mesh_publish`, `mesh_subscribe`, `mesh_unsubscribe` | **high** |
| `node` | `node_probe`, `node_status`, `node_logs` | medium |
| `deploy` | `deploy_list`, `deploy_status`, `deploy_health`, `deploy_create`, `deploy_stop` | medium |
| `discourse` | `discourse_list`, `discourse_create`, `discourse_show`, `discourse_run`, `discourse_status`, `discourse_turns` | medium |
| `models` | `models_list`, `models_search`, `models_info` | medium |
| `ingest` | `ingest_file`, `ingest_watch` | medium |
| `agent` | (none — top-level orchestrator) | low |
| `run` | `run_submit` | low |
| `publish` | `publish_artifact` | low |
| `job-status` | `job_status_lookup` | low |
| `status`, `tools`, `doctor` | (none — genuine single-purpose) | low |

**Interpretation:** the `RootArgs` umbrellas are a *deliberate escape hatch* — a way to run
any `prism <root> ...` CLI verb the typed tools don't cover. That is a reasonable design, but
it (a) defeats the type system for the model and (b) creates overlap noise. The SPEC's
answer (Batch 1) is **not to delete the umbrellas** (they are the v1 safety net) but to:
- give each umbrella a typed `subcommand` enum (the CLI verbs it accepts) so the model picks
  a real subcommand, and
- have each umbrella description explicitly point to its typed siblings ("prefer
  `billing_balance` over `billing args=["balance"]`").

### 2.3 The 10 `empty_schema()` tools

| Tool | Genuinely parameter-less? |
|---|---|
| `workflow_list` | ✅ yes |
| `discourse_list` | ✅ yes |
| `compute_gpus`, `compute_providers` | ✅ yes (list available resources) |
| `goal_list` | ✅ yes |
| `billing_balance`, `billing_usage`, `billing_history`, `billing_prices` | ✅ yes (read current state) |
| `node_probe`, `node_status` | ✅ yes (probe current node) |

These are fine as empty schemas; the SPEC normalizes them to the explicit
`{type:object,properties:{},additionalProperties:false}` form (which `empty_schema()` already
produces — `command_tools.rs:814-820`).

### 2.4 Description quality (Rust)

Spot-checked. The descriptions are **reasonable** (they state purpose and several state
"prefer the typed sibling"), but few contain a structured when-to-use / when-not / returns
block. The SPEC's description standard raises this floor.

---

## 3. Structural gap: the agent loop is chat-turn-shaped

This is the central finding for the research-agent mission. Verified in `agent_loop.rs`:

| Property | Today | Long-running-task need |
|---|---|---|
| Entry contract | `run_turn(user_message: &str)` (`:643`) | a `Task`/goal object that survives turns |
| Inner loop bound | `max_iterations = 20` (`types.rs:138`) | bounded per-step, unbounded across steps |
| Plan object in loop | **none** — only `traj_steps` within a turn (`:673`) | a plan + plan position the loop owns |
| Cross-turn trajectory | SESSION MEMORY block, read once at turn start (`load_session_memory` `:155`) | deterministic, writable by the loop |
| Loop-level resume | **none** — process death loses in-flight state | checkpoint + re-drive |
| Tool→tool data flow | in-context (history) + `recall` (provenance) for >30KB (`:59`) | typed artifact handles, task-scoped |
| Scratchpad | exists (`scratchpad.rs`) but **not model-facing** — audit/report log only | model-facing working memory for the task |

**What already exists and is reusable (do not rebuild):**
- Neural tool retrieval (RAG-MCP) — shipped, ON by default (`capability.rs`).
- Progressive-disclosure L1 menu — shipped (`capability_menu`).
- Large-result → handle spill — shipped (`process_large_result` + provenance + `recall`).
- Trajectory + session-memory injection — shipped (within-turn / read-only).
- A **durable task engine** — `crates/campaign` has checkpoint/resume/budget/approval
  (`CampaignState` `:161`, `resume()` `:413`, `run_iteration()` `:427`), **but** its iteration
  body is hardcoded propose/evaluate/rank and builds its **own** LLM client — it does **not**
  dispatch through `run_turn`'s tool catalog, hooks, permissions, or approval gate.

**The gap, precisely:** there is no single loop that is *both* LLM-tool-driven *and*
checkpoint/resume durable. The bridge architecture (Phase 4c) makes `run_turn` the executor
of a generalized campaign engine — one durable spine, one tool-dispatch path. This honors the
owner's 2026-07-04 "retire redundancy" decision (`CAPABILITY_REGISTRY_DESIGN §5a`).

---

## 4. Structural gap: three sources of truth for tool identity

Tool identity/permission can drift across three manually-maintained locations:

1. **Python** `Tool` definitions (`app/tools/*.py`) — own description/schema/source/approval.
2. **Rust** `CommandToolSpec` array (`command_tools.rs`) — own description/schema.
3. **Rust** `tool_permissions()` map (`permissions.rs:66-174`) — a **manually-maintained
   94-entry name→`PermissionMode` map** (test `all_known_tools_mapped` asserts exactly 94,
   `permissions.rs:418`).

Risk: a tool added in (1) or (2) but not added to (3) **silently defaults to
`WorkspaceWrite`** (`get_tool_permission`, `permissions.rs:179`). This is a quiet security
drift. **Mitigation (Phase 4a):** a test asserting every `COMMAND_TOOLS` name has an explicit
permission entry (fails loud on drift, not silent on default).

Additionally, nearly every Python tool hard-sets `source="builtin"` regardless of whether it
runs locally, calls `api.marc27.com`, or hits a partner facility. `source` is a trust/origin
axis (anti-spoofing, `tool_catalog.rs:76-94`), not an execution-location axis. This is
documented (not a defect to fix in this pass) but limits provenance richness.

---

## 5. Reproducing these numbers

```bash
# Python side (from repo root):
PRISM_DISABLE_MEMORY=1 python3 -c "
from app.plugins.bootstrap import build_full_registry
r,_,_ = build_full_registry(enable_mcp=False, enable_plugins=False)
tools=list(r.list_tools())
empty=[t.name for t in tools if t.input_schema.get('type')=='object' and not t.input_schema.get('properties')]
print(f'total={len(tools)} empty={len(empty)} names={empty}')
"

# Rust side:
grep -c 'CommandToolSpec {' crates/agent/src/command_tools.rs   # total specs
grep -cE 'kind:\s*CommandToolKind::RootArgs' crates/agent/src/command_tools.rs  # generic-args count
```

The Phase-4a CI lint will encode these checks so the counts can't silently regress.

---

## 6. Top structural gaps (→ SPEC + implementation)

Ordered by impact on the research-agent mission:

1. **`run_turn` has no Task object / no resume / no cross-turn plan.** (§3) — the blocker for
   task-driven research. → Phase 4c (bridge).
2. **18 `RootArgs` Rust tools carry zero type info.** (§2.2) — the real schema gap. → Phase
   4b Batch 1.
3. **No schema/description-quality CI gate.** (§4 + research §3.4) — quality can regress
   silently. → Phase 4a lint + drift test.
4. **Tool overlap (umbrellas vs typed siblings).** (§2.2) — selection ambiguity, the #1
   tool-use failure per Anthropic. → SPEC description standard; Batch 3 disambiguation.
5. **Permission-map drift risk.** (§4) — silent default-to-WorkspaceWrite. → Phase 4a drift
   test.
6. **`retrieval_text` embeds only `{name}:{description}`, no example queries.** (research
   §1.4) — highest-leverage retrieval refinement. → SPEC §retrieval-maturity; impl.
7. **Scratchpad not model-facing; SESSION MEMORY read-only.** (§3) — no "write" op for
   long-task working memory. → Phase 4c step 5.
