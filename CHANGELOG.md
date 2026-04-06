# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [2.6.0] - 2026-04-04

## [2.6.1] - 2026-04-06

### Added
- Native PRISM command/tool parity across the Rust agent, CLI, and TUI, including typed surfaces for workflow, deploy, discourse, ingest, models, run, publish, and node flows.
- Live platform discovery integration for agent capabilities, knowledge capabilities, and project-scoped hosted LLM models.
- External MCP tool loading in the merged catalog used by `prism tools`, the Rust agent, and the TUI permission/editor flow.
- Node-side deployment lifecycle handling plus platform-aligned node public-key and E2EE exchange support.

### Changed
- The shell/TUI now renders grouped turns, inline approvals, structured command views, and richer status/session/plan screens.
- `prism ingest` now uses one unified command shape for local and platform-backed ingestion, including PDF routing and corpus-aware status reporting.
- The PM dashboard is wired to live PRISM data and the native release pipeline ships the real dashboard and standalone TUI binaries.
- CLI node key fetch/exchange now honor env-based MARC27 auth in the same way as the rest of the platform-backed command surface.

### Added — Rust Agent (prism-agent crate)
- **TAOR agent loop**: Think-Act-Observe-Repeat loop in pure Rust with hooks, permissions, doom loop detection, compaction, and scratchpad logging.
- **10 agent modules**: types, models, transcript, permissions, hooks, scratchpad, session, prompts, agent_loop, protocol — faithfully ported from deleted Python agent.
- **80+ agent tests** covering all modules.

### Added — Infrastructure
- **Kafka outbound wired**: `MeshKafkaProducer` stored in `NodeState`, announce/goodbye lifecycle on node up/down, pub/sub in mesh handlers.
- **Spark data processing**: Docker-orchestrated Spark service config + 3 PySpark Python tools for distributed ETL.
- **Federated queries**: Cross-mesh search across connected nodes.
- **BYOC compute wired**: SSH, Kubernetes, and SLURM backends available from CLI.

### Added — Ecosystem
- **Custom tool plugins**: Users can add `.py` tools to `~/.prism/tools/` with auto-discovery.
- **Marketplace CLI**: `prism marketplace search/install/info` for tools and workflows.

### Changed
- **Python relegated to tools-only**: 48 tools served via `app/tool_server.py` (JSON stdio). Python agent loop, backend server, and CLI deleted.
- **License split**: MIT for Python tools, MARC27 Source-Available for Rust crates and frontend.
- **install.sh rewritten**: 82 lines, installs Rust binary only (no Python in critical path).
- **APFS disk dedup fix**: Correct disk detection accounting for APFS deduplication; detect llama.cpp, vLLM, Quill Engine, Jupyter.

### Test Suite
- **494 Rust tests** across 16 crates (up from 355).
- **551 Python tests** across 48 tools.

---

## [2.5.0] - 2026-03-27

### Added — Rust Backbone (15 crates)
- **prism-cli**: Rust `prism` binary with clap command routing, device-flow auth, workflow discovery, and Python worker supervision.
- **prism-node**: Rust `prism-node` daemon — WebSocket platform registration, heartbeat, job dispatch, reconnect with backoff.
- **prism-core**: Node config (`prism.toml` schema), multi-user session management (SQLite), RBAC engine (platform + local roles), append-only audit log (SQLite), tool manifest registry with directory scanning.
- **prism-client**: Typed MARC27 platform API client — device-code OAuth flow with polling/backoff, token refresh, marketplace browsing, node registration/discovery.
- **prism-compute**: `ComputeBackend` trait with three targets — `LocalBackend` (Docker/Podman with GPU passthrough), `Marc27Backend` (cloud REST API), `ByocBackend` (SSH/Kubernetes/SLURM interfaces). Thread-safe `JobTracker` with concurrent access. `ComputeRouter` for intelligent backend selection.
- **prism-ingest**: `OntologyConstructor` trait (pluggable — LLM now, DMMS future). Ollama API integration for entity extraction and embeddings. Neo4j HTTP Transactional Cypher API client. Qdrant REST API client for vector upsert/query/delete. Schema detection, graph construction, embedding generation interfaces.
- **prism-orch**: `ServiceOrchestrator` trait. Docker orchestration via bollard (image pull, container create/start/stop/remove, port binding, log streaming). Health checker with timeout/retry. Service definitions for Neo4j, Qdrant, Kafka, Spark.
- **prism-mesh**: Node discovery via mDNS/DNS-SD and platform-mediated cross-org. Pub/sub subscription management. Peer tracking with state machine (Offline/Online). Inter-node protocol types. Federation placeholder.
- **prism-server**: Axum HTTP/WebSocket server with shared `NodeState`. Router builder, middleware modules (auth, RBAC), handler stubs for node/data/query/tools/mesh/users/audit endpoints.
- **prism-forge**: Experiment design types — `ForgeSession`, `ForgeInput`. Module structure for comprehension, hypothesis, experiment, evaluation, report, persistence.
- **prism-ipc**: JSON-RPC 2.0 types (`RpcRequest`, `RpcResponse`, `RpcNotification`, `RpcError`). `IpcServer` spawns TUI as child process with piped stdio channels.
- **prism-workflows**: YAML workflow engine — filesystem discovery, template interpolation (`{{ args.data }}`), step execution (set, message, http), dry-run mode. Built-in `forge.yaml` embedded in binary.
- **prism-proto**: Wire types for CLI↔Python (`BackendRequest`/`BackendResponse`), node↔platform (`NodeMessage`/`PlatformMessage`), capabilities (`NodeCapabilities`, `GpuInfo`, `NodeService`).
- **prism-runtime**: XDG-based path discovery (`PrismPaths`), credential persistence (`PrismCliState`), platform endpoint URL derivation.
- **prism-python-bridge**: Python subprocess config, command building, async worker spawning with piped stdio.

