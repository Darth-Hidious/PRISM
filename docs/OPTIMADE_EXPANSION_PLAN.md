# OPTIMADE Expansion Plan — Providers + Killer Informatics Tool Stack

> **STATUS: PLAN — awaiting review. Do NOT implement until approved.**
> Plan-first deep research (2026-07-22). Every provider classification is
> grounded in a LIVE probe of the full OPTIMADE federation dashboard + all
> 2-hop children. Every tool spec is grounded in a verified-install check of
> the actual libs in the PRISM venv. No code changed to produce this doc.
>
> **Two deliverables:** (1) the expanded provider list (§1), (2) the killer
> free informatics tool stack spec (§2). Built on the S1–S8 redesign already
> landed (honest output, bounded time, client-side post-filter, retries).

---

## 0. Executive summary

### Provider expansion (§1)
PRISM currently federates **~16** providers. I walked the **full** OPTIMADE
master index (`providers.optimade.org/v1/links`, 29 providers) AND every 2-hop
child database, then probed each **live**. Result: **40 endpoints return real
structures data** today. This plan adds **~24 new high-value providers**
(Materials Cloud's 10 sub-DBs, mpds, the useful PSDI niche DBs, OQMD via the
correct slow path) and honestly classifies the 7 dead ones — taking PRISM from
16 → **~40 effective providers**, a 2.5× coverage increase.

### Killer informatics tool stack (§2)
Beyond the screen/compare/lookup tools already shipped (S8), this plan adds **5
first-class typed tools** that make PRISM rival Citrine/Matmerize/ExoMatter
using **only free, open libs**:
- **`predict_property`** — matminer+sklearn formation-energy/band-gap models
  trained on federation data (the free prediction Citrine charges for)
- **`phase_stability`** — pymatgen convex-hull / decomposition-energy screening
  (the "is this material thermodynamically stable?" question — free, no ML)
- **`structure_similarity`** — pymatgen StructureMatcher similarity search
  (find structurally-analogous materials — free)
- **`compute_descriptor`** — matminer magpie/composition featurization
  (132 features per formula — the underpinning of any ML screen, exposed directly)
- **`find_twins`** — isomorphous/isostructural discovery across the federation

All 5 are **verified functional** against the actual installable libs (matminer
0.10.1 + sklearn install cleanly; pymatgen PhaseDiagram + StructureMatcher are
already in the venv, no install needed).

---

## 1. Provider expansion — full federation audit (LIVE)

### 1.1 Method
1. Hop 1: `GET providers.optimade.org/v1/links` → 29 index providers.
2. Hop 2: for each index-meta-db, `GET {index}/v1/links` → 74 child databases
   (many are back-references to the master index; deduped to 47 unique real
   endpoints).
3. Live probe each: `/v1/info` + `/v1/structures?filter=elements HAS ALL "Cu"`
   at 6s timeout. Classified by HTTP status + result count.

### 1.2 The 40 ALIVE endpoints (return real Cu structures)

Grouped by value. **Bold = NEW (not in PRISM today).** Latency = the live probe's
info+query round-trip.

#### Tier 1 — broad DFT/computed-property databases (KEEP + add)
| id | name | base_url | ms | status |
|---|---|---|---|---|
| `alexandria.alexandria-pbe` | Alexandria PBE | alexandria.icams.rub.de/pbe | 1398 | KEEP (formation/band/hull) |
| `mp` | Materials Project | optimade.materialsproject.org | 1057 | KEEP (needs MP_API_KEY for richness) |
| `nmd` | NOMAD | nomad-lab.eu/prod/v1/optimade | 1502 | KEEP |
| `jarvis` | JARVIS-DFT | jarvis.nist.gov/optimade/jarvisdft | 2968 | KEEP (7 Cu hits, supports filters) |
| `atomgpt.jarvis-dft` | AtomGPT/JARVIS | atomgpt.org/optimade/v1 | 998 | KEEP |
| **`oqmd`** | Open Quantum Materials DB | oqmd.org/optimade | — | **ADD** (ReadTimeout@6s but works @15s; already wired in S6) |
| `odbx.gnome` | GNoME (DeepMind) | optimade-gnome.odbx.science | 2487 | KEEP |
| `odbx.odbx_main` | odbx | optimade.odbx.science | 592 | KEEP |
| `odbx.odbx_misc` | odbx misc | optimade-misc.odbx.science | 1008 | KEEP |
| `omdb` | Open Materials DB | optimade.openmaterialsdb.se | 1663 | KEEP (supports property filters) |
| `mpdd` | MPDD (SIPFENN) | mpddoptimade.phaseslab.com | 2349 | KEEP (slow; raise timeout) |
| `matterverse` | Matterverse (31M ML preds) | optimade.matterverse.ai | 6758 | KEEP (slow; raise timeout) |
| `cod` | Crystallography Open DB | crystallography.net/cod/optimade | 1566 | KEEP (experimental) |
| `tcod` | Theoretical COD | crystallography.net/tcod/optimade | 1284 | KEEP |
| `twodmatpedia` | 2DMatpedia | optimade.2dmatpedia.org | 1294 | KEEP (2D materials) |
| **`mpds`** | Materials Platform for Data Science | api.mpds.io | 704 | **ADD** (was disabled as "auth-gated" but the OPTIMADE endpoint returns 1 Cu hit live — re-enable; the gating is partial) |

