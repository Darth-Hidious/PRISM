<p align="center">
  <img src="docs/assets/prism-banner.png" alt="PRISM Banner" width="800">
</p>

<h1 align="center">PRISM</h1>
<p align="center"><strong>AI-Native Autonomous Materials Discovery Platform</strong></p>
<p align="center">
  <em>MARC27</em>
</p>
<p align="center">
  <code>v2.6.1</code> &nbsp;|&nbsp; <code>16 Rust crates</code> &nbsp;|&nbsp; <code>494 Rust tests + 551 Python tests</code> &nbsp;|&nbsp; <code>49 tools</code>
</p>

<p align="center">
  <a href="#quick-start">Quick Start</a> &bull;
  <a href="#architecture">Architecture</a> &bull;
  <a href="#commands">Commands</a> &bull;
  <a href="#capabilities">Capabilities</a> &bull;
  <a href="#license">License</a>
</p>

---

PRISM is an open-source, enterprise-deployable data platform node that connects
to the MARC27 network. One command &mdash; `prism node up` &mdash; bootstraps a
complete local data infrastructure that automatically converts raw datasets into
structured, queryable knowledge (ontologies, knowledge graphs, vector
embeddings). Running nodes can subscribe to each other, creating a federated
mesh of searchable data across departments, organizations, and MARC27's cloud.

## Quick Start

```bash
# Install
curl -fsSL https://prism.marc27.com/install.sh | bash

# Or via Homebrew
brew install marc27/tap/prism

# Authenticate with MARC27 platform
prism login

# Start local node (launches Neo4j + Qdrant containers)
prism node up

# Ingest materials data
prism ingest ./alloys.csv

# Query the knowledge graph
prism query "refractory alloys with hardness above 500 HV"
prism query --cypher "MATCH (a:Alloy)-[:CONTAINS]->(e:Element) RETURN a.name, e.name"
prism query --semantic "similar to Ti-6Al-4V"
prism query --federated "high entropy alloys"   # queries all mesh peers
```

## Architecture

PRISM compiles to two Rust binaries backed by a Python intelligence layer:

```
┌───────────────────────────────────────────────────────────────┐
│                         PRISM NODE                            │
│                                                               │
│  Rust Core                                                    │
│  ├── prism-cli ········· command routing, auth, config        │
│  ├── prism-node ········ daemon, probe, executor, E2EE        │
│  ├── prism-core ········ RBAC, audit, sessions, config        │
│  ├── prism-ingest ······ LLM ontology → Neo4j + Qdrant        │
│  ├── prism-server ······ Axum REST API + WebSocket + dashboard│
│  ├── prism-orch ········ Docker container lifecycle            │
│  ├── prism-mesh ········ mDNS + Kafka pub/sub + federation    │
│  ├── prism-compute ····· Docker/MARC27/SSH/K8s/SLURM jobs     │
│  ├── prism-policy ······ OPA/Rego policy engine               │
│  └── prism-workflows ··· YAML workflow engine                 │
│                                                               │
│  Managed Services (Docker containers or external)             │
│  ├── Neo4j ············· knowledge graph                      │
│  ├── Qdrant ············ vector embeddings                    │
│  ├── Kafka ············· mesh data sync (optional)            │
│  └── Spark ············· large-scale ETL (optional)           │
│                                                               │
│  Rust Agent (TAOR — Think-Act-Observe-Repeat)                 │
│  ├── prism-agent ······· agent loop, sessions, permissions    │
│  ├── OPA policy ········ every tool call checked via Rego     │
│  ├── SSE streaming ····· real-time token display              │
│  └── Approval flow ····· allow/deny/allow-all per tool call   │
│                                                               │
│  Python Tools (49 tools via JSON stdio)                       │
│  ├── 49 tools ·········· search, predict, simulate, visualize │
│  ├── Spark ETL ········· large-scale data processing          │
│  └── Custom plugins ···· drop .py in ~/.prism/tools/          │
│                                                               │
│  Rendering                                                    │
│  ├── Ink TUI ··········· TypeScript/React terminal UI         │
│  └── Web Dashboard ····· React SPA embedded in Axum           │
└───────────────────────────────────────────────────────────────┘
```

## Commands

