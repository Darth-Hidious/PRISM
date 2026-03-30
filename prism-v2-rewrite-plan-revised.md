# PRISM V2 — The First Galactic Empire

**Codename:** Order 66  
**Target Skeleton:** March 27, 2026  
**Author:** MARC27 Research Project  
**Status:** Planning — Revision 3 (Final)  

---

## 1. What PRISM V2 Actually Is

PRISM is an **open-source, enterprise-deployable data platform node** that connects to the MARC27 network. One command — `prism node up` — bootstraps a complete local data infrastructure that automatically converts raw datasets into structured, queryable knowledge (ontologies, knowledge graphs, vector embeddings). Once running, a PRISM node can subscribe to other PRISM nodes, creating a federated mesh of searchable data across departments, organizations, and MARC27's cloud.

### The Core Loop

```
Raw Data (CSVs, databases, files)
        │
        ▼
  ┌─────────────┐
  │  Ingestion   │ ← Kafka (streaming + batch)
  └──────┬──────┘
         ▼
  ┌─────────────┐
  │ Transformat. │ ← Spark (ETL, normalization, feature extraction)
  └──────┬──────┘
         ▼
  ┌─────────────┐
  │  Structuring │ ← LLM pipeline (auto-ontology construction)
  └──────┬──────┘
         │
    ┌────┴────┐
    ▼         ▼
┌───────┐ ┌────────┐
│Neo4j  │ │Vector  │ ← Knowledge graph + semantic embeddings
│(graph)│ │DB      │
└───┬───┘ └───┬────┘
    └────┬────┘
         ▼
  ┌─────────────┐
  │  Queryable   │ ← CLI, Web Dashboard, API, inter-node subscriptions
  │  Knowledge   │
  └─────────────┘
```

Your internal data becomes a searchable, subscribable knowledge base. Other nodes on the network (other departments, partner orgs, MARC27 cloud services) can subscribe to your published datasets and vice versa.

### Who Sees What

| Persona | Surface | Sees |
|---|---|---|
| **Project Manager** | Web dashboard (intranet) | Node status, indexed datasets, query activity, subscription health, audit logs |
| **Computational Engineer** | CLI/TUI | Ontology internals, graph structure, embedding quality, transformation pipelines, raw tool output |
| **Tooling / Extension Dev** | Python tool layer + CLI | Custom transformation pipelines, new data connectors, analysis tools |
| **Admin / DevOps** | CLI + dashboard | Node config, container health, role management, inter-node connections, resource usage |

All personas hit the same running PRISM instance. The Rust binary serves all of them through different access layers with role-based visibility.

---

## 2. Architecture

### 2.1 System Overview

```
┌─────────────────────────────────────────────────────────────────────┐
│                        PRISM NODE                                   │
│                                                                     │
│  ┌───────────────────── RUST CORE BINARY ─────────────────────────┐│
│  │                                                                 ││
│  │  ┌─────────────┐ ┌──────────────┐ ┌───────────────────────┐   ││
│  │  │ prism-core  │ │ prism-client │ │ prism-server          │   ││
│  │  │ • sessions  │ │ • platform   │ │ • Axum HTTP/WS        │   ││
│  │  │ • config    │ │   API client │ │ • embedded dashboard  │   ││
│  │  │ • RBAC      │ │ • auth       │ │ • REST API            │   ││
│  │  │ • audit log │ │ • marketplace│ │ • role-gated routes   │   ││
│  │  └─────────────┘ └──────────────┘ └───────────────────────┘   ││
│  │                                                                 ││
│  │  ┌──────────────┐ ┌──────────────┐ ┌───────────────────────┐  ││
│  │  │ prism-orch   │ │ prism-mesh   │ │ prism-runtime         │  ││
│  │  │ • container  │ │ • node       │ │ • Python tool         │  ││
│  │  │   lifecycle  │ │   discovery  │ │   executor            │  ││
│  │  │ • health     │ │ • mDNS local │ │ • venv management     │  ││
│  │  │   checks     │ │ • platform   │ │ • sandboxing          │  ││
│  │  │ • connect to │ │   cross-org  │ │ • streaming I/O       │  ││
│  │  │   existing   │ │ • pub/sub    │ │                       │  ││
│  │  └──────────────┘ └──────────────┘ └───────────────────────┘  ││
│  │                                                                 ││
│  │  ┌──────────────┐ ┌──────────────┐                             ││
│  │  │ prism-ipc    │ │ prism-ingest │                             ││
│  │  │ • JSON-RPC   │ │ • ontology   │                             ││
│  │  │   for TUI    │ │   pipeline   │                             ││
│  │  │ • stdin/out  │ │ • LLM-driven │                             ││
│  │  │   + WS push  │ │   structuring│                             ││
│  │  └──────────────┘ └──────────────┘                             ││
│  └─────────────────────────────────────────────────────────────────┘│
│                                                                     │
│  ┌───────────── MANAGED SERVICES (containers or external) ────────┐│
│  │                                                                 ││
│  │  ┌─────────┐  ┌─────────┐  ┌─────────┐  ┌──────────────────┐ ││
│  │  │  Kafka  │  │  Spark  │  │  Neo4j  │  │  Vector DB       │ ││
│  │  │ (stream │  │ (ETL,   │  │ (know-  │  │  (Qdrant/Milvus/ │ ││
│  │  │  ingest,│  │  batch  │  │  ledge  │  │   Weaviate)      │ ││
│  │  │  pub/sub│  │  proc.) │  │  graph) │  │                  │ ││
│  │  └─────────┘  └─────────┘  └─────────┘  └──────────────────┘ ││
│  │                                                                 ││
│  │  Mode A: PRISM manages Docker/Podman containers                ││
│  │  Mode B: Connect to existing instances (user-provided URLs)    ││
│  └─────────────────────────────────────────────────────────────────┘│
│                                                                     │
│  ┌───────────── RENDERING LAYER ──────────────────────────────────┐│
│  │  ┌──────────────────┐        ┌──────────────────────────────┐ ││
│  │  │  Ink.js TUI      │        │  Web Dashboard (embedded SPA)│ ││
│  │  │  (engineer view) │        │  (PM / admin / browser view) │ ││
│  │  └──────────────────┘        └──────────────────────────────┘ ││
│  └─────────────────────────────────────────────────────────────────┘│
│                                                                     │
│  ┌───────────── PYTHON TOOLS LAYER ───────────────────────────────┐│
│  │  ml-pipeline │ optimade │ mcp-bridge │ kg-query │ custom      ││
│  └─────────────────────────────────────────────────────────────────┘│
└──────────────────────────────┬──────────────────────────────────────┘
                               │
              ┌────────────────┼────────────────┐
              ▼                ▼                ▼
     ┌──────────────┐  ┌─────────────┐  ┌──────────────┐
     │ platform.    │  │ Other PRISM │  │ Other PRISM  │
     │ marc27.com   │  │ nodes       │  │ nodes        │
     │ (cloud,      │  │ (local net, │  │ (cross-org,  │
     │  registry,   │  │  mDNS       │  │  platform-   │
     │  marketplace)│  │  discovery) │  │  mediated)   │
     └──────────────┘  └─────────────┘  └──────────────┘
```

### 2.2 New Crates (vs. Revision 1)

**`prism-orch` — Container Orchestration**

Manages the lifecycle of Kafka, Spark, Neo4j, and the vector DB. Two modes:

- **Managed mode** (default): Pulls Docker/Podman images, starts containers with correct networking, monitors health, restarts on failure. PRISM owns the lifecycle.
- **External mode**: User provides connection URIs (`--kafka-broker kafka://existing:9092 --neo4j-uri bolt://existing:7687`). PRISM connects but doesn't manage. For orgs that already have infrastructure.

The orchestrator uses Docker Engine API (via bollard crate) or Podman's compatible API. No dependency on docker-compose or Kubernetes — PRISM manages containers directly. This keeps it simple and avoids requiring users to learn orchestration tools.

```rust
// prism-orch API surface (simplified)
pub trait ServiceOrchestrator {
    async fn start_all(&self, config: &NodeConfig) -> Result<ServiceHandles>;
    async fn stop_all(&self, handles: &ServiceHandles) -> Result<()>;
    async fn health_check(&self, handles: &ServiceHandles) -> HealthReport;
    async fn connect_external(&self, uris: &ExternalServices) -> Result<ServiceHandles>;
}
```

**`prism-mesh` — Node Discovery & Subscription**

Handles inter-node communication. Two discovery mechanisms:

- **Local network (mDNS/DNS-SD)**: Nodes announce themselves via `_prism._tcp.local`. Other nodes on the same LAN discover them automatically. Zero config for lab/department setups.
- **Platform-mediated (cross-org)**: Nodes register with platform.marc27.com. Cross-organization subscriptions go through the platform as a relay/registry. The platform handles auth, ACLs, and connection brokering. Data flows directly between nodes once the connection is established (platform is control plane, not data plane).

Subscription model:

```
Node A publishes: "alloy-compositions" dataset (ontology + embeddings)
Node B subscribes: gets a Kafka topic that streams updates
Node B's Neo4j gets a federated view: local graph + subscribed remote graphs
```

The pub/sub rides on Kafka. Each published dataset becomes a Kafka topic. Subscribing nodes consume that topic and integrate the data into their local stores. Schema compatibility is enforced through the ontology contract.

**`prism-ingest` — The Ontology Pipeline**

This is the core intelligence layer. Takes raw data and produces structured knowledge.

```
Input: CSV/Parquet/DB dump/API stream
  │
  ├─ Schema detection (column types, relationships, units)
  ├─ Entity extraction (LLM-driven: what are the materials, properties, processes?)
  ├─ Ontology mapping (map extracted entities to a materials science ontology)
  ├─ Graph construction (entities → Neo4j nodes, relationships → edges)
  ├─ Embedding generation (text/property vectors → vector DB)
  └─ Validation (consistency checks, coverage metrics)
  │
Output: Queryable knowledge graph + semantic search index
```

**Ontology pipeline strategy — UNDECIDED:**

Two approaches, to be resolved before implementation:

