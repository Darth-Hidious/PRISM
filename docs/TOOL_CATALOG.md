# PRISM Tool Catalog

All tools shipped with the PRISM agent, as registered by `app/plugins/bootstrap.build_full_registry()` (generated 2026-07-03, 72 tools; external MCP servers add more at runtime and are not listed here).

Regenerate with `PRISM_DISABLE_MEMORY=1 python3 scripts/gen_tool_catalog.py`
(from the repo root), or verify the count with:

```bash
PRISM_DISABLE_MEMORY=1 python3 -c "from app.plugins.bootstrap import build_full_registry; \
  r,_,_ = build_full_registry(enable_mcp=False, enable_plugins=False); print(len(list(r.list_tools())))"
```

Status legend: **working** = runs today (notes list per-tool auth/data caveats) · **needs-login** = requires `prism login` to the MARC27 platform · **needs-deps** = blocked on an environment dependency · **stub** = intentionally not live yet.

| Tool | Category | Source | Status | Notes | Description |
|---|---|---|---|---|---|
| `bash_task` | system | local | working |  | Inspect background bash tasks (started by `execute_bash` with `run_in_background=true`). |
| `discover_capabilities` | system | local | working |  | Discover all available PRISM capabilities: search providers, datasets, trained models, pre-trained GNNs, CALPHAD databases, simulation status, lab subscripti... |
| `execute_bash` | system | local | working |  | Execute a local bash command inside the current PRISM project. |
| `execute_python` | system | local | working |  | Execute Python code for data analysis, transformation, plotting, quick calculations, and local inspection of files or datasets. |
| `file` | system | local | working |  | Text file I/O inside the current PRISM project. |
| `session_context` | system | local | working |  | Session context builder — maintains a running structured knowledge base that survives chat history compaction. |
| `show_scratchpad` | system | local | working |  | Print the agent's execution log for this chat session — an ordered list of every tool the agent has called so far, along with the arguments and a short resul... |
| `stop_bash_task` | system | local | working |  | Terminate a background bash task started with `execute_bash run_in_background=true`. |
| `tool_reasoning` | system | local | working |  | KAG-style tool reasoning and planning. |
| `dataset` | data | local | working |  | Dataset I/O + analysis. |
| `materials_search` | data | local | working | keyless OPTIMADE federation | Federated search across every healthy materials database provider (Materials Project, OPTIMADE consortium members, user-installed providers). |
| `plot` | data | local | working |  | Generate a PNG plot of materials data. |
| `query_materials_project` | data | local | working | needs MP_API_KEY or `prism login` (proxy); else points to materials_search | Query Materials Project for detailed material properties — band gap, formation energy, bulk modulus, etc. |
| `list_models` | ml | local | working |  | ML property-prediction models — trained composition models + pre-trained GNN models (M3GNet, MEGNet) for materials property prediction. |
| `list_predictable_properties` | ml | local | working |  | List numeric properties in a dataset that can be predicted with ML. |
| `model_train` | ml | local | working | MP fetch needs MP key or login; local datasets always work | Train a composition→property ML regressor so predict(target='formula') works. |
| `predict` | ml | local | working | formula target needs a trained model — run model_train once | Predict a material property using ML. |
| `calphad` | simulation | sidecar | working | via py3.12 sidecar; needs TDB files in ~/.prism/databases | CALPHAD database catalog + IO operations (read-only / no compute). |
| `calphad_compute` | simulation | sidecar | working | via py3.12 sidecar; needs TDB files; approval-gated | CALPHAD thermodynamic calculations. |
| `check_hpc_queue` | simulation | sidecar | needs-deps | sidecar pyiron blocked on HDF5 | Inspect the HPC queue (SLURM/PBS/SGE) for running and queued atomistic simulation jobs. |
| `list_potentials` | simulation | sidecar | needs-deps | sidecar pyiron blocked on HDF5 | List interatomic potentials (EAM, MEAM, Tersoff, LJ, ...) available in the pyiron LAMMPS potential database for a given element / type. |
| `mace_cancel_job` | simulation | local | working |  | Cancel a queued or running MACE job. |
| `mace_compute_dilute_solute` | simulation | local | working | approval-gated; platform backend needs login | Compute the dilute solute formation/substitution energy for a single solute atom in a matrix supercell. |
| `mace_compute_elastic` | simulation | local | working | approval-gated; platform backend needs login | Compute the second-order elastic-constant tensor via strain-stress linear fits. |
| `mace_estimate_cost` | simulation | local | working |  | Estimate wall time, GPU seconds, and USD cost for a MACE primitive before submitting. |
| `mace_get_cached_structure` | simulation | local | working |  | Resolve a cache:// URI returned by a previous MACE primitive into the inline CIF text plus its provenance bundle path. |
| `mace_get_job` | simulation | local | working |  | Fetch the current status + result (if ready) of a MACE job by id. |
| `mace_list_jobs` | simulation | local | working |  | List MACE jobs in the local job store, filtered by status or tool. |
| `mace_md_equilibrate` | simulation | local | working | approval-gated; platform backend needs login | Run NVT molecular dynamics on a structure at target temperature to equilibrate thermal motion. |
| `mace_phonon_harmonic` | simulation | local | working | approval-gated; platform backend needs login | Compute the harmonic phonon spectrum via the finite-displacement method. |
| `mace_relax_structure` | simulation | local | working | approval-gated; platform backend needs login | Build a supercell from composition + phase and relax it to a local energy minimum using a MACE foundation interatomic potential. |
| `run_convergence_test` | simulation | sidecar | needs-deps | sidecar pyiron blocked on HDF5 | Run an atomistic convergence test: vary one parameter (encut, kpoints, ecutwfc, ...) across N values and return energies for each. |
| `run_workflow` | simulation | sidecar | needs-deps | sidecar pyiron blocked on HDF5 | Run a predefined named workflow on a structure. |
| `sim_job` | simulation | sidecar | needs-deps | sidecar pyiron blocked on HDF5 | Manage atomistic simulation jobs (started by `sim_run`). |
| `sim_run` | simulation | sidecar | needs-deps | sidecar pyiron blocked on HDF5 | Run an atomistic simulation. |
| `structure` | simulation | sidecar | needs-deps | sidecar pyiron blocked on HDF5 | Build, transform, and inspect atomistic crystal structures (via pyiron / ASE). |
| `structure_import` | simulation | local | working |  | Import an external crystal structure into PRISM so simulation tools can use it. |
| `acquire_materials` | skills | local | working | approval-gated | Search and collect materials data from multiple sources (OPTIMADE, Materials Project, OMAT24, literature, patents), normalize records, and save as a named da... |
| `analyze_phases` | skills | local | working | approval-gated | Analyze phase stability using CALPHAD: load a thermodynamic database, calculate the phase diagram, identify stable phases at given conditions, and generate a... |
| `generate_report` | skills | local | working |  | Generate a Markdown or HTML report for a dataset including summary, data preview, property statistics, correlations, ML prediction summary, validation qualit... |
| `materials_discovery` | skills | local | working | approval-gated | End-to-end materials discovery pipeline: acquire data from multiple sources, predict properties with ML, generate visualizations, and compile a report. |
| `plan_simulations` | skills | local | working | approval-gated | Generate a simulation job plan for top candidates in a dataset. |
| `predict_properties` | skills | local | working | approval-gated | Predict material properties for an existing dataset. |
| `select_materials` | skills | local | working |  | Filter and rank materials from a dataset by criteria (min/max thresholds), sort by a property, and save the top N candidates as a new dataset. |
| `cancel_background_research` | research | platform | needs-login |  | Cancel a running background research run. |
| `check_background_research` | research | platform | needs-login |  | Check a background research run. |
| `list_background_research` | research | platform | needs-login |  | List recent background research runs with status. |
| `prior_art_search` | research | local | working | patents need LENS_API_TOKEN | Federated prior-art search across scientific literature (arXiv, Semantic Scholar) AND patents (Lens.org). |
| `research` | research | platform | needs-login | money-spending, approval-gated | Run a deep research query against the MARC27 RLM (Recursive Language Model) engine. |
| `start_background_research` | research | platform | needs-login | money-spending, approval-gated | Launch a SEPARATE long-running research agent on the MARC27 platform (frontier model with knowledge-graph + web access). |
| `web` | research | local | working | Firecrawl key optional; DuckDuckGo fallback | Open-web access. |
| `knowledge` | knowledge | platform | needs-login |  | MARC27 Knowledge Plane — graph + semantic search over the live knowledge service (200K+ nodes, 6M+ edges, 6K+ vector embeddings, 200+ corpora catalog). |
| `knowledge_write` | knowledge | platform | needs-login |  | WRITE side of the MARC27 Knowledge Service — closes the agent's read/write asymmetry. |
| `compute` | compute | platform | needs-login |  | MARC27 Compute Broker — read-only and idempotent operations across providers (PRISM mesh nodes, RunPod, Lambda). |
| `compute_submit` | compute | platform | needs-login | money-spending, approval-gated | Dispatch a real containerized GPU/CPU job to the MARC27 Compute Broker. |
| `agent_capabilities` | platform | platform | needs-login |  | Ask the MARC27 platform to describe itself. |
| `billing_balance` | platform | platform | needs-login |  | Read MARC27 platform billing state. |
| `mcp_services` | platform | platform | needs-login |  | Read MARC27 platform-hosted MCP service instances (Model Context Protocol servers running in the platform, not locally). |
| `mcp_services_invoke` | platform | platform | needs-login | approval-gated | Invoke or scale a MARC27 platform-hosted MCP service instance. |
| `platform_jobs` | platform | platform | needs-login |  | Read/cancel MARC27 platform jobs. |
| `platform_jobs_submit` | platform | platform | needs-login | money-spending, approval-gated | Submit a new job to the MARC27 platform. |
| `platform_workflows` | platform | platform | needs-login |  | Read/cancel MARC27 platform workflows. |
| `platform_workflows_run` | platform | platform | needs-login | money-spending, approval-gated | Start a workflow or register a new workflow spec. |
| `policy_evaluate` | platform | platform | needs-login |  | Ask the MARC27 platform's policy engine whether a given action on a given resource is permitted for the current user. |
| `usage_status` | platform | platform | needs-login |  | Read current MARC27 platform usage telemetry for the user or a specific project. |
| `mesh_health` | mesh | platform | needs-login |  | Quick health check for the mesh subsystem. |
| `mesh_peers` | mesh | platform | needs-login |  | List all known mesh peers connected to this PRISM node. |
| `mesh_publish` | mesh | platform | needs-login | approval-gated | Publish a local dataset to the PRISM mesh so other nodes can discover and subscribe to it. |
| `mesh_subscribe` | mesh | platform | needs-login | approval-gated | Subscribe to a dataset published by another PRISM node on the mesh. |
| `mesh_subscriptions` | mesh | platform | needs-login |  | Show all datasets this node has published and all datasets it is currently subscribed to from other nodes. |
| `mesh_unsubscribe` | mesh | platform | needs-login | approval-gated | Unsubscribe from a dataset published by another node. |
| `labs` | labs | local | stub | browse/info real; submit NOT live (all services coming_soon) | MARC27 Premium Labs marketplace catalog — autonomous robotic synthesis (A-Labs), design-for-manufacturing assessment, hosted DFT/QE/CP2K, real quantum hardwa... |

## Notes

- **Approval gates**: tools marked approval-gated prompt the user before running because they spend compute/money or mutate shared state.
- **Science sidecar**: `structure`/`sim_*`/`calphad*` execute in a separate Python 3.12 venv (`~/.prism/venv-sci`, auto-provisioned) because pyiron/pycalphad don't install on the main interpreter. pycalphad works there; pyiron is currently blocked on an HDF5 build failure.
- **Labs**: the `labs` marketplace catalog is browsable, but job submission is not live for any service yet — the tool says so itself.
- **MACE**: composition inputs are integer atom counts (10 supported elements: Al, Fe, Hf, Mo, Nb, Ta, Ti, V, W, Zr). Structures can also be supplied via `structure_import` → `cache_ref`.
