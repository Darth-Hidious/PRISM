# PRISM Session Handoff — Claude Agent

You are picking up a PRISM development session. Below is a comprehensive summary of what was built, what was found, and what needs to happen next. Read carefully before acting.

## What PRISM Is

PRISM is an AI-native autonomous materials discovery platform by MARC27. It has a Rust backbone (43 crates), a Python tool plane (~104 tools), a Ratatui TUI, an agent loop with LLM routing, a provenance system (Turso), a workflow engine (YAML DAG), mesh networking, and a compute broker. The LLM agent designs search spaces from literature research; deterministic tools evaluate candidates. The LLM cannot predict stable compositions — physics-grounded tools do that.

## Repositories

- **PRISM (public)**: `https://github.com/Darth-Hidious/PRISM` — the core platform. Branch `fix/cli-research-targets-engine-verb-shim` has the TUI hardening + mesh tools + KAG tools. Branch `feat/kag-tool-reasoning` has the KAG tool reasoning + session context.
- **PRISM Alpha (private)**: `https://github.com/Darth-Hidious/prism-alpha` — the standalone materials discovery toolkit (GFlowNet + multi-fidelity evaluators). Separated from core PRISM because it's proprietary.

## What Was Built This Session

### 1. TUI Hardening (Patches 1-4B, on `fix/cli-research-targets-engine-verb-shim`)

Complete rewrite of the TUI test infrastructure:
- **Patch 1**: Enriched `AgentMsg` enum with all wire fields the backend already sends (ToolStart, ToolCard, ApprovalPrompt, Cost, etc. now capture call_id, preview, provenance_id, input_tokens, choices, risk_level)
- **Patch 1.5**: Clippy baseline clean
- **Patch 2**: ANSI/control sanitizer at all state ingress points (`sanitize_for_render` in `crates/tui/src/sanitize.rs`). Strips CSI, OSC, DCS, BEL, BS, CR, DEL, C1 controls. Preserves Unicode.
- **Patch 3A-3B**: Fake backend with 9 deterministic scenarios (`--fake-backend --scenario <name>`). Scenarios: basic_chat, streaming_answer, thinking_stream, tool_success, tool_error, approval_required, cost_metrics, backend_warning_error, ansi_injection.
- **Patch 3A.5**: SIGINT shutdown fix. Root cause: raw terminal mode swallows Ctrl-C on PTY. Fix: raw `libc::sigaction` handler sets AtomicBool, checked every 200ms via select! timeout.
- **Patch 3C**: Verification harness (`scripts/verify-tui.sh`), CJK language drift checker, AGENTS.md, PRISM_TUI_VERIFY.md, TUI MCP verification docs, Claude/GLM prompt templates.
- **Patch 3D**: cfg-gate SIGINT handler for Windows portability.
- **Patch 4A-4B**: 20 Ratatui snapshot tests via TestBackend + insta.

Test count: 148 unit + 20 snapshots + 8 PTY = 176 TUI tests, all passing.

### 2. Mesh Tools (6 tools in core PRISM)

`app/tools/mesh.py` — wraps the local PRISM node's mesh API:
- `mesh_peers`, `mesh_health`, `mesh_subscriptions` (read-only)
- `mesh_publish`, `mesh_subscribe`, `mesh_unsubscribe` (approval-gated)
- Node URL resolved from: PRISM_NODE_URL env → PRISM_NODE_PORT env → prism.toml [node] port → default 7327

### 3. PRISM Alpha (private repo, `prism-alpha`)

Standalone pip package for materials discovery. NOT in core PRISM.

**Tools:**
- `alpha_predict`: multi-fidelity composition evaluator (physics + M3GNet + MACE-MH-1)
- `alpha_discover`: GFlowNet-AL active learning discovery campaign
- `alpha_list_models` / `alpha_register_model`: pluggable model registry