| Approach | Pros | Cons |
|---|---|---|
| **Simple LLM pipeline** — GPT-4/Claude extracts entities and relationships, maps to a predefined schema, populates graph | Ships fast. Works "well enough" for 80% of tabular materials data. Easy to debug. | Fragile on novel data. Quality depends on prompt engineering. No physics grounding. |
| **DMMS + Riemannian Flow Matching** — Research-grade approach using manifold learning to discover latent structure in materials data | Physics-grounded. Can discover relationships that LLMs miss. Differentiable. | Not production-ready yet. Requires the SPARK dataset validation. Months of R&D. |
| **Hybrid** — LLM pipeline as default, DMMS as an optional advanced mode activated per-dataset | Best of both. Ship fast, upgrade in place. | Two codepaths to maintain. Need clean interface boundary. |

**Recommendation:** Ship with the LLM pipeline. Design the `prism-ingest` crate's trait boundary so that the ontology construction step is swappable. When DMMS matures, it slots in behind the same interface. The rest of the system (graph storage, embedding, query, subscription) doesn't care which engine built the ontology.

```rust
// prism-ingest: pluggable ontology construction
pub trait OntologyConstructor: Send + Sync {
    async fn analyze_schema(&self, source: &DataSource) -> Result<SchemaAnalysis>;
    async fn extract_entities(&self, source: &DataSource, schema: &SchemaAnalysis) -> Result<EntitySet>;
    async fn build_graph(&self, entities: &EntitySet) -> Result<GraphUpdate>;
    async fn generate_embeddings(&self, entities: &EntitySet) -> Result<EmbeddingBatch>;
}

// V1: LLM-based implementation
pub struct LlmOntologyConstructor { /* ... */ }

// Future: DMMS-based implementation
// pub struct DmmsOntologyConstructor { /* ... */ }
```

### 2.3 RBAC Model

**Design Constraint — Hardware Extensibility (Future):**

The PRISM node is designed as a persistent, addressable beacon on the MARC27 network. In V2, it serves data, queries, and compute. In the future, the same endpoint will accept connections from physical hardware — robotic arms, furnaces, characterization instruments, tensile testers. `prism forge` evolves from "plan experiments digitally" to "execute experiments physically" through connected lab equipment. This means:

- `prism-server`'s Axum router must support dynamic route registration so a future `prism-control` crate can add hardware endpoints without refactoring.
- The node's identity on the network (its beacon) must be stable and authenticated — hardware can't reconnect to a node whose address changes on restart.
- The `ComputeBackend` trait in `prism-compute` is already the right abstraction — a robot arm is just another backend that "runs an experiment and returns results."
- This is NOT V2 scope. But no V2 decision should block it.

Two-layer role system:

**Platform roles** (managed on platform.marc27.com):
- Govern access to cloud features: marketplace, managed LLM keys, cross-org subscriptions
- Synced to local node on login
- Standard: `owner`, `admin`, `member`, `viewer`

**Local roles** (managed by node admin via CLI or dashboard):
- Govern access to node features: which datasets, which tools, which views
- Stored in local SQLite
- Configurable per org, but defaults:

| Local Role | CLI Access | Dashboard Access | Tool Execution | Node Config | Data Publish |
|---|---|---|---|---|---|
| `node-admin` | Full | Full | Yes | Yes | Yes |
| `engineer` | Full | Read + Execute | Yes | No | Yes (own datasets) |
| `analyst` | Limited (query only) | Full read | Approved tools only | No | No |
| `viewer` | None | Read-only dashboard | No | No | No |

Auth flow:
1. User runs `prism login` or hits the dashboard login page
2. PRISM authenticates against platform.marc27.com (device flow or OAuth)
3. Platform returns user identity + platform roles
4. PRISM checks local role mapping (platform user → local role)
5. If no local mapping exists, node admin must approve and assign a local role
6. Session token issued with combined permissions

**Audit logging:** Every action (tool execution, data query, config change, subscription event) gets logged with user identity, timestamp, action, and result. Stored locally in SQLite. Exportable. This is non-negotiable for ESA/defense customers.

---

## 3. Repository Structure

```
prism/
├── Cargo.toml                          # Rust workspace root
├── Cargo.lock
├── package.json                        # pnpm workspace root
├── pnpm-workspace.yaml
├── docker/                             # Dockerfiles for managed services
│   ├── docker-compose.dev.yml          # Dev mode: all services local
│   ├── kafka/
│   │   └── Dockerfile                  # Custom Kafka config if needed
│   ├── neo4j/
│   │   └── Dockerfile                  # With APOC + GDS plugins
│   └── vector-db/
│       └── Dockerfile                  # Qdrant or chosen vector DB
│
├── crates/                             # ── Rust workspace members ──
│   ├── prism-core/                     # Domain logic, config, sessions, RBAC
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── config.rs               # Node config (TOML-based)
│   │       ├── session.rs              # Multi-user session management
│   │       ├── rbac.rs                 # Role-based access control engine
│   │       ├── audit.rs                # Audit log (append-only, SQLite-backed)
│   │       ├── registry.rs             # Tool registry, manifest parsing
│   │       └── workflow.rs             # Workflow/pipeline engine
│   │
│   ├── prism-client/                   # Platform API client (embedded SDK)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── api.rs                  # Typed HTTP client (reqwest)
│   │       ├── auth.rs                 # Device flow, token refresh
│   │       ├── marketplace.rs          # Browse, install, publish
│   │       ├── node_registry.rs        # Register/discover nodes via platform
│   │       └── models.rs              # Shared types
│   │
│   ├── prism-orch/                     # Container orchestration
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── docker.rs               # Docker/Podman API (bollard)
│   │       ├── services.rs             # Service definitions (Kafka, Spark, Neo4j, VectorDB)
│   │       ├── health.rs               # Health checks, restart logic
│   │       ├── external.rs             # Connect to user-provided instances
│   │       └── config.rs               # Port mappings, resource limits, image versions
│   │
│   ├── prism-mesh/                     # Node discovery & subscription
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── mdns.rs                 # Local network discovery (mDNS/DNS-SD)
│   │       ├── platform_discovery.rs   # Cross-org via platform.marc27.com
│   │       ├── subscription.rs         # Pub/sub management (Kafka topics)
│   │       ├── federation.rs           # Federated graph queries across nodes
│   │       └── protocol.rs            # Inter-node communication spec
│   │
│   ├── prism-ingest/                   # Data ingestion & ontology pipeline
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── pipeline.rs             # Orchestrates the full ingestion flow
│   │       ├── schema.rs               # Schema detection & analysis
│   │       ├── ontology.rs             # OntologyConstructor trait + LLM impl
│   │       ├── graph.rs                # Neo4j graph construction (bolt protocol)
│   │       ├── embeddings.rs           # Vector embedding generation
│   │       ├── connectors/             # Data source connectors
│   │       │   ├── mod.rs
│   │       │   ├── csv.rs
│   │       │   ├── parquet.rs
│   │       │   ├── postgres.rs
│   │       │   └── api.rs              # REST/GraphQL source ingestion
│   │       └── validation.rs           # Ontology consistency checks
│   │
│   ├── prism-runtime/                  # Python tool execution engine
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── executor.rs             # Subprocess management
│   │       ├── venv.rs                 # uv-managed virtual environments
│   │       ├── discovery.rs            # Tool manifest scanning
│   │       ├── sandbox.rs              # Resource limits, isolation
│   │       └── protocol.rs             # Tool ↔ runtime JSON communication
│   │
│   ├── prism-server/                   # Embedded web server
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── router.rs               # Axum route definitions
│   │       ├── middleware/
│   │       │   ├── mod.rs
│   │       │   ├── auth.rs             # Session token validation
│   │       │   └── rbac.rs             # Role-gated route middleware
│   │       ├── handlers/
│   │       │   ├── mod.rs
│   │       │   ├── node.rs             # Node status, health, config
│   │       │   ├── data.rs             # Dataset management, ingestion triggers
│   │       │   ├── query.rs            # Graph + semantic queries
│   │       │   ├── tools.rs            # Tool execution, listing
│   │       │   ├── mesh.rs             # Node discovery, subscriptions
│   │       │   ├── users.rs            # User management, role assignment
│   │       │   └── audit.rs            # Audit log access
│   │       ├── ws.rs                   # WebSocket (live updates, streaming)
│   │       └── static_assets.rs        # Embedded SPA (rust-embed)
│   │
│   ├── prism-ipc/                      # JSON-RPC for TUI communication
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── server.rs
│   │       ├── methods.rs
│   │       └── types.rs
│   │
│   └── prism-cli/                      # Main binary entry point
│       ├── Cargo.toml
│       └── src/
│           ├── main.rs
│           ├── commands/
│           │   ├── mod.rs
│           │   ├── node.rs             # `prism node up/down/status/logs`
│           │   ├── ingest.rs           # `prism ingest <source>`
│           │   ├── query.rs            # `prism query "find all NbMoTaW alloys with..."`
│           │   ├── subscribe.rs        # `prism subscribe <node>/<dataset>`
│           │   ├── publish.rs          # `prism publish <dataset>`
│           │   ├── run.rs              # `prism run <tool> <command>`
│           │   ├── install.rs          # `prism install <package>`
│           │   ├── login.rs            # `prism login`
│           │   ├── dashboard.rs        # `prism dashboard` (opens browser)
│           │   ├── users.rs            # `prism users add/remove/role`
│           │   ├── config.rs           # `prism config`
│           │   └── workflow.rs         # `prism workflow <name>`
│           └── bootstrap.rs            # Startup: init services, spawn TUI
│
├── tui/                                # ── Ink.js Full TUI ──
│   ├── package.json
│   ├── tsconfig.json
│   └── src/
│       ├── index.tsx
│       ├── rpc/
│       │   ├── client.ts               # JSON-RPC over stdin/stdout
│       │   └── types.ts
│       ├── components/
│       │   ├── App.tsx                 # Root: command router
│       │   ├── NodeStatus.tsx          # Service health, resource usage
│       │   ├── Repl.tsx                # Interactive query REPL
│       │   ├── IngestProgress.tsx      # Live ingestion pipeline view
│       │   ├── GraphExplorer.tsx       # Text-mode graph exploration
│       │   ├── MeshView.tsx            # Connected nodes, subscriptions
│       │   ├── ToolOutput.tsx          # Streaming tool execution
│       │   ├── Table.tsx
│       │   ├── StatusBar.tsx
│       │   └── Marketplace.tsx
│       ├── hooks/
│       │   ├── useRpc.ts
│       │   ├── useSession.ts
│       │   └── useStreaming.ts
│       └── theme/
│           └── colors.ts               # MARC27 branding
│
├── dashboard/                          # ── Embedded Web Dashboard ──
│   ├── package.json
│   ├── vite.config.ts
│   └── src/
│       ├── main.tsx
│       ├── api/
│       │   └── client.ts               # HTTP/WS to local prism-server
│       ├── pages/
│       │   ├── Overview.tsx            # Node status, activity feed
│       │   ├── Datasets.tsx            # Ingested data, ontology browser
│       │   ├── Graph.tsx               # Interactive knowledge graph viewer
│       │   ├── Query.tsx               # Natural language + Cypher query interface
│       │   ├── Mesh.tsx                # Connected nodes, subscription management
│       │   ├── Tools.tsx               # Tool catalog, execution UI
│       │   ├── Workflows.tsx           # Pipeline editor
│       │   ├── Users.tsx               # Role management (admin only)
│       │   ├── Audit.tsx               # Audit log viewer
│       │   └── Settings.tsx            # Node config
│       └── components/
│           └── ...
│
├── tools/                              # ── Python Tools ──
│   ├── README.md
│   ├── tool-template/
│   │   ├── pyproject.toml
│   │   ├── manifest.json
│   │   └── src/main.py
│   ├── ml-pipeline/
│   ├── structure-analysis/
│   ├── optimade-client/
│   ├── mcp-bridge/
│   └── kg-query/
│
├── sdk/                                # ── Python SDK (marc27-sdk) ──
│   ├── pyproject.toml                  # For external users scripting against platform
│   └── src/marc27/
│       ├── __init__.py
│       ├── client.py
│       ├── auth.py
│       └── models.py
│
├── ontology/                           # ── Ontology Schemas ──
│   ├── materials-science.owl           # Base ontology (OWL format)
│   ├── mappings/                       # Domain-specific mapping rules
│   │   ├── alloys.yaml
│   │   ├── ceramics.yaml
│   │   └── composites.yaml
│   └── validation/                     # SHACL shapes for graph validation
│       └── core-shapes.ttl
│
├── proto/                              # ── Protocol Definitions ──
│   ├── rpc-methods.json                # TUI ↔ Core JSON-RPC catalog
│   ├── tool-manifest.schema.json       # Tool manifest JSON Schema
│   ├── tool-protocol.md                # Tool ↔ Runtime I/O spec
│   ├── mesh-protocol.md                # Inter-node communication spec
│   └── subscription.schema.json        # Pub/sub contract schema
│
├── scripts/
│   ├── build.sh
│   ├── dev.sh
│   └── release.sh
│
├── .github/workflows/
│   ├── ci.yml
│   └── release.yml
│
└── docs/
    ├── architecture.md
    ├── deployment-guide.md             # Enterprise deployment (air-gapped, etc.)
    ├── tool-development.md
    ├── ontology-guide.md               # How auto-ontology works, how to customize
    ├── mesh-networking.md              # Node discovery, subscription model
    └── api-reference.md
```