```bash
# Node lifecycle
prism node up                    # Start services + register with platform
prism node up --external-neo4j bolt://db:7687  # Use existing infrastructure
prism node down                  # Graceful shutdown
prism node status                # Health check
prism node logs neo4j            # Stream container logs

# Data
prism ingest ./data.csv          # CSV/Parquet → schema → LLM → graph → embeddings
prism ingest --watch ./data/     # Watch directory for new files
prism ingest --llm-provider marc27 --llm-url https://platform.marc27.com/api/v1/llm

# Query
prism query "..."                # Natural language → Cypher
prism query --cypher "MATCH..."  # Direct Cypher
prism query --semantic "..."     # Vector similarity
prism query --federated "..."    # Query all mesh peers

# Mesh networking
prism mesh discover              # Find LAN nodes via mDNS
prism mesh publish my-dataset    # Publish dataset to mesh
prism mesh subscribe alloy-db --publisher <node-id>
prism mesh subscriptions         # Show active subscriptions

# Compute
prism run python:3.11 --input data=x.csv   # Local Docker job
prism run marc27/calphad:latest --backend marc27  # Cloud compute
prism job-status <uuid>

# Workflows
prism forge --paper arxiv:2106.09685 --dataset materials-project --target runpod:A100
prism workflow list

# Agent (Python TAOR)
prism                            # Interactive REPL (Ink TUI)
prism serve                      # Start as MCP server
```

## Capabilities

### Ingestion Pipeline (verified end-to-end)

```
CSV/Parquet → Schema Detection → LLM Entity Extraction → Neo4j Graph → Qdrant Embeddings
                                 (any provider)           (96 nodes)    (768-dim vectors)
```

Supports any LLM provider: MARC27 managed, Ollama (local), OpenAI, vLLM, LiteLLM.

### 49 Tools

| Category | Tools | Status |
|----------|-------|--------|
| Materials search (20+ OPTIMADE providers) | search_materials, query_materials_project | Working |
| Web browsing | web_read, web_search | Working |
| ML prediction | predict_property, predict_structure, list_models | Working |
| CALPHAD thermodynamics | phase diagrams, equilibrium, Gibbs energy | Requires pycalphad |
| Visualization | scatter, histogram, correlation matrix | Working |
| Code execution | execute_python (sandboxed, OPA-gated) | Working |
| Data I/O | import, export, read_file, write_file | Working |
| Spark ETL | spark_submit_job, spark_batch_transform, spark_status | Requires pyspark |
| MARC27 Knowledge Graph | graph search, entity lookup, paths, semantic search | Requires `prism login` |
| MARC27 Compute | GPU job submission, cost estimation | Requires `prism login` |
| Literature/Patent search | arXiv, Semantic Scholar, Lens.org | Working |
| Custom plugins | any .py in ~/.prism/tools/ auto-discovered | Working |

### Security

- **OPA/Rego policy engine**: role-based tool/workflow authorization
- **E2EE**: X25519 key agreement + ChaCha20-Poly1305
- **RBAC**: 4-tier local roles (admin, operator, agent, viewer)
- **Audit logging**: every action logged to SQLite
- **Rate limiting**: per-endpoint (sessions: 10/s, API: 100/s)

### Mesh Federation

- **mDNS discovery**: zero-config LAN (`_prism._tcp.local`)
- **Platform discovery**: cross-org via platform.marc27.com
- **Kafka data sync**: published datasets replicate to subscribers
- **Federated queries**: `--federated` queries all peers in parallel

## Configuration

PRISM reads `prism.toml` from `~/.prism/prism.toml` (global) and `.prism/prism.toml` (project):

```toml
[node]
name = "lab-alpha"
port = 7327

[services]
mode = "managed"   # or "external"
neo4j_uri = "bolt://db.internal:7687"

[indexer]
mode = "platform"  # MARC27 cloud LLM (default)
model = "claude-sonnet-4-6"

[searcher]
mode = "managed"   # local Ollama
model = "qwen2.5:7b"

[calphad]
mode = "local"
databases = ["thermodynamics/steel.tdb", "thermodynamics/ni-alloys.tdb"]

[mesh]
discovery = ["mdns", "platform"]
```

CLI flags always override config file values.

## Testing

```bash
cargo test --workspace          # 494 Rust tests
cargo clippy --workspace        # Lint
python3 -m pytest tests/ -q     # 551 Python tests
cd frontend && npx tsc --noEmit # TypeScript check
```

## License

PRISM uses a **dual license**:

| Component | License |
|-----------|---------|
| Python tools (`app/tools/`), tool server, config, plugins, tests | [MIT](LICENSE-MIT) |
| Rust crates, frontend, install, CI, agent, workflows | [MARC27 Source-Available](LICENSE-MARC27) |

See [LICENSE](LICENSE) for details. Commercial licensing: team@marc27.com

---

<p align="center">
  <img src="docs/assets/marc27-logo.svg" alt="MARC27" height="40">
</p>
