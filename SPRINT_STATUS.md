# PRISM Sprint Status — Full Audit (2026-04-04)

## What's Shipped (v2.6.1)

**Rust Agent (crates/agent/) — DONE:**
- [x] TAOR agent loop (think-act-observe-repeat)
- [x] 18-model registry with pricing (Anthropic/OpenAI/Google/Zhipu)
- [x] Transcript compaction (lazy at 20 turns, structured summaries)
- [x] 3-tier permissions (ReadOnly/WorkspaceWrite/FullAccess)
- [x] Pre/post hooks (safety/cost/audit)
- [x] Scratchpad (append-only execution log)
- [x] Session persistence (JSONL, rotation at 256KB, resume/fork)
- [x] Prompts (interactive + autonomous modes, 8-step thinking)
- [x] Doom loop detection (3 identical failing calls)
- [x] Large result handling (>30K chars → truncate + peek)
- [x] JSON-RPC protocol server (slash commands, model switching)

**Infrastructure — DONE:**
- [x] Kafka producer wired into NodeState + all mesh handlers
- [x] Kafka announce on startup, goodbye on shutdown
- [x] Spark Docker orchestration (bitnami/spark:3.5, ports 7077+8088)
- [x] 3 PySpark Python tools (submit_job, status, batch_transform)
- [x] Federated queries (cross-mesh search via REST)
- [x] BYOC compute (SSH, K8s, SLURM) wired to CLI
- [x] Custom tool plugins (~/.prism/tools/*.py)
- [x] Marketplace CLI (search, install, info)
- [x] APFS disk dedup fix
- [x] Software detection (llama.cpp, vLLM, QE, Jupyter, etc.)
- [x] Install.sh rewritten (Rust binary only, 82 lines)
- [x] License split (MIT tools, MARC27 everything else)

**Tests:** 494 Rust tests + 551 Python tests passing

---

## Sprint 4 Remaining — Must Finish

### ~~S4.1 LLM Streaming~~ — DONE
- Added `chat_with_tools_streaming()` with true SSE via `bytes_stream()`
- Agent loop emits `TextDelta` as tokens arrive from LLM

### ~~S4.2 Approval Flow Callback~~ — DONE
- Added `ApprovalResponse` enum (Allow/Deny/AllowAll)
- tokio::mpsc channel between protocol and agent loop
- `input.prompt_response` handler wired in protocol.rs
- Agent loop blocks on approval, denies if user says no

### S4.3 Model Switching at Runtime (LOW)
- **What:** `/model gpt-4o` updates config but LlmClient isn't reconstructed.
- **Fix:** Reconstruct LlmClient when model changes in protocol handler.
- **Where:** `crates/agent/src/protocol.rs`

### S4.4 MARC27 Platform API Fixes (EXTERNAL BLOCKER)
From `MARC27_CORE_FIXES_NEEDED.md`:
- `GET /api/v1/projects` returns empty — breaks 7 tools
- `GET /api/v1/knowledge/graph/search` returns empty — breaks 7 tools
- `POST /api/v1/knowledge/embed` broken — blocks ingest
- `GET /api/v1/compute/gpus` and `/providers` empty — 6 compute tools dead
- `GET /api/v1/marketplace/resources` empty — labs tool broken
- **Action:** These need marc27-core fixes. PRISM code is correct.

### S4.5 Support Tickets API (NOT STARTED)
From `MARC27_SUPPORT_API_SPEC.md`:
- `POST /api/v1/support/tickets` — create from `prism report`
- `GET /api/v1/support/tickets` — list user's tickets
- `PATCH /api/v1/support/tickets/{id}` — update status
- Dashboard page at platform.marc27.com/dashboard/support
- **Action:** Needs marc27-core implementation. PRISM's `prism report` command exists but currently only files GitHub issues.

### S4.6 13 Failing Python MCP Tests (LOW)
- Pre-existing failures in `test_mcp_server.py` — schema validation issues.
- Not from today's changes.

---

## Sprint 5 — Materials Tools + Jupyter

### S5.1 PyIron Expansion (HIGH — user knows the developer)
- **Current:** Basic bridge only (`app/tools/simulation/bridge.py`)
- **Need:** Full HPC orchestrator — SLURM/PBS/SGE job submission, dependency chains, VASP/LAMMPS/QE wrappers, provenance tracking

### S5.2 Quantum ESPRESSO Tool (HIGH)
- **Current:** Detected in software scan, no Python tool
- **Need:** Input file generation, job submission (via PyIron or direct), output parsing (bands, DOS, phonons, relaxation)

### S5.3 ASE Tool (MEDIUM)
- Structure manipulation, calculators, optimization, NEB for transition states

### S5.5 MACE / CHGNet (MARKETPLACE — not PRISM)
- These are marketplace products on MARC27 platform
- PRISM just needs the "install from marketplace" flow to work

---

## Sprint 6 — Flutter App

### S6.1 Flutter Multi-Platform App
- iOS/Android/Web/Desktop
- Connects to `crates/server/` via HTTP/WebSocket (Axum, already exists)
- Same JSON-RPC protocol as Ink TUI, just different transport

### S6.2 3D Materials Visualization
- Crystal structures with provenance
- Phase diagrams, property plots

### S6.3 Project Manager vs Researcher Views

---

## PLAN_V2.6.md Reconciliation

These were planned in PLAN_V2.6.md. Here's what's done vs what remains:

| Item | Status | Notes |
|------|--------|-------|
| Session persistence & resume | DONE | Rust `crates/agent/src/session.rs` |
| Intelligent compaction | DONE | Rust `crates/agent/src/transcript.rs` |
| Slash commands (7) | DONE | Rust `crates/agent/src/protocol.rs` |
| Permission modes (3-tier) | DONE | Rust `crates/agent/src/permissions.rs` |
| Streaming markdown buffer | NOT DONE | Needs LLM streaming first (S4.1) |
| Tool input validation | NOT DONE | Validate args against JSON Schema |
| Hook shell execution | PARTIAL | Rust hooks exist, shell hook runner not done |
| Multiline input | NOT DONE | Frontend TUI change |
| Model alias resolution | NOT DONE | `opus` → model ID |
| Output format toggle | NOT DONE | `--output-format json` |
| Command palette (Ctrl+K) | NOT DONE | TUI feature |
| Session export | NOT DONE | `prism session export --format md` |

---

## Codebase Size (as of 2026-04-04)
- 16 Rust crates: ~28,000 LOC
- Python tools: ~9,700 LOC
- Total: ~40,000 LOC
- Rust tests: 494 passing
- Python tests: 551 passing (13 pre-existing MCP failures)
