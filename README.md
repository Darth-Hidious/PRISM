
# PRISM: Platform for Research in Intelligent Synthesis of Materials

      ██████╗ ██████╗ ██╗███████╗███╗   ███╗
      ██╔══██╗██╔══██╗██║██╔════╝████╗ ████║
      ██████╔╝██████╔╝██║███████╗██╔████╔██║
      ██╔═══╝ ██╔══██╗██║╚════██║██║╚██╔╝██║
      ██║     ██║  ██║██║███████║██║ ╚═╝ ██║
      ╚═╝     ╚═╝  ╚═╝╚═╝╚══════╝╚═╝     ╚═╝

**An AI-powered command-line platform for materials science research.** PRISM connects 40+ materials databases, ML property prediction, atomistic simulation, and LLM-driven analysis into a single tool. Ask questions in natural language, run multi-step discovery workflows, or expose everything as an MCP server for Claude Desktop.

---

## Table of Contents

- [Overview](#overview)
- [Architecture](#architecture)
- [Quick Start](#quick-start)
- [Modes of Operation](#modes-of-operation)
- [Command Reference](#command-reference)
- [Tools](#tools)
- [Skills](#skills)
- [MCP Server](#mcp-server)
- [ML Pipeline](#ml-pipeline)
- [Atomistic Simulation](#atomistic-simulation)
- [Configuration](#configuration)
- [Development](#development)
- [License](#license)

---

## Overview

PRISM is built around a provider-agnostic agent that runs a Think-Act-Observe-Repeat (TAOR) loop. The agent has access to 17 atomic tools and 7 high-level skills that compose those tools into multi-step workflows. Everything is exposed through three interfaces:

- **Interactive REPL** -- conversational research sessions with streaming responses
- **Autonomous mode** -- give it a goal, let it work to completion
- **MCP server** -- expose all tools and skills to Claude Desktop or any MCP client

### What It Can Do

- Search 40+ materials databases via OPTIMADE and Materials Project APIs
- Predict material properties using composition-based ML models
- Generate scatter plots, histograms, and comparison charts
- Run atomistic simulations via pyiron (LAMMPS, VASP, etc.)
- Execute end-to-end discovery pipelines: acquire, predict, visualize, select, report
- Save and resume research sessions
- Export data to CSV and Parquet

---

## Architecture

```
User
 |
 +-- prism (REPL)          Interactive agent with streaming
 +-- prism run "goal"       Autonomous agent
 +-- prism serve            MCP server (stdio or HTTP)
 |
 +-- AgentCore (TAOR loop)
      |
      +-- ToolRegistry
      |    +-- Atomic Tools (17): data, system, visualization, prediction, simulation
      |    +-- Skills as Tools (7): acquire, predict, visualize, report, select, discover, sim_plan
      |
      +-- Backend (provider-agnostic)
           +-- Anthropic, OpenAI, Google Vertex AI, OpenRouter
```

Key design decisions:

- **Skills are Tools.** A Skill converts to a Tool via `to_tool()` and registers in the same ToolRegistry. The LLM invokes a skill exactly like any other tool.
- **Provider-agnostic.** Swap LLM backends without changing tool code.
- **Lazy dependencies.** pyiron, weasyprint, and other heavy packages are imported only when used.

---

## Quick Start

### Install

```bash
git clone https://github.com/Darth-Hidious/PRISM.git
cd PRISM
python3 -m venv .venv
source .venv/bin/activate   # Linux/macOS
pip install -e .
```

Optional extras:

```bash
pip install -e ".[ml]"          # scikit-learn, xgboost, lightgbm
pip install -e ".[simulation]"  # pyiron (LAMMPS, VASP integration)
pip install -e ".[reports]"     # Markdown-to-PDF report generation
pip install -e ".[dev]"         # pytest, black, flake8
```

### Configure

```bash
# Set your LLM provider API key
export ANTHROPIC_API_KEY="sk-..."

# Optional: Materials Project API key for enriched data
export MP_API_KEY="..."

# Or use the interactive configurator
prism configure --mp-api-key YOUR_KEY
```

### Run

```bash
# Start interactive REPL
prism

# Run a one-shot autonomous query
prism run "Find stable binary oxides with band gap above 3 eV"

# Direct structured search
prism search --elements "Ti,O" --nelements 2
```

---

## Modes of Operation

### Interactive REPL

Start with `prism` (no arguments). The REPL provides a conversational interface with streaming responses, tool call panels, and session management.

```
> Find materials containing lithium and cobalt with low formation energy

[search_optimade] 12 results
[plot_property_distribution] saved to distribution.png

Based on the OPTIMADE search, I found 12 Li-Co compounds...
```

#### REPL Commands

| Command | Description |
|---------|-------------|
| `/help` | Show available commands |
| `/tools` | List all registered tools |
| `/skill` | List available skills |
| `/skill <name>` | Show skill steps and details |
| `/plan <goal>` | Ask the LLM which skills apply to a goal |
| `/mcp` | Show MCP server connections and tools |
| `/save` | Save current session |
| `/load <id>` | Restore a saved session |
| `/sessions` | List saved sessions |
| `/export [file]` | Export last results to CSV |
| `/clear` | Clear conversation history |
| `/exit` | Exit the REPL |

### Autonomous Mode

Give PRISM a research goal and let it work autonomously:

```bash
prism run "Compare band gaps of perovskite oxides ABO3 where A=Ba,Sr,Ca"
```

The agent will break down the goal, call tools as needed, and present a structured answer. Use `--provider` and `--model` to select the LLM backend.

### MCP Server

Expose all PRISM tools and skills as an MCP server for Claude Desktop or any MCP-compatible client:

```bash
# stdio transport (for Claude Desktop)
prism serve

# HTTP transport
prism serve --transport http --port 8000

# Print Claude Desktop config JSON
prism serve --install
```

---

## Command Reference

### Core Commands

| Command | Description |
|---------|-------------|
| `prism` | Start interactive REPL |
| `prism run "<goal>"` | Autonomous mode |
| `prism search` | Structured OPTIMADE search |
| `prism ask "<query>"` | Natural language query with LLM |
| `prism serve` | Start MCP server |
| `prism setup` | Interactive workflow preferences wizard |
| `prism switch-llm` | Switch LLM provider |
| `prism configure` | Set API keys and configuration |

### Data Commands

| Command | Description |
|---------|-------------|
| `prism data list` | List saved datasets |
| `prism data show <name>` | Preview a dataset |
| `prism data delete <name>` | Delete a dataset |

### ML Commands

| Command | Description |
|---------|-------------|
| `prism model train --prop <property>` | Train an ML model on a dataset |
| `prism model status` | List trained models and metrics |
| `prism predict <formula>` | Predict properties for a formula |

### Simulation Commands

| Command | Description |
|---------|-------------|
| `prism sim status` | Show pyiron configuration and job counts |
| `prism sim jobs` | List simulation jobs |
| `prism sim init` | Initialize a pyiron project directory |

### Other Commands

| Command | Description |
|---------|-------------|
| `prism optimade list-dbs` | List all available OPTIMADE databases |
| `prism mcp init` | Create MCP server config template |
| `prism mcp status` | Show MCP server connections |

---

## Tools

PRISM provides 17 atomic tools that the agent can call:

### Data Tools
- **search_optimade** -- search 40+ databases using OPTIMADE filter syntax
- **query_materials_project** -- query Materials Project for band gap, formation energy, density, etc.
- **export_results_csv** -- export result data to CSV files

### System Tools
- **read_file** -- read file contents (sandboxed to project directory)
- **write_file** -- write file contents (sandboxed)
- **web_search** -- search the web for information

### Visualization Tools
- **plot_materials_comparison** -- scatter plot comparing two material properties
- **plot_property_distribution** -- histogram of a property distribution

### Prediction Tools
- **predict_property** -- predict a property from a chemical formula using trained ML
- **list_models** -- list trained models and their metrics

### Simulation Tools (requires pyiron)
- **create_structure** -- create crystal structures (FCC, BCC, HCP, etc.)
- **modify_structure** -- supercell, strain, vacancy, substitution operations
- **get_structure_info** -- inspect a stored structure
- **list_potentials** -- list available interatomic potentials
- **run_simulation** -- run energy minimization or MD simulations
- **get_job_status** / **get_job_results** / **list_jobs** / **delete_job** -- job management
- **submit_hpc_job** / **check_hpc_queue** -- HPC cluster submission
- **run_convergence_test** / **run_workflow** -- automated convergence and workflow execution

---

## Skills

Skills are multi-step workflows that compose atomic tools into pipelines. They register as tools in the same ToolRegistry, so the LLM invokes them naturally.

### Available Skills

| Skill | Description |
|-------|-------------|
| `acquire_materials` | Search and collect data from OPTIMADE and Materials Project, normalize, deduplicate, and save as a named dataset |
| `predict_properties` | Load a dataset, auto-train ML models if needed, predict properties for each formula, append prediction columns |
| `visualize_dataset` | Generate distribution histograms and pairwise comparison plots for all numeric columns in a dataset |
| `generate_report` | Compile a Markdown report with dataset summary, data preview, statistics, ML results, and plot references |
| `select_materials` | Filter by min/max criteria, sort, and select top N candidates into a new dataset |
| `materials_discovery` | End-to-end pipeline: acquire -> predict -> visualize -> report (graceful on partial failures) |
| `plan_simulations` | Generate simulation job plans for top candidates (planning only, no execution without confirmation) |

### Example: End-to-End Discovery

In the REPL or autonomous mode, the LLM can invoke the `materials_discovery` skill:

```
> Find alloys with W and Rh that are stable

[materials_discovery]
  Step 1: Acquiring data from OPTIMADE... 47 records
  Step 2: Predicting band_gap, formation_energy... done
  Step 3: Generating 4 plots... saved to output/
  Step 4: Compiling report... saved to output/w_rh_discovery_report.md

Found 47 W-Rh compounds. 12 have predicted formation energy below -0.5 eV/atom,
suggesting thermodynamic stability. See the full report for details.
```

### Viewing Skills

```bash
# In REPL:
/skill                          # List all skills
/skill materials_discovery      # Show steps for a specific skill
/plan "find stable perovskites" # Ask which skills to use
```

---

## MCP Server

PRISM exposes all tools, skills, and data as an MCP server compatible with Claude Desktop and other MCP clients.

### Resources

| Resource URI | Description |
|--------------|-------------|
| `prism://tools` | List all available tools |
| `prism://skills` | List skills with step details |
| `prism://sessions` | Saved research sessions |
| `prism://datasets` | Collected materials datasets |
| `prism://datasets/{name}` | Dataset metadata and preview |
| `prism://models` | Trained ML models and metrics |
| `prism://simulations/structures` | Stored atomistic structures (when pyiron installed) |
| `prism://simulations/jobs` | Simulation jobs |
| `prism://simulations/jobs/{id}` | Individual job details |

### Claude Desktop Integration

```bash
# Generate the config entry
prism serve --install

# Add the output to ~/Library/Application Support/Claude/claude_desktop_config.json
```

### External MCP Servers

PRISM can also consume tools from external MCP servers. Configure them in `~/.prism/mcp_servers.json`:

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/dir"]
    }
  }
}
```

---

## ML Pipeline

PRISM includes a complete ML pipeline for materials property prediction.

### Feature Engineering

Composition-based features are generated automatically from chemical formulas using Magpie-style descriptors: mean, min, max, range, and standard deviation of atomic mass, atomic number, electronegativity, and atomic radius.

### Training

```bash
# Train a model on a saved dataset
prism model train --prop band_gap --algorithm random_forest --dataset my_data

# Available algorithms: random_forest, gradient_boosting, linear, xgboost, lightgbm
```

### Prediction

```bash
# Predict from CLI
prism predict Fe2O3

# Or let the agent use it in conversation
> What's the predicted band gap for SrTiO3?
```

Models and metrics are persisted in the `models/` directory as joblib files with JSON metadata.

---

## Atomistic Simulation

PRISM integrates with pyiron for atomistic simulations. This is an optional dependency.

```bash
pip install prism-platform[simulation]
```

### Capabilities

- Create and modify crystal structures (FCC, BCC, HCP, diamond, etc.)
- Run LAMMPS and VASP calculations
- Energy minimization, molecular dynamics
- Convergence testing workflows
- HPC job submission (SLURM, PBS, LSF)
- Structure and job management with in-memory stores

### Example

```
> Create an FCC aluminum structure and run an energy minimization

[create_structure] struct_a1b2c3d4
[run_simulation] job_e5f6g7h8 — running LAMMPS energy minimization
[get_job_results] Energy: -3.36 eV/atom
```

---

## Configuration

### Workflow Preferences

Run `prism setup` to configure defaults for skills and workflows:

```bash
prism setup
```

This walks through:
- **Output format**: csv, parquet, or both
- **Default data providers**: optimade, mp
- **Max results per source**
- **ML algorithm**: random_forest, gradient_boosting, linear
- **Report format**: markdown or pdf
- **Compute budget**: local or hpc (with queue and core settings)

Preferences are saved to `~/.prism/preferences.json` and used by all skills.

### LLM Providers

PRISM supports multiple LLM backends:

| Provider | Environment Variable | Models |
|----------|---------------------|--------|
| Anthropic | `ANTHROPIC_API_KEY` | Claude 4.5/4.6 |
| OpenAI | `OPENAI_API_KEY` | GPT-4, GPT-4o |
| Google Vertex AI | `GOOGLE_CLOUD_PROJECT` | Gemini |
| OpenRouter | `OPENROUTER_API_KEY` | 200+ models |

Switch providers at any time:

```bash
prism switch-llm
```

### API Keys

```bash
# Materials Project API key (enables MP data enrichment)
prism configure --mp-api-key YOUR_KEY

# List current configuration
prism configure --list-config
```

---

## Development

### Prerequisites

- Python 3.10+
- pip

### Setup

```bash
git clone https://github.com/Darth-Hidious/PRISM.git
cd PRISM
python3 -m venv .venv
source .venv/bin/activate
pip install -e ".[dev]"
```

### Running Tests

```bash
python3 -m pytest tests/ -v
```

The test suite includes 265+ tests covering tools, skills, ML pipeline, CLI commands, agent integration, and end-to-end workflows.

### Project Structure

```
app/
  cli.py                    Main CLI entry point
  mcp_server.py             FastMCP server with dynamic handler generation
  mcp_client.py             External MCP server client
  agent/
    core.py                 AgentCore (TAOR loop)
    repl.py                 Interactive REPL
    autonomous.py           Autonomous mode
    backends/               LLM backend adapters
    memory.py               Session persistence
  tools/
    base.py                 Tool and ToolRegistry
    data.py                 OPTIMADE + Materials Project tools
    system.py               File I/O and web search
    visualization.py        Matplotlib plotting tools
    prediction.py           ML prediction tools
    simulation.py           Pyiron simulation tools (13)
  skills/
    base.py                 Skill, SkillStep, SkillRegistry
    registry.py             load_builtin_skills()
    acquisition.py          Multi-source data acquisition
    prediction.py           Dataset property prediction
    visualization.py        Dataset visualization
    reporting.py            Report generation (Markdown/PDF)
    selection.py            Materials filtering and ranking
    discovery.py            End-to-end discovery pipeline
    simulation_plan.py      Simulation job planning
  data/
    collector.py            OPTIMADECollector, MPCollector
    normalizer.py           Record normalization and deduplication
    store.py                Parquet-based DataStore
  ml/
    features.py             Composition-based feature engineering
    trainer.py              Model training pipeline
    predictor.py            Prediction engine
    registry.py             Model persistence (joblib)
  simulation/
    bridge.py               PyironBridge, StructureStore, JobStore
  config/
    preferences.py          UserPreferences (persistent workflow config)
    settings.py             Environment settings
    providers.py             OPTIMADE provider list
tests/
  test_*.py                 265+ tests
```

---

## License

MIT License. See [LICENSE](LICENSE) for details.

Built by the MARC27 team.
