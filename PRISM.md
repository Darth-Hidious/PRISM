# PRISM

**AI-Native Autonomous Materials Discovery Platform**

*by MARC27*

---

## What PRISM Is

PRISM is a single binary that gives you an AI agent for materials science.
It searches knowledge graphs, runs compute jobs, orchestrates workflows,
evaluates alloy compositions, and connects to a federated mesh of research
nodes — all from your terminal, all with full provenance.

Local-first. Cloud-optional. Every material discovered has a traceable
chain: which LLM proposed it, which tool evaluated it, with what parameters,
and what data fed it.

```
You: "Find me a high-strength Ti alloy for aerospace applications"

PRISM:
  → searches OPTIMADE federation for known Ti alloys
  → reasons over results with the LLM
  → generates new candidate compositions via MCMC
  → evaluates each against physics descriptors (density, VEC, entropy)
  → ranks by your objective
  → records every step to the provenance chain
  → presents the top 5 with full justification
```

## What PRISM Does Today

### The Agent Loop

The core of PRISM is an agent that reasons, selects tools, executes them,
and formats results — across multiple turns with full context retention.

- **Tool selection**: top-15 keyword-filtered tools per turn (was 99 = 21K
  tokens, now lean and relevant)
- **Streaming**: token-by-token output with thinking tokens separated and
  dimmed (Ctrl-T to expand)
- **Tool chaining**: the agent calls `alloy_sample` → `gfn_evaluate` across
  turns without being told to
- **Approval gating**: every tool call can require human approval (y/n/a)
  or run in auto-approve mode
- **Provenance**: every tool call is recorded to a Turso-backed provenance
  store with tool name, inputs, outputs, LLM model, session ID, and
  parent ID (causal chain)

### The TUI

A full-screen Ratatui terminal UI built on the Elm Architecture (TEA).

- Chat panel with scrollback (500-message sliding window)
- Streaming text with tokens/sec metric in the status bar
- Tool cards with elapsed time and success/error state
- Approval popup overlay (y = approve, n = deny, a = allow-all)
- Slash commands (`/tools`, `/status`)
- Key bindings: Ctrl-C quit, Ctrl-L clear, Ctrl-T thinking, Ctrl-M metrics,
  Ctrl-$ cost, Tab focus cycle, Esc blur
- Thinking tokens rendered dimmed and collapsible
- Cost display (toggle for local models where cost = $0)
- Panic hook restores terminal on crash so we don't brick your shell
- 53 unit tests + 16 end-to-end terminal tests (PTY-driven via pexpect)

### The Tools (99 registered)

**Materials Discovery**
- `alloy_sample` — MCMC (Metropolis-Hastings) alloy generation. Cold-start
  mode uses physics descriptors only — no ML model, no torch, no GPU.
- `alloy_discover` — multi-round MCMC with adaptive step size. Narrows
  search around best compositions found.
- `gfn_evaluate` — physics descriptors for a composition: delta H_mix,
  VEC, mixing entropy, density, melting point.
- `gfn_elements` — element table with physical constants.

**Search**
- `materials_search` / `acquire_materials` — federated OPTIMADE search
  across 20+ providers. Async httpx, circuit breaker per provider,
  8-concurrent-connection limit, result caching, early termination when
  2x requested limit is reached.
- Materials Project provider with API key auto-resolution from MARC27
  credentials.

**Knowledge Graph**
- `knowledge` — query the MARC27 knowledge graph (semantic search, Cypher,
  entity lookup, graph stats).
- `knowledge_write` — write entities and relationships to the graph.
- Ingest pipeline: LLM-extracted ontology → graph store → vector embeddings.

**Compute**
- `compute` — read-only broker queries (list GPUs, list providers, estimate
  cost, poll job status, cancel).
- `compute_submit` — dispatch real containerized GPU/CPU jobs to the
  MARC27 Compute Broker (RunPod, Lambda, PRISM mesh nodes). Approval-gated,
  money-spending, budget-capped.
- `run` / `job-status` — CLI commands for compute job submission and polling.

**Prediction**
- `predict` — unified property prediction (band gap, formation energy,
  elastic moduli) via M3GNet, MEGNet, matminer descriptors.
- Pre-trained models auto-discovered from installed packages.

**Simulation**
- `simulate` — MACE ML potential calculations via JAX-MD. Molecular
  dynamics, relaxed energies, elastic moduli.
- CALPHAD phase diagram analysis via pycalphad.
- pyiron atomistic simulation workflows.