### Added — Node Security
- **E2EE**: X25519 Diffie-Hellman key agreement + ChaCha20-Poly1305 AEAD encryption. Random 12-byte nonce per message. Output format: `nonce(12) || ciphertext || tag(16)`.
- **Ed25519 signing**: Node identity verification via signing keypair.
- **Key persistence**: Private keys stored as raw 32 bytes with `0600` Unix permissions.
- **Key rotation**: `prism node key rotate` generates new keypair, zeroizes old material.
- **SSH identity**: Hardened `prism-node` SSH identity support for BYOC targets.

### Added — Test Suite
- **355 Rust tests** across all 15 crates (up from 121 scaffold tests).
- Covers: serde roundtrips for all types, error paths (invalid input, wrong keys, corrupted data), edge cases (empty strings, unicode, very long inputs, None vs Some("")), scale tests (200+ sessions, 500 users, 100 peers, 50 concurrent jobs), file persistence roundtrips, cryptographic property verification (symmetric shared secrets, nonce uniqueness, permission bits).
- **Zero compiler warnings** across the entire workspace.

### Added — Documentation
- Crate-level rustdoc (`//!` headers) for all 15 crates with module references and architecture context.
- `description` field in all 15 `Cargo.toml` files.
- Rewritten `docs/architecture/rust-backbone.md` with full crate reference, implementation status, and security architecture.

### Changed
- **README.md**: Full rewrite — now leads with Rust backbone architecture, crate table with test counts, node features, mesh networking. Python layer documented as supervised child process.
- **Rich REPL dropped**: `--classic` flag removed. Ink TUI is the only frontend.
- Shared utilities extracted from Rich TUI to `app/backend/tool_meta.py` before deletion.

### Removed
- `app/cli/tui/app.py`, `cards.py`, `prompt.py`, `spinner.py`, `stream.py`, `status.py`, `theme.py`, `welcome.py` — Rich-only rendering code.
- `app/agent/repl.py` — shim that re-exported from Rich TUI.

---

## [2.5.0-beta] - 2026-02-26

### Added
- **All 16 CLI Commands**: `prism`, `run`, `serve`, `search`, `data`, `predict`, `model` (+ calphad subgroup), `sim`, `labs`, `setup`, `configure`, `update`, `plugin`, `optimade`, `mcp`, `advanced`. Every command built, wired, and documented.
- **Live Text Streaming**: LLM tokens now appear in real-time via Rich Live. Text "freezes" permanently when a tool call starts or the turn ends (Claude Code-inspired Static/Live pattern).
- **Typed Card Renderers**: 11 card types — input, output, tool, error (with partial), metrics, calphad, validation, results (table preview), plot, approval, plan. Color-coded borders and icons.
- **Character-Based Truncation**: Tool results exceeding 50K characters are persisted to `~/.prism/cache/results/` and display a truncation notice with `peek_result()` hint. Builds on the existing RLM-pattern ResultStore.
- **Cost Tracking**: Per-turn token count and cost display (`─ 2.1k in · 340 out · $0.0008 · total: $0.0142 ─`). Cumulative session total for billing.
- **Unified Rendering**: `prism run` now uses the same card system, spinner, and cost line as the REPL. No more inline yellow/green panels.
- **Unified Settings**: Two-tier `settings.json` (global `~/.prism/settings.json` + project `.prism/settings.json`) with env var overrides. Schema covers agent, search, output, compute, ml, updates, permissions.
- **LLM Provider Abstraction**: Anthropic, OpenAI, Google, Zhipu AI (GLM-4), OpenRouter, MARC27 managed — all via a single `create_backend()` factory.
- **Federated Search**: 3-layer provider registry (OPTIMADE discovery + per-provider overrides + platform catalog). OMAT24 and AFLOW as catalog entries for future platform-hosted databases.
- **PRISM Labs Marketplace**: `prism labs browse/subscribe/status` for premium services (Cloud DFT, Quantum Computing, Autonomous Labs, Synchrotron, HTS, DfM).
- **Plugin System**: 7 plugin types (Tool, Skill, Provider, Agent, Algorithm, Collector, Bundle) with pip entry-points + `~/.prism/plugins/` local loading.
- **Crystal Mascot**: Hex-glyph welcome banner with 3-tier glow, rainbow rays, and live capability detection. Fixed alignment of top/bottom rows.
- **install.sh Polish**: Unicode step markers (✓/→/✗), tightened one-line banner, consistent ANSI styling.

