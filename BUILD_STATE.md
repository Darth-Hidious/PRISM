# PRISM Build State — 2026-06-24

## Mission IR
PRISM is a private, permissioned AI compute fabric for materials discovery.
Every material discovered has full provenance: which LLM proposed it, which
tool evaluated it, with what parameters, and what data fed it.
Local-first (Turso + FalkorDB), cloud-optional (mesh sync + Turso sync).

## What's Done (working)

### Backend (Rust)
- [x] Compiles clean, 2400+ Rust tests pass, 848 Python tests pass
- [x] `prism backend` works end-to-end with local Gemma 4 12B via llama.cpp
- [x] Tool calling works: agent reasons → selects tool → executes → formats results
- [x] Tool stuffing fixed: top-15 keyword-filtered tools per turn (was 99 = 21K tokens)
- [x] `auto_approve` properly read from init params and wired through to agent loop
- [x] LLM routing: `prism use local` affects backend, query, ingest, node up
- [x] Double `/v1` URL bug fixed in LLM client
- [x] `reasoning_content` handled for Gemma 4 thinking mode (streaming + non-streaming)
- [x] stdin-close no longer kills pending turns (waits up to 300s)
- [x] Default timeout increased to 300s for local models

### TUI (Rust + Ratatui 0.30)
- [x] Full-screen ratatui TUI: chat panel, status bar, input box
- [x] Spawns `prism backend` as subprocess, JSON-RPC over stdio
- [x] Handles: streaming text, tool cards, approval prompts, slash commands
- [x] TTY detection (doesn't crash when stdin is piped)
- [x] TUI MCP tools installed: pty-debug, terminalcp, iterm-tmux, tui-driver, tui-test

### Provenance (Turso)
- [x] `prism-provenance` crate: local-first Turso DB, provenance records, causal chains
- [x] Schema: id, timestamp, session, action_type, actor, tool, llm_model, input, output, parent_id, material_ref, confidence, tags
- [x] Query by session, by material, trace full causal chain
- [x] 2/2 tests pass

### GFlowNet Tools (alloy-mogfn ported)
- [x] `gfn_sample` — cold-start MoGFN alloy generation (physics-only, zero data)
- [x] `gfn_discover` — active-learning discovery loop
- [x] `gfn_evaluate` — physics descriptors for a composition
- [x] `gfn_elements` — element table with physical constants
- [x] 18 Python tests pass
- [x] Agent successfully calls gfn_evaluate end-to-end with Gemma

### Dead Code Removed
- [x] boot::progress, boot::progress_done (never called)
- [x] ProxyHandle::shutdown (duplicated Drop impl)
- [x] LlmClient::post_no_retry (never called)
- [x] Stale #[allow(dead_code)] on LoginMode
- [x] Duplicate-name detection added to Python ToolRegistry

## What Remains

### High Priority — Core System
1. **FalkorDB graph store** — implement `GraphStore` trait for FalkorDB (Cypher-compatible,
   Redis-based). Replace `Neo4jGraphStore` in `crates/ingest/src/graph.rs`. FalkorDB crate
   is `falkordb = "0.10"` (already in Cargo.toml). Same Cypher queries, different transport.
2. **Wire provenance into agent loop** — record every tool call in `agent_loop.rs`:
   - After tool execution at line ~608, create a `ProvenanceRecord` with tool_name,
     input args, output result, llm_model, session_id, parent_id (previous tool call).
   - Store via `ProvenanceStore::record()`.
   - Add `prism provenance` CLI command to query the trail.
3. **Turso agent memory** — give the agent recall of past tool results:
   - `memory_recall` tool: search past provenance records by keyword/material.
   - `memory_materials` tool: list all materials discovered in this session.
   - Prevents redundant tool calls (agent can check "did I already evaluate this?").

### High Priority — Testing
4. **Test TUI with pty-mcp-server** — drive the TUI like a real user:
   - Spawn `prism tui` in a PTY
   - Type a message, verify streaming response appears
   - Type a tool-call request, verify tool card renders
   - Test approval flow (y/n/a keys)
   - Test slash commands (/tools, /status)
5. **Stress test** — long sessions, rapid tool calls, malformed inputs, connection drops

### Medium Priority — Production
6. **Terminal app polish** — all CLI commands work locally without cloud auth:
   - `prism billing`, `prism discourse`, `prism deploy` need graceful offline fallback
   - `prism research` needs local mode (use agent + tools instead of cloud API)
7. **Extract duplicate relogin logic** — 3 near-identical blocks in main.rs (Tui, Resume, Setup)
   ~95 lines duplicated. Extract into `ensure_fresh_auth()` function.

### Lower Priority — Cloud + Clients (after local is solid)
8. **marc27 platform rebuild** — port marc27-core, marc27-api, marc27-platform from SSD
9. **Swift macOS client** — scaffolded at macos/PRISMMac/, needs JSON-RPC client implementation
10. **Web app** — React/Vite chat client for non-Mac systems
11. **Mesh** — E2EE node-to-node, mDNS discovery (already works, needs stress testing)

### Tools Focus (once system is solid)
12. **Search tool overhaul** — the first thing the agent needs before proposing anything:
    - Fix `optimade` Python dep (currently not installed, search fails)
    - Add Turso-backed result caching (don't re-query external APIs for same search)
    - Add search→discovery loop: feed search results as seed data into gfn_sample
    - Add property-based ranking (sort by relevance to user's goal, not provider order)
    - Add local offline fallback (cache known materials in FalkorDB graph)
    - Wire search results into provenance chain (parent_id links search→evaluate)
    - Current: federated OPTIMADE architecture is solid but it's a thin HTTP wrapper

### Marketplace & Monetization (parallel track)
13. **`prism tools init`** — scaffold a new tool project:
    - Creates `tool.toml` manifest (name, description, schema, pricing, deps)
    - Creates `tool.py` with `run(**kwargs)` template
    - Creates `test_tool.py` with basic validation
    - One command, zero boilerplate
14. **`prism tools validate`** — dry-run the tool:
    - Schema validation (does input match TOOL_SCHEMA?)
    - Dependency check (are all imports available?)
    - Smoke test (call run() with sample args from schema)
    - Security check (no network access unless declared, no filesystem writes unless declared)
15. **`prism tools publish`** — one command to marketplace:
    - Packages tool + manifest into a tarball
    - Uploads to MARC27 marketplace API
    - Sets pricing (free / per-call / per-result / subscription)
    - Creator gets a slug (e.g. `predict.elastic_moduli.mace`)
16. **`prism marketplace install <slug>`** — already exists, needs:
    - Pricing awareness (show cost before install, require confirmation for paid tools)
    - Auto-install Python deps from tool manifest
    - Auto-register in tool catalog (already does this via custom_loader)
17. **Metering + billing** — every marketplace tool call:
    - Logged to Turso provenance (tool_name, caller, timestamp, cost)
    - Usage counted per user per billing period
    - Per-call: charge per invocation
    - Per-result: charge per returned result row
    - Subscription: monthly quota, overage billed
    - Creator payout: monthly, based on usage from all users
18. **Tool execution sandboxing** — marketplace tools run in restricted environment:
    - No filesystem access outside workspace
    - No network unless declared in manifest
    - CPU/memory limits
    - Timeout enforcement

### Science Tools (the actual product)
19. **JAX-MD integration** — molecular dynamics oracle for alloy-mogfn
    - Wire a MACE ML potential as the `energy_fn_builder` (relaxed energies, elastic moduli)
    - This gives the FIRST real physics signal beyond empirical rules of thumb
    - MACE-MH-1 is already on the marketplace (free) — install + wire into JaxMDOracle
20. **CALPHAD tools** — phase diagram calculations
21. **pyiron integration** — atomistic simulation workflows
22. **DFT tools** — Quantum ESPRESSO / VASP wrappers (needs HPC hooks)
23. **ML predictors** — property prediction models (sklearn, GNN)
24. **Multi-fidelity TTT** — materials design with uncertainty quantification

## Can PRISM Discover New Materials Today?

**Partially yes.** The core loop works:
- Agent receives a materials science question
- Agent selects the right tool (gfn_evaluate, gfn_sample, etc.)
- Tool executes and returns physics descriptors or sampled compositions
- Agent formats and presents results

**What's missing for full discovery:**
- **Knowledge graph** (FalkorDB) — no persistent memory of discovered materials yet
- **Provenance** — not wired into agent loop (no traceability yet)
- **Active learning loop** — `gfn_discover` works but needs an oracle (JAX-MD/DFT) to
  evaluate proposed compositions beyond physics descriptors
- **More tools** — only 4 GFlowNet tools + 95 inherited tools. Need JAX-MD, CALPHAD,
  pyiron, DFT, ML predictors for real materials discovery

**Bottom line:** The system can generate and evaluate alloy compositions today.
Once FalkorDB + provenance + more tools are wired in, it can run autonomous
discovery campaigns with full traceability.

## Key Files Modified

- `crates/llm/src/lib.rs` — URL fix, reasoning_content, timeout, chat_completions_url()
- `crates/cli/src/main.rs` — LLM routing (backend + build_llm_config + node up)
- `crates/agent/src/protocol.rs` — stdin-close fix, auto_approve from init params
- `crates/agent/src/agent_loop.rs` — tool filtering (definitions_for_query)
- `crates/agent/src/tool_catalog.rs` — definitions_for_query() method
- `crates/tui/` — new crate, full ratatui TUI
- `crates/provenance/` — new crate, Turso-backed provenance
- `app/tools/gflownet/` — alloy-mogfn source ported
- `app/tools/gflownet_tools.py` — PRISM tool wrappers
- `app/plugins/bootstrap.py` — gflownet tool registration
- `app/tools/base.py` — duplicate-name detection
- `crates/cli/src/boot.rs` — dead code removed
- `crates/cli/src/platform_bridge.rs` — dead code removed
- `Cargo.toml` — ratatui, crossterm, turso, falkordb deps added
- `pyproject.toml` — gflownet extras added

## How to Resume

```bash
cd ~/Downloads/PRISM
# llama-server with Gemma should be running on port 8081
# if not: llama-server --model ~/Downloads/gemma4-12b/gemma-4-12B-it-qat-UD-Q4_K_XL.gguf --port 8081 --ctx-size 32768 --n-gpu-layers 99 &
cargo build  # should compile clean
.venv/bin/python -m pytest tests/test_gflownet_tools.py -q  # 18 tests
cargo test -p prism-provenance  # 2 tests
# test backend:
printf '{"jsonrpc":"2.0","method":"init","id":1,"params":{"auto_approve":true,"resume":""}}\n{"jsonrpc":"2.0","method":"input.message","id":2,"params":{"text":"Evaluate Fe0.3 Ni0.3 Cr0.4 with gfn_evaluate"}}\n' | ./target/debug/prism --python .venv/bin/python backend --project-root .
```

## Updated Priority Build Order (2026-06-25)

### Phase 1: Local Foundation — DONE
- [x] Fix multi-turn context: message queue instead of rejecting
- [x] Swap GFlowNet → MCMC (alloy_sample, alloy_discover — no torch)
- [x] Fix LLM routing (backend reads config.toml [chat])
- [x] Fix LLM client (double /v1 URL, reasoning_content, timeout)
- [x] Fix stdin-close killing pending turns
- [x] Search engine: async httpx, circuit breaker, URL fixes, redirect following
- [x] MP API key auto-resolved from MARC27 credentials
- [x] CLI flags: --resume, --model, --auto-approve, --offline
- [x] Mesh RBAC gate (no auth = refuse to start)
- [x] Dead code removed
- [x] All Rust tests pass (1500+)
- [x] All Python tests pass (848)
- [x] Agent chains tools across turns (alloy_sample → gfn_evaluate)

### Phase 1b: Remaining Local Fixes — DONE
- [x] TUI rewritten with TEA architecture (ratatui-textarea, tokio::select!, panic hook)
- [x] Token-by-token streaming (162 deltas instead of 1 blob)
- [x] Thinking tokens separated from response (dimmed, collapsible, Ctrl-T to expand)
- [x] Streaming metrics: tokens/sec in status bar (Ctrl-M to toggle)
- [x] Cost display toggle (Ctrl-$ to hide for local models)
- [x] Loading spinner while waiting for first token
- [x] Approval popup overlay
- [x] TTY check with helpful error message
- [x] All 1500+ Rust tests pass
- [x] All 848 Python tests pass
- [x] Agent chains tools across turns (alloy_sample → gfn_evaluate)
- [x] Message queue (multi-turn context works)

### Phase 1c: Done
- [x] Cloud-only tools degrade gracefully (return errors, don't crash)
- [x] Mesh RBAC gate works (shows warning without auth, offline mode bypasses for testing)
- [x] Provenance wired into agent loop via hook system (Turso, every tool call recorded)
- [x] Release binary (56MB, 20MB RAM vs 289MB debug)
- [x] KV cache optimization (q4_0 K+V, flash attention, 7GB vs 12GB)
- [x] TUI memory efficient (500 message sliding window)

### Phase 2: Next
1. **Turso agent memory** — `memory_recall` tool that searches provenance DB
2. **FalkorDB graph store** — replace Neo4j
3. **marc27-core rebuild** — port from SSD, login, RBAC
4. **Marketplace** — tool publish + metering

### Phase 2: Discovery Engine (later)
4. Campaign agent crate (prism-campaign)
5. LLM-GFlowNet (LLM as GFlowNet policy)
6. Turso agent memory + provenance wiring
7. FalkorDB graph store

### Phase 3: Platform Integration (later)
8. marc27-core connection + login rebuild
9. Marketplace + metering
10. Swift + Web clients

### Phase 4: Advanced Tools (later)
11. JAX-MD oracle, DFT, CALPHAD, pyiron
12. Tmax training pipeline (tabled — no compute right now)

### Tabled for Later
- Tmax training pipeline (no compute)
- LLM-GFlowNet (needs API key)
- marc27 platform rebuild

## LLM-GFlowNet Design

Key papers:
- arXiv:2410.13768 (Buehler, MIT) — LLM multi-agent + GNN for alloy design
- arXiv:2210.12765 (MoGFN, Bengio/Miret) — multi-objective GFlowNets
- arXiv:2601.07966 (DataScribe) — "Use VAE, GFlowNet, or LLM-based generators"

Architecture: LLM as GFlowNet forward policy
- State: partial composition (element fractions so far)
- Action: add next element or terminate
- Policy: LLM(next_element | state, reward_signal, chemistry_context)
- Reward: physics descriptors + surrogate predictions + (optionally) DFT
- Training: trajectory-balance loss updates LLM to sample ∝ R
- Key advantage: LLM brings chemical knowledge, GFlowNet brings diversity

Implementation:
1. New module: app/tools/gflownet/llm_policy.py
2. Wraps LLM client (prism-llm) as GFlowNet policy
3. Prompts LLM with: current composition, available elements, reward history
4. Parses LLM output: next element + fraction
5. Integrates with existing MOGFNTrainer via Surrogate interface
6. Fine-tunes via LoRA for efficiency on local GPU