**Data**
- `dataset` — import/validate/export CSV, JSON, Parquet. Z-score outlier
  flagging, physical constraint checks, completeness scoring.
- `import_dataset` — CLI ingest into the PRISM DataStore.

**Memory**
- `recall` — hybrid search (BM25 keyword + semantic vector, RRF-fused)
  over the agent's local artifact store. Every meaningful tool output from
  this and prior sessions is auto-indexed.
- `fetch_artifact` — retrieve full verbatim data of one artifact by ID.
- `list_artifacts` — list by metadata (tool, session, time).
- `promote_artifact` — push a local artifact into the cross-session MARC27
  knowledge graph.

**Platform**
- `agent_capabilities` — ask the MARC27 platform to describe itself.
- `platform_status` — show platform connection, auth, project context.
- `platform_jobs` — list and manage platform compute jobs.
- `platform_workflows` — list and run platform-hosted workflows.
- `billing_balance` — read billing state (balance, usage, prices).
- `models_list` — discover hosted LLM models for the active project.

**General Purpose**
- `execute_bash` / `bash_task` / `stop_bash_task` — shell execution with
  background task management.
- `execute_python` — run Python code in the project venv.
- `web_search` — search the open web.
- `file` — text file I/O within the workspace.
- `code` — code execution and analysis.
- `visualization` — generate plots and visual artifacts.
- `spark` — Spark session for distributed data processing.

### The Workflow Engine

YAML-defined DAGs of tool-execution steps with template interpolation,
parallel execution, dependency tracking, and OPA policy enforcement.

```yaml
name: discover
description: Agentic materials discovery pipeline
command_name: discover
arguments:
  - name: goal
    type: string
    required: true
steps:
  - id: reason
    action: llm
    prompt: "Propose 3 candidate compositions for {{ goal }}."
    system: "You are a materials scientist."
    output_key: candidates
  - id: discover_loop
    action: loop
    max_iterations: 5
    until: "{{ done }}"
    steps:
      - id: sample
        action: tool
        name: alloy_sample
      - id: evaluate
        action: tool
        name: gfn_evaluate
      - id: check
        action: set
        values: { done: "true" }
```

**Step types** (9):
- `set` — assign values to context
- `message` — print a message
- `http` — HTTP request (with SSRF guard blocking localhost/metadata IPs)
- `tool` — call a PRISM tool via the local node API
- `llm` — call the LLM with a templated prompt (the agentic bridge)
- `if` — conditional branching with truthy evaluation
- `loop` — repeat sub-steps until a condition is met (max 100 iterations)
- `parallel` — fan out sub-steps concurrently (tokio::spawn)
- `workflow` — invoke a sub-workflow by name