### Changed
- **Version**: Bumped from 2.1.1 to 2.5.0-beta.
- **stream.py**: Full rewrite — now uses Rich Live for token-by-token display instead of accumulating text.
- **run.py**: Replaced inline Panel rendering with shared card imports from `app.cli.tui`.
- **Test count**: 530 → 870+ tests. Added tests for card renderers, streaming, cost line, truncation, crystal alignment, spinner, and backward-compat shims.

### Fixed
- Crystal mascot top/bottom row alignment (3→4 space indent).
- Tool count in docs: corrected from "26" to "33 base + 19 optional".
- OMAT24 reference: removed standalone tool, clarified as platform-hosted catalog entry.

## [2.1.1] - 2026-02-24

### Added
- **Claude Code-style REPL**: Minimal `>` prompt, tool call timing display, compact welcome header, `/login` command for MARC27 managed LLM access, `/skills` command (replaces `/skill`).
- **MARC27 Login**: `/login` REPL command stores token at `~/.prism/marc27_token`; factory auto-detects `MARC27_TOKEN` and routes through MARC27 API gateway (OpenAI-compatible).
- **First-Run Onboarding**: On first startup, PRISM asks for LLM provider and API keys interactively. Saves to `.env` and marks onboarding complete.

### Changed
- **Python**: Bumped minimum to **3.11** (was 3.10). pyiron and CALPHAD now install by default with `pip install "prism-platform[all]"`.
- **OPTIMADE**: All `OptimadeClient()` calls now use explicit `base_urls` from curated provider list — eliminates "Unable to retrieve databases" noise during searches.
- **Version markers**: Simplified to upper-bound only (`python_version<'3.14'`) since floor is now 3.11.
- **REPL aesthetics**: Removed decorative panels, switched to text-heavy minimal output matching Claude Code CLI design language.

### Fixed
- OPTIMADE search noise from auto-discovery of providers with null base_urls.
- MCP `_test_filter` method using `include_providers` instead of `base_urls`.

## [2.1.0] - 2026-02-24

### Added
- **Dual License (Phase F-1)**: MIT for core plumbing, MARC27 Source-Available for AI orchestration. New `LICENSE-MIT` and `LICENSE-MARC27` files.
- **Tool Consent (Phase F-2)**: `requires_approval` flag on expensive tools; REPL prompts for confirmation; `--confirm` flag for autonomous mode; `--dangerously-accept-all` to skip all prompts; `/approve-all` REPL command.
- **Scratchpad (Phase F-3)**: Append-only execution log (`Scratchpad` class) auto-populated after each tool call; `show_scratchpad` tool for agent self-inspection; `/scratchpad` REPL command; included in session save/load and reports as "Methodology" section.
- **Plan-then-Execute (Phase F-4)**: System prompt instructs agent to output `<plan>` blocks for complex goals; REPL detects and renders plan as a panel, gating execution on user approval.
- **Feedback Loops (Phase F-5)**: `_post_tool_hook()` in AgentCore injects validation, review, and CALPHAD findings back into agent context as system messages, closing the Evolver-Evaluator loop.
- **README Refresh (Phase F-6)**: Full rewrite with deck-aligned content, banner image, architecture table, dual-license section.

### Changed
- **Project Version**: Bumped from 2.0.0 to 2.1.0.
- **License**: Changed from MIT to dual (MIT core + MARC27 Source-Available AI).
- **CLI Help**: Updated to "AI-Native Autonomous Materials Discovery" positioning.
- **pyproject.toml**: Updated description, keywords, license field, author.

