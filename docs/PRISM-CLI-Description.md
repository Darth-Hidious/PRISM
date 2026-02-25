# PRISM CLI — Product Description

**Product:** PRISM (Platform for Research in Intelligent and Sustainable Materials)
**Version:** 2.5.0
**Developer:** MARC27
**License:** Dual — MIT (core plumbing) + MARC27 Source-Available (AI orchestration)

---

## 1. Product Overview

PRISM is an AI-native autonomous materials discovery platform distributed as a
command-line interface (CLI) application for Python 3.11+. It combines large
language model (LLM) orchestration, federated materials database access, machine
learning property prediction, thermodynamic modelling, and atomistic simulation
into a single tool for materials scientists and engineers.

PRISM operates in two primary modes:

- **Interactive REPL** — a conversational interface where users describe
  materials research goals in natural language, and the AI agent proposes and
  executes multi-step research plans using available tools.
- **Autonomous agent** (`prism run`) — a headless mode where the agent
  independently executes a stated goal using a Think-Act-Observe-Repeat (TAOR)
  loop for up to 30 iterations.

PRISM also functions as a **Model Context Protocol (MCP) server**, exposing its
tools to external AI applications (e.g., Claude Desktop) via the FastMCP 3.x
framework over stdio or HTTP transports.

---

## 2. CLI Commands

### 2.1 Core Commands

| Command | Purpose |
|---------|---------|
| `prism` | Launch the interactive REPL. On first run, triggers onboarding to configure API keys and workflow preferences. |
| `prism run "<goal>"` | Run the autonomous agent to achieve a stated goal. Supports `--max-iterations`, `--approve-all`, `--model`, and `--resume` flags. |
| `prism search` | Federated search across 20+ OPTIMADE-compliant materials databases. Supports filtering by elements, formula, band gap, formation energy, space group, and other properties. No LLM required. |
| `prism serve` | Start PRISM as an MCP server (stdio or HTTP transport) for integration with external AI applications. |

### 2.2 Data Commands

| Command | Purpose |
|---------|---------|
| `prism data collect` | Collect materials data from OPTIMADE providers into local storage. |
| `prism data import` | Import CSV, JSON, or Parquet files into the local data store. |
| `prism data status` | List all stored datasets with row counts and metadata. |

### 2.3 Prediction and Modelling Commands

| Command | Purpose |
|---------|---------|
| `prism predict` | Predict material properties from chemical composition using trained ML models. |
| `prism model train` | Train a custom ML model on a local dataset. Supports Random Forest, Gradient Boosting, XGBoost, LightGBM, and Linear algorithms. |
| `prism model status` | Display trained models, pre-trained graph neural networks (GNNs), and CALPHAD installation status. |
| `prism model calphad status` | Check CALPHAD (pycalphad) installation and available thermodynamic databases. |
| `prism model calphad databases` | List imported TDB thermodynamic database files. |
| `prism model calphad import` | Import a TDB file for use in phase diagram and equilibrium calculations. |

### 2.4 Simulation Commands

| Command | Purpose |
|---------|---------|
| `prism sim status` | Display pyiron simulation engine availability and configuration. |
| `prism sim jobs` | List simulation jobs with status and results summary. |
| `prism sim init` | Initialize a pyiron simulation project directory. |

### 2.5 Plugin and Marketplace Commands

| Command | Purpose |
|---------|---------|
| `prism plugin list` | List all loaded plugins (tools, skills, providers, agents, algorithms). |
| `prism plugin init` | Scaffold a new plugin package with entry-point registration. |
| `prism labs list` | Browse the PRISM Labs premium services marketplace. |
| `prism labs info <service>` | View details, pricing, and capabilities of a marketplace service. |
| `prism labs status` | Show current subscription status for marketplace services. |
| `prism labs subscribe <service>` | Subscribe to a marketplace service via the MARC27 platform. |

### 2.6 Configuration Commands

| Command | Purpose |
|---------|---------|
| `prism configure` | Set API keys (`--anthropic-key`, `--openai-key`, `--openrouter-key`, `--mp-api-key`, `--labs-key`), default model (`--model`), view configuration (`--show`), or reset to defaults (`--reset`). |
| `prism setup` | Interactive wizard for workflow preferences: output format, search providers, max results, ML algorithm, report format, compute budget, and update checks. |
| `prism update` | Check for newer versions of PRISM and display the appropriate upgrade command for the detected installation method (uv, pipx, pip, or curl). |