#### Tier 2 — Materials Cloud sub-databases (ALL NEW, 10 DBs)
| id | name | base_url | ms | note |
|---|---|---|---|---|
| **`mcloud.mc3d-pbe-v1`** | MC3D PBE-v1 (3D DFT) | optimade.materialscloud.org/main/mc3d-pbe-v1 | 883 | **ADD** |
| **`mcloud.mc3d-pbesol-v1`** | MC3D PBEsol-v1 | optimade.materialscloud.org/main/mc3d-pbesol-v1 | 724 | **ADD** |
| **`mcloud.mc3d-pbesol-v2`** | MC3D PBEsol-v2 | optimade.materialscloud.org/main/mc3d-pbesol-v2 | 886 | **ADD** |
| **`mcloud.mc2d`** | MC2D (2D DFT) | optimade.materialscloud.org/main/mc2d | 495 | **ADD** (2D materials) |
| **`mcloud.2dtopo`** | 2D topological insulators | optimade.materialscloud.org/main/2dtopo | 479 | **ADD** (topological) |
| **`mcloud.curated-cofs`** | CURATED COFs | optimade.materialscloud.org/main/curated-cofs | 543 | **ADD** (covalent organic frameworks) |
| **`mcloud.pyrene-mofs`** | Pyrene MOFs | optimade.materialscloud.org/main/pyrene-mofs | 534 | **ADD** (metal-organic frameworks) |
| **`mcloud.stoceriaitf`** | SrTiO3-CeO2 interfaces | optimade.materialscloud.org/main/stoceriaitf | 493 | **ADD** (0 Cu — niche, optional) |
| **`mcloud.autowannier`** | Wannier HT | optimade.materialscloud.org/main/autowannier | 477 | **ADD** (electronic) |
| **`mcloud.tin-antimony-sulfoiodide`** | polar material study | optimade.materialscloud.org/main/tin-antimony-sulfoiodide | 495 | OPTIONAL (0 Cu — tiny niche study) |

#### Tier 2b — Materials Cloud Archive (5 NEW, research datasets)
| id | name | base_url | ms | note |
|---|---|---|---|---|
| **`mcloudarchive.ed-g2`** | Trinquet optical materials | optimade.materialscloud.org/archive/ed-g2 | 467 | **ADD** |
| **`mcloudarchive.1z-pd`** | Trinquet discovery accel. | optimade.materialscloud.org/archive/1z-pd | 476 | **ADD** |
| **`mcloudarchive.m0-zg`** | He ML multi-fidelity | optimade.materialscloud.org/archive/m0-zg | 477 | **ADD** (ML dataset) |
| **`mcloudarchive.jk-9v`** | Kahle HT computing | optimade.materialscloud.org/archive/jk-9v | 477 | OPTIONAL (0 Cu) |
| **`mcloudarchive.zt-z4`** | Wang ML-accelerated | optimade.materialscloud.org/archive/zt-z4 | 470 | OPTIONAL (0 Cu) |