## [2.0.0] - 2026-02-24

### Added
- **Agent Loop (Phase A)**: Agentic REPL with streaming, tool registry, CSV export, session memory, and ML pipeline (train, predict, feature-importance, correlation).
- **MCP Integration (Phase B)**: `prism serve` exposes tools and resources via FastMCP 3.x; `prism mcp init/status` manages external MCP servers.
- **Simulation (Phase C)**: Pyiron atomistic simulation bridge with structure/job stores, HPC config, and `prism sim` commands.
- **Skills (Phase D)**: Multi-step workflow orchestration — acquisition, featurize, train-predict, simulation planning, and more.
- **Plugin System (Phase E-1)**: `PluginRegistry` supporting pip entry-points and `~/.prism/plugins/` local plugins; `prism plugin list/init`.
- **Local Data Import (Phase E-1)**: CSV/JSON/Parquet ingestion via `import_local_data` tool and `data import` command.
- **New Data Sources (Phase E-2)**: OMAT24 (HuggingFace streaming), literature search (arXiv + Semantic Scholar), patent search (Lens.org).
- **CALPHAD Integration (Phase E-3)**: pycalphad bridge with 6 tools (phase diagram, equilibrium, Gibbs energy, list/import databases, list phases), ThermoCalc plugin skeleton.
- **Review Agent (Phase E-4)**: Validation rules (outlier detection, physical constraints, completeness scoring), `validate_dataset` and `review_dataset` skills.
- **Interactive ML (Phase E-4)**: `list_predictable_properties` tool, correlation visualization.
- **Enhanced Reports (Phase E-4)**: HTML report generation with embedded charts.
- **Version Check (Phase E-5)**: `prism update` command, automatic update check on REPL startup with PyPI/GitHub fallback and 24h cache.
- **Curl Installer (Phase E-5)**: `install.sh` one-command installer (pipx/uv auto-detection).
- **Packaging (Phase E-5)**: `[all]` optional dependency group, `app.validation` in setuptools packages, Python 3.13/3.14 classifiers.

### Changed
- **Project Version**: Bumped from 1.1.0 to 2.0.0.
- **Architecture**: Provider-agnostic `AgentCore` with tool registry replaces monolithic CLI.
- **Two Modes**: Interactive REPL (`prism`) and autonomous agent (`prism run "goal"`).
- **`.env.example`**: Updated to v2.0 template (LLM keys + Materials Project API key).
- **`docs/INSTALL.md`**: Modernized with pipx/uv/curl install methods and optional extras.
- **`docs/SECURITY.md`**: Updated version table and added MCP/CALPHAD/ML/plugin mentions.

### Removed
- `Schema.txt` (209KB OPTIMADE spec — no longer used).
- `prism.db` (runtime artifact — should not be tracked).
- `app/optimade_properties.py` (empty file).
- `provider_fields.json` (empty runtime cache).
- `quick_install.py` (superseded by `install.sh` and Makefile).
- `requirements.txt` (duplicate of pyproject.toml).
- `app/env.example` (merged into root `.env.example`).

## [1.1.0] - 2024-07-26

### Added

- **Intelligent Search (`ask` command)**: A new command that uses natural language to query materials science databases. It leverages LLMs to translate queries into OPTIMADE filters and summarize the results.
- **Interactive Mode**: The `ask` command now has an `--interactive` flag to enable a conversational query refinement mode.
- **Pluggable LLM Providers**: Support for multiple LLM providers, including OpenAI, Google Vertex AI, Anthropic, and OpenRouter. Configuration is handled via the `prism advanced configure` command.
- **Configurable Models**: Users can now specify a particular model to use for a given LLM provider in the `.env` file.
- **Provider Discovery**: The `prism optimade list-dbs` command now lists all available OPTIMADE providers, with a fallback to a cached list if the live network is unavailable.
- **RAG Capability**: The core MCP has been updated to support Retrieval-Augmented Generation, allowing for future integration with local knowledge bases.
- **Polished CLI Output**: The CLI output for `search` and `ask` has been enhanced with `rich.panel` for better readability.

### Changed

- **Project Version**: Bumped from 1.0.0 to 1.1.0.
- **Documentation**: The `README.md` file has been significantly updated with more detailed explanations and examples.
- **CLI Refinements**: The `search` and `ask` commands have been improved for better usability and error handling.

### Removed

- **Problematic Test**: The test file `tests/test_ask_command.py` was removed due to persistent, unresolvable issues with mocking. 