### 2.7 Additional Commands

| Command | Purpose |
|---------|---------|
| `prism mcp` | MCP server management and diagnostics. |
| `prism optimade` | Direct OPTIMADE provider queries (advanced use). |
| `prism advanced` | Developer and debugging utilities. |

### 2.8 Deprecated Commands

| Command | Replacement | Status |
|---------|-------------|--------|
| `prism ask` | `prism run` | Deprecated; prints redirect notice. |
| `prism calphad` | `prism model calphad` | Deprecated; hidden alias preserved for backward compatibility. |

---

## 3. Agent Tools

The PRISM agent has access to 33 built-in tools (plus 19 additional tools when
optional dependencies are installed) that it may invoke during interactive or
autonomous sessions. Tool invocation requires user approval for operations
marked as expensive or irreversible.

### 3.1 Materials Data Tools

- **search_materials** — Federated search across 20+ OPTIMADE providers with
  parallel query execution, result fusion, deduplication, and provenance
  tracking.
- **query_materials_project** — Native Materials Project API queries (requires
  API key).
- **export_results_csv** — Export search results to CSV files.
- **import_dataset** — Import external data files into the local data store.
- **query_omat24** — Query Meta's Open Materials 2024 dataset (110M DFT
  calculations) via HuggingFace streaming. Filter by elements or formula.

### 3.2 Literature and Patent Tools

- **literature_search** — Search arXiv and Semantic Scholar for academic papers.
- **patent_search** — Search Lens.org for materials-related patents.

### 3.3 ML Prediction Tools

- **predict_property** — Predict material properties from chemical composition
  using trained models or composition-based feature engineering (matminer or
  built-in features).
- **predict_structure** — Predict properties from crystal structures using
  pre-trained GNNs (M3GNet, MEGNet via MatGL).
- **list_models** — List available trained models and pre-trained GNNs.
- **list_predictable_properties** — List properties that can be predicted.

### 3.4 CALPHAD Thermodynamic Tools

- **calculate_phase_diagram** — Compute phase diagrams using pycalphad.
- **calculate_equilibrium** — Compute thermodynamic equilibrium at specified
  conditions.
- **calculate_gibbs_energy** — Compute Gibbs free energy surfaces.
- **list_calphad_databases** — List available TDB thermodynamic databases.
- **list_phases** — List phases in a thermodynamic database.
- **import_calphad_database** — Import a TDB file for CALPHAD calculations.

### 3.5 Simulation Tools (via pyiron)

- **create_structure** — Create atomic structures for simulation.
- **modify_structure** — Apply transformations (supercell, strain, vacancy, etc.).
- **get_structure_info** — Retrieve structural metadata.
- **list_potentials** — List available interatomic potentials.
- **run_simulation** — Execute DFT, molecular dynamics, or energy minimisation.
- **get_job_status** / **get_job_results** / **list_jobs** / **delete_job** —
  Job management.
- **submit_hpc_job** / **check_hpc_queue** — HPC cluster submission.
- **run_convergence_test** — Automated convergence testing.
- **run_workflow** — Execute multi-step simulation workflows.

### 3.6 Visualization Tools

- **plot_materials_comparison** — Bar/scatter comparison charts.
- **plot_correlation_matrix** — Correlation heatmaps.
- **plot_property_distribution** — Histograms and distribution plots.

### 3.7 Data Quality Tools

- **validate_dataset** — Run data quality checks against configurable rules.
- **review_dataset** — Statistical summary and anomaly detection.

### 3.8 Labs Marketplace Tools

- **list_lab_services** — Browse available premium services.
- **get_lab_service_info** — Retrieve service details and pricing.
- **check_lab_subscriptions** — Check active subscriptions.
- **submit_lab_job** — Submit a job to a subscribed service (requires approval).

### 3.9 System Tools

- **execute_python** — Execute Python code in a subprocess (requires approval).
- **read_file** / **write_file** — File system access (requires approval for writes).
- **web_search** — Web search for supplementary information.
- **peek_result** — Page through large tool results stored in the result store.
- **discover_capabilities** — Enumerate all available resources (providers,
  models, databases, plugins, subscriptions).

---

## 4. Skills (Multi-Step Workflows)

Skills are pre-defined multi-step workflows the agent can invoke. Each skill
orchestrates multiple tool calls in sequence to accomplish a complex research
task. All skills are also registered as callable tools, making them directly
invocable by the agent during autonomous operation.