---

## 4. Command Surface

The full set of commands PRISM V2 exposes. Every command works through the Ink.js TUI. Every command has a corresponding REST endpoint on the embedded server (so the dashboard can do everything the CLI can).

### Node Management
```bash
prism node up                           # Start all services (Kafka, Spark, Neo4j, VectorDB)
prism node up --external-kafka ...      # Connect to existing Kafka instead of spinning up
prism node down                         # Graceful shutdown of all services
prism node status                       # Health of all services + resource usage
prism node logs [service]               # Stream logs from a specific service
prism node config                       # View/edit node configuration
```

### Data Ingestion
```bash
prism ingest ./data/alloys.csv          # Ingest a file → auto-ontology
prism ingest postgres://...             # Ingest from a database
prism ingest --watch ./data/            # Watch a directory for new files
prism ingest status                     # Pipeline progress, what's been indexed
prism ingest history                    # Past ingestion runs, what changed
```

### Query
```bash
prism query "NbMoTaW alloys with hardness > 500 HV"    # Natural language query
prism query --cypher "MATCH (a:Alloy)..."               # Direct Cypher
prism query --semantic "similar to Ti-6Al-4V"           # Semantic search
prism query --interactive                                # Open REPL for iterative querying
```

### Mesh / Federation
```bash
prism mesh status                       # Connected nodes, subscription health
prism mesh discover                     # Scan local network for PRISM nodes
prism subscribe <node>/<dataset>        # Subscribe to a remote dataset
prism unsubscribe <node>/<dataset>      # Remove subscription
prism publish <dataset>                 # Make a local dataset available to the mesh
prism unpublish <dataset>               # Stop publishing
```

### Tools
```bash
prism run <tool> <command> [args]       # Execute a Python tool
prism tools list                        # Installed tools
prism tools install <name>              # Install from marketplace
prism tools create <name>               # Scaffold a new tool from template
prism tools publish <name>              # Publish to MARC27 marketplace
```

### Users & Auth
```bash
prism login                             # Authenticate against platform.marc27.com
prism users list                        # List users on this node
prism users add <email> --role engineer # Invite user, assign local role
prism users role <email> <new-role>     # Change a user's local role
prism audit [--user X] [--since Y]      # View audit log
```

### Dashboard & Misc
```bash
prism dashboard                         # Open web dashboard in browser
prism workflow list                     # List defined workflows
prism workflow run <name>               # Execute a workflow
prism config get <key>                  # Read config value
prism config set <key> <value>          # Set config value
prism workflow create <n>            # Scaffold a new workflow YAML
prism config get <key>                  # Read config value
prism config set <key> <value>          # Set config value
prism version                           # Version info
```

### Workflows as Top-Level Commands

Any YAML workflow in `~/.prism/workflows/` automatically becomes a top-level CLI command. No registration, no compilation — drop a YAML file, the command exists.

```bash
# These are equivalent:
prism workflow run analyze-hea ./data/alloys.csv
prism analyze-hea ./data/alloys.csv      # ← promoted to top-level

$ prism --help

PRISM v2.0.0

COMMANDS:
  node, ingest, query, forge, run, subscribe, publish,
  login, dashboard, ...

WORKFLOWS:                                  ← auto-discovered from ~/.prism/workflows/
  analyze-hea     Full HEA analysis pipeline
  screen-alloys   Custom alloy screening
  weekly-report   Generate weekly lab report
```

**Workflow YAML spec:**

```yaml
# ~/.prism/workflows/analyze-hea.yaml
name: analyze-hea
description: "Full HEA analysis pipeline"

args:
  - name: data
    type: file
    required: true
  - name: target
    type: string
    default: "hardness"
  - name: models
    type: list
    default: ["rf", "xgb", "crabnet"]

steps:
  - name: train-models
    tool: ml-pipeline
    command: train
    parallel: true                          # Parallel where DAG allows
    foreach: "{{ args.models }}"
    inputs:
      data: "{{ args.data }}"
      target: "{{ args.target }}"
      model_type: "{{ item }}"

  - name: compare
    tool: ml-pipeline
    command: compare
    depends_on: [train-models]
    inputs:
      models: "{{ steps.train-models.outputs.*.model }}"

  - name: enrich-graph
    tool: kg-query
    command: relate
    depends_on: [compare]
    inputs:
      findings: "{{ steps.compare.outputs.report }}"

  - name: report
    tool: ml-pipeline
    command: report
    depends_on: [compare, enrich-graph]
    outputs:
      format: pdf
```

At startup, `prism-cli` scans the workflows directory and registers each as a dynamic clap subcommand. Args become CLI flags. Tab completion and `--help` work. Indistinguishable from built-in commands.

Workflows are also shareable via the marketplace:
```bash
prism workflow install marc27/analyze-hea   # Install from marketplace
prism analyze-hea ./data/alloys.csv         # Immediately a top-level command
prism workflow publish my-workflow           # Share yours
```

---

## 5. Key Interfaces & Contracts

### 5.1 Node Configuration (`prism.toml`)

```toml
[node]
name = "lab-alpha"                          # Human-readable node name
port = 7327                                 # Embedded server port
data_dir = "/var/prism/data"                # Persistent data directory

[services]
mode = "managed"                            # "managed" | "external"

[services.kafka]
image = "confluentinc/cp-kafka:7.6"
port = 9092
# Or for external:
# uri = "kafka://existing-broker:9092"

[services.spark]
image = "bitnami/spark:3.5"
master_port = 7077
ui_port = 8080

[services.neo4j]
image = "neo4j:5-enterprise"
bolt_port = 7687
http_port = 7474
plugins = ["apoc", "graph-data-science"]

[services.vector_db]
image = "qdrant/qdrant:v1.9"
port = 6333

[platform]
url = "https://platform.marc27.com"
# Credentials stored in OS keychain, not in config file

[mesh]
discovery = ["mdns", "platform"]            # Discovery mechanisms
publish_port = 7328                         # Port for inter-node data exchange

[ontology]
engine = "llm"                              # "llm" | "dmms" (future)
llm_provider = "platform"                   # Use platform-managed keys
# Or:
# llm_provider = "openai"
# llm_api_key_env = "OPENAI_API_KEY"       # BYOK

[auth]
session_timeout = "24h"
require_platform_auth = true                # Must authenticate via platform
allow_local_users = true                    # Admin can create local-only users
```

### 5.2 Inter-Node Subscription Contract

When Node A publishes a dataset, it declares a schema:

