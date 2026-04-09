<p align="center">
  <img src="docs/assets/prism-banner.png" alt="PRISM Banner" width="800">
</p>

<h1 align="center">PRISM</h1>
<p align="center"><strong>AI-Native Autonomous Materials Discovery Platform</strong></p>
<p align="center"><em>by MARC27</em></p>

---

PRISM is a single binary that gives you an AI agent for materials science. It searches knowledge graphs, runs compute jobs, orchestrates workflows, and connects to a federated mesh of research nodes.

```bash
prism                    # Launch interactive TUI
prism query --platform "nickel superalloys"
prism research "high-entropy alloys" --depth 1
prism billing            # Check credit balance
```

## Install

```bash
curl -fsSL https://prism.marc27.com/install.sh | bash
```

Or download from [GitHub Releases](../../releases):

| Platform | Archive |
|----------|---------|
| Linux x86_64 | `prism-linux-x86_64.tar.gz` |
| macOS Apple Silicon | `prism-macos-aarch64.tar.gz` |
| Windows x86_64 | `prism-windows-x86_64.zip` |

Or from source:

```bash
git clone https://github.com/Darth-Hidious/PRISM.git
cd PRISM && cargo build --release
cp target/release/prism ~/.local/bin/
```

## Interactive TUI

Run `prism` to launch the terminal UI with:
- Workspace tabs (Chat, Explorer, Models, Compute, Mesh, Workflows, Marketplace, Data, Settings)
- Command palette (type `/` to search 69+ commands)
- Model picker with 535 hosted models across 4 providers
- Sidebar browser per workspace (file tree, model catalog, mesh peers, GPU resources)
- Inline tool cards showing execution results
- Streaming AI responses with markdown rendering

## CLI Commands

### Setup & Auth
```
prism setup              # First-time setup + login
prism login              # Authenticate with MARC27
prism configure --show   # Show LLM config
prism status             # Auth state, paths, endpoints
```

### AI Agent
```
prism                    # Interactive TUI agent
prism query --platform "titanium alloys"
prism query --semantic "creep resistance"
prism query --cypher "MATCH (a:Alloy) RETURN a"
prism research "novel refractory alloys" --depth 1
```

### Knowledge Graph
```
prism ingest ./data.csv              # Ingest data
prism ingest --schema-only ./data.csv
prism ingest --watch ./data/
```

### Compute
```
prism run python:3.11 --input data=x.csv
prism run --backend marc27 --gpu A100
prism run --ssh user@host             # BYOC via SSH
prism run --k8s-context prod          # BYOC via Kubernetes
prism run --slurm head@cluster        # BYOC via SLURM
prism deploy create --name my-service --image img:latest
prism deploy list
prism job-status <uuid>
```

### Mesh & Nodes
```
prism node up            # Start local node (Neo4j + Qdrant)
prism node status
prism mesh discover      # Find LAN peers via mDNS
prism mesh publish       # Share dataset to mesh
```

### Models
```
prism models list        # 535 hosted LLMs
prism models search "claude"
```

### Workflows
```
prism workflow list
prism workflow run explore --space "Ni-Cr-Co" --target "yield_strength > 900"
prism forge --paper arxiv:2106.09685 --dataset materials-project --target runpod:A100
```

### Billing
```
prism billing            # Credit balance
prism billing usage      # Usage breakdown
prism billing topup      # Buy credits (Stripe)
prism billing prices     # Pricing table
```

### Other
```
prism tools              # List 108 available tools
prism marketplace search # Browse datasets, models, plugins
prism discourse list     # Multi-agent debate workflows
prism report "bug"       # File support ticket
```

## Architecture

```
prism (single Rust binary)
  prism-cli      Command routing, auth, TUI
  prism-llm      LLM client (OpenAI + MARC27 proxy)
  prism-agent    TAOR agent loop, tool calling, OPA policy
  prism-node     Daemon, probe, E2EE key exchange
  prism-ingest   Schema detection, ontology extraction
  prism-server   Axum REST API + dashboard
  prism-mesh     mDNS + Kafka pub/sub + federation
  prism-compute  Docker / MARC27 / SSH / K8s / SLURM
  prism-policy   OPA/Rego policy engine
  prism-workflows YAML workflow engine (8 step types)

Python tools (108 tools via JSON stdio)
  Search: OPTIMADE (20+ providers), arXiv, Semantic Scholar
  Predict: property prediction, structure prediction
  Simulate: CALPHAD, DFT planning
  Execute: Python, Bash (sandboxed)
  Platform: knowledge graph, compute, marketplace
  Custom: drop .py in ~/.prism/tools/
```

## Configuration

All config in `~/.prism/prism.toml`:

```toml
[llm]
provider = "marc27"
url = "https://api.marc27.com/api/v1/projects/{id}/llm"
model = "gemini-3.1-flash-lite-preview"

[node]
name = "lab-alpha"
port = 7327

[mesh]
discovery = ["mdns", "platform"]
```

## Security

- **OPA/Rego policy engine** — every tool call checked
- **E2EE** — X25519 + ChaCha20-Poly1305 for node-to-node data
- **RBAC** — 4-tier roles (admin, operator, agent, viewer)
- **Audit logging** — every action logged

## Contributing

- **TUI** — built with Ratatui, protocol documented in `docs/FRONTEND_PROTOCOL.md`
- **Tools** — new Python tools in `app/tools/`, custom tools in `~/.prism/tools/`
- **Workflows** — YAML in `~/.prism/workflows/`, auto-discovered as CLI commands

## License

| Component | License |
|-----------|---------|
| Python tools, tool server, config, plugins | [MIT](LICENSE-MIT) |
| Rust crates, TUI, agent, workflows | [MARC27 Source-Available](LICENSE-MARC27) |

Commercial licensing: team@marc27.com

---

<p align="center">
  <img src="docs/assets/marc27-logo.svg" alt="MARC27" height="40">
</p>