| Skill | Purpose |
|-------|---------|
| `materials_discovery` | End-to-end materials discovery pipeline: search, filter, predict, validate, report. |
| `acquire_materials` | Collect data from multiple sources (OPTIMADE, Materials Project, literature). |
| `select_materials` | Filter and rank material candidates against specified criteria. |
| `predict_properties` | Batch property prediction across a dataset using ML or GNN models. |
| `analyze_phases` | Phase stability analysis using CALPHAD tools. |
| `plan_simulations` | Automatically select and route simulations (CALPHAD vs DFT vs MD). |
| `visualize_dataset` | Generate a suite of plots for dataset columns. |
| `generate_report` | Produce Markdown, HTML, or PDF reports with embedded charts and statistics. |
| `validate_dataset` | Run configurable data quality checks. |
| `review_dataset` | Statistical review with anomaly detection. |

---

## 5. Plugin System

PRISM supports seven types of plugins that extend platform functionality:

| Plugin Type | Description |
|-------------|-------------|
| **Tool** | Single-action capability callable by the agent. |
| **Skill** | Multi-step workflow exposed as a callable tool. |
| **Provider** | Materials data source integrated into federated search. |
| **Agent** | Pre-configured agent profile with custom system prompt and tool set. |
| **Algorithm** | ML model factory for custom prediction algorithms. |
| **Collector** | Raw data collector for external data sources. |
| **Bundle** | Meta-package grouping multiple plugins. |

### Plugin Discovery Order

1. Built-in tools (`app/tools/`)
2. Optional tools (pyiron, pycalphad — loaded if installed)
3. Built-in skills (converted to callable tools)
4. Provider registry (OPTIMADE + catalog overrides)
5. Agent configurations from `catalog.json`
6. pip-installed entry-point plugins (`prism.plugins` entry point)
7. Local plugins (`~/.prism/plugins/*.py`)
8. MCP server tools (`~/.prism/mcp_servers.json`)

### Platform Catalog

PRISM ships with a plugin catalog (`catalog.json`) that declares built-in and
third-party provider integrations, including:

- **Materials Project** (native API, 155K+ materials)
- **AFLOW** (AFLUX API, 3.5M+ compounds)
- **MPDS** (Materials Platform for Data Science, subscription)
- **OMAT24** (Meta FAIR Open Materials 2024, 110M DFT calculations)

---

## 6. PRISM Labs Premium Marketplace

PRISM Labs is a marketplace for premium, computationally expensive services
operated by third-party providers and brokered through the MARC27 platform.
Subscription management, billing, and access control are handled by MARC27.

### Service Categories

| Category | Services | Description |
|----------|----------|-------------|
| **Autonomous Labs** | Berkeley A-Lab, Kebotix | Robotic synthesis and automated experimental workflows. |
| **Cloud DFT** | Matlantis (PFN), Mat3ra | Cloud-hosted density functional theory calculations. |
| **Quantum Computing** | HQS Quantum Simulations, AQT ARNICA | Quantum chemistry and materials simulation on quantum hardware. |
| **Design for Manufacturing** | Materials DfM Assessment | Manufacturability analysis for materials selection. |
| **Synchrotron** | SSRL Beamtime | Remote synchrotron beamtime scheduling and data collection. |
| **High-Throughput Screening** | HT Materials Screening | Automated combinatorial screening pipelines. |

---

## 7. Federated Search Architecture

PRISM's search engine federates queries across 20+ OPTIMADE-compliant materials
databases in parallel. Results are fused with full provenance tracking
(source attribution per material and per property value).

### Capabilities

- **Parallel execution** across all healthy providers
- **Result fusion and deduplication** with provenance tracking
- **Per-provider health monitoring** using a circuit breaker pattern
  (CLOSED / OPEN / HALF_OPEN states)
- **Disk-backed search cache** with 24-hour TTL
- **Retry with exponential backoff** (1s, 2s, 4s)

### Provider Tiers

| Tier | Providers |
|------|-----------|
| **Tier 1** | Materials Project, NOMAD, Alexandria, OQMD, COD, GNoME, MPDD |
| **Tier 2** | JARVIS, TCOD, 2DMatpedia, Materials Cloud |
| **Tier 3** | Matterverse, OMDB, odbx |

---

