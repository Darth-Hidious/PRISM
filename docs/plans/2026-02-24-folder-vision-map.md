# PRISM Folder-to-Vision Map

> What each folder does, what it owns in the pipeline, what exists, what's missing.
> **Updated:** 2026-02-25 — All CLI commands complete. 861 tests passing.

---

## The Pipeline (Your North Star)

```
USER PROMPT: "Find alloys with W and Rh that are stable, and phase stability"
  |
  v
1. DATA ACQUISITION         ─── fetch from everywhere
  |
  v
2. CURATION                 ─── deduplicate, normalize, store as DB/CSV
  |
  v
3. ML PREDICTION            ─── ask user what to predict, train, add columns
  |
  v
4. VISUALIZATION            ─── charts, correlation matrices, phase diagrams
  |
  v
5. LLM ANALYSIS / SEARCH    ─── feed to agent OR return directly
  |
  v
6. SIMULATION PLANNING      ─── CALPHAD, DFT, VASP, pyiron, user DBs
  |
  v
7. DOWNSTREAM SELECTION     ─── filtered candidate list
  |
  v
8. REVIEW + REPORT          ─── second agent checks graphs, makes PDF
  |
  v
  [FUTURE: GenAI → CALPHAD feedback → DFT feedback → DfM → FEM → Robots]
```

---

## Folder Map

### `app/cli/` — THE REPL & COMMANDS

**What it does:** Everything the user sees and touches. The Claude Code-like REPL, all Click commands, card rendering, prompt handling.

| Subfolder | Owns | Pipeline Stage |
|-----------|------|---------------|
| `cli/main.py` | 77KB Click monolith: `prism`, `prism run`, `prism serve`, `prism search`, `prism ask`, `prism data`, `prism predict`, `prism sim`, `prism calphad`, `prism plugin`, `prism update`, `prism configure` | Entry point for ALL stages |
| `cli/tui/app.py` | `AgentREPL` class — the REPL event loop | Orchestrates stages 1-8 interactively |
| `cli/tui/cards.py` | Typed output panels (input, output, tool, error, metrics, calphad, validation, results, plot, approval, plan) | Renders results from every stage |
| `cli/tui/theme.py` | All colors, icons, borders, indicators | Visual identity |
| `cli/tui/prompt.py` | prompt_toolkit input, approval dialogs, plan confirmation | Informed consent (tool approval) |
| `cli/tui/stream.py` | Bridges agent events to card renderers | Real-time display of multi-step execution |
| `cli/tui/welcome.py` | Crystal mascot, capability detection, version display | First thing user sees |
| `cli/tui/status.py` | Footer: modes, git status, message count | Always-visible context |
| `cli/tui/spinner.py` | Loading animation during tool execution | Stage progress feedback |
| `cli/slash/registry.py` | `/help`, `/tools`, `/skills`, `/scratchpad`, `/approve-all`, `/export`, etc. | REPL commands |
| `cli/slash/handlers.py` | Handler functions for each slash command | Command dispatch |

**EXISTS:** Full REPL with card-based output, approval flow, plan-then-execute, scratchpad, streaming. `cli/main.py` split into 17 command modules in `app/commands/`.
**MISSING:** Interactive ML property selection flow (stage 3) not wired through REPL yet.

---

### `app/agent/` — THE BRAIN (TAOR Loop)

**What it does:** Think-Act-Observe-Repeat. Provider-agnostic agent engine. Zero rendering code. This is the orchestrator that drives stages 1-8 autonomously.