```json
{
  "dataset": "refractory-heas",
  "version": "1.2.0",
  "node": "lab-alpha",
  "ontology_version": "materials-science-v3",
  "schema": {
    "entities": ["Alloy", "Element", "MechanicalProperty", "ProcessingRoute"],
    "relationships": ["CONTAINS", "HAS_PROPERTY", "PROCESSED_BY"],
    "embedding_dimensions": 768
  },
  "update_frequency": "on_change",
  "access": "network"
}
```

Subscribing nodes validate schema compatibility before accepting the subscription. If ontology versions mismatch, the subscription negotiates a mapping or rejects.

### 5.3 Tool Manifest, Tool Protocol, JSON-RPC Methods

These remain as specified in Revision 1 (see original plan, Sections 4.1–4.4). No changes — the tool layer is the same regardless of whether PRISM is a single-user CLI or a federated data node.

---

## 6. Model Strategy — The Dual-Brain Architecture

PRISM runs two models with completely separate roles. They never substitute for each other.

### 6.1 Model 1: The Indexer (Fine-tuned Qwen3.5-9B)

**Job:** Embedding generation + ontology construction. That's it.

This model never talks to users. It processes raw data in batch, builds the knowledge structure (graph + vectors), and goes quiet until new data arrives. It runs inside `prism-ingest`.

**What it's fine-tuned for:**

Two distinct capabilities on the same base model:

**Capability A — Domain Embeddings:**

This is where generic approaches fail. Materials data isn't text — it's compositions, processing parameters, property measurements, and the physical relationships between them. A naive text embedding will tell you that "NbMoTaW" and "TiAlV" are similar because they're both alloy strings. A good materials embedding will tell you they're dissimilar because one is a refractory BCC system and the other is a light aerospace alloy — fundamentally different physics.

**The Embedding Problem in Materials Science:**

There are three distinct types of data PRISM needs to embed, each with different requirements:

| Data Type | Example | What Similarity Means | Challenge |
|---|---|---|---|
| **Compositions** | Nb25Mo25Ta25W25 | Chemical similarity, crystal structure family, property class | Numeric fractions + element identities, not natural language |
| **Processing routes** | "MA 20h → HP 1400°C/50MPa" | Similar thermomechanical histories produce similar microstructures | Ordered sequences with continuous parameters |
| **Property descriptions** | "Hardness 542 HV at RT" | Functionally equivalent performance | Units, conditions, measurement methods matter |

A single embedding model that handles all three well doesn't exist off the shelf. The strategy is a **multi-representation approach** with domain-specific encoding at each level.

**Prior Art That Matters:**

**Mat2Vec** (Tshitoyan et al., Nature 2019): Trained Word2Vec on 3.3M materials science abstracts. Produced 200-dimensional element embeddings that capture latent chemical knowledge — e.g., the model could predict undiscovered thermoelectric materials from pure text patterns. These embeddings are still the de facto standard for element representation. Key insight: unsupervised word embeddings from materials literature encode real physical relationships without any labeled property data.

**CrabNet** (Wang et al., npj Comput. Mater. 2021): Uses Mat2Vec element embeddings + fractional encoding + self-attention to predict material properties from composition alone. Outperforms structure-aware models on many benchmarks. Proves that attention over composition-weighted element embeddings captures element-element interactions relevant to properties.

**Pettifor embeddings** (Jan 2026, npj Comput. Mater.): A non-orthogonal element representation based on chemical similarity in the Pettifor scale. Outperforms Mat2Vec, Magpie, CGCNN embeddings, and one-hot on average across 27 CrabNet benchmarks. Retains interpretability — angles between element vectors encode chemical similarity.

**Crystal CLIP** (Chemeleon, PMC 2025): Cross-modal contrastive learning that aligns text embeddings (from MatSciBERT) with graph neural network embeddings of crystal structures. The CLIP-style approach lets you search by text and retrieve structurally similar materials. Key architecture: separate encoders for text and structure, aligned via contrastive loss.

**MatSciBERT / MatBERT**: BERT models pre-trained on materials science corpora. MatBERT improves NER performance by 1-12% over general SciBERT. Domain-specific pre-training measurably helps even for well-established architectures.

**Embedding Architecture for PRISM:**

PRISM doesn't need to invent a new embedding model. It needs to compose existing approaches intelligently for the three data types:

```
Raw Data Row
    │
    ├─── Composition Parser ──────► Element-level Embedding
    │    "Nb25Mo25Ta25W25"          (Mat2Vec or Pettifor, 200d per element)
    │                                     │
    │                               Fractional Encoding
    │                               (CrabNet-style, sine/cosine)
    │                                     │
    │                               Self-Attention Pooling ──► Composition Vector (512d)
    │
    ├─── Processing Encoder ──────► Processing Embedding
    │    "MA 20h, HP 1400°C"        Structured encoding: method token +
    │                               normalized continuous params
    │                               MLP → Processing Vector (256d)
    │
    ├─── Property Encoder ────────► Property Embedding
    │    "Hardness 542 HV"          Property type token + normalized value
    │                               + condition tokens
    │                               MLP → Property Vector (256d)
    │
    ├─── Text Encoder (LLM) ──────► Contextual Embedding
    │    Free-text descriptions,     Fine-tuned Qwen3.5-9B last hidden state
    │    paper abstracts, notes      mean-pooled → Text Vector (768d)
    │
    └─── Fusion Layer ────────────► Final Record Embedding
         Learned projection of       MLP + LayerNorm → (768d)
         concatenated sub-vectors     Stored in Qdrant
         [512 + 256 + 256 + 768]     Used for semantic search
```

**Why this multi-encoder approach:**

The fundamental problem with using a single LLM to embed everything is that LLMs are bad at numerical reasoning. "Hardness 542 HV" and "Hardness 540 HV" should be nearly identical vectors. An LLM might or might not make them close — it depends on tokenization accidents. The structured encoders (composition, processing, property) handle numbers properly through normalization and continuous encoding. The LLM handles free text properly through its language understanding. The fusion layer learns how to weight and combine them.

**Dimension Selection:**

| Level | Dimensions | Rationale |
|---|---|---|
| Element embedding | 200d (Mat2Vec) or 128d (Pettifor) | Established, proven dimensionality for element identity. No reason to change what works. |
| Composition vector | 512d | After attention pooling over elements. Enough to distinguish complex multi-component alloys. Matches findings that 384-768d is the sweet spot for domain-specific retrieval. |
| Processing vector | 256d | Lower dimensionality — processing routes have fewer degrees of freedom than compositions. |
| Property vector | 256d | Same reasoning. Properties are simpler to represent than compositions. |
| Text vector | 768d | Qwen3.5-9B's hidden dimension is 4096, but mean-pooled last hidden state projected to 768d. Research consistently shows 768d captures semantic structure without waste for domain-specific tasks. |
| **Final fused vector (LOCAL)** | **768d** | Total input is 1792d (512+256+256+768), projected down to 768d via learned linear + LayerNorm. Matryoshka-trained: truncatable to 384d or 256d for edge nodes. |
| **Final fused vector (PLATFORM)** | **3,072d** | Same fusion architecture extended with image/audio/video encoders. Unified multimodal space — any file type embedded alongside text and tabular data. This is the paid tier's advantage. |

768d as the local node dimension is deliberate — research shows the retrieval accuracy curve flattens between 768-1024 for domain-specific tasks at the dataset sizes typical for on-prem deployments. At 768d with float32, 1M records = ~3GB in Qdrant — manageable on any server. The platform operates at 3,072d because at aggregate scale (millions of records, cross-user knowledge accumulation), the additional dimensions capture meaningful distinctions that compound.

**Contrastive Training Strategy:**

The fusion layer is trained with a multi-positive contrastive loss (InfoNCE variant). Training signal:

```
Anchor:   Nb25Mo25Ta25W25, MA 20h, HP 1400°C, Hardness 542 HV
Positive: Nb25Mo25Ta25W25, MA 25h, HP 1350°C, Hardness 528 HV
          (same system, similar processing, similar property)
Positive: Mo25Nb25Ta25W25, MA 20h, HP 1400°C, Hardness 537 HV
          (same composition written differently)
Negative: Ti-6Al-4V, SLM 1100°C, UTS 1050 MPa
          (completely different material system, process, property type)
Hard Neg: Nb25Mo25Ta25W25, MA 20h, HP 1400°C, Thermal_Cond 23 W/mK
          (same material, same processing, but DIFFERENT property — 
           should be similar in composition space, different in property space)
```

Hard negatives are critical. Without them, the model learns trivial "different material = different vector" and doesn't learn the fine-grained distinctions within a material system.

**What NOT to Do:**

- **Don't use OpenAI / Cohere / generic embeddings.** They don't understand that "Nb" and "niobium" are the same thing, that "HV" is Vickers hardness, or that composition fractions are meaningful numbers. You'll get mediocre retrieval that occasionally works by accident.
- **Don't train a single end-to-end embedding model on concatenated text.** Turning "Nb25Mo25Ta25W25, 20h MA, 1400°C HP, 542 HV" into a text string and feeding it to an LLM throws away all the structural information. The LLM doesn't know that 1400 is a temperature in Celsius and 542 is a hardness in Vickers — they're just tokens.
- **Don't use Mat2Vec alone.** Mat2Vec is great for element identity but it's Word2Vec — it doesn't understand compositions (alloy systems), only individual words/elements. You need the CrabNet-style attention pooling on top to go from element vectors to composition vectors.
- **Don't over-dimension on local nodes.** 3072d embeddings for a local materials database of 10K-100K records is overkill for on-prem. 768d with domain-specific training will outperform 3072d generic embeddings on materials retrieval. Save the 3072d for the platform (see below).

**Two-Tier Embedding Strategy:**

This is critical. Local PRISM nodes and platform.marc27.com operate at different embedding tiers. The gap between them is the monetization.

