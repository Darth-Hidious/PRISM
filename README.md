<p align="center">
  <img src="docs/assets/prism-banner.png" alt="PRISM Banner" width="800">
</p>

<h1 align="center">PRISM</h1>
<p align="center"><strong>AI-Native Autonomous Materials Discovery</strong></p>
<p align="center">
  <em>MARC27 &mdash; ESA SPARK Prime Contractor | ITER Supplier</em>
</p>

<p align="center">
  <a href="#quick-start">Quick Start</a> &bull;
  <a href="#architecture">Architecture</a> &bull;
  <a href="#capabilities">Capabilities</a> &bull;
  <a href="#license">License</a>
</p>

---

PRISM is an AI-native platform for autonomous materials discovery. It combines
large language models, multi-step agent orchestration, CALPHAD thermodynamics,
ML property prediction, and federated data access into a single CLI that
researchers can use to go from a hypothesis to a validated alloy candidate.

Built for the [OSIP](https://ideas.esa.int/servlet/hype/IMT?documentTableId=45087607031874021&userAction=Browse&templateName=&documentId=17c4b07a1c3b309ca9d56ea19d8e8fc0) programme, PRISM targets refractory high-entropy alloys (RHEAs)
for space propulsion but is domain-agnostic by design.

## Architecture

PRISM implements a four-module closed-loop architecture inspired by biological
evolution:

| Module | Role | Current Implementation |
|---|---|---|
| **Evolver (ACE)** | Propose candidate compositions | Agent + GFlowNet (Phase G) |
| **Mutator Fleet** | Perturb and explore the design space | Skills + tool chaining |
| **Evaluator** | Three-tier validation (surrogate / CALPHAD / experiment) | ML predict + CALPHAD bridge + validation rules |
| **MKG** | Materials Knowledge Graph for memory and retrieval | Session memory + DataStore + scratchpad |

The agent runs a **Think-Act-Observe-Repeat (TAOR)** loop with provider-agnostic
LLM backends (Anthropic, OpenAI, OpenRouter), a tool registry, and skill
orchestration.

## Capabilities

### Data Access
- **40+ databases** via OPTIMADE federation (Materials Project, OQMD, COD, JARVIS, AFLOW...)
- **Materials Project** native API with formation energy, band gap, hull distance
- **OMAT24** (Meta) via HuggingFace streaming
- **Literature** search (arXiv + Semantic Scholar)
- **Patent** search (Lens.org)
- **Local data import** (CSV, JSON, Parquet)

### AI & ML
- **Property prediction** with auto-training (Random Forest, XGBoost, LightGBM)
- **Feature importance** and correlation analysis
- **CALPHAD** phase diagrams, equilibrium, and Gibbs energy (pycalphad)
- **Simulation planning** with auto-routing (CALPHAD vs DFT vs MD)

### Agent Orchestration
- **10 skills**: acquire, predict, visualize, report, select, discover, simulate, analyze phases, validate, review
- **Plan-then-execute**: agent proposes a plan, user approves before execution
- **Tool consent**: expensive operations require explicit approval
- **Scratchpad**: append-only execution log for reproducibility
- **Feedback loops**: validation and CALPHAD findings feed back into agent context

### Infrastructure
- **Two modes**: interactive REPL (`prism`) and autonomous (`prism run "goal"`)
- **MARC27 managed LLM**: `/login` for managed model access via MARC27 account
- **MCP server**: `prism serve` exposes tools and resources via FastMCP 3.x
- **Plugin system**: pip entry-points + `~/.prism/plugins/` local plugins
- **Session memory**: save, load, and resume conversations
- **Reports**: Markdown, HTML, and PDF with embedded charts

## Quick Start

### One-command install

```bash
curl -fsSL https://prism.marc27.com/install.sh | bash
```

### Manual install

```bash
git clone https://github.com/Darth-Hidious/PRISM.git
cd PRISM
python3 -m venv .venv && source .venv/bin/activate
pip install -e ".[all,dev]"
```

### Configure

```bash
prism                      # First run triggers onboarding wizard
# or manually:
prism advanced configure   # Set up LLM provider + API key
```

### Run

```bash
# Interactive REPL
prism

# Autonomous mode
prism run "Find W-Rh alloys that are thermodynamically stable"

# MCP server for Claude Desktop
prism serve
```

See [INSTALL.md](INSTALL.md) for full details.

## Commands

| Command | Description |
|---|---|
| `prism` | Interactive agent REPL |
| `prism run "goal"` | Autonomous agent mode |
| `prism run "goal" --confirm` | Autonomous with tool consent |
| `prism serve` | Start as MCP server |
| `prism search --elements Fe,Ni` | Structured OPTIMADE search |
| `prism ask "query"` | Natural-language query |
| `prism update` | Check for updates |
| `prism setup` | Workflow preferences wizard |
| `prism plugin list` | List installed plugins |
| `prism calphad status` | CALPHAD installation status |
| `prism sim status` | Simulation status |

### REPL Commands

| Command | Description |
|---|---|
| `/help` | Show available commands |
| `/tools` | List available tools |
| `/skills [name]` | List skills or show details |
| `/plan <goal>` | Suggest skills for a goal |
| `/scratchpad` | Show execution log |
| `/status` | Platform capabilities |
| `/approve-all` | Auto-approve all tool calls |
| `/login` | Connect MARC27 account |
| `/save` | Save current session |
| `/load ID` | Load a saved session |
| `/export [file]` | Export last results to CSV |

## Project Structure

```
app/
  agent/          # MARC27 License — Agent core, REPL, autonomous, memory, scratchpad
  skills/         # MARC27 License — 10 multi-step workflow skills
  ml/             # MARC27 License — ML pipelines and algorithm registry
  simulation/     # MARC27 License — Pyiron bridge + CALPHAD bridge
  validation/     # MARC27 License — Rule-based validation engine
  plugins/        # MARC27 License — Plugin framework
  cli.py          # MIT License — CLI entry point
  config/         # MIT License — Settings, branding, preferences
  db/             # MIT License — SQLAlchemy models and database
  data/           # MIT License — DataStore and collectors
  tools/          # MIT License — Tool definitions and registry
tests/            # MIT License — 519 tests
docs/             # MIT License — Documentation and assets
```

## Testing

```bash
python3 -m pytest tests/ -v --ignore=tests/test_mcp_roundtrip.py --ignore=tests/test_mcp_server.py
```

519 tests covering agent core, tools, skills, data collectors, ML pipelines,
CALPHAD integration, validation rules, plugins, and CLI commands.

## Roadmap

### Current (v2.1.1)
- Claude Code-style minimal REPL with tool timing
- MARC27 managed LLM access (`/login`)
- OPTIMADE noise-free searches (explicit provider list)
- Python 3.11+ with pyiron/CALPHAD out of the box
- Dual license (MIT core + MARC27 proprietary AI)
- Tool consent, scratchpad, plan-then-execute, feedback loops

### Next (Phase G — deferred to ESA/seed funding)
- GFlowNet generative sampler
- GNN surrogate models
- Active learning loops
- Multi-agent coordination
- Playbook system

### Future (Phase H — deferred)
- A-Lab robotic integration
- Federated compute
- Automated experimental validation

## License

PRISM uses a **dual license**:

| Component | License |
|---|---|
| CLI, data layer, tools, tests, docs | [MIT](LICENSE-MIT) |
| Agent core, skills, ML, simulation, validation, plugins | [MARC27 Source-Available](LICENSE-MARC27) |

See [LICENSE](LICENSE) for details. Commercial licensing: licensing@marc27.com

## Links

- [INSTALL.md](INSTALL.md) — Installation guide
- [SECURITY.md](SECURITY.md) — Security policy
- [CHANGELOG.md](CHANGELOG.md) — Version history

---

<p align="center">
  <img src="docs/assets/bimo-logo.svg" alt="Bimo Tech" height="40">
</p>
