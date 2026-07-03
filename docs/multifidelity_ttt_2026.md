# Multi-Fidelity Test-Time Adaptation for Engineering World Models

**Status: IDEA — not the strategic direction.**

The strategic direction for PRISM is the **[PRISM Fabric](./prism_fabric_2026.md)** —
Tailscale-style private compute network with heterogeneous-device joint
inference. THAT is the roadmap.

This doc captures a *separate idea* discussed on 2026-05-08: what a
project-local test-time adaptation loop could look like if PRISM ever
wanted to layer one on top of the Fabric. It is interesting and
plausibly valuable for engineering-design workflows, but it is **not
the build target**. Treat as a notebook entry, not a sprint plan.

If you're reading this looking for "what is PRISM building toward,"
read [prism_fabric_2026.md](./prism_fabric_2026.md) instead.

> **One-line thesis:** A stateful world model with project-local
> test-time adaptation, driven by multi-fidelity simulation triage.
>
> The loop: **generate → predict → triage → simulate → adapt locally
> → optimize → validate.**

---

## What this is, in one paragraph

When the user says *"design a BWB drone for this mission"*, PRISM doesn't
just run a frozen pretrained world model once. It generates 500 candidate
designs, runs cheap physics on all of them, mid-fidelity sims on the top
50, high-fidelity CFD/FEM on the top 5, and **injects those new
simulation labels into a local adaptation layer** so the model becomes
project-specific. The base model stays frozen. The local adapter learns
the quirks of *this* design family for *this* user. After the project,
the adapter is either preserved as project memory or its data is
promoted (after validation) into the global training set.