| | Local PRISM Node (Free) | MARC27 Platform (Paid) |
|---|---|---|
| **Embedding dimensions** | 768d | 3,072d |
| **Modalities** | Text + tabular data only | **Multimodal** — text, images, audio, video, PDFs, diagrams, any file type in one unified vector space |
| **Cross-modal search** | No — query text, get text | Yes — query an SEM micrograph, get the related paper + dataset + lab recording + processing video |
| **Knowledge graph** | Static — grows only when user ingests new data | **Living** — grows with every query via recursive search-embed-graph loop |
| **Context window** | Limited by local LLM (4K-32K typical for quantized 9B) | ~9-10M effective via recursive deep search |
| **Query behavior** | Single-pass: ask → retrieve → answer | Recursive: search → find → embed → graph → search deeper → repeat |
| **Data sources** | Local datasets + subscribed nodes | Local + subscribed + MARC27 public datasets + internet search (embedded on-the-fly) |
| **Compound improvement** | No — your node serves only you | Yes — every query from every user enriches the shared knowledge graph |

The recursive loop on the platform is the real moat:

```
User query on platform.marc27.com
  │
  ├─► LLM searches knowledge graph + vector DB (3,072d)
  │   Finds partial answer
  │
  ├─► LLM identifies gaps, searches internet / subscribed nodes
  │   Finds new information
  │
  ├─► New information is embedded (3,072d) and added to knowledge graph
  │   Graph is now richer than before this query
  │
  ├─► LLM searches AGAIN with enriched context
  │   Finds deeper connections
  │
  ├─► Repeats until satisfied or iteration limit
  │
  └─► Returns answer
      (Knowledge graph is permanently richer.
       Next user asking a similar question gets better results.)
```

This is a flywheel. The knowledge graph compound-grows with usage. No local PRISM node can replicate this because it doesn't have the aggregate query volume feeding the loop, and it doesn't have the multimodal embedding capability. The 3,072d embeddings encode cross-modal semantics — text, images, audio, video, documents — in one unified space, enabling queries like "find everything related to this SEM image" that return the paper, the composition data, the processing video, and the meeting recording where someone discussed the results. These cross-modal connections compound over millions of queries into a knowledge structure no single node could build.

**Why 768d is still right for local nodes:** Local nodes serve one team, maybe one department. Their datasets are typically 1K-100K records of text and tabular data. At that scale and modality, 768d captures the semantic structure with minimal waste. The platform operates at 3,072d because it embeds across modalities — text, images (SEM micrographs, XRD patterns, phase diagrams), audio (lab recordings, meeting notes), video (processing footage, experimental recordings), PDFs, diagrams, and arbitrary file types — all in one unified vector space. Cross-modal similarity requires far more representational capacity than text alone. A 768d space cannot meaningfully represent the relationship between a fracture surface image and the paper describing the failure mechanism. 3,072d can.

**Implementation Path:**

1. **Start with the composition encoder.** Use Pettifor embeddings (Jan 2026, best performing) + CrabNet-style fractional encoding + attention pooling. This alone gives you composition similarity search, which is immediately useful.
2. **Add the text encoder.** Mean-pool the fine-tuned Qwen3.5-9B hidden states. This handles free-text descriptions, paper abstracts, notes.
3. **Add processing + property encoders.** Structured MLP encoders with normalized inputs.
4. **Train the fusion layer.** Contrastive learning over the concatenated sub-vectors. This is where the SPARK dataset is gold — you have real data to build training pairs from.
5. **Matryoshka training.** Train the fusion layer with matryoshka representation learning so the first 256 dimensions capture the most important information. Local nodes use 768d (text + tabular). Platform uses 3,072d with additional image/audio/video encoders feeding into the fusion layer — same architecture, extended modalities, full output. Edge nodes can truncate to 256d or 384d.

**Capability B — Structured Extraction (Ontology Construction):**
Given raw data (CSV schema + sample rows, or free text), produce a structured JSON ontology mapping: entities, relationships, property types, units.

Example training pair:
```
Input:
  Schema: [Composition, MA_Time_h, HP_Temp_C, HP_Pressure_MPa, Hardness_HV, Density_g_cm3]
  Sample: ["Nb25Mo25Ta25W25", 20, 1400, 50, 542, 12.8]

Output:
  {
    "entities": [
      {"type": "Alloy", "name": "Nb25Mo25Ta25W25", "system": "NbMoTaW"},
      {"type": "Element", "names": ["Nb", "Mo", "Ta", "W"], "fractions": [0.25, 0.25, 0.25, 0.25]},
      {"type": "Process", "name": "Mechanical Alloying", "params": {"time_h": 20}},
      {"type": "Process", "name": "Hot Pressing", "params": {"temp_C": 1400, "pressure_MPa": 50}},
      {"type": "Property", "name": "Hardness", "value": 542, "unit": "HV"},
      {"type": "Property", "name": "Density", "value": 12.8, "unit": "g/cm³"}
    ],
    "relationships": [
      {"from": "Nb25Mo25Ta25W25", "rel": "CONTAINS", "to": "Nb", "weight": 0.25},
      {"from": "Nb25Mo25Ta25W25", "rel": "PROCESSED_BY", "to": "Mechanical Alloying", "order": 1},
      {"from": "Nb25Mo25Ta25W25", "rel": "PROCESSED_BY", "to": "Hot Pressing", "order": 2},
      {"from": "Nb25Mo25Ta25W25", "rel": "HAS_PROPERTY", "to": "Hardness"}
    ]
  }
```

**Training data strategy:**

| Source | Type | Volume | Notes |
|---|---|---|---|
| SPARK NbMoTaW dataset | Ground truth — you know the correct ontology | ~50-100 examples | Gold standard. Build extraction pairs manually. |
| Materials Project / AFLOW / OPTIMADE | Structured databases → reverse-engineer extraction pairs | 1000+ examples | Scrape structured entries, reconstruct the "raw input → structured output" pairs. |
| Published papers (with tables) | Extract tables from PDFs, pair with known structured data | 500+ examples | Harder to build, higher quality signal. |
| Synthetic augmentation | LLM-generated variations of real examples | 2000+ examples | Use Claude/GPT to generate plausible raw data formats with known answers. Validate against real data. |

Target: 3000-5000 high-quality training pairs. This is the bottleneck — not compute, not model architecture.

**Fine-tuning recipe:**

```python
# Unsloth bf16 LoRA on Qwen3.5-9B
# Hardware: A100 40GB (RunPod, ~$2-3/hr)
# Expected training time: 2-4 hours for ~4000 examples

from unsloth import FastModel
import torch

model, processor = FastModel.from_pretrained(
    model_name="unsloth/Qwen3.5-9B",
    max_seq_length=4096,        # Long enough for schema + full extraction
    load_in_4bit=False,         # QLoRA NOT recommended for Qwen3.5
    load_in_16bit=True,         # bf16 LoRA
    full_finetuning=False,
)

tokenizer = processor.tokenizer  # CRITICAL: extract text tokenizer from VLM processor

model = FastModel.get_peft_model(
    model,
    r=32,                       # LoRA rank — 32 is good for structured extraction
    target_modules=["q_proj", "k_proj", "v_proj", "o_proj",
                    "gate_proj", "up_proj", "down_proj"],
    lora_alpha=64,
    lora_dropout=0.05,
)

# Training args
# per_device_train_batch_size = 2
# gradient_accumulation_steps = 4
# num_train_epochs = 3
# learning_rate = 2e-4
# warmup_steps = 10
# optim = "adamw_8bit"
```

**Shipping the model:**

```
Fine-tuned model (bf16 LoRA adapter)
         │
         ▼
    Merge adapter into base model
         │
         ▼
    Quantize to GGUF (Q4_K_XL recommended)
         │
         ▼
    Ship as: marc27/prism-indexer-9b-Q4_K_XL.gguf (~6GB)
```

The GGUF lives on Hugging Face (public) or MARC27's CDN. On first `prism node up`, the binary downloads it (or the user pre-downloads for air-gapped). The Rust binary runs it via llama.cpp (C API binding or managed `llama-server` subprocess).

```toml
# prism.toml — Indexer model config
[services.indexer]
mode = "managed"                                    # PRISM manages llama-server
model = "marc27/prism-indexer-9b-Q4_K_XL.gguf"     # Downloaded on first run
port = 8100                                          # Local inference endpoint
context_length = 4096                                # Enough for extraction tasks
gpu_layers = 99                                      # Offload everything to GPU

# Or connect to external vLLM deployment:
# mode = "external"
# uri = "http://gpu-cluster:8000/v1"
```

### 6.2 Model 2: The Searcher (General LLM, Not Fine-tuned)

**Job:** Query the structured knowledge that Model 1 built. Answer user questions. The Palantir layer.

This model IS the user-facing intelligence. It receives a user's natural language question, translates it into graph queries (Cypher) and semantic searches (vector similarity), retrieves results from Neo4j + Qdrant, synthesizes an answer, and presents it.

**This model is NOT fine-tuned.** It's a general-purpose LLM with:
- A system prompt describing the ontology schema and available query tools
- RAG over the knowledge graph (retrieval-augmented generation)
- Tool-use capability (generate Cypher, call vector search, combine results)

**Model selection — customer's choice:**

| Option | Where it runs | Cost | Quality |
|---|---|---|---|
| MARC27 Platform (managed) | platform.marc27.com | Pay per query | Claude / GPT-4 class |
| Self-hosted open model | Local node (vLLM/llama.cpp) | Hardware cost | Qwen3.5-9B (base, not fine-tuned) or larger |
| BYOK (Bring Your Own Key) | External API | Customer's API costs | Whatever they want |

```toml
# prism.toml — Searcher model config
[services.searcher]
mode = "platform"                   # Use MARC27 managed inference
# mode = "external"
# uri = "https://api.openai.com/v1"
# api_key_env = "OPENAI_API_KEY"
# mode = "managed"
# model = "Qwen3.5-9B-Q4_K_XL.gguf"  # Base model, no fine-tune needed
```

### 6.3 How They Work Together

