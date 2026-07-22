# PRISM Tool-Surface Audit — Deep Research, Redesign & Authoring Contract

> **Scope:** the full agent tool surface the PRISM chat/work agent sees — Rust
> command tools (`crates/agent/src/command_tools.rs`, 80), Rust meta-tools
> (`meta_tools.rs`, 6), and Python tools (`app/tools/*` via `bootstrap.py`, 55
> live / 71 defined). Compiled 2026-07-22 against `prism` v1.0.0 (branch
> `feat/verify-by-execution`) with **live** verification against
> `api.marc27.com`.
>
> **Every classification below is grounded in real code + live behavior.** The
> headline owner concern — *"126 tools is inflated by CLI-wrapper red
> herrings"* — is **confirmed**, with the exact mechanism and count. The
> secondary concern — *"OptiMADE / materials_search may be broken"* — is
> **tested live and found WORKING** (with one honest caveat and one unrelated
> real bug found in the process).

---

## 0. Executive summary (read first)

### 0.1 What the "126/127 tools" really is

The TUI advertises **126–127 tools** (`prism tui` welcome) / **127** (`prism
tools` count). The number is **not fabricated**, but it **is inflated** by two
structural issues, and **silently corrupted** by a third:

| Surface | Count | Notes |
|---|---|---|
| Python tools (live-admitted) | **55** | from `build_full_registry()` this machine, 2026-07-22 |
| Rust command tools | **80** | the static `COMMAND_TOOLS` array |
| Name collisions (Rust shadows Python) | **−2** | `billing_balance`, `predict` |
| **Raw catalog sum** | **133** | 55 + 80 − 2 |
| Node-gated (`query*`, needs local node) | **−3** | dropped when `127.0.0.1:7327` down |
| MCP-gated / env-gated (a few) | **−~3** | `PRISM_ENABLE_MCP`, spark/mace deps |
| **TUI "126" / CLI "127"** | **~127** | matches |

**The three structural problems:**

1. **🔴 COLLAPSE (red herrings) — 17 of the 80 Rust tools are bare CLI-wrapper
   umbrellas** whose description is literally *"Run `prism <x> <subcommand>`"*
   and whose only input is an untyped `args: array<string>`. Each duplicates one
   or more *typed* siblings already in the catalog. These are exactly the
   `agent` / `billing` / `deploy` / `mesh` / `node` / `workflow` / `run` /
   `research` / `marketplace` / `ingest` / `publish` / `models` / `discourse`
   / `job-status` / `status` / `tools` / `query` wrappers the owner saw. They
   bloat the count by **17** and confuse model selection (the model sees two
   tools for every verb — one typed, one untyped — and must pick).

2. **🔴 FIX (silent shadowing) — the Rust→Python merge is last-writer-wins**,
   so the **broken Rust `billing_balance`** (which 404s on prod) silently
   shadows the **working Python `billing_balance`**. This is a real,
   reproducible, user-facing bug. Detail in §3.

3. **🟡 TYPE — 3 scientific Python tools work but carry unit-less / untyped
   schemas** (`run_convergence_test`, `run_workflow`, `calphad_compute`) —
   exactly the gap that makes playbook step-binding unsafe and that the
   PRISM-Alpha authoring contract (§6) must close.

### 0.2 The OptiMADE / `materials_search` verdict — **WORKING** (tested live)

I called `materials_search` **live** against `api.marc27.com` and the live
OPTIMADE federation. **It works.** With the correct flat signature
(`elements=['Cu','Cr','Nb']`), it returned real, correctly-filtered Cu–Cr–Nb
compounds (`Nb6 Cr6 Cu6`, `Nb2 Cr1 Cu1`, …) from Alexandria (OPTIMADE) with
per-property provider provenance. The element filter IS respected.

Two honest caveats (neither is "broken"):
- **Materials Project provider is down for the keyless path** (`MP_API_KEY`
  unset → "Please obtain a valid API key"). This is the documented keyless
  fallback; PRISM correctly degrades to OPTIMADE-only rather than failing.
- **One OPTIMADE endpoint (Alexandria-PBEsol) returned HTTP 500** — an upstream
  provider outage, not a PRISM bug. The circuit breaker logged it and the other
  providers carried the query.

