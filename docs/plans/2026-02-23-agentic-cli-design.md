# PRISM Agentic CLI Design Document

**Date:** 2026-02-23
**Status:** Approved
**Scope:** Transform PRISM from a single-shot query tool into a Claude Code-style agentic materials science CLI

---

## Vision

PRISM becomes an autonomous materials science research agent. Users interact via a persistent REPL (like Claude Code) or fire-and-forget autonomous mode. The agent reasons about materials science problems, uses tools (databases, simulations, ML, visualization), and iterates until it solves the user's research question.

---

## Architecture

### Provider-Agnostic Agent Core

```
User
  |
  v
PRISM CLI (Rich terminal REPL / autonomous mode)
  |
  v
AgentCore (provider-agnostic interface)
  |
  +-- AnthropicBackend (Agent SDK) <-- preferred
  +-- OpenAIBackend (custom TAOR loop)
  +-- OpenRouterBackend (custom TAOR loop)
  +-- VertexAIBackend (custom TAOR loop)
  |
  +-- Tool Registry (provider-agnostic tool definitions)
  |     |
  |     |-- DATA TOOLS
  |     |     search_optimade, query_materials_project,
  |     |     search_literature, query_job_database
  |     |
  |     |-- PREDICTION TOOLS
  |     |     predict_properties, train_model, benchmark_models
  |     |
  |     |-- SIMULATION TOOLS (Pyiron)
  |     |     create_structure, run_lammps, run_vasp,
  |     |     submit_hpc_job, check_job_status, get_job_results
  |     |
  |     |-- ANALYSIS / VISUALIZATION TOOLS
  |     |     plot_band_structure, plot_phonon_dispersion,
  |     |     plot_energy_volume, compare_materials, export_plot
  |     |
  |     |-- STRUCTURE TOOLS
  |     |     build_supercell, create_defect, create_surface,
  |     |     relax_structure
  |     |
  |     |-- SYSTEM TOOLS
  |           read_file, write_file, bash, web_search,
  |           web_fetch, ask_user_question
  |
  +-- MCP Client (consumes external MCP servers)
  +-- MCP Server (exposes all tools to external hosts)
  |
  +-- Memory & Context
        Session state, persistent memory (PRISM.md), task tracking
```

### Key Design Principle: Tool Interface Abstraction

Every tool defined once, provider-agnostically:

```python
class Tool:
    name: str
    description: str
    input_schema: dict  # JSON Schema
    def execute(self, **kwargs) -> dict: ...
```

AgentCore translates to the right format for each backend:
- Anthropic Agent SDK: registered as SDK tools
- OpenAI: converted to tools parameter format
- MCP: exposed as MCP tool definitions

No vendor lock-in. Tools written once, work everywhere.

---

## Two Modes

### Interactive REPL (`prism`)

Persistent session. User types messages, agent responds with reasoning + tool use.
Streaming Rich output. User can interrupt, redirect, ask follow-ups.

### Autonomous Mode (`prism run "goal"`)

Agent runs to completion, produces a final report. Good for CI/pipeline/batch use.

---

## Agent Loop (TAOR: Think-Act-Observe-Repeat)

```python
async def process(self, message: str):
    self.history.append({"role": "user", "content": message})
    while True:
        response = await self.backend.complete(self.history, self.tools)
        if response.has_tool_calls:
            for call in response.tool_calls:
                result = await self.tools[call.name].execute(**call.args)
                self.history.append(tool_result_message(call, result))
        else:
            self.history.append({"role": "assistant", "content": response.text})
            break  # No tool calls = loop terminates
```

---

## HPC Integration

Pyiron natively supports HPC submission via its Server abstraction:

```python
job.server.queue = "slurm"
job.server.cores = 32
job.server.run_time = 3600
job.run()
```

The submit_hpc_job tool wraps this. Long-running jobs work via poll:
submit -> check_status -> get_results.

---

## Phased Delivery

### Phase A: Agentic Loop + Built-in Tools
- Provider-agnostic AgentCore with AnthropicBackend + OpenAIBackend
- Interactive REPL with Rich streaming output
- Autonomous mode (prism run)
- Built-in tools: SearchOPTIMADE, QueryMP, PredictProperties, Plot
- Persistent session memory
- Existing cleanup (Tasks 12-15) and ML pipeline (Tasks 16-27) feed into this

### Phase B: MCP Integration
- PRISM as MCP server (expose all tools via FastMCP)
- PRISM as MCP client (consume external MCP servers)
- Configurable MCP server connections

### Phase C: Full Pyiron Integration
- Simulation tools: create_structure, run_lammps, run_vasp
- HPC submission: submit_hpc_job, check_status, get_results
- Job database queries
- Structure manipulation: supercells, defects, surfaces

### Phase D: Skills System
- Pluggable skill files (YAML/Markdown)
- Domain skills: literature review, phase diagram analysis, alloy design
- User-defined skills

---

## Dependencies (New)

```
# Agent SDK (primary backend)
claude-agent-sdk>=0.1.0

# MCP
fastmcp>=0.1.0
mcp>=1.0.0

# Pyiron (Phase C)
pyiron_base>=0.9.0
pyiron_atomistics>=0.6.0
pyiron_workflow>=0.1.0

# Visualization
matplotlib>=3.7.0
plotly>=5.0.0

# Existing deps remain
```

---

## File Structure (Target)

```
PRISM/
  app/
    __init__.py
    cli.py                 # Entry points: prism, prism run
    agent/
      __init__.py
      core.py              # AgentCore class
      backends/
        __init__.py
        anthropic.py       # AnthropicBackend (Agent SDK)
        openai.py          # OpenAIBackend (custom loop)
        openrouter.py      # OpenRouterBackend
      events.py            # ToolCallEvent, TextEvent, etc.
      memory.py            # Session + persistent memory
    tools/
      __init__.py
      base.py              # Tool base class + registry
      data.py              # SearchOPTIMADE, QueryMP, etc.
      prediction.py        # PredictProperties, TrainModel
      simulation.py        # Pyiron tools (Phase C)
      visualization.py     # Plotting tools
      structure.py         # Structure manipulation (Phase C)
      system.py            # ReadFile, Bash, WebSearch, etc.
    mcp/
      __init__.py
      server.py            # PRISM as MCP server (Phase B)
      client.py            # PRISM as MCP client (Phase B)
    config/
      settings.py
      branding.py
      providers.py
    data/                  # Data pipeline (from existing plan)
    ml/                    # ML models (from existing plan)
    db/
    skills/                # Skills system (Phase D)
  tests/
  models/
  data/
  docs/
```
