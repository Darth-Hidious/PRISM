# Materials-Science Tool Layer — what makes PRISM not just another chat client

**Status:** Backlog / vision doc. Not in flight; captures the roadmap so it
doesn't get lost while Phase 1 (provider architecture, chat reliability)
finishes. Will become Phase 5+ when Phase 1–4 ship. Will be expanded with
deep-research findings in a sibling doc once those land.

**One-line product question:** if a user (not a materials scientist) prompts
PRISM with *"design a creep-resistant nickel-based alloy that operates at
800 °C and is cheaper than Inconel 718"*, what tools does the agent need to
actually answer that — not just describe how an answer would be produced?

---

## The set of tools that would make this real

Grouped by what they do, not by what library implements them:

### 1. Configuration & structure manipulation
- **ASE** (Atomic Simulation Environment) — the lingua franca for atomic structures in Python. Already pretty broadly available; PRISM should expose it as MCP tools (`make_supercell`, `mutate_composition`, `relax_geometry`, …).
- **pymatgen** — Materials Project's structure + analysis library. Already partially wired via `mcp_prism_python_tool_query_materials_project`.

### 2. Cluster expansion (CE) — the user's specific ask
CE is the workhorse for predicting energies of alloy configurations without
running a DFT calculation for each one. PRISM should support **all three**
of the production-grade CE toolchains so domain experts can bring their own
fitted models:

| Tool | Why it matters | Integration shape |
|---|---|---|
| **[CASM](https://github.com/prisms-center/CASMcode)** | Caltech's canonical CE+Monte Carlo workflow tool. The reference implementation for CE in the alloy-thermo community. | Python wrapper → MCP tool: `casm_predict_energy(config)`, `casm_run_canonical_mc(temperature, composition)`. CASM has its own DSL — wrap it. |
| **[CLEASE](https://gitlab.com/computationalmaterials/clease)** | Modern ASE-integrated, fits CE models against arbitrary structures. Cleaner Python API than CASM. | MCP tool: `clease_fit(structures, energies)`, `clease_predict(config)`. |
| **[ICET](https://icet.materialsmodeling.org/)** | Fast CE library with active-learning hooks. | MCP tool: `icet_fit`, `icet_predict`, `icet_active_learn`. |
| User-supplied CE models | Researchers ship their own fitted CE objects | Marketplace item type: "CE Model" — a binary blob + a `.toml` describing the fit. PRISM downloads, exposes as `predict_<model_name>`. |

### 3. Differentiable physics (JAX)
The big win — gradient-based discovery instead of just exhaustive search:

| Tool | What it unlocks |
|---|---|
| **[JAX-MD](https://github.com/jax-md/jax-md)** | Differentiable molecular dynamics. Train interatomic potentials end-to-end. Compute property gradients w.r.t. composition. |
| **[Allegro](https://github.com/mir-group/allegro)** / **[NequIP](https://github.com/mir-group/nequip)** | Equivariant NN potentials. Order-of-magnitude better data efficiency than classical ML. |
| **[FlaxMD](https://github.com/google-research/flax)** + custom heads | Differentiable property predictors trained on user-supplied datasets. |
| User-supplied JAX models | Same marketplace pattern: ship a serialized model bundle + `.toml`, PRISM exposes the model as a tool the LLM can call. (Note: serialization format must NOT be Python's pickle for security — see open questions below.) |

The agent loop benefits from gradients: "I want creep resistance ≥ X, density ≤ Y, cost ≤ Z" becomes a constrained optimisation problem the LLM coordinates step-by-step using these gradient-aware tools.

### 4. DFT job submission & monitoring
The slow but ground-truth path:

| Capability | Backed by |
|---|---|
| VASP / Quantum ESPRESSO / LAMMPS submission | MARC27 compute broker → SLURM/PBS on user's cluster, OR pre-canned cloud GPU |
| Live job monitoring (queue position, walltime, log tail) | PRISM already has the bones (`prism node status`, `prism mesh discover`); needs a tool wrapper agents can poll |
| Result parsing (energy, forces, magnetic moments) | ASE + pymatgen — already pip-installable |

### 5. Property predictors (cheap surrogates)
Used by the agent to triage 1000s of candidates before paying for DFT:

- **[MACE](https://github.com/ACEsuit/mace)** / **[M3GNet](https://github.com/materialsvirtuallab/m3gnet)** universal NN potentials
- **[SchNet](https://github.com/atomistic-machine-learning/schnetpack)** for property predictions on arbitrary composition
- User-trained surrogates from the marketplace

### 6. Phase diagram / thermodynamics
- **[Thermo-Calc](https://thermocalc.com/)** integration (commercial — wrap their CLI/API)
- **[OpenCALPHAD](http://www.opencalphad.com/)** open-source alternative
- **[pycalphad](https://pycalphad.org/)** Python-native, easy MCP target

---

## How they plug into PRISM (architecture)

All of the above land as **Python MCP tools** through `prism-python` MCP
server (the one already at `app/`). Three integration patterns:

| Pattern | Example | Lives where |
|---|---|---|
| **Built-in** — ships with PRISM, always available | `ase_make_supercell`, `pymatgen_query` | `app/tools/` |
| **Marketplace pip extras** — user runs `prism marketplace install jax-md`, PRISM pulls the wheel + registers tools | `jax_md_simulate`, `casm_predict` | downloaded to `~/.prism/extras/` |
| **User-supplied models** — researcher uploads model bundle + `.toml` describing inputs/outputs, PRISM treats it as a tool with a typed schema | `predict_creep_resistance` (someone's trained surrogate) | downloaded to `~/.prism/models/` |

The Stage 2.1 retriever (EmbeddingGemma) handles the "we have 500 tools, only 13 are relevant to this query" pruning that's already wired. So adding 100 more tools doesn't blow the prompt budget — the embedder narrows them per turn.

The agent doesn't need to know WHICH library implements a tool — just what it does. The tool description is the contract.

---

## What enables the user's actual use case

Going back to the prompt: *"design a creep-resistant Ni-based alloy at 800 °C, cheaper than Inconel 718"*.

A reasonable agent loop with the above tools:

1. **Knowledge retrieval** — query Materials Project + literature for known Ni superalloys (uses `mcp_prism_python_tool_query_materials_project`, `knowledge_search`).
2. **Cost screening** — pull commodity prices for constituent elements (cheap MCP tool, ships with PRISM).
3. **CE-based composition sweep** — use the user's pre-fitted CE model for Ni-X-Y systems to predict formation energies of 10 000 candidate compositions in seconds (`casm_predict_energy` or user's CE model).
4. **Surrogate ranking** — use a NN surrogate (MACE / user-trained) to rank top 100 by predicted creep resistance proxy.
5. **Discourse round** — invoke a multi-agent debate (PRISM's existing `discourse` engine, alloy-debate-style spec) on the top 10 — let metallurgist + theorist agents argue about which to verify.
6. **DFT verification** — submit DFT jobs for the top 3 to the compute broker, monitor, return final energies.
7. **Report** — render the result as a structured candidate-alloy summary the user can act on.

**This is the product.** Without these tools, PRISM is just chat-with-pretty-tools. With them, it's the only tool that makes alloy discovery accessible to non-experts.

---

## Phasing

This work is **NOT for the current architecture refactor** (Phase 1). It's
flagged here so it doesn't get lost. Sequence:

1. **Phase 1 (in flight)** — provider architecture, reliable chat, tool-call streaming. *Foundation. Without this, none of the below works.*
2. **Phase 2 (next)** — TUI cosmetics, boot redesign, Apple-feel polish.
3. **Phase 3** — README/positioning/copy.
4. **Phase 4** — PRISM IDE / Canvas (separately scoped, design doc draft).
5. **Phase 5 — materials-science tool layer (this doc)**:
   - 5a. Wrap ASE + pymatgen properly as MCP tools (broadens reach today).
   - 5b. CE: integrate CLEASE first (cleanest API, ASE-native), then ICET, then CASM. Ship one CE model in the marketplace as proof.
   - 5c. JAX-MD + a NequIP/Allegro example. Ship one NN potential in the marketplace as proof.
   - 5d. Marketplace patterns for "CE model" and "JAX/PyTorch model" item types — schema + upload flow + auto-register.
   - 5e. Compute broker hardening for real DFT submissions (VASP/QE wrappers).
   - 5f. End-to-end alloy-discovery demo: prompt → CE screen → discourse → DFT verify → report. **The product launch demo.**

Each sub-phase of 5 is independently shippable; we don't need all of it at
once. CE (5b) is probably the highest-leverage entry point given the user's
ask — most of materials thermo runs through CE today.

---

## Open questions for later

- Do we ship CE/JAX models pre-baked or always via marketplace? (Latter is
  cleaner for licensing but slower for first-time users.)
- Compute broker: own GPUs (capex), bring-your-own-cluster (SLURM/PBS), or
  cloud-burst (RunPod/Lambda)? Probably all three eventually.
- How does PRISM authenticate to ThermoCalc when their API requires per-seat
  licenses? — same story as any commercial API: BYO-key in env vars, MARC27
  doesn't intermediate.
- For user-supplied JAX/PyTorch models: which serialization format? Python's
  default binary serializer is a remote-code-execution risk on load — must
  use safetensors / ONNX / Flax-orbax with signed manifests. Belongs to the
  post-MVP **security pen-test** task already on the backlog.
- What's actually state of the art in late 2025 / early 2026? Deep-research
  pass needed before Phase 5 lock-in — see sibling doc
  `materials_science_tools_research_2026.md` (in progress).