```
User: "What refractory HEAs in our database have hardness above 500 HV
       and were processed below 1500°C?"
                    │
                    ▼
            ┌──────────────┐
            │  Model 2     │  (The Searcher)
            │  (General    │
            │   LLM)       │
            └──────┬───────┘
                   │
      ┌────────────┼────────────┐
      ▼            ▼            ▼
  Generate     Semantic      Combine
  Cypher       Search        Results
      │            │            │
      ▼            ▼            │
  ┌───────┐   ┌────────┐       │
  │ Neo4j │   │ Qdrant │       │
  │       │   │        │       │
  └───┬───┘   └───┬────┘       │
      │           │             │
      └─────┬─────┘             │
            ▼                   ▼
     Retrieved data ──────► Synthesized answer
                              back to user

Meanwhile, in the background (or on data arrival):

  New CSV uploaded
       │
       ▼
  ┌──────────────┐
  │  Model 1     │  (The Indexer)
  │  (Fine-tuned │
  │   Qwen3.5)   │
  └──────┬───────┘
         │
    ┌────┴────┐
    ▼         ▼
  Neo4j    Qdrant
  (graph   (embeddings
   built)   generated)
```

Model 1 builds the world. Model 2 navigates it. They never interact directly.

### 6.4 Why Not One Model For Both?

- **Different optimization targets.** Embedding quality and structured extraction reward different training signals than conversational fluency and tool use. A model that's great at both is either huge or mediocre at each.
- **Different runtime profiles.** The Indexer runs in batch, infrequently, on large inputs. The Searcher runs interactively, frequently, on small inputs. Different serving requirements.
- **Upgradeability.** You can swap the Searcher (better LLM comes out? Switch. Customer wants Claude instead of GPT? Switch.) without retraining. You can retrain the Indexer on new data without changing anything about the user-facing experience.
- **Cost.** The Indexer runs a small, quantized, local model. The Searcher can use expensive cloud APIs because it only fires on user queries, not on every row of data.

---

## 7. Phased Delivery

### Phase 0 — Skeleton (Target: March 27, 2026)

What ships: The repo structure, the architecture doc, and proof that the core plumbing works.

- [ ] Rust workspace with all crates stubbed (compiles, `cargo test` passes with no-op tests)
- [ ] pnpm workspace with TUI and dashboard scaffolded
- [ ] `prism node up` starts a single Docker container (Neo4j) and confirms connectivity
- [ ] IPC handshake: Rust binary spawns Ink TUI, they exchange a ping/pong
- [ ] Embedded Axum server serves a placeholder dashboard on `localhost:7327`
- [ ] One Python tool (ml-pipeline) runs end-to-end via `prism run ml-pipeline train`
- [ ] CI builds on Linux (GitHub Actions)
- [ ] README with architecture diagram

**This is the "Declaration of Empire" — the structure exists, the first systems respond.**

### Phase 1 — Node Core (Weeks 2–4)

The node actually works as a local data platform.

- [ ] `prism node up` brings up Kafka + Neo4j + VectorDB (Spark deferred)
- [ ] `prism node down`, `prism node status`, `prism node logs` working
- [ ] External mode: `--external-neo4j bolt://...` connects instead of spinning up
- [ ] Health monitoring with auto-restart for crashed containers
- [ ] Basic `prism ingest ./file.csv` → schema detection → LLM entity extraction → Neo4j graph
- [ ] `prism query "..."` → natural language → Cypher → results
- [ ] Session management (SQLite-backed, multi-user)
- [ ] Audit logging (every action logged with user, timestamp, action)
- [ ] TUI renders: node status, ingestion progress, query results
- [ ] Dashboard shows: node overview, health, basic query interface

### Phase 2 — RBAC & Platform Integration (Weeks 4–6)

Multi-user and cloud-connected.

- [ ] `prism login` → device flow auth against platform.marc27.com
- [ ] Platform roles synced to node
- [ ] Local role assignment (`prism users add/role`)
- [ ] Role-gated API routes (dashboard shows different views per role)
- [ ] Marketplace browsing and tool install
- [ ] Managed LLM keys for ontology pipeline
- [ ] `prism publish` → push tool to marketplace

### Phase 3 — Mesh Networking (Weeks 6–8)

Nodes talk to each other.

- [ ] mDNS discovery: `prism mesh discover` finds LAN nodes
- [ ] Platform discovery: register node, find cross-org nodes
- [ ] `prism publish <dataset>` → Kafka topic creation, schema advertisement
- [ ] `prism subscribe <node>/<dataset>` → Kafka consumer, local graph integration
- [ ] Federated queries: query spans local + subscribed graphs
- [ ] Subscription health monitoring in dashboard

### Phase 4 — Ontology Pipeline Maturity (Weeks 8–10)

The auto-structuring gets good.

- [ ] Spark integration for batch ETL on large datasets
- [ ] Embedding generation pipeline (configurable models)
- [ ] Semantic search via vector DB
- [ ] Watch mode: `prism ingest --watch` monitors directories
- [ ] Ontology customization: user-provided mapping rules
- [ ] Validation: SHACL shapes for graph quality checks

### Phase 5 — Distribution & Polish (Weeks 10–12)

Ship it.

- [ ] Cross-compiled release builds (Linux, macOS Intel/ARM, Windows)
- [ ] Install script: `curl -fsSL https://prism.marc27.com/install | sh`
- [ ] Homebrew formula, AUR package
- [ ] Bun-compiled TUI (if stable) or bundled Node runtime
- [ ] Full tool migration (all v1 tools ported with manifests)
- [ ] Workflow engine (multi-step pipelines as YAML)
- [ ] Documentation site
- [ ] Enterprise deployment guide (air-gapped, external services)

---

## 8. Open Design Questions (Revised)

### 7.1 Vector DB Selection (BLOCKING for Phase 1)

| Option | Pros | Cons |
|---|---|---|
| **Qdrant** | Rust-native, fast, good filtering, gRPC API | Newer, smaller community |
| **Milvus** | Mature, scalable, GPU support | Heavy (needs etcd + MinIO), over-engineered for single-node |
| **Weaviate** | Built-in vectorization, GraphQL API | Go-based, heavier than Qdrant |
| **Chroma** | Simple, Python-native, easy to embed | Not production-grade for large datasets |
| **pgvector** | Lives in Postgres, no extra service | Slower than purpose-built vector DBs at scale |

**Recommendation:** Qdrant. Rust-native aligns with the stack philosophy. Lightweight enough for single-node, scales for enterprise. Good Rust client crate.

### 7.2 Kafka Alternatives (Worth considering)

Full Kafka might be overkill for small deployments. Consider:

| Option | When |
|---|---|
| **Apache Kafka** | Default for enterprise. Known, battle-tested. |
| **Redpanda** | Drop-in Kafka replacement, single binary, lower resource usage. Better for small nodes. |
| **NATS JetStream** | Lighter than Kafka. Good for pub/sub. Less ecosystem support for streaming ETL. |

**Recommendation:** Redpanda as default (Kafka API compatible, but single binary = simpler orchestration). Support connecting to real Kafka in external mode. This is a detail — the code talks Kafka protocol either way.

### 7.3 Node.js Dependency (Carried from Rev 1)

Still needs resolution. See Rev 1, Section 7.1. Test `bun compile` for the TUI immediately.

### 7.4 Ontology Engine (DEFERRED)

LLM pipeline ships first. DMMS integration deferred. The trait boundary in `prism-ingest` ensures the engine is swappable. Decision point: after SPARK dataset validation of DMMS.

### 7.5 Spark Necessity (Worth questioning)

For Phase 1, do you actually need Spark? The ingestion pipeline for CSVs and small databases can run in-process (Rust + Polars). Spark earns its place when:
- Single files exceed available RAM
- You need distributed processing across cores/machines
- You're doing complex multi-source joins at scale

**Recommendation:** Use Polars (Rust-native DataFrame library) for Phase 1 ETL. Add Spark in Phase 4 when dataset sizes justify it. This removes an entire container from the initial `prism node up`, making first-run experience much faster.

---

## 9. Risk Register (Revised)

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Container orchestration complexity (Docker API edge cases, networking, port conflicts) | High | High | Start with ONE container (Neo4j) in Phase 0. Add services one at a time. Never launch all four at once untested. |
| Ontology quality from LLM pipeline is poor on real materials data | High | High | Use the SPARK NbMoTaW dataset as ground truth. If the LLM pipeline can't correctly structure data you already understand, fix it before trying on unknown data. |
| mDNS discovery unreliable across network segments / VPNs / firewalls | Medium | Medium | Platform-mediated discovery as fallback. mDNS is the "nice to have" for lab setups; platform discovery is the reliable path. |
| Scope is now 3x larger than Rev 1 | Certain | High | Phased delivery is non-negotiable. Phase 0 and Phase 1 are the only things that matter right now. Phases 3–5 can slip without killing the project. |
| Multi-user / RBAC adds significant complexity to every endpoint | High | Medium | Implement RBAC as middleware, not per-handler logic. One middleware layer checks permissions; handlers assume they're authorized. |
| Kafka/Redpanda + Neo4j + VectorDB resource usage on a dev laptop | Medium | Medium | Provide a "light mode" that skips Kafka and VectorDB, runs only Neo4j. Good enough for local development and small-scale use. |
| Inter-node data consistency across federated graphs | High | High | Defer federated writes. V2 is read-only federation: you can query across nodes, but each node owns its own data. No distributed transactions. |

---

## 10. The March 27 Deliverable (Unchanged)

Three days. Same scope as Rev 1:

**Ship the skeleton.** Repo structure, architecture doc, Rust workspace compiles, pnpm workspace compiles, one container starts, IPC handshake works, one tool runs, CI builds. The Empire is declared.

**Realistic full V2 beta: 10–12 weeks** (increased from 6–8 due to mesh networking, RBAC, and container orchestration scope).

---

## 11. Dependency Summary

### Rust Crates
- **tokio** — async runtime
- **axum** — web server + WebSocket
- **clap** — CLI arg parsing
- **reqwest** — HTTP client (platform API)
- **serde / serde_json** — serialization
- **rusqlite** — session storage, audit log, RBAC
- **bollard** — Docker Engine API
- **rust-embed / include_dir** — embed SPA assets
- **rdkafka** — Kafka protocol client (works with Redpanda)
- **neo4rs** — Neo4j Bolt protocol client
- **qdrant-client** — Vector DB client
- **polars** — DataFrame operations (Phase 1 ETL, pre-Spark)
- **mdns-sd** — mDNS service discovery
- **tracing** — structured logging
- **jsonrpc-core** or custom — IPC protocol