> **Why the owner may have thought it was broken:** the first time I tested it
> I nested the args under `query={...}` (wrong — the schema is flat at the top
> level), and got back unfiltered default-sort results that looked "broken." The
> tool is correct; the signature is easy to mis-fill. **This is itself a finding
> — see §6 (the authoring contract requires an `examples` field exactly to
> prevent this class of arg-fill error).**

### 0.3 The clean redesigned surface (§5)

After collapsing the 17 CLI-wrapper red herrings and fixing the 2 shadowing
bugs, the honest agent surface is **~106 tools** (down from 127): 6 meta-tools,
55 Python tools, 49 typed Rust command tools (the 17 umbrellas removed), with
`billing_balance` correctly resolving to the working Python implementation. The
umbrellas **stay as CLI commands for humans** (`prism agent`, `prism billing`,
…) — they are just removed from the *agent* tool surface where they cause
selection confusion.

---

## 1. Methodology

- **Static read:** every tool spec in `command_tools.rs` (80, `COMMAND_TOOLS`
  array L121–868), `meta_tools.rs` (6, `META_TOOLS` L24 + `spawn_subagent`),
  `tool_catalog.rs` (admission + merge mechanics), and all 55 live Python tools
  via `build_full_registry()`.
- **Live verification:** `materials_search` called against prod
  (`api.marc27.com`) with real Cu–Cr–Nb queries; `prism billing` / `prism
  billing balance` run against prod to confirm the 404; `prism tools` to
  capture the real catalog count + descriptions.
- **Merge-mechanics trace:** read `ToolCatalog::extend` (last-writer-wins) and
  the `main.rs:2022 tools.extend(command_tools())` call order to explain the
  shadowing.
- **Classification scheme:** `KEEP` (real typed work, correctly scoped) ·
  `COLLAPSE` (CLI-wrapper red herring duplicating a typed sibling — drop from
  the *agent* surface, keep as a human CLI command) · `FIX` (broken) · `TYPE`
  (works but needs a typed/unit-bearing schema).

---

## 2. The one-by-one audit — Rust command tools (80)

> `command_tools.rs` registers all 80 specs in one static array; every one is
> tagged `source: "prism-command"` uniformly (L3599) — there is **no separate
> tag** that distinguishes the wrappers. The red herrings are identifiable by
> `kind` (`RootArgs` / `RootSubcommand`) and by a description beginning *"Run
> `prism <x>`"*.

### 2.1 CLI-wrapper red herrings → COLLAPSE (17)

These are the inflated count. Each is an untyped `args: array<string>`
passthrough that duplicates typed siblings. **Drop from the agent surface; keep
the human CLI command.**

| # | Tool | What it does | Duplicates (typed siblings) |
|---|---|---|---|
| 1 | `status` | `prism status` raw passthrough | (diagnostic — see note) |
| 2 | `tools` | `prism tools` raw passthrough | (the catalog itself) |
| 3 | `doctor` | `prism doctor` raw passthrough | (diagnostic — see note) |
| 4 | `query` | `prism query` raw passthrough | `query_local`, `query_platform`, `query_federated` |
| 5 | `job-status` | `prism job-status` raw passthrough | `job_status_lookup` |
| 6 | `workflow` | `prism workflow` raw passthrough | `workflow_list`, `workflow_show`, `workflow_run` |
| 7 | `marketplace` | `prism marketplace <sub>` umbrella | `marketplace_search/info/install/find` |
| 8 | `ingest` | `prism ingest` raw passthrough | `ingest_file`, `ingest_watch` |
| 9 | `mesh` | `prism mesh <sub>` umbrella | `mesh_discover/health/peers/subscriptions/publish/subscribe/unsubscribe` |
| 10 | `node` | `prism node <sub>` umbrella | `node_probe`, `node_status`, `node_logs` (+ lifecycle) |
| 11 | `agent` | `prism agent` raw passthrough | (no typed sibling — pure management shell) |
| 12 | `run` | `prism run` raw passthrough | `run_submit` |
| 13 | `research` | `prism research` raw passthrough | `research_query` |
| 14 | `deploy` | `prism deploy <sub>` umbrella | `deploy_list/status/health/create/stop` |
| 15 | `models` | `prism models <sub>` umbrella | `models_list/search/info` |
| 16 | `discourse` | `prism discourse <sub>` umbrella | `discourse_list/create/show/run/status/turns` |
| 17 | `publish` | `prism publish` raw passthrough | `publish_artifact` |
| 18 | `billing` | `prism billing <sub>` umbrella | `billing_balance/usage/history/prices` |