| File | Owns | Pipeline Stage |
|------|------|---------------|
| `core.py` | `AgentCore`: TAOR loop, tool dispatch, approval callback, scratchpad injection, max iterations, doom loop detection, ResultStore for large results | ALL stages — the agent decides which tools to call and in what order |
| `autonomous.py` | `run_autonomous()`: headless agent execution for `prism run "goal"` | Batch mode for ALL stages |
| `factory.py` | `create_backend()`: reads settings.json + env vars, auto-detects provider | Infrastructure |
| `models.py` | `ModelConfig` registry: 18 models across 4 providers (Anthropic, OpenAI, Google, Zhipu) with pricing, context windows, capabilities | Infrastructure |
| `memory.py` | Session save/load, conversation history | Persistence |
| `events.py` | `TextDelta`, `ToolCallStart`, `ToolCallResult`, `TurnComplete`, `ToolApprovalRequest`, `UsageInfo` | Event protocol between agent and REPL |
| `scratchpad.py` | Append-only execution log: what was done, by which tool, with what result | **Reporting backbone** — stages 1-8 all log here |
| `backends/anthropic_backend.py` | Claude API adapter with prompt caching, token tracking | LLM calls |
| `backends/openai_backend.py` | OpenAI API adapter with token tracking | LLM calls |
| `backends/base.py` | `Backend` ABC: `complete()`, `complete_stream()`, retry with exponential backoff | Interface |

**EXISTS:** Full TAOR loop with streaming, approval gating, scratchpad logging, multi-turn execution, plan-then-execute. Model config registry (18 models, 4 providers). Prompt caching (Anthropic). Token & cost tracking. Retry with exponential backoff. Doom loop detection. ResultStore + peek_result for large results. Reads settings from unified settings.json.
**MISSING:** Multi-agent orchestration (review agent as second agent). Feedback loops between stages (e.g., CALPHAD result feeds back to selection). Currently single-agent with `_post_tool_hook` for basic feedback.

---

### `app/tools/` — ATOMIC ACTIONS

**What it does:** Individual tools the agent can call. Each tool does ONE thing. The agent composes them.

| File | Owns | Pipeline Stage |
|------|------|---------------|
| `base.py` | `Tool` dataclass, `ToolRegistry`, Anthropic/OpenAI format export | Infrastructure |
| `data.py` | `search_optimade()`, `query_materials_project()`, `import_dataset()`, `export_results_csv()` | **Stage 1** (acquisition), **Stage 2** (curation/export) |
| `search.py` | `literature_search()` (arXiv + Semantic Scholar), `patent_search()` (Lens.org) | **Stage 1** (literature & patents) |
| `prediction.py` | `predict_properties()`, `train_model()`, `list_predictable_properties()`, `get_feature_importance()`, `get_correlation_matrix()` | **Stage 3** (ML prediction) |
| `visualization.py` | `plot_scatter()`, `plot_heatmap()`, `compare_materials()`, `visualize_structure()` | **Stage 4** (visualization) |
| `property_selection.py` | `select_by_property()`, `rank_candidates()` | **Stage 7** (downstream selection) |
| `simulation.py` | `create_structure()`, `submit_dft_job()`, `submit_md_job()`, `check_job_status()`, `extract_results()` | **Stage 6** (DFT/MD simulation) |
| `calphad.py` | `phase_diagram()`, `equilibrium()`, `gibbs_energy()`, `list_databases()`, `import_database()` | **Stage 6** (CALPHAD thermodynamics) |
| `system.py` | `read_file()`, `write_file()`, `web_search()`, `show_scratchpad()`, `execute_python()` | Utility (all stages) |
| `capabilities.py` | `discover_capabilities()`: aggregates all subsystems (search, models, databases, labs, plugins) | Infrastructure — auto-injected into system prompt |
| `labs.py` | `list_lab_services()`, `get_lab_service_info()`, `check_lab_subscriptions()`, `submit_lab_job()` | Premium marketplace services |
| `pretrained.py` | Pre-trained GNN models (M3GNet, MEGNet via MatGL) | **Stage 3** (GNN prediction) |

**EXISTS:** 26+ tools for every stage. Full Python code execution (with approval). Unified capability discovery auto-injected into system prompt. Labs marketplace tools. Pre-trained GNN tools.
**MISSING:** Interactive property selection tool (asks user which properties to predict). ThermoCalc connector tool. DFT result parser tools. Structure generation tools for GenAI materials.

---

### `app/skills/` — MULTI-STEP WORKFLOWS

**What it does:** Skills are sequences of tool calls that form a pipeline stage. The agent can call a skill as if it were a single tool.