### TypeScript / Node
- **ink 5.x** — TUI framework
- **React 18+** — dashboard
- **Vite** — dashboard build
- **TailwindCSS + shadcn/ui** — dashboard UI

### Python
- **uv** — tool environment management (bundled)
- **pydantic** — tool I/O validation
- **structlog** — tool logging

### Infrastructure Images
- **Apache Kafka** (KRaft mode, Apache 2.0 — offline pub/sub)
- **Neo4j 5 Community** (GPLv3/AGPL — accepted, see License Audit)
- **Qdrant** (vector DB, Apache 2.0)
- **Spark** (Phase 4+, deferred, Apache 2.0)

---

## REVISION 3 ADDENDUM — Architectural Decisions & License Audit

*Decisions made after Rev 2, superseding conflicting sections above.*

### A. License Audit

Every dependency in the PRISM stack audited for commercial viability.

| Component | License | Commercial Use | Ship with PRISM? | Verdict |
|---|---|---|---|---|
| **Qwen3.5-9B** | Apache 2.0 | ✅ Unrestricted | Yes (as GGUF) | ✅ CLEAN |
| **Qdrant** | Apache 2.0 | ✅ Unrestricted | Yes (Docker image) | ✅ CLEAN |
| **Apache Kafka** | Apache 2.0 | ✅ Unrestricted | Yes (Docker image, KRaft mode) | ✅ CLEAN |
| **Apache Spark** | Apache 2.0 | ✅ Unrestricted | Deferred (Phase 4) | ✅ CLEAN |
| **Ink.js** | MIT | ✅ Unrestricted | Yes (bundled TUI) | ✅ CLEAN |
| **React / Vite / Tailwind** | MIT | ✅ Unrestricted | Yes (embedded SPA) | ✅ CLEAN |
| **Axum / Tokio / Clap / Serde** | MIT or Apache 2.0 | ✅ Unrestricted | Yes (compiled into binary) | ✅ CLEAN |
| **Polars** | MIT | ✅ Unrestricted | Yes (compiled into binary) | ✅ CLEAN |
| **rdkafka (librdkafka)** | BSD-2-Clause | ✅ Unrestricted | Yes (compiled into binary) | ✅ CLEAN |
| **neo4rs** | MIT | ✅ Unrestricted | Yes (compiled into binary) | ✅ CLEAN |
| **bollard (Docker API)** | Apache 2.0 | ✅ Unrestricted | Yes (compiled into binary) | ✅ CLEAN |
| **Mat2Vec** | MIT (GitHub repo) | ✅ Unrestricted | Pretrained embeddings only | ✅ CLEAN |
| **CrabNet** | MIT (GitHub repo) | ✅ Unrestricted | Architecture reference, not shipped | ✅ CLEAN |
| **Pettifor embeddings** | CC-BY 4.0 (npj Comput. Mater.) | ✅ With attribution | Embedding vectors, cite paper | ✅ CLEAN (cite) |
| **uv** | MIT or Apache 2.0 | ✅ Unrestricted | Bundled for Python tool mgmt | ✅ CLEAN |
| **Neo4j Community** | GPLv3 + AGPL + Commons Clause | ⚠️ See below | Yes (Docker image) | ⚠️ ACCEPTED RISK |
| **Unsloth** | Apache 2.0 (core) / LGPL-3.0 (unsloth_zoo) | ⚠️ Multi-GPU commercial restriction | Training-time ONLY, never shipped | ✅ NOT A SHIPPING DEP |
| **Redpanda** | BSL (not open source) | ⚠️ Source-available, not OSS | **REMOVED from stack** | ❌ REPLACED by Kafka |

**Neo4j — Accepted Risk, Documented:**

Neo4j Community Edition carries AGPL with a Commons Clause restriction. The AGPL risk: if PRISM is considered a "derivative work" of Neo4j (by wrapping/embedding it), the AGPL could require PRISM to also be AGPL-licensed. The Commons Clause risk: PRISM cannot offer Neo4j as its primary value proposition as a paid service without a commercial Neo4j license.

**Mitigations:**
- PRISM connects to Neo4j over the Bolt protocol (network API). It does NOT embed, link, or modify Neo4j code. This is the standard interpretation under which AGPL does not propagate.
- Neo4j is an optional, swappable component. PRISM's `prism-ingest` crate talks to a `GraphStore` trait. Neo4j is one implementation. The architecture supports replacement with Apache AGE, Kuzu, or any Cypher-compatible store.
- If a customer's legal team rejects AGPL, PRISM can run with Apache AGE on PostgreSQL (Apache 2.0) as a drop-in alternative. This fallback MUST be implemented and tested.
- For MARC27's own cloud-hosted PRISM nodes, a Neo4j Enterprise commercial license should be obtained eventually.

**Decision: Proceed with Neo4j Community for V2. Implement the `GraphStore` trait boundary so it's swappable. Document the AGPL risk for enterprise customers. Build the Apache AGE fallback by Phase 3.**

**Redpanda — Removed:**

Replaced with Apache Kafka in KRaft mode (no ZooKeeper dependency since Kafka 3.3+). Kafka is Apache 2.0 — no license ambiguity for an open-source project. Heavier than Redpanda but legally clean and universally understood by enterprise customers.

### B. Dual Subscription Architecture (Supersedes Rev 2 Section 2.2 prism-mesh)

PRISM nodes subscribe to each other through two independent paths. Both coexist in the same binary.

**Path 1: Platform-Mediated (Internet-Connected)**

```
Node A                    platform.marc27.com                    Node B
  │                              │                                  │
  ├── prism node up ────────────►│ (registers, notice shown)        │
  │   "Registered with MARC27"  │                                  │
  │                              │◄──────────── prism node up ──────┤
  │                              │  (registers, notice shown)       │
  │                              │                                  │
  │                              │◄── prism subscribe nodeA/heas ───┤
  │                              │  (platform brokers connection)   │
  │                              │                                  │
  │◄─────────────── WSS/GraphQL/REST (direct, peer-to-peer) ──────►│
  │                  (data flows directly, platform is control      │
  │                   plane only, not data plane)                   │
```

- Node registration is mandatory but non-blocking. A notice is shown: `"Registered with MARC27 — node id: prism-a3f7b2"`
- Even unregistered/anonymous nodes are tracked (node ID, IP, version, uptime)
- Authentication (login) is separate — unlocks marketplace, managed LLM keys, cross-org subscriptions
- Platform brokers initial connection, then nodes communicate directly via WSS/GraphQL/REST
- If platform goes down, existing peer-to-peer connections continue. New discovery stops.

**Path 2: Local/Air-Gapped (Kafka)**

```
Node A                    Kafka (local network)                  Node B
  │                              │                                  │
  ├── prism node up --offline ──►│ (Kafka broker starts locally)    │
  │   (no registration attempt) │                                  │
  │                              │◄── prism node up --offline ──────┤
  │                              │                                  │
  │── publishes "heas" topic ───►│                                  │
  │                              │── Node B subscribes to topic ───►│
  │                              │                                  │
  │── new data ────────────────►│──────────────── streams to B ────►│
```

- `prism node up --offline` skips platform registration entirely
- Starts a local Kafka broker (KRaft mode, no ZooKeeper)
- mDNS for node discovery on the local network
- Subscriptions use Kafka topics directly
- This is the ESA / defense / classified / air-gapped path
- No MARC27 cloud dependency whatsoever

**Detection logic in prism-mesh:**

```rust
pub async fn init_mesh(config: &NodeConfig) -> Result<MeshHandle> {
    if config.offline_mode {
        // Air-gapped: start local Kafka, mDNS discovery
        let kafka = start_local_kafka(&config.kafka).await?;
        let mdns = start_mdns_discovery().await?;
        return Ok(MeshHandle::Offline { kafka, mdns });
    }

    // Online: register with platform, try WSS
    let platform = register_with_platform(&config.platform).await;
    match platform {
        Ok(registration) => {
            info!("Registered with MARC27 — node id: {}", registration.node_id);
            // Kafka still available as fallback for LAN nodes
            let kafka = if config.kafka.enabled {
                Some(start_local_kafka(&config.kafka).await?)
            } else {
                None
            };
            Ok(MeshHandle::Online { registration, kafka })
        }
        Err(e) => {
            warn!("Could not reach platform.marc27.com: {}. Running in degraded mode.", e);
            // Fallback to offline behavior
            let kafka = start_local_kafka(&config.kafka).await?;
            let mdns = start_mdns_discovery().await?;
            Ok(MeshHandle::Offline { kafka, mdns })
        }
    }
}
```

### C. Node Registration Behavior

On every `prism node up`:

```
$ prism node up

  ┌─────────────────────────────────────────────┐
  │  PRISM v2.0.0                               │
  │  Node: lab-alpha                            │
  │                                             │
  │  Starting services...                       │
  │  ✓ Neo4j        bolt://localhost:7687       │
  │  ✓ Qdrant       http://localhost:6333       │
  │  ✓ Indexer      http://localhost:8100       │
  │  ✓ Dashboard    http://localhost:7327       │
  │                                             │
  │  Registered with MARC27 — node ab7f3c      │
  │  (anonymous • login for full features)      │
  └─────────────────────────────────────────────┘
```

If `--offline`:

```
$ prism node up --offline

  ┌─────────────────────────────────────────────┐
  │  PRISM v2.0.0 (OFFLINE MODE)                │
  │  Node: lab-alpha                            │
  │                                             │
  │  Starting services...                       │
  │  ✓ Neo4j        bolt://localhost:7687       │
  │  ✓ Qdrant       http://localhost:6333       │
  │  ✓ Indexer      http://localhost:8100       │
  │  ✓ Kafka        localhost:9092              │
  │  ✓ Dashboard    http://localhost:7327       │
  │                                             │
  │  Running offline — no platform connection   │
  │  LAN discovery via mDNS active              │
  └─────────────────────────────────────────────┘
```

### D. Python Tools Migration (From PRISM V1)