**Architecture:**
- **JAX MoGFN-PC** trainer (`prism_alpha/gfn/__init__.py`): torch-free rewrite of Jain et al. (ICML 2023) Algorithm 1. Trajectory balance loss (Malkin et al., NeurIPS 2022). LeakyReLU MLP with progress feature + legal action masking. Separate Adam optimizers (lr=1e-3 policy, lr_z=1e-2 logZ). Validated: loss 955→3.5 in 500 steps on W-Mo-Ta-Nb-Cr.
- **Active learning loop** (`prism_alpha/al/__init__.py`): pluggable generator (gfn, mcmc, random, custom). All hyperparameters from agent config — nothing hardcoded. RandomForest bootstrap uncertainty for UCB acquisition. Pareto front tracking.
- **Pluggable model registry** (`prism_alpha/registry.py`): JSON-based config in `~/.prism/alpha/models/`. Supports builtin, local_python, local_subprocess, cluster_exp, mesh_remote, marc27_compute. Anyone can register a model (professor's CE, custom ML, remote DFT).

**Validated against literature:**
- W-Nb-Mo-Ta refractory HEA: GFlowNet found delta=2.39% (literature: Senkov equiatomic = 4.3%). Better than known baseline.
- MACE-MH-1 formation energy for WNbMoTa: -0.0084 eV/atom (correct — stable single-phase HEA).
- M3GNet elastic constants benchmarked vs NIST JPCRD experimental values: C11/C12 within 7% for FCC metals, C44 systematically 70-300% too high (known ML limitation).

### 4. KAG Tool Reasoning + Session Context (on `feat/kag-tool-reasoning`)

Two Python-side tools that help the LLM reason about tool selection:

- **`tool_reasoning`** (`app/tools/tool_reasoning.py`): KAG-style intent classification (arXiv:2409.13731). Decomposes user requests into logical forms → recommends tool sequences with data-flow graphs. 9/9 intent tests pass. Verified with local Gemma 4 12B — agent called the tool, understood the recommendation, produced a correct 5-step plan.
- **`session_context`** (`app/tools/session_context.py`): Running structured knowledge base that survives chat history compaction. DIKW hierarchy (Data → Information → Knowledge → Wisdom). Records evaluations, tracks best-per-objective, element systems explored. Persists to `~/.prism/sessions/`. 18 tests pass.

27 total tests, all passing.

## Critical Findings

### Finding 1: Python 3.14 + torch = segfault
torch 2.12.1 crashes on Python 3.14.3. This breaks:
- The old torch-based GFlowNet trainer (`gfn/trainer.py`)
- MEGNet via matgl (uses torch internally)
- `test_gflownet_tools.py` (segfaults during pytest)
- `test_tool_server.py` (segfaults under full suite load)

**Workaround**: MACE-MH-1 runs in a separate Python 3.13 venv (`~/.prism/venv-mace/`). JAX GFlowNet replaces torch GFlowNet. M3GNet (matgl) works on 3.14 via TF/JAX backend.

### Finding 2: M3GNet formation energy is unreliable for complex alloys
The M3GNet Eform model evaluates a crystal STRUCTURE, not just a composition. The SQS structure builder creates approximate FCC decorations that give wildly wrong energies (+5.9 eV/atom for Cu-Ni instead of ~0, +98 eV/atom in some cases).

**Workaround**: Use MACE-MH-1 for formation energies. MACE computes total energy minus reference energies = correct formation energy. Documented in the M3GNetEformVerifier docstring.

### Finding 3: Agent loop tool selection is lexical only
`tool_catalog.rs:130` uses `query_lower.contains(word)` keyword matching. No embeddings, no semantic similarity. "Evaluate this alloy's properties" won't match `alpha_predict`. The `definitions_for_query` function also uses the ORIGINAL user message for every iteration, not the current context.

**Partial fix**: The KAG `tool_reasoning` tool (Python-side) gives the agent a structured way to plan tool sequences. But the Rust-side `definitions_for_query` still limits which tools the LLM sees.

### Finding 4: Compaction loses structured data
The Rust transcript compaction (`transcript.rs:215`) builds a text summary of old entries. Structured data (compositions, property values, provenance IDs) is lost. After compaction, the agent can't query specific results.

**Partial fix**: The `session_context` tool maintains structured knowledge in Python that persists to disk. But it requires the agent to proactively call it.

### Finding 5: Alpha code was pushed to public repo
The proprietary PRISM Alpha code was accidentally pushed to the public PRISM repo in commits `58bf14b` through `1c737fd`. Removed from HEAD in commit `437e524`, but still exists in git history.

**Action needed**: Consider force-pushing rewritten history to scrub the old commits, or accept the exposure.

## What Needs To Happen Next

### High Priority
1. **Scrub git history** on public PRISM repo if Alpha code exposure is a concern
2. **Fix M3GNet structure builder** — use proper relaxed structures or composition-only prediction
3. **Wire MACE-MH-1 into alpha_discover** as the expensive oracle — it works standalone but hasn't been tested end-to-end in the AL loop
4. **Benchmark MACE-MH-1 elastic constants** vs NIST experimental values (C44 should be better than M3GNet)
5. **Run alpha_discover with mixed verifiers** (physics for GFlowNet training, MACE-MH-1 for oracle evaluation)

### Medium Priority
6. **Cluster expansion integration** — professor's models. JSON format defined in `registry.py`, Cantor alloy (CoCrFeMnNi) CE data available on Zenodo (Chen 2026)
7. **Custom MACE head fine-tuning** — MACE v0.3.16 supports `--foundation_model mh-1 --freeze --heads prism_custom`. Need to wire `alpha_finetune` tool.
8. **Crystal-GFN architecture** (Hernández-García et al., NeurIPS 2023) — currently the GFlowNet only samples compositions (atom bags). Crystal-GFN samples space group → composition → lattice parameters. This would give much richer structure generation.

### Architecture (for the user's spine rewrite)
9. **Token counting before LLM calls** — `TurnBudget` has `max_input_tokens: 200_000` but no actual counting happens
10. **Dynamic system prompt** — currently static. Should include tool relationship graph and domain context
11. **Background compaction** — use idle time (tool execution) to compact and build ontologies
12. **Tool filtering should use current iteration context** — not just the original user message

## How To Verify Everything Works

```bash
# TUI hardening
cd ~/Downloads/PRISM
bash scripts/verify-tui.sh  # 6 checks: fmt, build, test, clippy, PTY, CJK

# KAG tools
.venv/bin/python -m pytest tests/test_kag_tools.py -v  # 27 tests

# PRISM Alpha
cd ~/Downloads/prism-alpha
pip install -e .
.venv/bin/python -c "from prism_alpha.tools import alpha_predict; print(alpha_predict(formula='W0.25 Mo0.25 Ta0.25 Nb0.25', verifiers='physics'))"

# Local LLM test
printf '{"jsonrpc":"2.0","method":"init","id":1,"params":{"auto_approve":true,"resume":""}}\n{"jsonrpc":"2.0","method":"input.message","id":2,"params":{"text":"Use tool_reasoning to plan: find stable W-Mo-Ta-Nb alloy"}}\n' | PRISM_PYTHON=.venv/bin/python ./target/debug/prism backend --project-root .
```

## Key Files

| Purpose | Location |
|---------|----------|
| TUI tests | `crates/tui/tests/unit.rs`, `crates/tui/tests/render_snapshots.rs` |
| TUI sanitizer | `crates/tui/src/sanitize.rs` |
| Fake backend | `crates/tui/src/backend.rs` |
| Mesh tools | `app/tools/mesh.py` |
| KAG tool reasoning | `app/tools/tool_reasoning.py` |
| Session context | `app/tools/session_context.py` |
| PRISM Alpha package | `~/Downloads/prism-alpha/` (private repo) |
| JAX GFlowNet | `prism_alpha/gfn/__init__.py` |
| AL loop | `prism_alpha/al/__init__.py` |
| Model registry | `prism_alpha/registry.py` |
| MACE-MH-1 venv | `~/.prism/venv-mace/` (Python 3.13 + torch + mace-torch) |
| Verification harness | `scripts/verify-tui.sh` |
| MACE formation energy script | `/tmp/mace_formation.py` (validated WNbMoTa = -0.0084 eV/atom) |

## Rules

- Do not edit `crates/agent/src/{agent_loop,protocol,tool_catalog,transcript,hooks,command_tools}.rs` — the user is actively rewriting those.
- Build tools in Python (`app/tools/`), not in the Rust harness.
- Tool descriptions are load-bearing — they drive tool selection. Clear, keyword-rich descriptions.
- Return structured JSON from every tool.
- Everything goes to Turso provenance via the agent loop hook.
- PRISM Alpha is a separate package — do not push proprietary code to the public PRISM repo.
- Test on the local LLM (Gemma 4 12B via `prism backend`), not just unit tests.