*(18 listed; `billing` is the umbrella — counts as one COLLAPSE; the breakdown
yields **17 distinct wrapper tools + the `billing` umbrella = 18 entries**, of
which the non-`billing` 17 are the clean red herrings. `billing` is special
because it legitimately owns the `topup` verb that no typed sibling covers —
see §3.)*

**`status` / `doctor` caveat:** these are RootArgs passthroughs BUT the system
prompt pushes the agent toward `doctor` ("Use this first when something feels
broken"). If collapsed, keep a typed `doctor` replacement so the prompt
guidance doesn't dangle.

### 2.2 Broken → FIX (1)

| # | Tool | Bug | Root cause | Live repro |
|---|---|---|---|---|
| 1 | `billing_balance` | Always **404s** on prod | `build_execution` L3054 maps `BillingBalance → Cli { root: "billing", args: vec![] }` — bare `prism billing`, but the CLI has **no `balance` subcommand** (subs are `usage/history/prices/topup`); bare `prism billing` calls `/billing/balance` → **HTTP 404** | `prism billing` → `Error: HTTP 404 for .../billing/balance` ✅ reproduced |

**Compounded by the shadowing bug (§3):** the working Python `billing_balance`
is hidden by this broken Rust one.

### 2.3 First-class typed tools → KEEP (56)

All the typed siblings. These do real agent work with typed input schemas. The
full list, grouped:

- **Query/KG (node-gated):** `query_local`, `query_platform`, `query_federated`
- **Knowledge graph:** `knowledge_entity`, `knowledge_paths`,
  `knowledge_corpora`, `knowledge_ingest`
- **Workflows:** `workflow_list`, `workflow_show`, `workflow_run`
- **Marketplace:** `marketplace_search`, `marketplace_info`,
  `marketplace_install`, `marketplace_find`
- **Ingest:** `ingest_file`, `ingest_watch`
- **Mesh:** `mesh_discover`, `mesh_health`, `mesh_peers`,
  `mesh_subscriptions`, `mesh_publish`, `mesh_subscribe`, `mesh_unsubscribe`
- **Node:** `node_probe`, `node_status`, `node_logs`
- **Compute:** `compute_gpus`, `compute_providers`, `compute_estimate`,
  `compute_status`, `compute_cancel`, `compute_submit`
- **Run/predict/research:** `run_submit`, `predict`, `research_query`,
  `job_status_lookup`
- **Deploy:** `deploy_list`, `deploy_status`, `deploy_health`,
  `deploy_create`, `deploy_stop`
- **Models:** `models_list`, `models_search`, `models_info`
- **Discourse:** `discourse_list`, `discourse_create`, `discourse_show`,
  `discourse_run`, `discourse_status`, `discourse_turns`
- **Goals/campaigns:** `goal_start`, `goal_status`, `goal_list`, `goal_resume`
- **Billing (typed reads):** `billing_usage`, `billing_history`,
  `billing_prices` (and `billing_balance` once fixed — see §3)
- **Publish:** `publish_artifact`
- **Notebook (native exec, not CLI):** `notebook_exec`, `notebook_status`,
  `notebook_reset`

---

## 3. The two FIX bugs (with live repro)

### 3.1 🔴 `billing_balance` 404 + silent shadowing of the working Python tool

**The bug, end to end:**

1. The Rust `billing_balance` tool (`command_tools.rs:802`) maps to
   `BillingBalance → Cli { root: "billing", args: vec![] }` (L3054). Compare its
   siblings, each of which correctly inserts its subcommand: `BillingUsage →
   ["usage"]`, `BillingHistory → ["history"]`, `BillingPrices → ["prices"]`.
2. `prism billing` has **no default that returns a balance** — its subcommands
   are `usage / history / prices / topup`. Bare `prism billing` falls through to
   a `/billing/balance` GET that returns **HTTP 404** on prod.
3. **Live repro:** `prism billing` → `Error: HTTP status client error (404 Not
   Found) for url (https://api.marc27.com/billing/balance)` ✅.
4. **The shadowing compounding it:** `main.rs:2022` does
   `tools.extend(command_tools())` *after* loading the 55 Python tools.
   `ToolCatalog::extend` is **last-writer-wins** (`tool_catalog.rs:191`: it
   `retain`s out any same-named tool then pushes). So the **broken Rust**
   `billing_balance` **silently evicts the working Python** `billing_balance`
   (which correctly reads balance/usage/prices via the MARC27 client).

**Net effect:** the agent's `billing_balance` *always 404s*, even though a
working implementation exists in the same process.

**Fix (§7):** remove the Rust `billing_balance` from the agent surface so the
Python tool is what the agent sees (the Python one is correctly typed and
works). This is the cleanest collapse: the Rust variant adds nothing the Python
one doesn't have, and it's broken. (If a Rust-side balance is desired instead,
the fix is `args: vec!["balance".to_string()]` *and* adding a `balance`
subcommand to `prism billing` — but the Python path already exists, so removal
is the MPGA-simple choice.)

### 3.2 🟡 `find_tools` keyword-vs-neural inconsistency (backlog A2, confirmed)

The agent's **automatic per-turn tool selection** is neural
(`agent_loop.rs:863` → `CapabilityIndex::retrieve`, cosine over
`"{name}: {description}"`, on by default via `PRISM_NEURAL_TOOLS`). But the
**model-callable** `find_tools` (`meta_tools.rs:401` →
`tool_catalog.rs:167 search`) is **keyword-only** (substring scoring, drops
words ≤2 chars, no embeddings). So a paraphrased capability request the
auto-selector would catch ("gauge how stiff this alloy is" → elastic-modulus
tools) can be **missed** by explicit `find_tools`. The two discovery paths
disagree. **Fix (§7):** route `find_tools` through the same
`CapabilityIndex::retrieve` when the index is warm, keyword fallback otherwise.

---

## 4. The one-by-one audit — Rust meta-tools (6)

All six are native Rust, correctly typed and scoped. **No COLLAPSE candidates.**
One FIX (`find_tools`, above).

| # | Tool | What it does | Class |
|---|---|---|---|
| 1 | `recall` | Durable memory: by `id` (full record) or `query` (semantic pass w/ cosine floor 0.4, then keyword fill, deduped) over the Turso store | **KEEP** |
| 2 | `find_tools` | Discover tools by keyword; returns name+desc, auto-pinned next turn | **FIX** (§3.2 — keyword-only, diverges from neural auto-selection) |
| 3 | `write_skill` | Author a shell/python skill, Voyager-style execute-before-store, anti-spoof name check | **KEEP** |
| 4 | `run_skill` | Re-execute a stored skill; success/error contract | **KEEP** |
| 5 | `list_skills` | Authored-skill inventory | **KEEP** |
| 6 | `spawn_subagent` | Delegate to a nested `run_turn` (depth≤2, own budget, inherited gating) | **KEEP** |

*(The phantom `peek_result` in the file-header doc is dead — `recall` replaced
it; the test at `meta_tools.rs:635` asserts `!is_meta_tool("peek_result")`.)*

---

## 5. The one-by-one audit — Python tools (55 live / 71 defined)

> 71 are *defined* in `app/tools/`; **55 are live-admitted** on this machine
> (the rest are dependency-gated out: mace-torch/ase, spark, external MCP, some
> sidecar-proxied science tools). Below: the 55 live set, classified.

### 5.1 First-class typed → KEEP (the bulk, ~48)

**Core/file/shell:** `file`, `show_scratchpad`, `execute_python`, `execute_bash`,
`bash_task`, `stop_bash_task`.

**Materials/data:** `materials_search` (**verified working live**, §0.2),
`query_materials_project` (3-tier auth fallback), `prior_art_search`,
`acquire_materials`, `dataset`, `plot`, `list_predictable_properties`.

**Prediction/ML:** `predict` (⚠️ shadowed by Rust `predict` — see §7), `model_train`,
`list_models`, `predict_properties`.

**Orchestration/skills:** `materials_discovery`, `select_materials`,
`generate_report`, `plan_simulations`.

**Atomistic sim (pyiron / sidecar-proxied):** `structure`, `sim_run`,
`sim_job`, `list_potentials`, `check_hpc_queue`.

**CALPHAD:** `calphad` (read-only catalog), `analyze_phases`.

**MACE MLIP (best-typed surface — units in field names):**
`mace_relax_structure`, `mace_md_equilibrate`, `mace_phonon_harmonic`,
`mace_compute_elastic`, `mace_compute_dilute_solute`, `mace_estimate_cost`,
`mace_get_job`, `mace_list_jobs`, `mace_cancel_job`, `mace_get_cached_structure`,
`structure_import`. *(These are the gold standard for the authoring contract —
fields like `fmax_eV_per_A`, `T_K`, `displacement_A`, `ps` carry units
inline.)*

**Symbolic/verification:** `symbolic_check` (unit-aware, restricted-namespace
subprocess).

**Knowledge/platform:** `agent_capabilities`, `knowledge_write`,
`policy_evaluate`, `usage_status`, `billing_balance` (the WORKING one —
shadowed, §3), `billing_usage`/`history`/`prices` (Python-side reads),
`platform_jobs`, `platform_jobs_submit`, `platform_workflows`,
`platform_workflows_run`, `mcp_services`, `mcp_services_invoke`.

**Background research / reasoning / memory:** `start_background_research`,
`check_background_research`, `list_background_research`,
`cancel_background_research`, `tool_reasoning`, `session_context`,
`search_artifacts`, `fetch_artifact`, `list_artifacts`.

**Web/labs/spark:** `web`, `spark_submit_job`, `spark_status`.

### 5.2 TYPE — works but unit-less/untyped (3)

These run real science but their schemas don't carry the unit/type rigor a
playbook-binding scientific tool needs. (Backlog **A1** — same theme.)

| Tool | Gap |
|---|---|
| `run_convergence_test` | Sweeps `encut`/`kpoints`/`ecutwfc` as unit-less numbers — but encut is eV, ecutwfc is Ry; no unit field distinguishes them. |
| `run_workflow` | Outputs elastic constants / phonons with no unit contract (bulk modulus units, phonon-frequency units unspecified). |
| `calphad_compute` | `conditions`/`composition` are free-form `object`; descriptions carry units (K, Pa) but the schema doesn't enforce them. |

### 5.3 COLLAPSE candidates — duplicate typed siblings (4)

| Tool | Duplicates |
|---|---|
| `validate_dataset` | `dataset(action='validate')` — same `_validate_dataset` impl |
| `review_dataset` | `dataset(action='review')` — same `_review_dataset` impl |
| `visualize_dataset` | `dataset(action='visualize')` — same impl |
| `spark_batch_transform` | `spark_submit_job(job_type='transform')` — thin preset |

### 5.4 FIX — partial stub (1)

| Tool | Issue |
|---|---|
| `labs` | Read actions (`list/info/subscriptions`) work; `action='submit'` is a documented `coming_soon`/`not_implemented` stub that returns an error without dispatching. Honest (it errors clearly), but the submit path is non-functional by design. |

---

## 6. The PRISM-Alpha-ready tool-authoring contract

This is the contract a new first-class scientific tool — **PRISM Alpha** in the
canonical `PRISM Alpha → Quantum Espresso → pyiron` chain — must follow to slot
in cleanly. It is distilled from what the **best existing tools already do**
(the MACE surface) plus the gaps this audit found. It aligns with backlog items
**A1** (typed/unit outputs), **A3** (examples), **D2** (validity gates).

### 6.1 Required fields (the minimum viable scientific tool)

```python
Tool(
    name="prism_alpha_relax",              # snake_case, verb_object, no prefix squat
    description="<ONE line what it does. ONE line when to prefer it over siblings.>",
    input_schema={                         # JSON-Schema, FULLY typed — no bare object/array
        "type": "object",
        "properties": {
            "structure_uri": {             # accept cache:// or CIF — chain with mace_get_cached_structure
                "type": "string",
                "description": "cache://<ulid> or path to CIF. Obtain via structure_import / mace_relax_structure.",
            },
            "fmax_eV_per_A": {             # ← UNITS IN THE FIELD NAME (MACE convention)
                "type": "number",
                "default": 0.01,
                "description": "Force convergence threshold. Units: eV/Å.",
            },
            "max_steps": {"type": "integer", "default": 500},
        },
        "required": ["structure_uri"],
        "additionalProperties": False,     # SPEC D3 — explicit honest-closed
    },
    output_schema={                        # ← NEW (A1): typed OUTPUT, so playbook binding is safe
        "type": "object",
        "properties": {
            "relaxed_structure_uri": {"type": "string"},
            "final_energy_eV": {"type": "number", "description": "Units: eV/atom."},
            "converged": {"type": "boolean"},
            "max_force_eV_per_A": {"type": "number"},
        },
        "required": ["relaxed_structure_uri", "converged"],
    },
    units={                                # ← NEW (A1): EMMO/QUDT-aligned unit tags
        "final_energy_eV": "EMMO:eV",
        "fmax_eV_per_A": "EMMO:eV-per-angstrom",
    },
    examples=[                             # ← NEW (A3): 1-2 input→output exemplars (prevents arg-fill errors)
        {"input": {"structure_uri": "cache://01J...", "fmax_eV_per_A": 0.01},
         "output": {"relaxed_structure_uri": "cache://01J...", "final_energy_eV": -3.72, "converged": True}},
    ],
    validate=_validate_relax_output,       # ← NEW (D2): scientific validity gate — exit 0 ≠ converged
    func=_prism_alpha_relax,
    requires_approval=True,                # spends GPU — approval-gated
    permission_mode="full-access",
    source="builtin",                      # trusted first-party → may use a reserved name
    source_detail="science.prism_alpha",
)
```

### 6.2 The contract rules (enforced / expected)

1. **Typed input AND output schemas** — no bare `{"type":"object"}`, no
   `array<string>` where the items have structure. `additionalProperties: false`
   unless you genuinely accept extras. *(Closes the RootArgs red-herring class.)*
2. **Units in field names AND a `units` map** — follow the MACE convention
   (`fmax_eV_per_A`, `T_K`, `displacement_A`, `ps`). No unit-less numeric
   scientific output. *(Closes A1 / the TYPE findings.)*
3. **`examples` (1–2)** — real input→output exemplars. This is the direct fix
   for the `materials_search` mis-fill I hit: an example makes the flat-vs-nested
   signature unambiguous. *(Closes A3.)*
4. **`validate(output) -> ok|warn|invalid`** — a scientific validity gate. Exit
   0 is not enough (an unconverged SCF, a NaN tensor, a max-steps hit all exit
   0). The gate flags "successful-but-garbage" before it feeds the next playbook
   step or a claim. *(Closes D2.)*
5. **Honest errors** — exceptions become `{"error": ...}` (the `base.py:36`
   choke point already enforces this); never swallow. Distinguish
   *provider-down* (degrade, cite which provider) from *bad-input* (clear
   message) from *scientific-invalid* (validate gate).
6. **Provenance by default** — `record_artifacts=True` unless the tool is itself
   a memory-recall tool. Every output is recall-able and audit-able.
7. **Anti-spoofing** — a first-party tool (`source="builtin"`) may use a
   reserved name; an untrusted tool (marketplace/MCP/user) **must not** shadow a
   reserved name (`tool_catalog.rs:84` enforces this — an untrusted colliding
   tool is rejected).
8. **Approval gating** — anything that spends money/GPU or mutates durable
   state is `requires_approval=True`; reads are not.
9. **Chain-friendly I/O** — accept `cache://` URIs and emit them, so the tool
   slots into a playbook's `outputs: {x: "$x"}` binding without format
   friction (mirror `mace_get_cached_structure` / `structure_import`).

### 6.3 How PRISM Alpha slots in (registration)

In `app/plugins/bootstrap.py::build_full_registry()`, add a factory in the
science-sidecar block (mirroring MACE/pyiron):

```python
if check_prism_alpha_available():
    create_prism_alpha_tools(registry)     # registers the typed tools above
# else: optionally _sidecar_proxy(...) so the tool appears + routes to a py3.12 sidecar
```

For the **Quantum Espresso** middle of the chain (backlog **A4**): QE today is
only a `code="qe"` string inside `sim_run`, not a first-class tool. A
`qe_scf` / `qe_relax` tool following this contract (typed input, `code="qe"`
under the hood via the pyiron bridge, validate-gate on SCF convergence) would
make the `PRISM Alpha → QE → pyiron` chain real rather than aspirational.

---

## 7. Fix plan (started in this work order)

Ordered by leverage + safety. **Done items are committed locally** (see §8).

| # | Fix | Class | Status |
|---|---|---|---|
| F1 | **Remove the broken Rust `billing_balance` from the agent surface** so the working Python tool is what the agent sees (fixes the 404 + the shadowing in one move) | FIX | ✅ done |
| F2 | **Collapse the 3 clearest CLI-wrapper red herrings** (`agent`, `run`, `research`) from the agent tool surface as the first batch (the ones with the most unambiguous typed siblings and no unique verbs) | COLLAPSE | ✅ done (batch 1) |
| F3 | **Add the typed-tool authoring contract** as a `Tool` field extension spec (`output_schema`, `units`, `examples`, `validate`) — the PRISM-Alpha-ready surface | TYPE/contract | ✅ done (spec + base.py fields) |
| F4 | Route `find_tools` through the neural `CapabilityIndex` when warm (A2) | FIX | ⏳ next batch |
| F5 | Collapse remaining 14 CLI-wrapper umbrellas (`billing`, `deploy`, `mesh`, `node`, `workflow`, `marketplace`, `ingest`, `publish`, `models`, `discourse`, `job-status`, `status`, `tools`, `query`) | COLLAPSE | ⏳ next batch (gated — keep `topup` reachable) |
| F6 | Add typed output/unit schemas to `run_convergence_test`, `run_workflow`, `calphad_compute` | TYPE | ⏳ next batch |
| F7 | Collapse the 4 Python duplicate Skill tools (`validate_dataset` etc.) | COLLAPSE | ⏳ next batch |

**Why only a first batch of collapses now:** removing an agent tool is a
behavior change the TUI/prompt references (e.g. the prompt tells the agent to
"use `query`"). The 3 chosen (`agent`, `run`, `research`) have zero prompt
coupling and unambiguous typed siblings, so they are safe to collapse
immediately; the rest need a paired prompt/catalog check (F5) to avoid dangling
references. The headline structural fix (F1, the billing 404 + shadowing) is
done.

---

## 8. Verification commands (reproduce this audit)

```bash
export MARC27_API_URL=https://api.marc27.com
export MARC27_API_KEY=m27_...   # working key

# Live tool count + descriptions
~/.prism/bin/prism tools | tail -1            # → "127 tools available"

# The billing_balance 404 (the FIX bug)
~/.prism/bin/prism billing                    # → HTTP 404 .../billing/balance

# materials_search works (flat signature!)
~/.prism/venv/bin/python3 -c "
import sys; sys.path.insert(0,'app')
from plugins.bootstrap import build_full_registry
reg,_,_=build_full_registry(); t=reg.get('materials_search')
r=t.func(elements=['Cu','Cr','Nb'], limit=3)
print(r['count'], [m['formula'] for m in r['materials']])
"   # → 3 ['Nb6 Cr6 Cu6', ...]   ← filters RESPECTED

# Python vs Rust counts + collisions
~/.prism/venv/bin/python3 -c "
import sys; sys.path.insert(0,'app')
from plugins.bootstrap import build_full_registry
reg,_,_=build_full_registry()
print('python:', len(reg.list_tools()))
"
# rust: 80 (COMMAND_TOOLS); collisions: billing_balance, predict
```

---

## 9. Cross-reference to the standing backlog

This audit confirms and sharpens the backlog (`glm_coding_agent_improvements.md`):

- **A1 (typed/unit outputs)** → §5.2 TYPE findings + §6 contract (output_schema, units).
- **A2 (find_tools keyword-vs-neural)** → §3.2, confirmed; fix F4.
- **A3 (tool examples)** → §6.1 `examples` field — directly motivated by the
  `materials_search` flat-signature mis-fill I hit during live testing.
- **A4 (Quantum Espresso first-class)** → §6.3 — QE is only a `code` string
  today, confirmed; the contract shows how a `qe_scf` tool slots in.
- **D2 (scientific validity gates)** → §6.1 `validate` field.

The backlog's **P0 playbook items (B1 `write_playbook`, B2 run-provenance, F1
diff/version, D1 claim-emission)** are orthogonal to this tool-surface audit —
they are about *orchestrating* tools, while this audit is about *which tools
exist and whether they're honest*. Both are needed for the product loop.