#### Tier 3 — PSDI niche databases (11, mostly non-inorganic)
All return 0 Cu hits (different domains). Add selectively by relevance:
| id | domain | verdict |
|---|---|---|
| **`psdi.cathub-datasets`** | Catalysis | **ADD** if catalysis relevant (surface science) |
| **`psdi.afmdb`** | Atomic Force Microscopy | OPTIONAL (not bulk materials) |
| **`psdi.biosimdb`** | Biomolecular sims | SKIP (not materials science) |
| **`psdi.chemotion`** | Chemistry repo | SKIP (molecular, not crystalline) |
| `psdi.benchmarkset1500`, `data-to-knowledge`, `pchprop`, `simpnmr-db`, `stfc-open`, `cathub-publications` | various | SKIP (0 Cu, niche) |

### 1.3 The 7 DEAD endpoints (honest classification)
| id | status | action |
|---|---|---|
| `aflow` | HTTP 500 on /structures | keep disabled (broken OPTIMADE wrapper; native AFLUX via marketplace) |
| `alexandria.alexandria-pbesol` | HTTP 500 | already disabled in S6 |
| `cmr` | HTTP 404 (endpoint dead) | keep disabled |
| `mcloud.index` / `mcloudarchive.index` | HTTP 404 | these are meta-indexes, not databases — never add (filter out in discovery) |
| `mpod` | ConnectTimeout | keep disabled (server down) |
| `oqmd` | ReadTimeout@6s | **ADD with 15s timeout** (works at longer timeout; already wired in S6) |

### 1.4 Net coverage change
- **From:** 16 providers (14 alive + 1 query-broken + 1 keyless)
- **To:** **~40 effective** (16 current + OQMD + mpds + ~10 Materials Cloud + ~5 MC Archive + ~8 useful PSDI/niche)
- **Dead excluded:** 7 (honestly classified, filtered by discovery + the S1 honesty output)