| File | Owns | Pipeline Stage |
|------|------|---------------|
| `acquisition.py` | `acquire_materials()`: queries OPTIMADE + MP + OMAT24, merges results | **Stage 1** |
| `prediction.py` | `predict_properties()`: load dataset, train model, predict, save | **Stage 3** |
| `visualization.py` | `visualize_dataset()`: auto-detect columns, generate plots | **Stage 4** |
| `selection.py` | `select_materials()`: filter by property ranges, rank candidates | **Stage 7** |
| `simulation_plan.py` | `plan_simulations()`: auto-route CALPHAD vs DFT vs MD | **Stage 6** |
| `phase_analysis.py` | `analyze_phases()`: CALPHAD phase stability queries | **Stage 6** |
| `validation.py` | `validate_dataset()`: outlier detection, physical constraints, completeness | **Stage 8** (rule-based review) |
| `review.py` | `review_dataset()`: comprehensive QA with structured findings | **Stage 8** (review) |
| `reporting.py` | `generate_report()`: Markdown/HTML/PDF with data summary | **Stage 8** (report) |
| `discovery.py` | `materials_discovery()`: **THE MASTER SKILL** — acquire -> predict -> visualize -> report | **Stages 1-4 + 8** end-to-end |
| `registry.py` | `load_builtin_skills()`: loads all 10 skills | Infrastructure |
| `base.py` | `Skill`, `SkillStep`, `SkillRegistry` dataclasses | Infrastructure |

