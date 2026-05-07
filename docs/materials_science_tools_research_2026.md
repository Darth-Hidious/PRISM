# Deep Research — what makes PRISM the materials-discovery app of the dream

**Status:** Vision/landscape doc. Companion to `materials_science_tools_2026.md`.
Researched 2026-05-07 via WebSearch (no EXA — paid tier; user said skip).
Sources at the bottom.

**Frame for this doc:** The user is not a materials scientist. The user has
ideas for alloys/materials they want to develop. The dream version of PRISM
is the tool that lets them describe an alloy in plain English and the system
*actually figures out a candidate* — not by hand-waving, by running the same
tooling a real lab would run, end-to-end. This doc maps the 2026 state of
the art in that space and identifies the concrete gap PRISM has to fill.

---

## 1 · The competitive landscape (May 2026)

There are five distinct kinds of "AI for materials" product/research today.
PRISM straddles three of them and competes head-on with none yet:

### 1.1 Foundation generative models

| Product | What it does | Constraint |
|---|---|---|
| **[Microsoft MatterGen](https://www.microsoft.com/en-us/research/blog/mattergen-a-new-paradigm-of-materials-design-with-generative-ai/)** ([Nature 2025](https://www.nature.com/articles/s41586-025-08628-5)) | Diffusion model that generates stable inorganic crystal structures conditioned on property targets (mechanical, electronic, magnetic). Trained on 608K stable materials from MP + Alexandria. | Open-source weights ([HF](https://huggingface.co/microsoft/mattergen)) but a model, not a product. Needs orchestration around it to be useful to a non-expert. |
| **[DeepMind GNoME](https://deepmind.google/discover/blog/millions-of-new-materials-discovered-with-deep-learning/)** | GNN ensemble + active learning, generated **2.2M candidate structures, 380K thermodynamically stable**. Berkeley A-Lab synthesised 41/58 of GNoME's targets in 17 days (71% hit rate). | Predictive model, not a workflow. The structures are a database, not advice. |
| Equivariant-NN potentials (foundation, "uMLIPs") | Drop-in replacements for DFT during energy/force evaluations. State-of-the-art models in **Matbench Discovery** rankings: **eqV2-M, ORB, MACE-MPA, MatterSim, SevenNet, GRACE-2L-OAM, M3GNet, CHGNet, DPA**. Benchmarked across phonons, MOFs, electrolytes. | All ASE-compatible. Already pip-installable. **Plug-and-play candidate to add to PRISM.** |

### 1.2 Agentic LLM systems for materials

| Project | Pattern | Where PRISM compares |
|---|---|---|
| **[MARS](https://phys.org/news/2026-01-multi-agent-ai-robots-automate.html)** (Multi-Agent + Robot System) | 19 LLM agents in 5 functional groups: Orchestrator, Scientist, Engineer, Executor, Analyst. Closed-loop with physical robotic platforms. | This is the **north star**. PRISM has the discourse engine which is a slimmer version (debate-only). To match MARS we'd add the 5-role orchestration as a workflow template. |
| **MatterChat** ([Nature MI 2025](https://www.nature.com/articles/s42256-026-01214-y)) | Multimodal LLM with plug-and-play interatomic potential backends (CHGNet, MACE) for property predictions + interpretable reasoning. | Closest to PRISM's positioning. We have the chat + tools surface; we don't yet have the multimodal grounding (structure images, phonon plots) wired in. |
| **AlloyGPT** ([Thermo-Calc webinar](https://resources.thermocalc.com/recording-watch-alloygpt-an-agent-based-llm-framework-for-the-design-of-additively-manufactured-structural-alloys)) | Agent framework specifically for AM (additive-manufacturing) alloys; uses Thermo-Calc Python API for CALPHAD calculations. Trained on CALPHAD-based ICME data. | Narrow (AM-only) but **shows the integration shape** PRISM should adopt for CALPHAD. |
| **[LLMatDesign](https://arxiv.org/abs/2406.13163)** | LLM-driven autonomous materials discovery loop with mutation/crossover guided by language reasoning. | Single-agent, no tool-use. PRISM's agentic loop + tool catalog goes deeper. |
| **[LangSim](https://github.com/jan-janssen/LangSim)** | LangChain agent that drives MACE-MP-0 forcefields to predict bulk modulus etc. | Demo-scale. PRISM's tool router + marketplace is more general. |

### 1.3 Self-driving / autonomous labs (SDLs)

| Project | What | Lesson for PRISM |
|---|---|---|
| **A-Lab** (Berkeley + DeepMind) | DFT prior → robotic synthesis → XRD identification → active-learning recipe optimisation. **41/58 air-stable inorganics synthesised in 17 days.** | The closed-loop pattern: predict → synthesise → measure → re-predict. PRISM's compute broker is the analog of "robotic platform" for the *computational* version of this. |
| **[RoboChem-Flex](https://www.nature.com/articles/s44160-026-01053-0)** | Low-cost, modular SDL with Python + Bayesian optimisation. | Good prior art for how to wire experiments. Most relevant for chemistry but the workflow architecture (state machines + BO) maps to PRISM's `discourse` + `workflow` modules. |
| **RSC SDL benchmarks** ([2026 review](https://pubs.rsc.org/en/content/articlehtml/2026/dd/d5dd00337g)) | Benchmark suite for SDL performance. | When PRISM ships a closed-loop workflow, this is the leaderboard to target. |

### 1.4 CALPHAD / thermodynamics

| Tool | Status | PRISM action |
|---|---|---|
| **[Thermo-Calc Python API](https://thermocalc.com/products/thermo-calc/python-api/)** | Commercial, gold standard. Already used by AlloyGPT. | Wrap as MCP tool; user provides their own license env var. |
| **[pycalphad](https://github.com/pycalphad/pycalphad)** | Open-source, BSD. Active dev. | First CALPHAD integration to ship — covers most academic users. |
| **[OpenCALPHAD](https://www.opencalphad.com/)** | Open-source alternative. | Lower priority than pycalphad but worth wrapping for licensing diversity. |
| **CALPHAD + uMLIP coupling** ([Acta Materialia 2026](https://www.sciencedirect.com/science/article/abs/pii/S1359645425000400)) | Recent papers couple CALPHAD with foundation NN potentials for faster phase diagrams. | This is a real research frontier; PRISM is a natural orchestrator (it already has the agent layer + tool dispatch). |

### 1.5 Cluster expansion (CE) — the user's specific ask

| Tool | Why | Status |
|---|---|---|
| **[ICET](https://icet.materialsmodeling.org/)** (Chalmers) | Pythonic API, C++ inner loop, mature, ASE-native. Linear/Bayesian regression, feature selection, CV, MC sampling. | **Recommended first integration.** Cleanest API. |
| **[CLEASE](https://gitlab.com/computationalmaterials/clease)** | ASE-integrated, fits CE against arbitrary structures. ECI fitting + MC. | Second integration — different pattern (training-from-DFT-data). |
| **[CASM](https://github.com/prisms-center/CASMcode)** (Caltech) | The reference. Heavy DSL but most complete. | Third — wrap once the ICET/CLEASE patterns settle. |

---

## 2 · Reference architectures we can learn from

### 2.1 MARS-style 5-role orchestration

```
                    ┌─────────────┐
                    │ Orchestrator│   (PRISM agent loop, today)
                    └──────┬──────┘
            ┌──────────────┼──────────────┐
            ▼              ▼              ▼
       ┌────────┐    ┌─────────┐    ┌──────────┐
       │Scientist│   │Engineer │    │Executor  │
       │(retrieval│  │(design→ │    │(robotic / │
       │+design)│   │protocol)│    │compute)  │
       └────────┘    └─────────┘    └──────────┘
            ▲              ▲              ▲
            └──────────────┼──────────────┘
                    ┌──────▼──────┐
                    │  Analyst    │
                    └─────────────┘
```

Today PRISM has an **Orchestrator** (the chat agent) and *some* of the
Scientist tools (knowledge_search, query_materials_project). The Engineer
(design → executable protocol) is partial via discourse YAML specs. The
Executor (compute) is partial via the broker. The Analyst (interpret data)
is missing as a distinct role.

**Action:** ship a `discourse spec` template for this 5-role pattern. Users
who want full MARS-style autonomy run it; users who want chat just chat.

### 2.2 MatterChat-style "LLM + plug-and-play uMLIP"

LLM acts as the planner; an interatomic potential acts as the physics
predictor. The LLM doesn't know the physics; the potential knows nothing
about reasoning. **Both are tools the agent calls.**

PRISM's existing `Stage 2.1 retrieval` (EmbeddingGemma top-K) is exactly
what makes this scalable when there are many such backends. Adding ORB,
MACE-MP-0, etc., as marketplace items is the path.

### 2.3 A-Lab-style closed-loop

```
DFT prior
    ↓
GNoME / MatterGen suggestions
    ↓
ranked candidates  ←──── active learning loop
    ↓
robotic synthesis  (real lab) OR
DFT verification   (PRISM compute broker — the lab analogue)
    ↓
property measurement
    ↓
update prior, repeat
```

PRISM has the digital half of this loop. Wiring the analytical half
(structured candidate-table → re-rank → next round) is the missing piece.
The `discourse` engine is already a one-shot version of this; making it
*iterative* with explicit memory across rounds is the upgrade.

---

## 3 · What PRISM uniquely has

Not all of these existing systems do these things; PRISM does:

1. **Local agent + cloud platform split.** PRISM CLI runs on user's machine; MARC27 owns the heavy infrastructure. Other agentic systems are notebook-bound.
2. **Provider-agnostic chat** (after the architecture refactor lands). User picks gpt-5.5, claude-sonnet-4, local llama, etc. Not tied to one model vendor.
3. **Marketplace for tools and models** — not a research demo's scoped feature, a real installable extension surface.
4. **Discourse engine for explicit multi-agent debates** — the only system in this list that has it as a first-class CLI command (`prism discourse run alloy-debate`). The rest reinvent it per-paper.
5. **TUI-native UX.** Materials scientists live in clusters and SSH; a TUI fits their workflow far better than a notebook UI.

**Translation:** the value PRISM is buying is "the unified harness." The
underlying physics is borrowed from the open ecosystem (ASE, pymatgen,
MACE/ORB, ICET, pycalphad). The product is the **agent + tools + workflow +
marketplace** stitching.

---

## 4 · Concrete gap analysis — what PRISM is missing today

Ranked by leverage (most impact per unit work first):

### Gap 1 — Foundation NN potential as a tool — TWO-TIER
Two-tier strategy: **local-built-in** + **cloud-paid**, sharing one MCP
tool surface from the agent's perspective.

**Tier 1 — Local, built-in, free (~1-2 days)**
- Ship `MACE-MP-0b3` + `MACE-MPA-0` (the MPtraj+sAlex SOTA variant).
- Both are MIT-licensed code + permissive weights, ASE-compatible via
  `mace.calculators.mace_mp()`. Pip-installable, GPU-optional.
- MCP tool name: `predict_energy(structure) → {energy, forces, stress}`.
- Lets a user with no internet get instant energy/force predictions
  for any structure. The lowest-friction way to feel the product work.

**Tier 2 — Cloud, MARC27-hosted, paid (~1 week)**
- Ship `MACE-mh-1` (multi-head, non-linear interaction blocks — the
  smartest current variant; trained on OMAT + RGD1 + MATPES-R2SCAN +
  MPtraj + OMOL + SPICE + OC20-2M with replay).
- Hosted on MARC27 GPU pool. New endpoint
  `/api/v1/projects/{pid}/atomistic/mh1/predict`. Returns same shape
  as the local tool.
- Bundled with MARC27-curated dataset access: pre-relaxed Materials
  Project structures, OMAT slices for licensed users, fine-tuned
  variants per user's domain (Ni-superalloys, perovskites, etc.).
- Reasons to put this in the cloud:
  1. OMAT-derived weights inherit a more restrictive license than
     MPtraj — cloud-served fits the licensing model better than local
     redistribution.
  2. mh-1 with non-linear blocks is real-compute (A100-class) for
     production-scale screens.
  3. Continuous fine-tuning server-side — local users get the latest
     smartness without re-installing.
  4. Curated datasets are a real paid-tier value-add (something free
     local mode can't replicate).

**Same MCP tool name** from the agent — the agent's call to
`predict_energy(...)` resolves to local tier when offline / no
MARC27 token, and to MARC27 cloud when authed. Cost-aware routing
(cheap local for screening, MARC27-mh-1 for final ranking) is a
future optimisation in Stage 2.1's retrieval layer.

This is the moat: nobody else has the local-immediate +
cloud-smarter-with-data split. Foundation-model papers ship one or
the other; PRISM ships both behind one prompt.

### Gap 2 — One CE backend wired (~2–3 days)
**ICET** as MCP tool: `ce_fit(structures, energies)`, `ce_predict(config)`,
`ce_run_mc(temperature, n_steps)`. This is what materials scientists actually
reach for. The user explicitly asked for this.

### Gap 3 — Iterative discourse with persistent state (~2 days)
Today `discourse run` is one-shot. Add `discourse iterate <instance> --feedback "<json>"`
so the agent can run round 1, see results, run round 2 with refined
constraints, etc. This unlocks the closed-loop pattern of A-Lab without
needing a robot.

### Gap 4 — pycalphad MCP wrapper (~1 day)
Phase diagram queries are routine in alloy work; pycalphad is open-source
and trivial to wrap. Once wired, `discourse` specs can use phase predictions
as a constraint.

### Gap 5 — Compute broker hardening for VASP/QE (~1 week)
Today the broker is per-user-cluster. Add canonical input-deck generators
for VASP and QE so the agent can submit a well-formed job, not just shell
out an arbitrary command. **Without this the loop never closes** — DFT
verification stays manual.

### Gap 6 — Materials-aware retrieval index for Stage 2.1 (~3 days)
The current EmbeddingGemma top-K embeds tool *descriptions*. For materials
work, also embed:
- Common composition formulas (Inconel 718 → Ni-Cr-Fe-Mo-Nb-Ti)
- Property names (creep resistance, oxidation, σ-phase formation)
- Phase symbols (γ', δ, η)

So the agent can retrieve tools by domain term even when the user says
"resists γ' coarsening" instead of "phase stability tool."

### Gap 7 — User-supplied model marketplace pattern (~1 week)
Schema for "researcher uploads a fitted CE / surrogate model + manifest →
PRISM downloads + auto-registers it as a tool." Manifest format must avoid
the unsafe-by-default deserialization risk (use safetensors / ONNX /
JSON+npz, never raw Python binary serialization). Belongs in the
**security pen-test** scope.

### Gap 8 — MatterGen / GNoME as tools (~2 days each)
Both have HF Hub weights. Wrap MatterGen for "generate me 10 candidates with
property X" and GNoME for "is this structure likely stable?" Easy wins.

### Gap 9 — Multimodal grounding (~2 weeks, depends on Phase 4 IDE/Canvas)
Plot phonon dispersions, render crystal structures, show phase diagrams
inline. The TUI can't render images natively — this is where the **canvas**
piece of Phase 4 earns its keep.

---

## 5 · The materials-science tool stack PRISM should ship with

Reasonable v1 inventory after Gaps 1–4 land:

| Layer | Tool | Source |
|---|---|---|
| **Structure manipulation** | ASE | Built-in |
| **DB queries** | pymatgen + Materials Project | Built-in |
| **Universal NN potential** | MACE-MP-0 (or ORB) | Built-in |
| **Cluster expansion** | ICET | Built-in |
| **Phase diagrams** | pycalphad | Built-in |
| **Generative design** | MatterGen | Marketplace |
| **Stability filter** | GNoME | Marketplace |
| **DFT submission** | VASP/QE wrappers | MARC27 broker |
| **Active learning loop** | discourse iterate | Built-in (gap 3) |
| **Property surrogates** | user-trained models | Marketplace (gap 7) |
| **Commercial CALPHAD** | Thermo-Calc Python API | Marketplace (BYO license) |

That's the **app of the dream** — every layer is a real, citable, working
piece of the open materials-science stack, stitched into one chat-driven
workflow that a non-expert can drive.

---

## 6 · Recommended order to ship

Once Phase 1 (provider architecture) lands:

| Order | Item | Time | Why first/last |
|---|---|---|---|
| 1a | MACE-MP-0b3 + MACE-MPA-0 as built-in (local, free) | 1–2 d | Instant offline atomic-scale prediction; the demo moment when a user types and the answer renders |
| 1b | MACE-mh-1 as MARC27-cloud service (paid, with curated data) | 1 w | The "smarter" tier — multi-head non-linear blocks, OMAT-trained, GPU-hosted, fine-tunable per domain. Same MCP tool name; routing decided by auth state |
| 2 | pycalphad wrapper | 1 d | Phase stability is in every alloy conversation |
| 3 | ICET CE wrapper | 2–3 d | User's specific ask; makes alloy thermo accessible |
| 4 | Iterative discourse | 2 d | Unlocks A-Lab pattern in pure software |
| 5 | VASP/QE input-deck generators | 1 w | Closes the verification loop |
| 6 | MatterGen + GNoME marketplace items | 2 d each | Generative side; differentiator |
| 7 | User-supplied model marketplace | 1 w (+ security review) | Scales the catalog without engineering |
| 8 | Canvas multimodal panel | depends on Phase 4 | The "see your alloy" moment |

After items 1–4 ship, PRISM can demo: *"prompt → screen 10 000 candidates →
filter by phase stability → debate top 10 → output structured candidate
table with confidence intervals,"* all in one chat. That demo is what would
make the product feel like the dream.

---

## Sources

### Foundation models / generative materials AI
- Microsoft MatterGen: [research blog](https://www.microsoft.com/en-us/research/blog/mattergen-a-new-paradigm-of-materials-design-with-generative-ai/), [Nature 2025](https://www.nature.com/articles/s41586-025-08628-5), [GitHub](https://github.com/microsoft/mattergen), [HF](https://huggingface.co/microsoft/mattergen)
- DeepMind GNoME: [Nature 2023](https://www.nature.com/articles/s41586-023-06735-9), [A-Lab synthesis 41/58](https://www.nature.com/articles/s41586-023-06734-w)

### MACE foundation potential family (from full search 2026-05-07)
- MACE code: [ACEsuit/mace](https://github.com/ACEsuit/mace) — MIT
- MACE foundation models: [ACEsuit/mace-foundations](https://github.com/ACEsuit/mace-foundations) — MP / OMAT / mh-0 / mh-1 lineup
- MACE-MP-0 (1.6M MPtraj crystals, 89 elements): [Matbench Discovery](https://matbench-discovery.materialsproject.org/models/mace-mp-0), [Rowan overview](https://rowansci.com/features/mace-mp-0), [arXiv 2401.00096](https://arxiv.org/abs/2401.00096)
- MACE-MPA-0 (MPtraj + sAlex, SOTA on Matbench): [Matbench Discovery](https://matbench-discovery.materialsproject.org/models/mace-mpa-0)
- MACE foundation docs: [mace-docs.readthedocs.io/foundation_models](https://mace-docs.readthedocs.io/en/latest/guide/foundation_models.html)
- MACE on HF: [mace-foundations](https://huggingface.co/mace-foundations), [cyrusyc/mace-universal](https://huggingface.co/cyrusyc/mace-universal)
- pip package: [mace-torch](https://pypi.org/project/mace-torch/)

### Agentic systems
- MARS multi-agent + robot system, [phys.org Jan 2026](https://phys.org/news/2026-01-multi-agent-ai-robots-automate.html)
- MatterChat multimodal materials LLM, [Nature MI 2026](https://www.nature.com/articles/s42256-026-01214-y)
- LLMatDesign autonomous materials discovery, [arXiv 2406.13163](https://arxiv.org/pdf/2406.13163)
- AlloyGPT for additive-manufactured alloys, [Thermo-Calc webinar](https://resources.thermocalc.com/recording-watch-alloygpt-an-agent-based-llm-framework-for-the-design-of-additively-manufactured-structural-alloys)
- AI-Agent platform for computational materials workflows, [JMI 2026](https://www.oaepublish.com/articles/jmi.2025.69)
- Survey of foundation models + agents in materials science, [arXiv 2506.20743](https://arxiv.org/html/2506.20743v1)
- Towards Agentic Intelligence for Materials Science, [arXiv 2602.00169](https://arxiv.org/html/2602.00169v1)

### Universal NN potentials
- Universal MLIP phonon benchmark (M3GNet, CHGNet, MACE-MP-0, SevenNet-0, MatterSim, ORB, eqV2-M), [npj 2025](https://www.nature.com/articles/s41524-025-01650-1)
- MOFSimBench (20 uMLIPs), [npj 2025](https://www.nature.com/articles/s41524-025-01872-3)
- Solid-state electrolyte uMLIP benchmark, [ACS Materials Letters 2026](https://pubs.acs.org/doi/10.1021/acsmaterialslett.5c00336)
- Orb interatomic potential intro, [Orbital](https://www.orbitalindustries.com/posts/technical-blog-introducing-the-orb-ai-based-interatomic-potential)
- Matbench Discovery leaderboard, [matbench-discovery.materialsproject.org](https://matbench-discovery.materialsproject.org/)

### CALPHAD + ML
- pycalphad GitHub, [pycalphad/pycalphad](https://github.com/pycalphad/pycalphad)
- CALPHAD + uMLIPs for Pt-W, [arXiv 2508.01028](https://arxiv.org/html/2508.01028v1)
- CALPHAD + universal MLIP for complex alloy phase diagrams, [Acta Materialia 2026](https://www.sciencedirect.com/science/article/abs/pii/S1359645425000400)
- OpenCALPHAD, [opencalphad.com](https://www.opencalphad.com/)

### Cluster expansion
- ICET — Chalmers Python+C++ CE library, [icet.materialsmodeling.org](https://icet.materialsmodeling.org/), [Adv Theory Sim 2019](https://onlinelibrary.wiley.com/doi/full/10.1002/adts.201900015)
- CLEASE — ASE-integrated CE, [academia.edu paper](https://www.academia.edu/107502597/CLEASE)
- CASM, [GitHub](https://github.com/prisms-center/CASMcode)

### Self-driving labs
- SDL technology review, [Royal Society Open Sci 2025](https://royalsocietypublishing.org/rsos/article/12/7/250646/235354/Autonomous-self-driving-laboratories-a-review-of)
- SDL benchmarking, [Digital Discovery 2026](https://pubs.rsc.org/en/content/articlehtml/2026/dd/d5dd00337g)
- RoboChem-Flex modular SDL, [Nature Synthesis 2026](https://www.nature.com/articles/s44160-026-01053-0)
- Toward SDL 2.0, [Materials Horizons 2026](https://pubs.rsc.org/en/content/articlehtml/2026/mh/d5mh01984b)
- Bayesian active learning closed-loop discovery, [Nat Comm 2020](https://www.nature.com/articles/s41467-020-19597-w)

### Curated lists
- best-of-atomistic-machine-learning, [GitHub JuDFTteam](https://github.com/JuDFTteam/best-of-atomistic-machine-learning)