This is similar in spirit to test-time training (the [original Sun et al.
2020](https://yueatsprograms.github.io/ttt/home.html) work updates a
model on a self-supervised task at inference time; the [TTT-layers
2024 work](https://arxiv.org/abs/2407.04620) makes the hidden state
itself a small learnable model updated during test sequences) — but
**stronger**, because PRISM can query simulators to *generate new
labels*, not just adapt on unlabelled data.

The right name is **multi-fidelity test-time adaptation for
engineering world models** — or **simulation-in-the-loop test-time
adaptation**. Not pure TTT.

---

## Why this fits PRISM specifically

PRISM has three things that no off-the-shelf TTT system has, all
already partially built:

| Existing | What it gives the loop |
|---|---|
| Generative design (`crates/explore` sampler, marketplace-installable design plugins) | Step 1 of the loop — produce 500 candidate designs |
| Universal NN potentials + classical/empirical surrogates (`materials_search` federation, `prediction.py`, future MACE/ORB cloud) | Step 2 — cheap physics for all candidates |
| Compute broker (`marc27-core /compute`, `compute_submit`/`compute_status`/`compute_cancel`, lab services) | Steps 3-4 — mid- and high-fidelity sims with real cost control |
| Persistent KG (`research_session` indexing) | Project memory layer for adaptation history |
| Discourse engine | Cross-checking when fidelities disagree, "metallurgist vs theorist" arguing about which design to validate |

What's *missing* is the **adaptation layer + fidelity-aware label
plumbing**. That's what this doc is scoping.

---

## The three adaptation layers — keep them distinct

Mixing these is the failure mode that turns a serious system into "an
AI blender full of CFD-flavoured hallucinations."

### 1. Fast project memory
Structured facts about *this* project. Not ML training. Just a key-
value / graph store keyed by project id.

```
project: BWB-uav-2026-05
  payload: 2 kg
  endurance: 90 min
  shell: carbon
  battery: Li-ion
  priority: low-noise
  notes: "CFD showed pitch instability in design #14 (run a312bb5)"
  ruled_out: [design_07, design_14]
```

This is exactly what PRISM's `research_session` KG does today for
research questions. We extend the same pattern to projects.

### 2. Local test-time adapter
Where TTT-style logic actually fits. The base world model stays frozen;
a small **per-project adapter** is what's trainable:

```text
base world model:           FROZEN (geometry encoder, physics latents)
project LoRA / adapter:     trainable
calibration head:           trainable
uncertainty head:           trainable
```

This is the only place inference-time gradient updates happen. Bounded
parameter count keeps catastrophic forgetting impossible by construction:
the base never changes, only the adapter does, and the adapter is
scoped to one project.

### 3. Global retraining
Only **after** simulation results pass quality checks do they get
promoted into the global training set:

```text
raw simulation result
  ↓ quality check (convergence, residuals, sanity bounds)
  ↓ fidelity tag (low / mid / high / experimental)
  ↓ append to dataset with metadata
  ↓ periodic retraining (offline, weekly/monthly)
  ↓ new base model release
```

**Never let random generated simulation data immediately corrupt the
global model.** The validated dataset is the contract; the adapter is
the sandbox.

---

## Fidelity triage — the metadata is the design

Every simulation result carries:

```json
{
  "vehicle_class": "BWB_UAV",
  "simulation_type": "CFD_RANS",
  "fidelity": "high",
  "solver": "SU2",
  "mesh_quality": "...",
  "convergence": true,
  "residuals": "...",
  "validated_against": null,
  "cost": "expensive",
  "wall_time_s": 14400,
  "confidence": 0.84
}
```

Then weights for the adapter loss become:

```text
empirical formula / handbook estimate:    0.2
low-fidelity panel / vortex model:         0.4
coarse CFD:                                0.6
RANS CFD with good convergence:            0.8
validated wind-tunnel / flight data:       1.0
```

But fidelity alone isn't enough — a bad high-fidelity CFD run shouldn't
beat a clean medium-fidelity result. So:

```python
data_weight = (
    fidelity
    * convergence_quality
    * domain_relevance       # is this regime where this fidelity is trusted?
    * uncertainty_value      # does this point reduce ensemble disagreement?
)
```

Domain-relevance example:
- BWB design near stall → high-fidelity CFD is valuable; low-fid is wrong.
- BWB design in easy cruise regime → low/mid-fidelity may be enough; high-fid is overkill.

This is the [multi-fidelity surrogate modelling](https://arxiv.org/abs/2503.00566)
problem, and there's recent active-learning work showing
[uncertainty-triggered sampling cuts high-fidelity CFD cost while
retaining RANS-level accuracy](https://arxiv.org/html/2604.13247v1)
on airfoil optimisation (2026).

---

## The full architecture

```
                ┌─────────────────────┐
                │ User design command │
                └──────────┬──────────┘
                           ↓
                ┌─────────────────────┐
                │ Mission IR / State  │   (project memory layer 1)
                └──────────┬──────────┘
                           ↓
                ┌─────────────────────┐
                │ Generative Design   │
                │ sampler + design    │   (existing: crates/explore + marketplace)
                │ plugins             │
                └──────────┬──────────┘
                           ↓
                ┌─────────────────────┐
                │ Base World Models   │   FROZEN
                │ Aero/Struct/Battery │
                └──────────┬──────────┘
                           ↓
                ┌─────────────────────┐
                │ Fidelity Triage     │   NEW
                │ cheap/mid/high sims │
                └──────────┬──────────┘
                           ↓
                ┌─────────────────────┐
                │ Local TTT Adapter   │   NEW (layer 2)
                │ project-specific    │
                │ adaptation          │
                └──────────┬──────────┘
                           ↓
                ┌─────────────────────┐
                │ Updated Predictions │
                └──────────┬──────────┘
                           ↓
                ┌─────────────────────┐
                │ Optimizer / Planner │   (next iteration)
                └─────────────────────┘
```

The base model gives broad physics intuition. The local adapter learns
the quirks of this exact BWB design family. Layer 3 (global retraining)
runs offline against the validated dataset, separate from this loop.

---

## The TTT update step (pseudo-code)

```python
base_model = frozen_world_model
adapter = fresh_project_adapter()

for design_round in range(N):
    candidates = generator.sample(mission)
    predictions = base_model(candidates, adapter)

    # Fidelity triage — pick which candidates get expensive sims
    selected = triage_by_uncertainty_and_value(predictions, budget)

    # Run multi-fidelity sims, get back labels with metadata
    sim_results = run_multifidelity_sims(selected)

    # Update adapter only — base stays frozen
    adapter = update_adapter(
        adapter,
        candidates=selected,
        labels=sim_results,
        loss=fidelity_weighted_loss,
    )

    candidates = optimizer.improve(candidates, base_model, adapter)
```

Loss components:
```
Loss =
    prediction error on new simulation labels (fidelity-weighted)
  + physics constraint penalty
  + consistency loss between fidelities  (low ↔ mid ↔ high agreement)
  + uncertainty calibration loss
  + regularization to prevent adapter drift from base
```

---

## What gets updated, what stays frozen

```text
✅ ADAPT
  adapter weights (LoRA-style)
  calibration heads
  uncertainty head
  local latent memory
  project-specific retrieval database
  small residual correction model

❌ DO NOT TOUCH AT TEST TIME
  entire foundation world model
  geometry encoder backbone
  global physics latent space
```

The TTT survey literature consistently flags **stability, distribution
shift, and forgetting** as the core problems
([Liang et al. 2024 TTA survey](https://arxiv.org/abs/2303.15361)).
Updating only the adapter is the cheap structural answer.

---

## Where JEPA-style world models fit

The base model produces latent states:

```
z_geometry, z_aero, z_structure, z_battery, z_thermal, z_stability
```

At test time, new simulations update the **project latent** (not the
encoder):

```
old BWB latent
  + new CFD evidence
  + new structural evidence
  + new battery evidence
  ↓
adapted BWB project latent
```

That's better than retraining everything. As the framing goes:

> *TTT does not rewrite the brain.
> TTT updates the working memory and local intuition for this project.*

---

## Where end-to-end TTT connects

The newer [TTT-layer / TTT-MLP work](https://arxiv.org/abs/2407.04620)
treats learning at inference as part of the model's computation. The
[E2E TTT long-context paper (2024)](https://arxiv.org/abs/2407.04620)
frames long-context modelling as continual learning: the model keeps
learning at test time from the context and compresses that context
into weights, with **meta-learning used to improve the starting point
for test-time learning**.

PRISM's engineering analogue:

```
Long-context tokens          → test-time weight adaptation
Engineering design project   → test-time physics adapter adaptation
```

But PRISM has something language models don't: **the ability to query
simulators to produce new labels**. That makes the adaptation
supervised, not just self-supervised. The adapter has ground truth.

---

## The four risks (and the fixes)

### Risk 1 — Self-confirming bullshit loop
The generator proposes a bad design. The surrogate evaluates it badly.
The low-fidelity sim confirms the same bias. The adapter learns the
wrong thing.

**Fix**:
- Force occasional **high-fidelity ground-truth checks** (not optional).
- Use **disagreement between models** as the trigger for high-fidelity.
- Track an out-of-distribution score per candidate.
- Penalize overconfident extrapolation (uncertainty head must agree).

### Risk 2 — Catastrophic forgetting
The adapter overfits to one weird BWB design and loses generality.

**Fix**:
- Base model frozen.
- Small adapters only (LoRA bottleneck dimension constrains capacity).
- Replay buffer with samples from the project's earlier rounds.
- L2 regularization back to base predictions on a held-out validation set.

### Risk 3 — Fidelity contamination
The model treats low-fidelity and high-fidelity results as equivalent.

**Fix**:
- Every label carries fidelity metadata (mandatory, no defaults).
- Train **fidelity-conditioned prediction heads** so the model knows
  which fidelity it's currently emulating.
- Learn an explicit **low → high fidelity correction**.
- Never mix raw labels blindly; weight by `data_weight` formula above.

### Risk 4 — Cost explosion
Every user request triggers CFD; platform dies.

**Fix**:
- Cheap models always run.
- Mid-fidelity selectively (top-K by predicted value × predicted variance).
- High-fidelity only when uncertainty × value × disagreement justifies it.
- Hard per-project budget (configurable; default modest).

---

## How this fits with the existing PRISM Fabric work

Two complementary directions:

| Layer | PRISM Fabric | Multi-Fidelity TTT |
|---|---|---|
| What it gives | Private compute network where heterogeneous devices contribute | Stateful design loop that adapts to one specific project |
| Time horizon | Months (8-step roadmap) | Months (similar scope) |
| Where it sits | Network + compute + ML planes | ML plane + agentic layer |
| Existing PRISM code it builds on | `crates/mesh`, `crates/node`, MARC27 compute broker | `crates/explore`, `prediction.py`, `marketplace`, `research_session` KG |

These compose. Once the Fabric exists, the multi-fidelity sims can
*run on peer nodes* (fidelity triage = "send the cheap sim to my Mac
mini, the medium-fidelity one to my friend's GPU box, the high-
fidelity one to MARC27 cloud"). The Fabric is the substrate; TTT is
the loop on top.

---

## What this doc is NOT

- Not a sprint plan. The build has prerequisites (the existing Phase 1
  refactor merges first; tools cleanup; Fabric milestones 1-4).
- Not authorization to start ripping out code or training models.
- Not a claim that PRISM has any of this today. It doesn't. The
  building blocks exist (generative design, surrogates, compute broker,
  KG memory) but the adaptation loop + fidelity metadata layer is
  green-field.
- Not narrow to BWB drones / aerospace. The same architecture applies
  to alloy design (where the user's actual interest lies): generate
  500 alloy compositions → empirical formula screen → CALPHAD on
  top-50 → DFT/MD on top-5 → adapter learns this alloy family →
  improve next-round candidates.

---

## Ordered roadmap

| # | Milestone | Effort | Depends on |
|---|---|---|---|
| 1 | Mission IR / project state schema (project memory layer 1) | days | nothing — can start now |
| 2 | Fidelity-tagged label format + write path into KG | days | (1) |
| 3 | Single-project replay buffer + adapter scaffold (frozen base, LoRA-only training, no real loop yet) | week | (1)(2) |
| 4 | Cheap-physics surrogate ensemble + uncertainty head | weeks | (3) |
| 5 | Triage policy: pick K candidates for next-fidelity-up | week | (4) |
| 6 | First closed loop on a small alloy problem (CALPHAD as mid-fidelity, DFT as high) | 2 weeks | (5) + materials-science tools layer |
| 7 | Validation + offline retraining pipeline (Layer 3) | weeks | (6) |
| 8 | Aerospace surface — BWB drone demo | months | the materials demo proves the loop first |

Milestones 1-3 are independent enough to ship before the rest of the
Fabric work. Milestones 6-8 want the Fabric in place so the simulations
can fan out across nodes.

---

## Sources

- [Sun et al. 2020 — Test-Time Training with Self-Supervision for Generalization under Distribution Shifts](https://arxiv.org/abs/1909.13231)
- [TTT-layers / TTT-MLP — Sun, Li et al. 2024](https://arxiv.org/abs/2407.04620)
- [Liang et al. 2024 — Comprehensive Survey on Test-Time Adaptation](https://arxiv.org/abs/2303.15361)
- [Multi-Fidelity Surrogate Modeling for Time-Series Outputs (Acta Mechanica 2025)](https://link.springer.com/article/10.1007/s00466-024-02540-x)
- [Active multi-fidelity surrogate modeling for airfoil optimization (2026)](https://arxiv.org/html/2604.13247v1)
- [JEPA — Joint Embedding Predictive Architecture, LeCun 2022 path-towards-autonomous-machine-intelligence](https://openreview.net/forum?id=BZ5a1r-kVsf)

---

## What I'm doing about this right now

**Nothing.** The strategic direction is the PRISM Fabric (the
Tailscale-style private compute network), not this. This doc exists
solely so a good architectural idea doesn't get lost — but the
Fabric is what gets built.

Current work-in-progress (as of 2026-05-08):
- PR #12 finishing the chat-target refactor (Phase 1 architecture)
- Tool grind continuing on the ~50 MCP tools (descriptions, schemas)

Neither of those depends on this doc. If at some future date the
team decides to invest in test-time adaptation for engineering
projects, this doc will be here. Until then, ignore it.