**Features**:
- Template interpolation: `{{ context.path }}` in any field
- Retry with exponential backoff per step
- `on_start` / `on_complete` hooks
- OPA/Rego policy enforcement (per-workflow and per-tool-call)
- SSRF guard (blocks `localhost`, `169.254.169.254`, RFC1918, non-http schemes)
- Auto-generated step IDs for LLM-friendly YAML
- "Did you mean?" suggestions on action typos
- Per-file failure isolation (one bad YAML doesn't break discovery)
- 40 tests (29 original + 11 new for llm/loop steps)

### The Provenance System

Every tool call is recorded to a Turso-backed provenance store.

**Schema**: id, timestamp, session, action_type, actor, tool, llm_model,
input, output, parent_id, material_ref, confidence, tags.

**Capabilities**:
- Query by session
- Query by material
- Trace the full causal chain (parent_id links)
- Hook-based recording (wired into agent_loop.rs, non-blocking)

### The Mesh

Federated networking between PRISM nodes.

- mDNS discovery (find peers on the local network)
- E2EE node-to-node communication
- RBAC gate (no auth = refuse to start; offline mode bypasses for testing)
- Cross-site inference (two orgs run PRISM, share models/data via mesh)
- Dataset publishing and subscription management

### The LLM Layer

- **Local**: llama.cpp / Gemma 4 12B via OpenAI-compatible API
- **Cloud**: MARC27 platform proxy (~644 hosted models: Anthropic, Google,
  OpenAI, etc.)
- **Direct vendor**: Anthropic, OpenAI API keys
- `prism use local|provider|marc27` — hot-swap chat target without restart
- Token-by-token streaming with reasoning_content support (Gemma thinking mode)
- 300s default timeout (local models can be slow)
- KV cache optimization (q4_0 K+V, flash attention, 7GB vs 12GB VRAM)

### The Marketplace

Browse, install, and publish tools and workflows from the MARC27 marketplace.

- `prism marketplace search` — list/search resources
- `prism marketplace find` — semantic discovery (cosine similarity search)
- `prism marketplace install <slug>` — install a tool or workflow
- `prism marketplace info <slug>` — show details
- `prism marketplace update` — pull tool updates from the marketplace
  (remote wins silently; `--dry-run` to preview)
- **Auto-update on startup**: every `prism tui` / `prism backend` / `prism
  resume` invocation kicks off a background task that diffs the local tool
  manifest against the marketplace and pulls updates. Non-blocking,
  fails silently when offline.
- Tool manifest at `~/.prism/tools/.manifest.json` tracks installed
  versions to skip unchanged tools.

### The CLI

```
prism                    # Launch interactive TUI
prism tui                # Explicit TUI launch
prism backend            # JSON-RPC agent backend (for custom frontends)
prism resume [id]        # Resume a previous conversation
prism query              # Query the knowledge graph
prism research           # Start a research loop
prism workflow           # List and run YAML workflows
prism tools              # List available Python tools
prism ingest             # Ingest a data file into the knowledge graph
prism node up/down       # Start/stop the PRISM node daemon
prism mesh               # Mesh networking commands
prism marketplace        # Browse and install tools
prism compute            # Compute broker commands
prism deploy             # Deploy a model or service
prism models             # Discover hosted LLM models
prism billing            # Credit balance and usage
prism use                # Switch chat target (local/provider/marc27)
prism doctor             # Diagnostic snapshot
prism status             # Runtime paths, endpoints, auth
prism login              # Authenticate against MARC27
prism setup              # First-time setup
```

### Architecture

**Rust backbone** (43 crates):
- `prism-cli` — command routing, auth, worker supervision
- `prism-core` — config, sessions, RBAC, audit
- `prism-agent` — agent loop, tool catalog, hooks, permissions, prompts
- `prism-llm` — LLM client (local, MARC27, direct vendor)
- `prism-tui` — Ratatui terminal UI (TEA architecture)
- `prism-workflows` — YAML workflow engine
- `prism-provenance` — Turso-backed provenance store
- `prism-python-bridge` — worker launch + supervision
- `prism-client` — platform API, marketplace, device-flow auth
- `prism-compute` — backend routing (Docker/Cloud/BYOC)
- `prism-mesh` — node discovery + pub/sub
- `prism-node` — daemon, hardware probe, job execution
- `prism-ingest` — ontology pipeline (LLM → Graph)
- `prism-orch` — container lifecycle orchestration
- `prism-policy` — OPA/Rego policy engine
- `prism-proto` — wire types (gRPC/protobuf)
- `prism-ipc` — JSON-RPC 2.0
- `prism-server` — Axum REST API + WebSocket
- `prism-runtime` — path discovery, environment
- `forge_*` (24 crates) — experiment design engine, conversation repo,
  streaming, markdown repair, model management, UI, VS Code integration

**Python intelligence plane** (app/):
- `tools/` — 99 registered tools across 30+ modules
- `plugins/` — bootstrap, tool registry, custom tool loader
- `search_engine/` — federated OPTIMADE search with circuit breaker
- `simulation/` — MACE/JAX-MD, CALPHAD, pyiron bridges
- `memory/` — artifact store with BM25 + vector + RRF hybrid recall
- `workflows/` — Python-side workflow engine and registry

**Storage**:
- Turso (SQLite-compatible) — provenance, agent memory, CLI state
- FalkorDB (Cypher-compatible, Redis-based) — knowledge graph (planned)
- Qdrant — vector embeddings
- Filesystem — `~/.prism/` (config, tools, workflows, cache, venv)

## What PRISM Aspires To Be

### Full Autonomous Discovery

Today PRISM can generate and evaluate alloy compositions. The aspiration is
**autonomous discovery campaigns**: the agent runs for hours or days,
proposeing candidates, evaluating them, learning from results, and narrowing
the search — all with full traceability and human-in-the-loop checkpoints.

**What's missing**:
1. **FalkorDB graph store** — persistent memory of discovered materials
   (currently using Neo4j interface; FalkorDB is the target for local-first)
2. **Active learning oracle** — generative discovery works but needs a real
   physics oracle (JAX-MD/DFT) to evaluate beyond empirical descriptors
3. **Campaign agent crate** (`prism-campaign`) — long-running orchestrator
   that manages discovery campaigns with budget limits, checkpoint/resume,
   and human approval gates at key milestones
4. **LLM-guided generation** — use the LLM as a generative sampling policy. The LLM
   brings chemical knowledge; the sampler brings diversity. Updates via
   reward-weighted updates push the LLM to sample ∝ reward.

### The Discovery Loop (Target)

```
User: "Design a refractory high-entropy alloy for turbine blades,
       maximize creep resistance at 1200°C"

PRISM Campaign Agent:
  1. Search → find known RHEAs in the literature
  2. LLM reason → propose 10 candidate compositions
  3. Loop (budget: 100 evaluations):
     a. Sample → MCMC generates variations around candidates
     b. Evaluate → physics descriptors + MACE ML potential
     c. Rank → by scalarized reward (creep proxy)
     d. Adapt → narrow search around top performers
     e. Checkpoint → save state every 10 iterations
  4. DFT validation → submit top 5 to Quantum ESPRESSO on HPC
  5. Report → full provenance chain, ranked results, justification
  6. Promote → push winners to the MARC27 knowledge graph
```

### Science Tool Roadmap

- **JAX-MD integration** — molecular dynamics oracle with MACE ML potential
  as the energy function. Gives the first real physics signal beyond
  empirical rules of thumb.
- **CALPHAD tools** — phase diagram calculations for alloy design
- **pyiron integration** — atomistic simulation workflows
- **DFT tools** — Quantum ESPRESSO / VASP wrappers (needs HPC hooks)
- **ML predictors** — property prediction models (sklearn, GNN, MACE)
- **Multi-fidelity TTT** — materials design with uncertainty quantification

### Platform Integration

- **marc27-core rebuild** — port the platform backend (login, RBAC,
  marketplace, billing) from the SSD archive
- **Swift macOS client** — native Mac app at `macos/PRISMMac/`
- **Web app** — React/Vite chat client for non-Mac systems
- **Mesh stress testing** — E2EE, mDNS, cross-site inference under load

### Marketplace & Monetization

- `prism tools init` — scaffold a new tool project (zero boilerplate)
- `prism tools validate` — dry-run, schema validation, security check
- `prism tools publish` — one command to the marketplace
- `prism marketplace install <slug>` — with pricing awareness
- **Metering + billing** — every marketplace tool call logged to provenance,
  usage counted per user per billing period
- **Tool execution sandboxing** — restricted filesystem, network, CPU/memory
- **Creator payouts** — monthly, based on usage from all users

## Technical Stats

| Metric | Value |
|--------|-------|
| Rust crates | 43 |
| Rust tests | 109 suites, all passing |
| Python tests | 873 passing, 5 skipped (torch/3.14) |
| Registered tools | 99 |
| Workflow step types | 9 (set, message, http, tool, llm, if, loop, parallel, workflow) |
| Workflow tests | 40 |
| TUI unit tests | 53 |
| TUI e2e tests | 16 |
| Release binary | 69 MB |
| Release warnings | 0 |
| VRAM (local Gemma 4 12B) | 7 GB (q4_0 K+V, flash attention) |
| Hosted models (MARC27 proxy) | 644 |
| OPTIMADE providers | 20+ (federated) |
| License | MARC27 Source-Available + MIT (components) |

## License

PRISM is licensed under the **MARC27 Source-Available License** v1.0.
Select components are MIT-licensed. See `LICENSE-MARC27` and `LICENSE-MIT`.

## Quick Start

```bash
# Install
curl -fsSL https://prism.marc27.com/install.sh | bash

# Or from source
git clone https://github.com/Darth-Hidious/PRISM.git
cd PRISM && cargo build --release
cp target/release/prism ~/.local/bin/

# First-time setup (creates ~/.prism/, logs in to MARC27)
prism setup

# Launch the TUI
prism

# Or run a one-shot query
prism query --platform "nickel superalloys"
prism research "high-entropy alloys" --depth 1

# Local model (Gemma 4 12B via llama.cpp)
llama-server --model gemma-4-12B-it-qat-UD-Q4_K_XL.gguf --port 8081 --ctx-size 32768 --n-gpu-layers 99 &
prism use local
prism

# Offline mode (no MARC27 platform connection)
prism --offline

# List available tools
prism tools

# Run a workflow
prism workflow discover --goal "high-strength Ti alloy" --execute
```

---

*PRISM is built by MARC27. Every material discovered has full provenance.*