## 8. LLM Provider Support

PRISM supports multiple LLM providers for agent orchestration. Users select
their provider and model during onboarding or via `prism configure`.

| Provider | Models | Context Window |
|----------|--------|----------------|
| **Anthropic** | Claude Opus 4.6, Sonnet 4.6, Haiku 4.5 | 200K tokens |
| **OpenAI** | GPT-4o, GPT-4.1, GPT-5, o3, o3-mini | 128K–1M tokens |
| **Google** | Gemini 2.5 Pro, Gemini 2.5 Flash, Gemini 3.1 Pro | 1M tokens |
| **Zhipu AI** | GLM-5, GLM-4.7, GLM-4.5 Air | 128K–200K tokens |
| **MARC27** | Managed backend (via `/login`) | Provider-dependent |

Advanced LLM features include prompt caching (Anthropic), token usage tracking
(input, output, cache creation, cache read), per-run cost estimation, retry with
exponential backoff, large result truncation, and doom loop detection.

---

## 9. Configuration and Data Storage

### Configuration Files

| File | Purpose | Managed By |
|------|---------|------------|
| `.env` | API keys and secrets | `prism configure`, onboarding |
| `~/.prism/preferences.json` | Workflow preferences | `prism setup`, onboarding |
| `~/.prism/.update_check` | Version check cache (24h TTL) | `prism update` |
| `~/.prism/cache/` | Search cache, provider health | Search engine |
| `~/.prism/databases/` | TDB thermodynamic databases | `prism model calphad import` |
| `~/.prism/labs_subscriptions.json` | Lab service subscriptions | `prism labs subscribe` |
| `~/.prism/mcp_servers.json` | External MCP server configuration | User-managed |

### API Keys

| Environment Variable | Service |
|---------------------|---------|
| `ANTHROPIC_API_KEY` | Anthropic (Claude) |
| `OPENAI_API_KEY` | OpenAI |
| `OPENROUTER_API_KEY` | OpenRouter (200+ models) |
| `MATERIALS_PROJECT_API_KEY` | Materials Project |
| `PRISM_LABS_API_KEY` | PRISM Labs marketplace |

---

## 10. Security

- API keys are stored in `.env` files and never committed to version control.
- Tool invocations that are expensive, irreversible, or access external services
  require explicit user approval before execution.
- Python code execution operates in a subprocess with an approval gate.
- Vulnerability reports should be sent to team@marc27.com (not public issues).
- Input validation is applied before all external API calls.
- Built-in rate limiting protects against excessive external API usage.

---

## 11. Installation Methods

PRISM supports four installation methods:

| Method | Command |
|--------|---------|
| **pipx** (recommended) | `pipx install prism-platform[all]` |
| **uv** | `uv tool install prism-platform[all]` |
| **curl** | `curl -fsSL https://prism.marc27.com/install.sh \| bash` |
| **pip** | `pip install prism-platform[all]` |

Optional feature sets: `[ml]`, `[simulation]`, `[calphad]`, `[data]`,
`[reports]`, `[all]`.

---

## 12. Licensing

PRISM uses a dual-license model:

- **MIT License** — Core plumbing: CLI, data layer, tools, search engine,
  tests, documentation.
- **MARC27 Source-Available License** — AI orchestration: agent core, skills,
  ML pipelines, simulation bridge, validation engine, plugin framework.

The license boundary is enforced at the directory level within the source tree.
See `LICENSE`, `LICENSE-MIT`, and `LICENSE-MARC27` in the project root.

---

## 13. Dependencies and Attribution

PRISM builds on the following open-source projects (non-exhaustive):

| Category | Key Dependencies |
|----------|-----------------|
| **CLI** | Click, Rich, prompt_toolkit |
| **Data** | pandas, NumPy, SQLAlchemy |
| **Search** | OPTIMADE client, httpx, tenacity |
| **ML** | scikit-learn, XGBoost, LightGBM, Optuna, matminer, MatGL |
| **Simulation** | pyiron, pycalphad, ASE |
| **Materials** | pymatgen, mp-api |
| **LLM** | Anthropic SDK, OpenAI SDK, Google Vertex AI SDK |
| **MCP** | FastMCP 3.x |
| **Reporting** | Markdown, WeasyPrint |

Full attribution is provided in `ACKNOWLEDGMENTS.md`.

---

*Document generated for PRISM v2.5.0. MARC27, 2026.*