Existing Python tools from PRISM V1 port directly into the `tools/` directory. Each tool gets:

1. A `manifest.json` (describes inputs, outputs, commands — see Section 5 above)
2. Its existing Python code, wrapped with the tool protocol (JSON over stdin/stdout)
3. A `pyproject.toml` for dependency management via uv

The actual ML/analysis logic does NOT need to be rewritten. The wrapper is thin — typically 50-100 lines of boilerplate that reads JSON from stdin, calls existing functions, writes JSON to stdout.

```
PRISM V1 tool (Python)           PRISM V2 tool (Python)
├── train.py                     ├── manifest.json          ← NEW
├── predict.py                   ├── pyproject.toml         ← NEW
├── analyze.py                   ├── src/
│                                │   ├── main.py            ← NEW (thin wrapper)
│                                │   ├── train.py           ← UNCHANGED
│                                │   ├── predict.py         ← UNCHANGED
│                                │   └── analyze.py         ← UNCHANGED
```

### E. PRISM License (The Project Itself)

PRISM V2 is open source. Recommended license for the PRISM repository itself:

| Option | Pros | Cons |
|---|---|---|
| **Apache 2.0** | Maximally permissive. Enterprises love it. No copyleft obligations. Patent grant. | Competitors can fork and never contribute back. |
| **MIT** | Even simpler than Apache. Widely understood. | No patent grant. |
| **AGPL** | Forces network-use modifications to be shared. Protects against SaaS competitors. | Scares enterprise customers. Conflicts with "open for everyone" positioning. |
| **BSL** | Source-available but not OSS. Prevents competitors from offering it as a service. Converts to Apache 2.0 after N years. | Not "open source" by OSI definition. Mixed community reception. |

**Recommendation: Apache 2.0.** PRISM's moat is not the code — it's the platform (platform.marc27.com), the fine-tuned models, the curated ontologies, the marketplace, and the network effects of the node mesh. The code being fully open and permissively licensed drives adoption. Adoption drives platform registrations. Platform registrations drive revenue.

The MARC27 platform itself (platform.marc27.com) is proprietary and never open-sourced. That's where the commercial value lives.

```
Open Source (Apache 2.0)          Proprietary
├── PRISM binary                  ├── platform.marc27.com
├── TUI                          ├── Managed LLM inference
├── Dashboard                    ├── Multimodal embedding system (3,072d)
├── Python tools                 ├── Recursive search-embed-graph engine
├── SDK                          ├── Marketplace curation
└── Ontology schemas             ├── Enterprise support
                                 ├── Fine-tuned model weights (optional)
                                 └── Premium ontology datasets
```

### F. `prism forge` — The Autonomous Research Loop

This is PRISM's core differentiator. Not a chatbot. Not a query interface. An autonomous materials research agent that reads, hypothesizes, trains, tests, and iterates.

**Command:**
```bash
prism forge paper.pdf                          # Start from a paper
prism forge ./data/alloys.csv                  # Start from raw data
prism forge "NbMoTaW alloys for turbine blades" # Start from a research question
prism forge --resume forge-session-a3f7        # Resume a previous forge run
```

**The Loop:**

```
Input (paper, data, question)
  │
  ▼
1. COMPREHENSION ─── LLM reads input, understands domain,
  │                  extracts ontology, identifies known/unknown
  ▼
2. HYPOTHESIS ────── LLM formulates what to predict, what
  │                  relationships to model, what data exists
  ▼
3. MODEL SELECTION ─ Picks ML architecture based on data type,
  │                  target property, dataset size
  │                  (RF, XGBoost, CrabNet, GNN, etc.)
  ▼
4. TRAINING ──────── Dispatches to compute tier:
  │                  local process / MARC27 cloud / BYOC
  │                  Runs in sandbox (process, container, or VM
  │                  depending on compute tier)
  ▼
5. EVALUATION ────── LLM reviews results: metrics, feature
  │                  importance, failure modes, physical plausibility
  ▼
6. SATISFIED? ──NO──► Adjust features / try different architecture /
  │                   request more data / modify hypothesis
  │                   └──────────────► back to step 2 or 3
  YES
  │
  ▼
7. OUTPUT ────────── Trained model + findings + ontology updates + report
```

**What the LLM does vs. what it doesn't:**

The LLM is the research director — it plans, delegates, interprets. It does NOT run the actual training (that's PyTorch/scikit-learn in the compute tier), does NOT choose hyperparameters by magic (structured search), does NOT access GPUs directly, does NOT invent physics. Human review is always the final step.

**Implementation — new crates:**

**`prism-forge`:** Orchestrates the full loop. Core type is `ForgeSession` — a persistent state machine stored in SQLite. Sessions can span hours or days. Users kick off training overnight, review results in the morning.

```rust
pub struct ForgeSession {
    id: SessionId,
    input: ForgeInput,                    // Paper, data, or question
    comprehension: Option<Comprehension>, // LLM's understanding of the domain
    hypotheses: Vec<Hypothesis>,          // What to test
    experiments: Vec<Experiment>,         // Training runs (completed + in-progress)
    findings: Vec<Finding>,              // Results and interpretations
    iteration_count: u32,
    max_iterations: u32,                  // Safety limit (default: 10)
    compute_target: ComputeTarget,        // Where to run training
}

pub enum ForgeInput {
    Paper(PathBuf),                       // PDF → LLM extracts research context
    Data(PathBuf),                        // CSV/Parquet → LLM analyzes schema
    Question(String),                     // NL → LLM searches node's data
    ResumeSession(SessionId),             // Pick up where we left off
}
```

**`prism-compute`:** Abstracts over where training runs. One trait, three implementations.

```rust
pub trait ComputeBackend: Send + Sync {
    async fn submit(&self, plan: &ExperimentPlan) -> Result<JobId>;
    async fn status_stream(&self, job: &JobId) -> Result<impl Stream<Item = JobUpdate>>;
    async fn results(&self, job: &JobId) -> Result<ExperimentResult>;
    async fn cancel(&self, job: &JobId) -> Result<()>;
}

pub struct LocalBackend { /* subprocess executor */ }
pub struct Marc27Backend { /* platform API client */ }
pub struct ByocBackend { /* generic HTTP job submission */ }
```

The forge loop doesn't care where training runs — same code path regardless.

**Where DMMS slots in (future):** Step 3 (model selection) currently relies on LLM judgment. When DMMS matures, it analyzes the actual manifold structure of the data and recommends architectures based on geometry, not heuristics. Same loop, better decisions.

### G. Compute Tier Strategy

Three tiers. Path of least resistance is always MARC27 Cloud.

**Tier 1: Local**
```toml
[compute]
target = "local"
```
Training runs as a subprocess on the user's machine. No containers — just a Python process with the tool's virtualenv. GPU access via PyTorch/CUDA directly. Always works, even offline.

**Tier 2: MARC27 Cloud**
```toml
[compute]
target = "marc27"
# Requires: prism login
```
Dispatches training jobs to platform.marc27.com GPU cluster (A100/H100). Jobs run in containers on MARC27's infrastructure. Pay-per-use (billed per GPU-hour). Results stream back via WSS. **This is the path MARC27 optimizes for.** Best docs, best UX, fastest setup.

**Tier 3: BYOC (Bring Your Own Compute)**
```toml
[compute]
target = "byoc"
endpoint = "https://my-gpu-cluster.internal:8443"
```
User provides a compute endpoint that implements PRISM's job submission API. The API spec exists in the codebase (it's open source). **No official documentation. No tutorials. No support.** Enterprise customers who want this can read the code or pay for MARC27 Enterprise Support. This prevents AWS/GCP from easily wrapping PRISM as a managed service — the integration path is deliberately not turnkey, and by the time they reverse-engineer it, MARC27 has the network effects.

**The economics:**
```
Local:  Free, but slow (consumer GPU) or expensive (own A100)
MARC27: ~$2-4/GPU-hour, zero setup, managed, results in minutes
BYOC:   Your infra costs + your engineering time to integrate
```

For most users, MARC27 Cloud is the obvious choice.

### H. Updated Repo Structure (Forge Additions)

```
crates/
  ...existing crates...
  ├── prism-forge/                      # Autonomous research loop
  │   ├── Cargo.toml
  │   └── src/
  │       ├── lib.rs
  │       ├── session.rs                # ForgeSession state machine
  │       ├── comprehension.rs          # LLM paper/data reading
  │       ├── hypothesis.rs             # Hypothesis generation
  │       ├── experiment.rs             # Experiment planning
  │       ├── evaluation.rs             # Result interpretation
  │       ├── report.rs                 # Final report generation
  │       └── persistence.rs            # SQLite session storage
  │
  ├── prism-compute/                    # Compute dispatch abstraction
  │   ├── Cargo.toml
  │   └── src/
  │       ├── lib.rs
  │       ├── backend.rs                # ComputeBackend trait
  │       ├── local.rs                  # Subprocess execution
  │       ├── marc27.rs                 # Platform API dispatch
  │       ├── byoc.rs                   # Generic HTTP job submission
  │       └── job.rs                    # Job tracking, streaming

crates/prism-cli/src/commands/
  ...existing commands...
  ├── forge.rs                          # prism forge <input>
  │                                     # prism forge --resume <id>
  │                                     # prism forge status <id>
  │                                     # prism forge history
  │                                     # prism forge cancel <id>
```

### I. Updated Phased Delivery (Forge Integration)

Forge does NOT ship in Phase 0 or Phase 1. It requires the compute and LLM infrastructure to be stable first.

| Phase | Forge Status |
|---|---|
| Phase 0-1 | Not present. Core node only (ingest, query, tools, dashboard). |
| Phase 2 | Compute dispatch to MARC27 Cloud proven. `ComputeBackend` trait finalized. |
| Phase 3 | **`prism forge` ships.** LLM comprehension → model selection → local training → evaluation loop. MARC27 Cloud dispatch working. |
| Phase 4 | BYOC backend implemented (code exists, undocumented). DMMS integration point designed. |
| Phase 5 | Forge maturity: session history, resume, multi-day sessions, report generation, iteration intelligence. |