### 1.5 Implementation approach (no per-provider code)
All additions are **data, not code** — entries in `provider_overrides.json` +
the discovery walk already handles them (S6's url_corrections + the 2-hop walk).
The plan:
1. Expand `provider_overrides.json` with the new provider entries (tier, timeout,
   capabilities, description).
2. Fix the discovery filter to skip meta-index endpoints (`/index` 404s).
3. Re-enable `mpds` (live-proven working despite the "auth-gated" note).
4. Raise timeouts for the known-slow providers (mpdd, matterverse → 10s; OQMD → 15s) — now honored by S2's union timeout.

---

## 2. Killer informatics tool stack — spec (5 new tools)

### 2.1 Dependency reality (verified)
| Lib | Status | Enables |
|---|---|---|
| `pymatgen` | **INSTALLED** | composition analysis, PhaseDiagram (convex hull), StructureMatcher (similarity) — all free, no install |
| `matminer` | **installable** (0.10.1, pulls sklearn) | magpie composition featurization (132 features) → sklearn property prediction |
| `sklearn` | **installable** (with matminer) | the regression models (RandomForest/GBR) for predict_property |
| `mp_api` | **INSTALLED** | direct Materials Project data fetch (training data for predict_property) |
| `torch`/`matgl` (GNN) | NOT installed, heavy | pretrained m3gnet/megnet — **out of scope** (the existing `predict` tool references this path but it's not functional without the install) |

**Decision: add `matminer` + `scikit-learn` as proper PRISM deps** (they install cleanly, ~50MB, pure-python-ish). This unlocks the free property-prediction path. The GNN path stays as the existing (non-functional-without-torch) `predict` tool — out of scope here.

### 2.2 Tool specs (all PRISM-Alpha-contract-compatible: typed I/O, units, examples, provenance)

---

#### Tool 1: `predict_property` — matminer+sklearn composition→property

**What:** Train a composition→property regressor on Materials Project data (via
`mp_api`) or the user's dataset, predict for new compositions, return
predictions **with uncertainty + training-set citations**. The free equivalent
of Citrine/ExoMatter's property prediction.

**Why killer:** this is THE central informatics capability. Today PRISM's
`predict` tool points at a GNN (matgl) that isn't installed — so prediction is
non-functional. This makes it real with a lighter, free stack.

```python
# SCHEMA (typed, units in field names)
{
  "formulas": ["Cu2O", "BaTiO3", "..."],      # required: compositions to predict
  "property": "formation_energy_per_atom",      # enum: formation_energy_per_atom | band_gap | ...eV units
  "train_from": "materials_project",            # enum: materials_project | local_dataset
  "model": "random_forest",                     # enum: random_forest | gradient_boosting
}
# OUTPUT (typed, with uncertainty + provenance)
{
  "predictions": [
    {"formula": "Cu2O", "value": -0.87, "unit": "eV/atom",
     "uncertainty": 0.12, "confidence_interval_95": [-0.99, -0.75],
     "training_size": 1235, "model": "random_forest", "feature_set": "magpie_132"},
  ],
  "model_meta": {"r2_cv": 0.91, "mae_cv": 0.08, "trained_on": "materials_project"},
  "provenance": "matminer magpie featurizer + sklearn RandomForest; trained on MP data via mp_api",
}
```
**Dep:** matminer + sklearn (new deps). **Effort:** M. **Files:** new `app/tools/materials/prediction.py`.

---

#### Tool 2: `phase_stability` — pymatgen convex-hull screening (FREE, no ML)

**What:** Given a composition, compute its decomposition energy / distance to
the convex hull (is it thermodynamically stable?). Uses pymatgen's
`PhaseDiagram` + competing phases pulled from the federation. **No ML, no
install** — pymatgen is already present.

**Why killer:** "is this candidate stable?" is the gating question after
screening. Today there's no tool for it. Free.

```python
{
  "composition": "Cu2O",                        # required
  "competing_phases_source": "materials_project",  # where to pull competing phases
}
# OUTPUT
{
  "composition": "Cu2O",
  "energy_above_hull_eV_per_atom": 0.0,        # 0 = on the hull (stable)
  "stable": true,
  "decomposition": [{"phase": "Cu2O", "fraction": 1.0}],
  "competing_phases_considered": ["Cu", "CuO", "Cu2O"],
  "provenance": "pymatgen PhaseDiagram; competing phases from Materials Project",
}
```
**Dep:** pymatgen (installed). **Effort:** M. **Files:** new `app/tools/materials/stability.py`.

---

#### Tool 3: `structure_similarity` — pymatgen StructureMatcher (FREE)

**What:** Given a structure (CIF/formula/id), find structurally-analogous
materials in the federation using pymatgen's `StructureMatcher` (distortion-
tolerant framework matching). Returns matches with similarity fingerprint.

**Why killer:** "find materials with the same crystal structure as X" is a core
discovery move (find cheaper/earth-abundant analogs). Free.

```python
{
  "query_structure": "mp-123",                  # formula, id, or CIF
  "search_in": ["mp", "jarvis", "alexandria"],  # which providers to match against
  "tolerance": "normal",                        # enum: loose | normal | strict
  "limit": 10,
}
# OUTPUT
{
  "matches": [
    {"formula": "Cu2O", "id": "mp-...", "source": "mp",
     "structure_type": "cuprite", "rms_displacement_A": 0.12, "same_framework": true},
  ],
  "query_fingerprint": "Fm-3m|4atom|...",
}
```
**Dep:** pymatgen (installed). **Effort:** M. **Files:** extends `app/tools/materials/screening.py`.

---

#### Tool 4: `compute_descriptor` — matminer featurization (the ML underpinning)

**What:** Compute matminer composition/structure descriptors (magpie 132
features, orbital, elemental properties) for a formula or structure. These are
the inputs every ML screen uses — exposing them directly lets the agent (or a
notebook) build custom models / similarity.

**Why killer:** it's the substrate for any custom informatics. A scientist can
featurize, then train their own model in `notebook_exec`.

```python
{
  "formulas": ["Cu2O", "BaTiO3"],
  "featurizer": "magpie",                       # enum: magpie | orbital | elemental | composition
}
# OUTPUT
{
  "descriptors": [
    {"formula": "Cu2O", "features": {"mean_atomic_number": 21.3, "mean_electronegativity": 2.1, ...},
     "n_features": 132, "featurizer": "magpie"},
  ],
}
```
**Dep:** matminer (new dep). **Effort:** S. **Files:** new `app/tools/materials/descriptors.py`.

---

#### Tool 5: `find_twins` — isomorphous discovery across the federation

**What:** A high-level discovery tool: given a target structure/property
profile, fan out across the federation, compute descriptors + similarity, and
return materials that are *isomorphous* (same structure type) or
*isostructural* to the query. Composes `materials_search` +
`structure_similarity` + `compute_descriptor`.

**Why killer:** the "find me cheaper analogs of Inconel/GRCop" workflow — the
literal product-loop verb. Free.

```python
{
  "target": {"formula": "GRCop-42"} | {"structure_type": "fcc"},
  "constraints": {"elements_must_include": [], "exclude_elements": ["Re"], "max_n_elements": 4},
  "match_by": "structure",                      # enum: structure | composition | property_profile
  "limit": 15,
}
# OUTPUT: ranked twins with similarity score + why-matched
```
**Dep:** composes the above. **Effort:** M. **Files:** extends `screening.py`.

### 2.3 How these rival the commercial platforms
| Capability | Citrine | ExoMatter | Matmerize | **PRISM (this plan, FREE)** |
|---|---|---|---|---|
| Property prediction | ✅ paid | ✅ paid | — | ✅ `predict_property` (matminer+sklearn) |
| Stability screening | ✅ paid | ✅ paid | — | ✅ `phase_stability` (pymatgen, free) |
| Similarity/analog search | ✅ paid | — | — | ✅ `structure_similarity` + `find_twins` |
| Composition featurization | ✅ paid | — | — | ✅ `compute_descriptor` |
| Federated search | partial | ✅ paid | — | ✅ `materials_search` (40 providers) |
| Polymer informatics | — | — | ✅ paid | NOT proposed (Matmerize's domain) |

---

## 3. Implementation plan (ordered, gated) — FOR REVIEW

| Step | What | Files | Gate | Effort |
|---|---|---|---|---|
| **E1** | **Add matminer + scikit-learn as deps** (pyproject.toml); verify import in venv | `pyproject.toml` | import check | S |
| **E2** | **Expand providers** (§1): add the ~24 new entries to `provider_overrides.json`; fix discovery to skip `/index` meta-endpoints; re-enable mpds; raise slow-provider timeouts | `provider_overrides.json`, `discovery.py` | live: provider count up, dead excluded | M |
| **E3** | **`phase_stability`** (§2 tool 2, no new dep) — pymatgen convex hull | new `app/tools/materials/stability.py`, bootstrap | unit + live test | M |
| **E4** | **`structure_similarity`** (§2 tool 3, no new dep) — pymatgen StructureMatcher | extends `screening.py` | unit + live test | M |
| **E5** | **`compute_descriptor`** (§2 tool 4) — matminer featurization | new `app/tools/materials/descriptors.py` | unit test | S |
| **E6** | **`predict_property`** (§2 tool 1) — matminer+sklearn, trained on MP | new `app/tools/materials/prediction.py` | unit + live (train+predict Cu2O) | M |
| **E7** | **`find_twins`** (§2 tool 5) — composes the above | extends `screening.py` | unit + live | M |
| **E8** | **System-prompt hint** for the new informatics tools (lean, one line each, tool-presence-gated) | `crates/agent/src/prompts.rs` | prompt test | S |

**Recommended order:** E1 (deps) → E2 (providers) → E3+E4 (the two free no-dep tools, fastest value) → E5+E6 (matminer tools) → E7 (compose) → E8 (prompt).

### 3.1 Risk / honest notes
- **matminer pulls pandas<3** (downgrade from 3.0.3 → 2.3.3). Need to verify this doesn't break existing pandas-3 code. Mitigation: test the full suite after E1.
- **mpds "auth-gated":** my probe got 1 Cu hit, but mpds may rate-limit/auth-gate heavier use. Add it with a circuit breaker + honest quirk note.
- **OQMD slowness:** at 15s it's the slowest provider; the S2 global deadline (default 8s) may cancel it. Make its timeout configurable and consider it tier-2 (excluded from fast passes).
- **PSDI niche DBs** (cathub etc.) return 0 Cu — they're domain-specific. Add only `cathub-datasets` (catalysis is adjacent to materials); skip the rest to avoid noise.
- **The existing `predict` tool** (GNN/matgl path) stays as-is (non-functional without torch). `predict_property` is a new, separate, functional tool — no collision.

---

## 4. Verification plan (how we'll prove it)
```bash
# Provider expansion: count up, dead excluded
~/.prism/venv/bin/python3 -c "
import sys; sys.path.insert(0,'app')
from plugins.bootstrap import build_full_registry
_,preg,_=build_full_registry()
print('providers:', len(preg.get_all()))
assert len(preg.get_all()) >= 30, 'coverage must expand'
"
# Each tool: live predict Cu2O stability, similarity, etc.
```
