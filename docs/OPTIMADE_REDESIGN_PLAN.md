# OPTIMADE / `materials_search` Redesign Plan — Deep Research & Proposal

> **STATUS: PLAN — awaiting review. Do NOT implement until approved.**
> This is plan-first deep research. All findings are grounded in live probes
> against `api.marc27.com` + the live OPTIMADE federation (2026-07-22) and a
> line-by-line code audit of `app/tools/search_engine/`. No code was changed
> to produce this document.
>
> **One-line verdict:** `materials_search` *runs* but is **dishonest and
> fragile**: it blocks the agent for the full fan-out duration, reports every
> failed/timed-out provider as `ok: true`, drops all warnings/errors, silently
> clips per-provider timeouts, has zero retries, and advertises property-range
> filters that most providers reject. 14 of 16 configured providers are alive,
> but 1 is query-broken and the coverage is gappy. This plan fixes all four
> asks: non-blocking, dead-provider resilience, comprehensive coverage, and a
> free first-class materials-informatics tool stack.

---

## 0. Executive summary — the four problems and the four fixes

| # | Owner's requirement | Empirical finding (live) | Proposed fix |
|---|---|---|---|
| 1 | **Non-blocking / async** with per-provider timeouts + concurrency | **Confirmed broken.** A fresh `asyncio.new_event_loop()` per call (`tools.py:200`) runs `loop.run_until_complete(engine.search())` — the **agent thread is fully blocked** for the whole fan-out. Measured: **5.03s wall-clock** for a Cu-Cr-Nb query (9 of 16 providers hit the 5s timeout, yet the call only returns after the slowest finishes). | Make the tool genuinely async at the tool-server boundary; add a hard **whole-fan-out deadline** + per-provider timeouts that compose by *union* not clipping; cancel slow tasks on early-completion. (§3) |
| 2 | **Audit all providers; drop/skip dead ones; honest partial results** | **Confirmed.** `alexandria-pbesol` returns HTTP 500 on every structures query (circuit breaker *did* trip it open after 4 failures — it works, but late). Worse: **the tool output marks every failed/timed-out provider `ok: true`** (`tools.py:224` `getattr(log,"ok",True)` — `ProviderQueryLog` has no `ok` field), drops all `status`/`error`/`warnings`, and records cumulative search time as per-provider latency. | Probe-and-classify every provider (§1); wire the breaker to exclude known-dead ones pre-fan-out; **fix the output schema** to surface real `ok`/`status`/`error` per provider + a top-level `warnings` array. (§2, §4) |
| 3 | **Comprehensive coverage** | **Confirmed gappy.** Property-range filters (`band_gap >= 0.5`) return **HTTP 400 on 12 of 15** providers — they only support element/formula filters. Multi-element `HAS ALL "Cu","Cr","Nb"` returns 0 from 8 of 15 (sparse indexing, not a bug, but the agent isn't told). 2-hop index providers (Materials Cloud, CMR/C2DB, PSDI) are reachable but not in the registry. | Capability-aware query planning: probe each provider's `/info` for supported filters, translate the query to the strongest filter each provider supports, post-filter the rest client-side, and surface coverage honestly. Add the missing 2-hop providers. (§5) |
| 4 | **SOTA free tool stack** (Citrine/MP/Matmerize-grade) | PRISM has a strong *search* surface but a thin *informatics* surface: property prediction (matminer/GNN), CALPHAD, MACE MLIP exist, but there's no first-class **property-range screening**, **structure↔property linkage**, or **pymatgen-powered deep lookup** as a typed tool the agent composes. | Propose 4 free first-class typed tools that close the gap to Citrine/ExoMatter/Matmerize without paid APIs. (§6) |

---

## 1. Dead-provider audit (LIVE probes, 2026-07-22)

### 1.1 The 16 configured providers — probe results

Each provider was probed live with: (a) `/v1/info`, (b) a real structures query
`elements HAS ALL "Cu"` at the **configured 5s timeout**, (c) a multi-element
`Cu,Cr,Nb` query, (d) a property-range `band_gap >= 0.5` query.

| Provider | id | info | element query | multi-element | property-range | **Verdict** |
|---|---|---|---|---|---|---|
| Alexandria (PBE) | `alexandria.alexandria-pbe` | ✅ 200 | ✅ OK | ✅ 3 | ❌ 400 | **KEEP — fast, rich** |
| Alexandria (PBEsol) | `alexandria.alexandria-pbesol` | ✅ 200 | ❌ **500** | ❌ **500** | ❌ 400 | **FIX/DROP — query-broken** (breaker already `open`, 4 failures) |
| Crystallography Open DB | `cod` | ✅ 200 | ✅ OK | ⚠️ 0 | ❌ 400 | **KEEP — experimental structures, no computed props** |
| Materials Project (OPTIMADE) | `mp` | ✅ 200 | ✅ OK (0 on Cu) | ⚠️ 0 | ❌ 400 | **KEEP — keyless path is thin; needs `MP_API_KEY` for richness** |
| MPDD | `mpdd` | ✅ 200 | ✅ OK | ⚠️ ReadTimeout@5s | ❌ 400 | **KEEP but slow — raise its timeout** |
| Matterverse | `matterverse` | ✅ 200 | ✅ OK | ⚠️ ReadTimeout@5s | ❌ 400 | **KEEP but slow — 31M ML predictions, worth the wait** |
| NOMAD | `nmd` | ✅ 200 | ✅ OK | ✅ 3 | ❌ 400 | **KEEP — fast, rich** |
| odbx (main) | `odbx.odbx_main` | ✅ 200 | ✅ OK (0 Cu) | ⚠️ 0 | ❌ 400 | **KEEP** |
| odbx (misc) | `odbx.odbx_misc` | ✅ 200 | ✅ OK | ⚠️ 0 | ❌ 400 | **KEEP** |
| odbx (GNOME) | `odbx.gnome` | ✅ 200 | ✅ OK | ✅ 3 | ❌ 400 | **KEEP — fast** |
| OMDB | `omdb` | ✅ 200 | ✅ OK | ⚠️ 0 | ✅ OK (0) | **KEEP — one of few supporting property filters** |
| JARVIS-DFT | `jarvis` | ✅ 200 | ✅ OK (7!) | ⚠️ 0 | ✅ OK (0) | **KEEP — NIST, supports filters** |
| TCOD | `tcod` | ✅ 200 | ✅ OK | ⚠️ 0 | ❌ 400 | **KEEP — theoretical structures** |
| 2DMatpedia | `twodmatpedia` | ✅ 200 | ✅ OK | ⚠️ 0 | ❌ 400 | **KEEP — 2D materials** |
| AtomGPT | `atomgpt` | ✅ 200 | ✅ OK | ✅ 3 | ✅ OK (3!) | **KEEP — best filter support** |
| Materials Project (native) | `mp_native` | ❌ 404 | — | — | — | **KEYLESS — needs `MP_API_KEY`; degrades gracefully, not "dead"** |

**Hard-disabled in `provider_overrides.json` (correctly excluded already):**
`aflow` (OPTIMADE endpoint broken), `mpds` (auth-gated), `cmr`, `mpod`.

**Summary: 14 alive, 1 query-broken (`alexandria-pbesol`), 1 keyless (`mp_native`).**
The owner's "MANY are dead" is, empirically, *one* query-broken provider plus
the *perceived* deadness caused by the `ok:true` lying bug (9 providers timed
out at 5s on the Cu-Cr-Nb query but were all reported as successful — so to a
user it *looks* like most providers returned nothing = "dead"). **Fixing the
honesty bug (§4) is what makes the real dead-provider count visible.**

### 1.2 Major OPTIMADE providers MISSING from PRISM's registry

Probed against the [OPTIMADE providers dashboard](https://www.optimade.org/providers-dashboard/):

| Provider | Status (live probe) | Note |
|---|---|---|
| **OQMD** (Open Quantum Materials Database) | ⚠️ ReadTimeout@8s | Major DFT DB; slow/unstable endpoint — worth adding with a long timeout + breaker |
| **Materials Cloud** (`mcloud`) | 404 on `/v1/info` | **2-hop index provider** — has ~10 child sub-databases; needs the discovery walk, not a direct hit |
| **CMR** (C2DB, etc.) | 404 on `/v1/info` | **2-hop** — disabled in PRISM but the children (C2DB 2D materials) are alive |
| **PSDI** | 403 | Auth/restriction — skip |
| **matcloud** | ConnectTimeout | Skip (unreachable from here) |

**Action:** add OQMD (with generous timeout), and run the 2-hop discovery for Materials Cloud / CMR children (C2DB is a high-value 2D-materials DB). PSDI/matcloud stay out.

---

## 2. The honesty bugs (the real reason it *looks* broken)

These are concrete, line-cited bugs in the current code. Fixing them is **prerequisite** to everything else — without honest output, the agent can't reason about partial results.

### 2.1 `ok` is ALWAYS `True` (the lying bug)
`tools.py:224`:
```python
"ok": getattr(log, "ok", True),   # ProviderQueryLog has NO `ok` attribute
```
`ProviderQueryLog` (`result.py:40-61`) has `status` (one of `success|timeout|http_error|parse_error|circuit_open|skipped`) but **no `ok` field**. So `getattr` falls back to `True` for *every* provider, including timeouts and errors. The agent is told dead providers succeeded.

**Fix:** `"ok": log.status == "success"`, and surface `status` + `error` in the output.

### 2.2 `status`, `error`, `http_status_code`, `result_count` all dropped
`tools.py:216-229` reduces the rich `ProviderQueryLog` to `{provider, endpoint, latency_ms, ok}`. The engine carefully records *why* each provider failed (`engine.py:128-141`) and the tool throws it away. **Fix:** include the full per-provider log.

### 2.3 `warnings` array entirely omitted
`engine.py:141` builds `result.warnings` (e.g. `"Provider 'x' failed: TimeoutError"`). `tools.py:216-229` never includes it in the returned dict. **Fix:** add `"warnings": result.warnings`.

### 2.4 Per-provider latency is wrong for failures
Exception-branch logs use the *search-wide* `start` (`engine.py:133-135`), recording cumulative time as that provider's latency. **Fix:** each provider task records its own monotonic start.

### 2.5 Per-provider timeout overrides silently clipped
`engine.py:190-195`:
```python
timeout = self._global_timeout   # 5.0 (tools.py:187 passes no override)
per_provider = ep.behavior.timeout_ms / 1000
timeout = min(timeout, per_provider)   # min(5.0, 15.0) = 5.0
```
OQMD's configured 15s (`provider_overrides.json:89`) becomes 5s. **Fix:** the global should be a *ceiling on total wall-clock*, not a clip on each provider. Per-provider timeout wins for the per-provider call; the global caps the whole fan-out. (See §3.)

---

## 3. Async / non-blocking redesign

### 3.1 Current blocking path (confirmed)
```
agent tool call (sync)
  → _materials_search (tools.py:189)
    → asyncio.new_event_loop() (tools.py:200)        # NEW LOOP PER CALL
      → loop.run_until_complete(engine.search())     # BLOCKS the agent thread
        → asyncio.gather(*tasks)                     # waits for ALL, incl. slow
```
The agent is frozen for `max(provider_latencies)` ≈ the configured timeout (5s) because the gather waits for the slowest task. With 9 of 16 providers timing out at 5s, **every call costs ~5s of frozen agent**, even when the first 7 providers returned in <1s.

### 3.2 Proposed non-blocking design

**Principle:** the agent should never wait longer than the *fastest sufficient* answer, and never longer than a hard deadline.

```
agent tool call
  → _materials_search (async-aware tool shim)
    → asyncio.create_task(engine.search()) with a HARD DEADLINE
      → per-provider tasks, each with its OWN timeout (union, not min)
      → early-termination CANCELS in-flight slow tasks (task.cancel())
      → whole-fan-out wrapped in asyncio.wait_for(deadline)
```

Concrete changes:

1. **Make the tool-server path natively async.** The Python tool layer (`app/tools/base.py`) currently calls `func(**kwargs)` synchronously. The redesign adds an optional `async def execute_async` path so `materials_search`'s coroutine runs *on the agent's loop* instead of blocking a fresh one. (If the tool server can't host a coroutine yet, a `run_in_executor` threadpool wrapper is the minimal fallback — still non-blocking to the agent loop, at the cost of a thread. **Recommendation: native async; threadpool as fallback.**)

2. **Hard whole-fan-out deadline (new).** Add `asyncio.wait_for(engine.search(query), timeout=GLOBAL_DEADLINE)` where `GLOBAL_DEADLINE` defaults to **8s** (configurable per-call via an optional `timeout_seconds` arg). This is the ceiling the owner asked for — the agent is *guaranteed* to get a result (partial or complete) within ~8s.

3. **Per-provider timeout by union, not clip (fix §2.5).** Each provider task uses `asyncio.wait_for(provider.search(), timeout=provider.timeout_ms/1000)` — the provider's *own* configured timeout wins. The global deadline (above) is separate and caps the total. A slow provider with a 15s config gets up to 15s *unless* the global deadline fires first.

4. **Cancel slow tasks on early termination (fix the misleading comment at `engine.py:89-90`).** When `collected["count"] >= early_target`, call `task.cancel()` on all not-yet-complete tasks (currently `early_event` only short-circuits tasks that haven't entered the semaphore — `engine.py:96-107` — and running tasks are never cancelled). This is the single biggest latency win: once we have enough results, stop waiting for the laggards.

5. **Half-open breaker concurrency lock (fix audit Q5).** Add an `asyncio.Lock` around the half-open transition so only one canonical probe runs when a circuit transitions open→half_open, not a thundering herd.

### 3.3 Retry policy (new — currently zero retries)

Add a **bounded retry** for transient failures only, with exponential backoff:
- Retry on: `429 Too Many Requests` (honor `Retry-After`), `503 Service Unavailable`, connection resets. **One** retry (not a chain) with backoff `min(0.5 * 2^n, 2.0)s`.
- **Never retry** on: `400` (bad filter — a real error, retrying wastes time), `404`, `500` (persistent server error — let the breaker handle it), timeouts (a timeout already cost the full budget; retrying doubles it).
- This directly fixes the live failure I saw in TASK 1: `prior_art_search` hit 429s on arXiv/Semantic Scholar with zero recovery. (Note: that's a different tool, but the same retry module belongs in `resilience/` and is shared.)

New file `app/tools/search_engine/resilience/retries.py` with a `retry_transient(coro)` helper. The `resilience/` package already *implies* retries exist (it only has `circuit_breaker.py` today) — this fills the implied gap.

---

## 4. Honest partial-results output schema (the contract)

**Current (lying) output** (`tools.py:216-229`):
```json
{"materials": [...], "count": 10,
 "providers_queried": [{"provider":"x","endpoint":"...","latency_ms":5022,"ok":true}],
 "query_hash": "..."}
```

**Proposed (honest) output:**
```json
{
  "materials": [...],
  "count": 10,
  "providers_queried": [
    {
      "provider": "alexandria.alexandria-pbe",
      "endpoint": "https://...",
      "status": "success",            // success|timeout|http_error|circuit_open|skipped
      "ok": true,                      // status == "success"
      "latency_ms": 977,               // THIS provider's real latency
      "result_count": 3,               // how many it contributed (pre-fusion)
      "http_status": 200,              // null if no HTTP (timeout/circuit)
      "error": null                    // human-readable failure reason, else null
    },
    {
      "provider": "alexandria.alexandria-pbesol",
      "status": "circuit_open", "ok": false, "latency_ms": 0,
      "result_count": 0, "http_status": null,
      "error": "circuit open (4 consecutive failures; last HTTP 500)"
    }
  ],
  "providers_summary": {                // NEW — one-glance health
    "succeeded": 7, "failed": 5, "skipped": 4, "circuit_open": 1
  },
  "warnings": [                         // NEW — engine-level warnings reach the agent
    "Provider 'mpdd' timed out after 5.0s",
    "Property-range filter 'band_gap' unsupported by 12/15 providers (post-filtered client-side where possible)"
  ],
  "coverage": {                         // NEW — what the query could/couldn't do
    "filter_strength": "element",       // element|formula|property — strongest filter applied
    "providers_supporting_property_filter": ["omdb","jarvis","atomgpt"],
    "client_side_post_filtered": true    // were results narrowed locally?
  },
  "query_hash": "..."
}
```

This makes partial results **honest and actionable**: the agent can say *"I got 10 results from 7 of 16 providers; 5 timed out and 1 is circuit-broken; property filters weren't widely supported so I post-filtered client-side."*

---

## 5. Coverage fix — capability-aware query planning

### 5.1 The coverage problem (confirmed live)
PRISM's `MaterialSearchQuery` advertises rich filters: `band_gap`, `formation_energy`, `bulk_modulus`, `debye_temperature`, `crystal_system`. But **12 of 15 providers return HTTP 400 for property-range filters** — only `omdb`, `jarvis`, `atomgpt` support them. Today the engine sends the same translated filter to every provider; the 12 that can't handle it fail the whole query for that provider.

### 5.2 Capability-aware translation
1. **Probe `/info` once per provider (cache in the registry, refresh weekly).** The OPTIMADE `/info` endpoint advertises supported query fields (`query_available_fields` / `query_supported_features`). Store this per provider.
2. **Translate per provider.** For a query with `band_gap >= 0.5`:
   - Providers that support `band_gap` → send the native filter.
   - Providers that don't → send the strongest supported filter (e.g. just `elements`), fetch a bounded set, and **post-filter client-side** on `band_gap` from the returned properties (many providers *return* band_gap in results even if they don't *filter* on it).
   - Providers that neither filter nor return the property → query without it, mark the gap in `coverage`.
3. **Honest degradation.** If no provider supports a property filter, the tool says so upfront (`coverage.filter_strength = "element"`) rather than silently failing or returning unfiltered results pretending to be filtered.

### 5.3 Multi-element `HAS ALL` honesty
`elements HAS ALL "Cu","Cr","Nb"` returning 0 from most providers is **correct** (most DBs don't index that exact ternary) — but the agent should be told it's a sparsity result, not a failure. The `providers_summary.succeeded` count + per-provider `result_count` (§4) makes this visible.

### 5.4 Add the missing 2-hop providers
Run the OPTIMADE 2-hop discovery for index providers (Materials Cloud, CMR/C2DB) so their child databases enter the registry. Add OQMD directly with a generous timeout (15s) + breaker. This broadens coverage from 14 effective providers toward ~25-30.

---

## 6. Free first-class materials-informatics tool stack (the §4 ask)

### 6.1 What the SOTA platforms actually offer (research)
- **[Citrine Informatics](https://citrine.io/)**: generative AI + materials data infrastructure; formulation optimization, property-prediction pipelines, solvent-blend discovery. *Enterprise SaaS, paid.*
- **[ExoMatter](https://www.exomatter.ai/)**: cloud AI platform for **inorganic** materials screening — identify/evaluate/simulate candidates in seconds; aggregates global inorganic DBs; AI enrichment + simulation. *Paid.*
- **[Matmerize](https://www.matmerize.com/)**: **polymer** informatics (PolymRize) — AI/ML for polymer & formulation design. *Paid, polymer-focused.*
- **[Materials Project](https://nextgen.materialsproject.org/)**: open computed properties — the free backbone everyone builds on.

**The pattern:** paid platforms add *AI screening + formulation + curated pipelines* on top of the same open data (MP/OQMD/OPTIMADE) PRISM already federates. PRISM's moat should be: **the best free federated search + the informatics primitives the paid platforms charge for, runnable locally.**

### 6.2 Proposed FREE first-class typed tools (no paid APIs)

These reuse libraries PRISM already depends on (pymatgen, matminer, ase, scikit-learn) and the existing `materials_search` federation. Each follows the `PRISM_TOOL_SURFACE_AUDIT.md` authoring contract (typed I/O, units, examples, validate).

| Tool | What it does | Closes the gap to | Library |
|---|---|---|---|
| **`screen_materials`** | Property-range screening across the federation: "find me 3-element alloys with band_gap ∈ [0.5, 3] eV, bulk_modulus > 150 GPa" — capability-aware (§5), post-filters client-side, returns a ranked candidate set with per-property provenance. | ExoMatter's screening; Citrine's query pipelines | `materials_search` + `pymatgen` |
| **`predict_property`** *(upgrade existing)* | Train a composition→property model (matminer descriptors + sklearn) on MP data + the user's dataset, predict for new compositions, return predictions **with uncertainty + training-set citations**. PRISM has `predict`/`model_train` already — this promotes them to first-class with typed outputs (units, uncertainty, provenance). | Citrine/ExoMatter property prediction | `matminer` + `sklearn` (already deps) |
| **`lookup_structure`** | Deep structure lookup: given a formula/composition, return the relaxed structure (CIF), space group, lattice params, and available computed properties (band structure, DOS, elastic tensor if present) from MP/JARVIS/OQMD in one typed call — with units. Closes the "I found a material, now give me its crystal structure + properties" step that today needs 3 calls. | Citrine structure-property linkage | `pymatgen` + `materials_search` |
| **`compare_materials`** | Side-by-side comparison of N candidate materials across a property set (the "find & compare compatible materials" verb from the product loop), returns a typed comparison table with provenance per cell + a convergence/compatibility score. | ExoMatter's candidate comparison | `pandas` + `materials_search` |

**Why these are free:** they compose open data (OPTIMADE/MP/JARVIS) with open libs (pymatgen, matminer) that are *already* PRISM dependencies. No paid API. They give PRISM the screening + prediction + comparison primitives that Citrine/ExoMatter charge for, runnable locally — exactly the "PRISM Alpha → Quantum Espresso → pyiron" chain's front end.

**Not proposed (kept as paid/marketplace):** generative formulation design (Citrine's differentiator), polymer informatics (Matmerize's domain), sustainability/lifecycle scoring (ExoMatter's angle) — these are marketplace/PRISM-Alpha territory, not free tools.

---

## 7. Implementation plan (ordered, gated) — FOR REVIEW

Each step is independently committable and testable. **Nothing executes until you approve.**

| Step | What | Files | Gate | Effort |
|---|---|---|---|---|
| **S1** | **Honesty fixes** (§2): real `ok`/`status`/`error`/`warnings`/`providers_summary` in the output schema; fix per-provider latency; the prerequisite that makes everything else debuggable | `tools.py`, `result.py`, `engine.py` | unit test: a mocked timeout → `ok:false, status:"timeout"` | S |
| **S2** | **Timeout composition fix** (§3.2.3, §2.5): per-provider timeout by union; separate global deadline; stop clipping OQMD | `engine.py`, `tools.py` (pass `global_timeout`) | unit test: a 15s-config provider isn't clipped to 5s | S |
| **S3** | **Cancel-on-early-termination** (§3.2.4): `task.cancel()` when enough results gathered | `engine.py` | unit test: once `early_target` hit, in-flight tasks are cancelled | S |
| **S4** | **Transient retry** (§3.3): `resilience/retries.py`; retry 429/503/reset once w/ backoff; never 400/404/500/timeout | new `resilience/retries.py`, `optimade.py` | unit test: 429→retry→success; 400→no retry | S–M |
| **S5** | **Non-blocking tool path** (§3.2.1–3.2.2): native async tool execution (or threadpool fallback) + hard 8s deadline | `base.py`, `tools.py`, tool-server | integration test: agent loop not frozen; deadline enforced | M |
| **S6** | **Dead-provider hygiene** (§1): drop/disable `alexandria-pbesol` (query-broken) in overrides; add OQMD (15s + breaker); add 2-hop discovery for mcloud/CMR children; half-open lock | `provider_overrides.json`, `discovery.py`, `circuit_breaker.py` | live probe: pbesol excluded; OQMD queried; no thundering herd | M |
| **S7** | **Capability-aware coverage** (§5): `/info`-based filter support cache; per-provider translation; client-side post-filter; `coverage` in output | `providers/optimade.py`, `translator.py`, `engine.py`, `query.py` | live test: `band_gap` query → partial provider filter + client post-filter, reported honestly | M–L |
| **S8** | **Free informatics tools** (§6): `screen_materials`, `compare_materials`, `lookup_structure`, `predict_property` upgrade — typed, units, examples, validate | new `app/tools/materials/`, `bootstrap.py` | unit + live tests per tool | M (can land after S1–S7) |

**Recommended first batch for build (after approval): S1 + S2 + S3.** These are all small, fix the lying/blocking behavior the owner felt, and are pure bug-fixes (no new surface). S4–S8 build on that honest foundation.

---

## 8. Verification plan (how we'll prove it's fixed)

```bash
# 1. Honest output: a query where some providers fail must show ok:false + warnings
~/.prism/venv/bin/python3 -c "
import sys; sys.path.insert(0,'app')
from plugins.bootstrap import build_full_registry
reg,_,_=build_full_registry()
r=reg.get('materials_search').func(elements=['Cu','Cr','Nb'], limit=10)
import json
pq=r['providers_queried']
assert any(not p['ok'] for p in pq), 'must show failures honestly'
assert 'warnings' in r, 'warnings must reach output'
assert 'providers_summary' in r
print('HONEST:', r['providers_summary'])
"

# 2. Non-blocking: wall-clock <= global deadline even when providers are slow
# (measure: time the call; assert < deadline + slack)

# 3. Dead-provider skip: alexandria-pbesol must be circuit_open/skipped, not 500
# 4. Coverage: a band_gap query reports filter_strength + which providers supported it
```

---

## 9. Cross-reference

- Builds on `docs/PRISM_TOOL_SURFACE_AUDIT.md` (TASK 0): `materials_search` was classified KEEP there because it *works*; this plan addresses *how* it works (honesty/blocking/coverage) — the deeper layer.
- Backlog alignment: `A1` (typed/unit outputs — the §4 schema), `A3` (examples — the §6 tools), `D2` (validity gates — applies to the §6 tools).
- The retry module (§3.3) also fixes the live `prior_art_search` 429-stall seen in TASK 1.