**EXISTS:** 10 skills covering stages 1-4, 6-8. Master discovery pipeline.
**MISSING:** Explicit stage 7 integration in master pipeline (selection happens but isn't a named step). Review agent as separate LLM call (currently rule-based only). Interactive ML selection skill. GenAI materials generation skill. Feedback loop skills (CALPHAD result -> re-select -> re-simulate).

---

### `app/data/` — DATA SOURCES & STORAGE

**What it does:** Fetches data from external sources, normalizes it, stores it locally. This is the plumbing behind stage 1 and 2.

| File | Owns | Pipeline Stage |
|------|------|---------------|
| `base_collector.py` | `DataCollector` ABC, `CollectorRegistry`, default registry factory | **Stage 1** infrastructure |
| `collector.py` | `OPTIMADECollector`, `MPCollector` | **Stage 1** (OPTIMADE + MP) |
| `omat24_collector.py` | `OMAT24Collector`: HuggingFace streaming | **Stage 1** (OMAT24) |
| `literature_collector.py` | `LiteratureCollector`: arXiv + Semantic Scholar | **Stage 1** (papers) |
| `patent_collector.py` | `PatentCollector`: Lens.org | **Stage 1** (patents) |
| `normalizer.py` | Formula normalization, element extraction, property name mapping | **Stage 2** (curation) |
| `store.py` | `DataStore`: Parquet + metadata JSON, in-memory tabular data | **Stage 2** (storage) |

**EXISTS:** 5 collectors, normalizer, Parquet store.
**MISSING:** LLM-based graph knowledge collector (future). Deep literature mining (future). User preference for DB vs CSV (setup step). Schema enforcement on import. Deduplication across sources beyond source_id.

---

### `app/ml/` — MACHINE LEARNING

**What it does:** Feature engineering, model training, prediction, interpretability. Drives stage 3.

| File | Owns | Pipeline Stage |
|------|------|---------------|
| `algorithm_registry.py` | `AlgorithmRegistry`: RF, XGBoost, LightGBM, GBR, Linear — plugin-extensible | **Stage 3** |
| `features.py` | Magpie elemental features, ACSF, composition-based extraction | **Stage 3** |
| `predictor.py` | `MaterialsPredictor`: train/predict wrapper | **Stage 3** |
| `trainer.py` | Train-val-test, Optuna hyperparams, cross-validation | **Stage 3** |
| `registry.py` | `ModelRegistry`: per-property model caching | **Stage 3** |
| `viz.py` | Feature importance plots, confidence bounds | **Stage 4** (ML-specific viz) |

**EXISTS:** 5 algorithms (RF, XGBoost, LightGBM, GBR, Linear), Magpie features (matminer 132 features + 22 built-in fallback), Optuna tuning, model caching, pre-trained GNNs (M3GNet, MEGNet via MatGL), plugin-extensible algorithm registry.
**MISSING:** Custom GNN models (future). Surrogate models (future). GFlowNet samplers (future). Uncertainty quantification. Active learning loops. GenAI material generators. The hook to add these is `AlgorithmRegistry.register()`.

---

### `app/simulation/` — ATOMISTIC SIMULATION

**What it does:** Bridges to pyiron (DFT, MD) and pycalphad (thermodynamics). Drives stage 6.

| File | Owns | Pipeline Stage |
|------|------|---------------|
| `bridge.py` | pyiron: `StructureStore`, `JobStore`, HPC config, availability check | **Stage 6** (DFT/MD) |
| `calphad_bridge.py` | pycalphad: database loaders, phase diagram compute, equilibrium | **Stage 6** (CALPHAD) |

**EXISTS:** pyiron bridge (optional), pycalphad bridge (optional).
**MISSING:** ThermoCalc connector (TC-Python). VASP input generator. DFT result parsers. User-supplied database connectors. Job monitoring. Compute resource detection ("what compute is available").

---

### `app/plugins/` — EXTENSIBILITY

**What it does:** Loads external tools, skills, collectors, and algorithms. This is how users add their own capabilities.

| File | Owns | Pipeline Stage |
|------|------|---------------|
| `registry.py` | `PluginRegistry`: facade over tool/skill/provider/agent/collector/algorithm registries | Infrastructure |
| `bootstrap.py` | `build_full_registry()`: one-call loader for everything (tools, skills, providers, agents, labs, capabilities) | Infrastructure |
| `loader.py` | `discover_all_plugins()`: pip entry-points + `~/.prism/plugins/` scan, tracks `_loaded_plugins` | Infrastructure |
| `catalog.json` | Unified plugin catalog: providers (mp_native, aflow_native, mpds, omat24), agents, bundles | Platform |
| `labs_catalog.json` | Premium marketplace: 9 services across 6 categories (A-Labs, Cloud DFT, Quantum, DfM, Synchrotron, HT Screening) | Marketplace |
| `thermocalc.py` | ThermoCalc plugin skeleton | **Stage 6** (future) |

**EXISTS:** 7 plugin types (tool, skill, provider, agent, algorithm, collector, bundle). Plugin discovery from entry-points and local directory. Unified catalog. Labs marketplace with subscriptions. Provider + agent registries.
**MISSING:** `prism plugin install <url>` (download and install). Plugin marketplace API backend. Plugin versioning. Plugin sandboxing.

---

### `app/validation/` — DATA QUALITY

**What it does:** Rule-based checks on data. First half of stage 8 (rules before LLM review).

| File | Owns | Pipeline Stage |
|------|------|---------------|
| `rules.py` | IQR outlier detection, physical constraints, completeness scoring, custom rules | **Stage 8** (rule-based review) |

**EXISTS:** Outlier detection, constraint checking, completeness scoring.
**MISSING:** Domain-specific material rules (e.g., "band gap must be > 0 for semiconductors"). Cross-property consistency checks. Thermodynamic feasibility rules.

---

### `app/search/` — FEDERATED SEARCH ENGINE (v2.5 NEW)

**What it does:** Queries 40+ materials databases via OPTIMADE, fuses results, caches, circuit-breaks. Full docs: `docs/search.md`.

| File/Dir | Owns | Pipeline Stage |
|----------|------|---------------|
| `engine.py` | `SearchEngine`: fan-out, health checks, fusion, caching | **Stage 1** (orchestrator) |
| `query.py` | `MaterialSearchQuery`, `PropertyRange` (Pydantic v2) | **Stage 1** (query model) |
| `result.py` | `Material`, `PropertyValue`, `SearchResult`, `ProviderQueryLog` | **Stage 1** (result model) |
| `translator.py` | `QueryTranslator.to_optimade()` — query to OPTIMADE filter string | **Stage 1** (query translation) |
| `fusion.py` | `fuse_materials()` — dedup + merge across providers | **Stage 2** (fusion) |
| ~~`marketplace.json`~~ | Moved to `app/plugins/catalog.json` — unified plugin catalog | **Stage 1** (platform providers) |
| `providers/discovery.py` | 2-hop OPTIMADE auto-discovery + cache + overrides + Layer 3 | **Stage 1** (provider discovery) |
| `providers/registry.py` | `ProviderRegistry`, `build_registry()` (3-layer merge) | **Stage 1** (provider management) |
| `providers/endpoint.py` | `ProviderEndpoint` Pydantic model (auth, behavior, capabilities) | **Stage 1** (endpoint config) |
| `providers/provider_overrides.json` | Layer 2: tiers, capabilities, quirks, URL corrections | **Stage 1** (overrides) |
| `providers/base.py` | `Provider` ABC, `ProviderCapabilities` | **Stage 1** (interface) |
| `providers/optimade.py` | `OptimadeProvider` — OPTIMADE filter queries | **Stage 1** (OPTIMADE impl) |
| `providers/materials_project.py` | `MaterialsProjectProvider` — MP native API | **Stage 1** (MP native impl) |
| `cache/engine.py` | `SearchCache` — in-memory + disk, query hash keying, TTL | **Stage 1** (caching) |
| `resilience/circuit_breaker.py` | `HealthManager`, `ProviderHealth` — per-provider circuit breaker | **Stage 1** (resilience) |

**EXISTS:** Complete federated search: auto-discovery, 3-layer registry, query translation, async fan-out, result fusion, caching, circuit breakers, 40+ providers live. Catalog moved to unified `app/plugins/catalog.json`.
**MISSING:** `AflowProvider` (AFLUX native API — catalog entry exists, needs provider impl). `DatasetProvider` base class (for OMAT24 etc). Marketplace API backend (currently reads catalog.json locally).

---

### `app/config/` — SETTINGS & PREFERENCES

**What it does:** Branding, provider configs, user preferences, app settings.

| File | Owns | Pipeline Stage |
|------|------|---------------|
| `settings.py` | `MAX_RESULTS_PER_PROVIDER`, `MAX_FILTER_ATTEMPTS`, env path resolution | Infrastructure |
| `settings_schema.py` | **Unified settings.json system** — `PrismSettings` with agent/search/output/compute/ml/updates/permissions sections. Two-tier: `~/.prism/settings.json` (global) + `.prism/settings.json` (project). Merge: defaults < global < project < env vars. | Infrastructure |
| `branding.py` | `PRISM_BRAND` dict | UI |
| `preferences.py` | `UserPreferences`: legacy workflow prefs, onboarding flow. Syncs to settings.json on save. | Setup / infrastructure |

**EXISTS:** Unified settings.json (Claude Code pattern). Two-tier global + project settings. Factory, AgentCore, CLI startup all read from settings. User preferences with onboarding. Env var overrides.
**MISSING:** ThermoCalc database paths config. Per-plugin settings.

---

### `app/db/` — LOCAL DATABASE

**What it does:** Optional SQLAlchemy ORM. Most data flows through `DataStore` (Parquet), but this provides a SQL layer.

| File | Owns | Pipeline Stage |
|------|------|---------------|
| `database.py` | SQLAlchemy engine, `Base`, session factory | **Stage 2** (optional SQL storage) |
| `models.py` | `Material` ORM model | **Stage 2** |

**EXISTS:** Basic ORM.
**MISSING:** Schema evolution. Multi-table relationships. Query interface for agent. This layer is underused — most data goes through `DataStore`.

---

### `app/commands/` — CLI SUBCOMMAND MODULES (v2.5 COMPLETE)

**What it does:** Click command groups extracted from the 77KB `cli/main.py`. As of v2.5, all major commands live here.

| File | Owns | Status |
|------|------|--------|
| `search.py` | `prism search --elements --formula --band-gap-min --providers --refresh` | **DONE** (v2.5) |
| `ask.py` | ~~`prism ask`~~ — **DEPRECATED**, redirects to `prism run` | **DEPRECATED** (v2.5.1) |
| `run.py` | `prism run "goal" --agent --provider --model --confirm` — autonomous agent | **DONE** (v2.5.1) |
| `serve.py` | `prism serve --host --generate-nginx` — MCP server mode | **DONE** (v2.5) |
| `data.py` | `prism data collect/import/status` + execute_python tool | **DONE** (v2.5.1) |
| `predict.py` | `prism predict --dataset --target --algorithm` — matminer + GNN | **DONE** (v2.5.1) |
| `model.py` | `prism model train/status/calphad` — CALPHAD subgroup | **DONE** (v2.5.1) |
| `optimade.py` | `prism optimade` — direct OPTIMADE queries | **DONE** (v2.5) |
| `sim.py` | `prism sim init/status/jobs` — DFT/MD simulation | **DONE** (v2.5) |
| `calphad.py` | `prism calphad` — deprecated alias for `prism model calphad` | **DEPRECATED** (v2.5.1) |
| `plugin.py` | `prism plugin list/init` | **DONE** (v2.5) |
| `labs.py` | `prism labs list/info/status/subscribe` — premium marketplace | **DONE** (v2.5.1) |
| `configure.py` | `prism configure --show --anthropic-key --openai-key --model --reset` | **DONE** (v2.5.1) |
| `setup.py` | `prism setup` — workflow prefs wizard, syncs to settings.json | **DONE** (v2.5.1) |
| `update.py` | `prism update --check-only` — auto-detect install method | **DONE** (v2.5.1) |
| `mcp.py` | `prism mcp` — MCP server management | **DONE** (v2.5) |
| `advanced.py` | `prism advanced` — dev/debug commands | **DONE** (v2.5) |
| `docs.py` | `prism docs` — documentation browser/generator | **DONE** (v2.5) |

**EXISTS:** All 18 command modules complete (17 + labs). All handler logic in `app/commands/`. `cli/main.py` only has Click group wiring.
**REMAINING:** None — all commands complete.

---

### Other Top-Level `app/` Files

| File | Owns | Pipeline Stage |
|------|------|---------------|
| `llm.py` | `LLMService` + 4 provider implementations | Infrastructure (LLM calls outside agent) |
| ~~`mcp.py`~~ | ~~`ModelContext`, `AdaptiveOptimadeFilter`~~ | **DELETED** (v2.5.1 — ask deprecated) |
| `mcp_client.py` | External MCP server discovery | Infrastructure (tool loading) |
| `mcp_server.py` | `prism serve` — exposes tools/skills via FastMCP, `prism://capabilities` resource | Infrastructure (MCP hosting) |
| `prompts.py` | System prompts: `ROUTER_PROMPT`, `SUMMARIZATION_PROMPT`, etc. | Agent instructions |
| `update.py` | Version checker (PyPI + GitHub, configurable cache TTL), install method detection (uv/pipx/pip) | Infrastructure |

---

## Pipeline Coverage Summary

| Stage | Folder(s) | Status |
|-------|-----------|--------|
| **1. Data Acquisition** | `app/search/` (federated), `app/data/` (collectors), `app/tools/data.py`, `app/tools/search.py`, `app/skills/acquisition.py` | **Complete (v2.5)** — Federated search engine (40+ OPTIMADE providers, MP native, auto-discovery, 3-layer registry, circuit breakers, caching). Also: OMAT24, literature, patents collectors |
| **2. Curation** | `app/data/normalizer.py`, `app/data/store.py`, `app/db/` | **Exists (basic)** — normalize + Parquet store. Missing: user pref for DB/CSV, schema enforcement |
| **3. ML Prediction** | `app/ml/`, `app/tools/prediction.py`, `app/skills/prediction.py` | **Complete (v2.5.1)** — 5 algorithms, matminer (132 Magpie features) + built-in fallback (22), pre-trained GNNs (M3GNet, MEGNet). Missing: interactive property selection, custom GNNs, surrogates |
| **4. Visualization** | `app/tools/visualization.py`, `app/skills/visualization.py`, `app/ml/viz.py` | **Exists** — scatter, heatmap, structure. Missing: phase diagrams in viz, correlation matrices |
| **5. LLM Analysis** | `app/agent/core.py` (TAOR loop) | **Exists** — agent feeds results to LLM naturally |
| **6. Simulation** | `app/simulation/`, `app/tools/simulation.py`, `app/tools/calphad.py`, `app/skills/simulation_plan.py`, `app/skills/phase_analysis.py` | **Exists (optional)** — pyiron + pycalphad. Missing: ThermoCalc, VASP, user DB connectors, compute detection |
| **7. Selection** | `app/tools/property_selection.py`, `app/skills/selection.py` | **Exists** — range filter + rank. Missing: multi-objective Pareto, downstream list formatting |
| **8. Review + Report** | `app/validation/`, `app/skills/validation.py`, `app/skills/review.py`, `app/skills/reporting.py`, `app/agent/scratchpad.py` | **Exists** — rules + Markdown/PDF report + scratchpad. Missing: LLM review agent, automated figure captioning |
| **Informed Consent** | `app/cli/tui/prompt.py`, `app/tools/base.py` (`requires_approval`) | **Exists** — tool-level approval, `--dangerously-accept-all`, `/approve-all` |
| **What Was Done (Reporting)** | `app/agent/scratchpad.py` | **Exists** — append-only log, auto-populated after each tool call |
| **Multi-Step Execution** | `app/agent/core.py` (TAOR), `app/skills/discovery.py` (master pipeline) | **Exists** — plan-then-execute, multi-turn, scratchpad |
| **Plugin Extensibility** | `app/plugins/`, `app/ml/algorithm_registry.py` | **Complete (v2.5.1)** — 7 plugin types, entry-points + local dir, unified catalog, labs marketplace. Missing: `prism plugin install` |
| **One-Command Install** | `install.sh`, `pyproject.toml` | **Exists** — curl + pipx/uv, --upgrade flag, install method auto-detection. Missing: brew formula, PyPI auto-publish |
| **Settings System** | `app/config/settings_schema.py` | **Complete (v2.5.1)** — Two-tier settings.json (global + project), Claude Code pattern, env var overrides |
| **Capability Discovery** | `app/tools/capabilities.py` | **Complete (v2.5.1)** — Auto-discovers all subsystems, injected into agent system prompt |
| **Labs Marketplace** | `app/commands/labs.py`, `app/plugins/labs_catalog.json` | **Complete (v2.5.1)** — 9 services, 6 categories, subscription management |

---

## What's Buildable Now vs Future Updates

### Done in v2.5 / v2.5.1
- ~~Split `cli/main.py` into `cli/commands/`~~ — **18 command modules extracted**
- ~~Federated search engine~~ — **40+ providers, auto-discovery, caching, circuit breakers**
- ~~`prism search` command with full flags~~ — **elements, formula, band-gap, providers, --refresh**
- ~~Provider auto-discovery from OPTIMADE consortium~~ — **2-hop chain, weekly cache**
- ~~Layer 3 marketplace catalog~~ — **mp_native, aflow_native, mpds, omat24**
- ~~Model config registry~~ — **18 models, 4 providers, pricing, prompt caching**
- ~~Token & cost tracking~~ — **per-turn and cumulative usage**
- ~~Retry with exponential backoff~~ — **429, 500, 502, 503 with Retry-After**
- ~~Doom loop detection~~ — **3 identical failures triggers system warning**
- ~~Large result handling~~ — **ResultStore + peek_result tool**
- ~~ML prediction upgrade~~ — **matminer features, pre-trained GNNs (M3GNet, MEGNet)**
- ~~CALPHAD under model~~ — **`prism model calphad` with deprecated alias**
- ~~Premium labs marketplace~~ — **9 services, 6 categories, subscription management**
- ~~Capability auto-discovery~~ — **system prompt injection of all available resources**
- ~~Unified settings.json~~ — **global + project tier, Claude Code pattern**
- ~~Configure upgrade~~ — **all API keys, --model, --show, --reset**
- ~~Update upgrade~~ — **auto-detect install method (uv/pipx/pip), --check-only**
- ~~Python code execution~~ — **execute_python tool with approval gate**
- ~~Legal documentation~~ — **PRISM-CLI-Description.md for IP/legal**

### Buildable Now (no new external dependencies)
- Interactive ML property selection in REPL (wire `list_predictable_properties` -> user choice -> `predict`)
- LLM review agent (second `AgentCore` call in review skill)
- `AflowProvider` — AFLUX native API adapter (marketplace entry exists, needs provider impl)
- Expose OMAT24 collector as agent tool
- Multi-objective Pareto selection
- Correlation matrix visualization
- `prism plugin install` command
- Domain-specific validation rules
- Better downstream candidate list formatting
- Automated figure captioning (LLM describes charts)
- Marketplace API backend (replace local `marketplace.json` with platform API call)

### Next Updates (new deps or APIs)
- Custom GNN models (`torch`, `torch_geometric`)
- Surrogate models (Gaussian Process, neural network)
- GFlowNet samplers (`gflownet` package)
- ThermoCalc connector (`tc-python` — commercial)
- VASP input/output parsing (`pymatgen.io.vasp`)
- GenAI material generation (diffusion models, VAEs)
- Active learning loops
- Compute resource auto-detection

### Future (requires external systems)
- LLM graph knowledge
- Deep literature mining (full-text extraction)
- DfM (Design for Manufacturing)
- FEM orchestrators
- Process robot integration (A-Lab style)
- Multi-agent orchestration (supervisor + workers)
- Federated compute

---

## MARC27 SDK Integration (`marc27-sdk`)

PRISM's platform connector. A separate Python package (`pip install marc27-sdk`) that talks to `platform.marc27.com/api/v1`. Thin REST client — no logic, just auth + typed responses.

**Package:** `marc27-sdk` (repo: `marc27-sdk/`, design docs there)
**Install:** `pip install marc27-sdk` (will be an optional PRISM dependency)
**Import:** `from marc27 import PlatformClient`

### What it gives PRISM

| Capability | SDK Method | PRISM Integration Point |
|------------|-----------|------------------------|
| Device login (`prism login`) | `client.login()` | `app/cli/main.py` — rewrite login command |
| Managed LLM keys | `client.get_llm_key()` | `app/agent/factory.py` — platform-first, local-fallback |
| Marketplace search | `client.marketplace.search()` | `app/cli/main.py` — `prism marketplace` commands |
| Plugin install | `client.marketplace.install()` | `app/plugins/loader.py` — download + register |
| Model download | `client.marketplace.download()` | `app/ml/` — load models from marketplace |
| HPC job submission | `client.compute.submit_job()` | `app/tools/simulation.py` — new tool |
| Lab booking | `client.labs.book()` | `app/tools/` — new tool |
| Usage metering | `client.projects.get_usage()` | `app/cli/main.py` — `prism usage` command |
| Org/project switching | `client.switch_project()` | `app/cli/main.py` — `prism projects` command |

### How PRISM uses it

```python
# In app/agent/factory.py — managed key takes priority over local env
try:
    from marc27 import PlatformClient
    client = PlatformClient()         # reads ~/.prism/credentials.json
    key = client.get_llm_key()        # managed key for active project
    if key:
        return backend_from_key(key)  # use platform-managed provider
except Exception:
    pass                              # fall back to ANTHROPIC_API_KEY etc.
```

### SDK modules (for reference)

```
src/marc27/
├── client.py            ← PlatformClient (the one class PRISM imports)
├── auth.py              ← device auth flow (like gh auth login)
├── credentials.py       ← ~/.prism/credentials.json read/write
├── models.py            ← Pydantic: User, Org, Project, ManagedKey, Resource, Job, Booking
├── exceptions.py        ← AuthError, QuotaExceededError, NotFoundError, etc.
└── api/
    ├── base.py          ← httpx client, auth header injection, retry
    ├── marketplace.py   ← search, install, download, publish
    ├── projects.py      ← llm key, usage, resources
    ├── orgs.py          ← org CRUD, members
    ├── compute.py       ← HPC job submit/status/cancel
    └── labs.py          ← booking, availability, results
```

### Key facts
- **Thin wrapper** — every method = 1 API endpoint, no business logic
- **Auth is transparent** — auto-reads credentials, injects JWT/API-key headers
- **Identity headers** — `X-User-ID`, `X-Project-ID` on every request (audit trail)
- **Typed errors** — `AuthError`, `QuotaExceededError`, `NotFoundError` (PRISM catches these)
- **Platform-first, local-fallback** — if logged in, use managed keys; if not, existing `.env` flow works
- **Detailed design:** `marc27-sdk/docs/plans/2026-02-25-sdk-detailed-plan.md`
