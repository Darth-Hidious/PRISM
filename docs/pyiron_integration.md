# PRISM ↔ pyiron Integration

**Status:** Active boundary  
**Owner:** `app/tools/simulation/bridge.py`  
**Authors:** Sid + Claude (2026-05-08)  
**Audience:** PRISM contributors who need to add or modify simulation tools

## TL;DR

> **PRISM is an AI-native research workspace.**  
> **pyiron is an atomistic-simulation IDE / orchestrator.**  
> They sit at different levels of the stack. pyiron is a tool inside PRISM, not a competitor.

Use `pyiron_atomistics` + `pyiron_base` + transitively `executorlib`. **Do not** use `pyiron_workflow` or `pyiron_core`. **Do not** duplicate HDF5 storage, the pyiron job database, atomistic SLURM submission, or ASE-compatible structure objects. Always go through `app/tools/simulation/bridge.py`.

## What pyiron is (and isn't)

pyiron is a modular ecosystem of ~10 packages on the [pyiron GitHub org](https://github.com/pyiron). The relevant ones:

| Package | Role | We use it? |
|---|---|---|
| `pyiron_base` | Workflow + job mgmt + HDF5/SQL data storage. The orchestration layer. | ✅ via pyiron_atomistics |
| `pyiron_atomistics` | LAMMPS / VASP / GPAW / S/PHI/nX wrappers + ASE compatibility. The atomistic layer. | ✅ this is our integration point |
| `executorlib` | HPC executor (SLURM dispatch) | ✅ pulled in transitively |
| `atomistics` | Lower-level wrapper, pure-python | ✅ transitive |
| `pylammpsmpi`, `vaspparser`, `lammpsparser` | Code/parser bindings | ✅ transitive when running specific codes |
| `pyiron_workflow` | Graph-based workflow framework | ❌ overlaps with our skills/workflows |
| `pyiron_core` | Visual GUI for graph workflows | ❌ pure GUI |

What pyiron *does*:

1. Wraps simulation codes (LAMMPS, VASP, etc.) with one Python interface
2. Manages atomistic *jobs* (create → run → store output → query results)
3. HDF5-backed hierarchical storage of simulation outputs
4. SQL job database (job_id, status, metadata)
5. HPC submission via `executorlib` (SLURM/PBS/SGE)
6. ASE-compatible structure objects
7. Hash-based caching for parameter sweeps

What pyiron is *evolving toward* (per the lead developer at the 2026 symposium): LLM-orchestrated atomistic workflows, smarter conversational sim setup, more "intelligent" parameter sweep navigation. This is good news for us — we'll inherit those features through the integration. It does **not** change PRISM's positioning.

## Where PRISM and pyiron sit

| Layer | Owner |
|---|---|
| Chat LLM, agent loop, tool selection | **PRISM** |
| Embedding-based tool retrieval (Stage 2.1 EmbeddingGemma) | **PRISM** |
| Stateful artifact memory + recall across sessions | **PRISM** |
| Knowledge graph reasoning (MARC27 KG) | **PRISM** |
| Federated DB search (NOMAD, Materials Project, OQMD, ...) | **PRISM** |
| Generic compute broker (RunPod, Lambda, PRISM mesh — training, inference, generic GPU) | **PRISM** |
| Code-writing agent (JAX, custom kernels) | **PRISM** |
| The Fabric (multi-site federated AI compute mesh with RBAC) | **PRISM** |
| Atomistic simulation orchestration (LAMMPS/VASP/GPAW jobs) | **pyiron** |
| HDF5 storage of simulation outputs | **pyiron** |
| Atomistic SLURM/PBS submission | **pyiron** (via executorlib) |
| ASE-compatible structure objects | **pyiron / ASE** |
| Parameter-sweep result aggregation (`pyiron_table`) | **pyiron** |

The crisp test: *"Does this involve actually running LAMMPS / VASP / a DFT code?"* If yes, it's pyiron's turf. If no, it's PRISM's.

## Integration architecture

```
PRISM agent loop
    │
    ▼
sim_tools (app/tools/sim_tools.py)
    │   create_structure, run_simulation, get_job_results, ...
    ▼
PyironBridge  (app/tools/simulation/bridge.py)  ← only place PRISM imports pyiron
    │
    ├── get_project()       → pyiron_atomistics.Project (lazy)
    ├── StructureStore      → in-memory map of struct_id → ASE Atoms
    ├── JobStore            → in-memory map of job_id → pyiron job ref
    └── HPC config          → ~/.prism/hpc_config.json
                                ↓
                              applied to job.server.queue / cores / walltime
                                ↓
                              executorlib submits to SLURM
                                ↓
                              actual LAMMPS / VASP run
                                ↓
                              pyiron writes HDF5 + SQL row (its turf)
                                ↓
                              we read summaries through bridge.jobs.get(job_id)
```

The bridge is **thin**. It holds *references* to pyiron objects, not duplicate data. HDF5 is owned by pyiron. The PRISM artifact store records summary + provenance pointer; if the agent needs the full result, it fetches through the bridge, which reads from pyiron.

## What goes where (decision table)

| Question | Answer |
|---|---|
| "Where does the simulation result actually live?" | pyiron HDF5 file (under the pyiron Project directory) |
| "Where does PRISM remember that the result exists?" | Local artifact store (`~/.prism/artifacts.db`) — stores `job_id` as provenance, plus the summary the LLM saw |
| "How does the agent re-fetch the result later?" | `recall("...")` returns the artifact → `bridge.jobs.get(job_id)` → pyiron reads HDF5 |
| "Where does the SLURM submission happen?" | `executorlib.SlurmClusterExecutor`, configured by `bridge.apply_hpc_config()` |
| "What if I want to run a non-atomistic GPU job (training, ML inference)?" | PRISM compute broker (`compute(action='submit')`), NOT pyiron |
| "What about a parameter sweep DataFrame?" | Use `pyiron_table` — it's the right tool. Don't roll our own aggregator. |
| "Can I write a graph-based workflow?" | If you need atomistic-specific node-graph orchestration, YES, but at the SIM-level (rare, advanced). For PRISM-level workflows, use our YAML skills system. **Do not import `pyiron_workflow` into PRISM tools.** |
| "Which Python versions support pyiron?" | 3.9–3.14 as of pyiron_atomistics 0.6+ / atomistics 0.3.6+ (April 2026) |

## Dependency notes

pyiron's pinned core deps:

```
ase==3.27.0      numpy==2.3.5      scipy==1.17.0      spglib==2.7.0
+ pandas, h5py, pyfileindex, executorlib, structuretoolkit, sqlalchemy
```

Three real risks:

1. **Numpy pin conflict.** pyiron pins `numpy==2.3.5`. If any other PRISM dep needs `numpy>=2.4`, install fails. *Mitigation:* pyiron is in the `[simulation]` extra (opt-in), not core. Users who don't need atomistic sim avoid the conflict entirely.

2. **Heavy install.** Full pyiron + LAMMPS/VASP wrappers + h5py + pandas + scipy + sqlalchemy = several hundred MB. Conda-forge has prebuilt binaries; pip install can hit native-extension compilation hell on macOS / Windows.  
   *Recommendation:* document conda-forge as the supported install path:  
   ```bash
   conda install -c conda-forge pyiron_atomistics
   ```  
   The `pip install prism-platform[simulation]` path works on Linux but is brittle elsewhere.

3. **Native simulation binaries.** LAMMPS / VASP / etc. are separate executables. pyiron's docs cover the resource-directory layout and how to wire your own builds. We don't need to repeat that here, but in PRISM's `prism configure` UX we should at least *check* whether a usable LAMMPS is on `PATH` and warn if not.

## Anti-patterns to flag in code review

- `from pyiron_workflow import ...` anywhere in PRISM → **reject in review**
- `from pyiron_core import ...` → **reject in review**
- A new HDF5 storage layer on the PRISM side that mirrors pyiron's — **reject**, use `bridge.jobs.get(job_id)` and read from pyiron
- A parallel SLURM submitter for atomistic jobs — **reject**, use `bridge.apply_hpc_config(job)` + `executorlib`
- A custom ASE-Atoms-equivalent class — **reject**, use pyiron's

## When pyiron grows LLM features

The pyiron team is moving toward LLM-orchestrated atomistic workflows. When that lands:

- Their LLM features will be **better than what we'd build for atomistic-specific work** because they're solving a narrower problem with deeper domain context.
- We **inherit** those features through the integration with no extra work.
- PRISM's positioning is unchanged — we're a different, broader research workspace (federated DB search, paper reasoning, code-writing agent, multi-site Fabric, KG-grown-from-research, stateful memory across heterogeneous tool outputs).

In other words: pyiron getting smart is a non-event for PRISM strategy. Stay in our lane; let theirs improve under us.

## See also

- `app/tools/simulation/bridge.py` — the only place PRISM imports pyiron
- `app/tools/sim_tools.py` — the user-visible simulation tool surface
- `docs/stateful_tools_2026.md` — how pyiron job results flow into the artifact store
- [pyiron.org](https://pyiron.org/) — main IDE site
- [pyiron_atomistics docs](https://pyiron-atomistics.readthedocs.io/en/latest/README.html)
- [pyiron_base docs](https://pyiron-base.readthedocs.io/en/latest/README.html)
- [pyiron 2019 paper — original architecture](https://pyiron.org/publications/2019/06/01/pyiron.html)
