# Long-Running Research — Architecture & Plan

> Owner intent (recurring): a **goal** is a persistent object owned by a
> long-running loop, with progress and cost visible in the TUI/dashboard.
> One-shot research tools are *components*, not the product. Nobody exits the
> TUI to type CLI commands. Everything the CLI can do, the server and the
> agent can do.

## What exists (as of 2026-07-06, all proven by gates/tests)

### The goal object (one object, five surfaces)
The durable unit is a **campaign checkpoint**: `~/.prism/campaigns/{id}.json`
(goal, config, iteration, candidates, spend, paused/completed + reason).
The engine (`crates/campaign`) owns propose → evaluate → rank loops with:
- `budget_usd` hard cap (loop stops, reason `budget_exhausted`)
- `max_iterations` cap (`iteration_limit`)
- `approval_gate_at` — pauses for a human, resume continues
- checkpoint every N iterations + final checkpoint
- provenance events (`campaign.start/complete`) into provenance.db

Surfaces over that one object:
| Surface | Verbs | Mechanism |
|---|---|---|
| CLI | `campaign start/resume/continue/status/list` (`--detach` on start/resume) | engine in-process; `--detach` spawns worker |
| Agent tools | `goal_start`, `goal_resume` (always detached), `goal_status`, `goal_list` | command-tools → CLI |
| HTTP REST | `POST /api/goals`, `POST /api/goals/{id}/resume`, `GET /api/goals[/{id}]` | routes → single-tool executor → CLI `--detach` |
| Relay | `POST /nodes/{id}/tools/goal_status/invoke` etc. (read verbs; start/resume are approval-gated so relay refuses — by design) | platform hub → node WS |
| TUI | `/campaign …` slash today; palette entries = **GAP (planned below)** | slash → CLI-backed root |

### Detach semantics (the piece that makes it "long-running")
`--detach`: validate → write **initial checkpoint** (id exists immediately) →
spawn worker `prism campaign continue <id>` (own process group, stdio null) →
return id. The checkpoint file is the only coordination channel; every
surface polls it. Worker crash ⇒ checkpoint stops moving ⇒ status shows the
last honest state; `resume --detach` continues from it.

### Cost pipeline (cloud side)
Every debit lands on its run: `debit_attributed(source_id, source_metric)` →
`GET /usage/runs/{run_id}` (org-scoped, total + by-metric). llm-service
attributes stream debits via `X-Run-Id`; research engine sends its session id.
(marc27-core PR #72.)

## Gaps → build order

1. **TUI palette parity (GAP-C)** — palette entries for goals
   (start/status/list/resume with progress+cost rendering), workflows,
   marketplace, deployments, compute, knowledge. `goal.set` (standing-goal
   string) ≠ campaign; keep both, label clearly.
2. **Goal progress+cost in TUI** — a Goals pane reading `GET /api/goals`
   (poll), showing iteration/candidates/spend/best-so-far per goal.
3. **GAP-A tool cluster** — CLI verbs with NO command-tool (agent+bridge
   unreachable): `billing usage/history/prices` (agent must check spend before
   billable ops), `marketplace find/update`, `use show/…`, `doctor`,
   `federation whoami/peers`, `notebook`, `pyiron`, `report`, `node key`.
   Priority: billing (read) → marketplace find → doctor → use show → rest.
4. **Generalize goal types** — today the campaign engine is materials-
   discovery-shaped (propose compositions). Long-form *literature/knowledge*
   research goals should be the same object: same checkpoint contract, a
   different iteration body (search → read → extract → cite loop), reusing
   the research modules + entailment gate (marc27-core discourse jury).
5. **Cloud-owned goals** — mirror the goal object into marc27-core agent-runs
   (Nemotron via NVIDIA Build) so a goal can run when the laptop is closed;
   node goals and cloud goals list in one view. (ESA_READINESS_PLAN
   `/agent-runs` orchestrator item.)

## Invariants (non-negotiable)
- No blocking tool calls for long work — detach + poll, always.
- The checkpoint/run row is the truth; no surface invents state.
- Start/resume = spending ⇒ approval-gated everywhere; remote relay can
  never approve.
- Every surface goes through the single-tool executor (one audit trail).
- Honest degradation: no LLM ⇒ 503 with the reason; worker dead ⇒ stale
  checkpoint visible, never fake "running".

See `docs/PARITY_AUDIT.md` for the full CLI ↔ tools ↔ REST ↔ palette matrix
this plan is derived from.
