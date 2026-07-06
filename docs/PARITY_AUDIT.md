# PRISM Programmatic-Parity Audit (2026-07-06)

> Requirement: HTTP server at 100% parity with the CLI; TUI palette reaches
> every capability (no exit-to-CLI). Audited by agent, matrices verified from
> code. GAP-B #1 (POST /api/goals + resume) CLOSED same day (397c7942).
> Remaining gaps are the parity drain work list.

(matrix appended below)

## GAP-A — CLI verbs with NO command-tool (agent + HTTP bridge unreachable)
1. `configure` — agent/HTTP cannot set LLM provider/URL/model
2. `use marc27|local|provider|show|reset` (5) — can't inspect/switch chat routing programmatically
3. `billing usage|history|prices|topup` (4) — agent can't check spend before billable ops
4. `marketplace find` (semantic discovery! help text tells agents to use it) + `marketplace update`
5. `doctor` — no self-diagnostic tool
6. `federation whoami|peers` (2)
7. `node key show|rotate|fetch|exchange` (4) — E2EE key mgmt
8. `notebook start|list|stop` (3)
9. `pyiron status|install|update` (3)
10. `report` — no programmatic bug filing

## GAP-B — first-class REST warranted but missing
1. ~~POST /api/goals + /api/goals/{id}/resume~~ **CLOSED 397c7942**
2. /api/marketplace (browse/search/find/install)
3. POST /api/deployments/{id}/health
4. /api/models (list/search/info)
5. /api/discourse/* and /api/knowledge/* node routes
6. /api/billing/* (double gap with GAP-A #3 — unreachable by ANY http path)

## GAP-C — missing from TUI command palette (worst = not even slash-reachable)
1. Campaign/goals — no palette entry (slash /campaign only); goal.set is a standing-goal string, NOT campaigns
2. knowledge entity|paths|corpora|ingest — AGENT-ONLY (not slash-reachable)
3. use local|provider|show|reset — AGENT-ONLY for palette; slash intentionally disabled
4. predict — scaffold only
5. workflows — Mission Control pane is a stub ("no live run list wired yet")
6. marketplace, 7. deployments, 8. discourse, 9. compute (only gpus present), 10. mesh/node/federation
11. models search|info, 12. notebook (toast stub), 13. pyiron, 14. billing history|prices|topup, 15. report/run/job-status/agent/publish

## Strengths (verified)
70 command-tool specs; query/ingest/mesh/deploy/compute/models/discourse/knowledge/campaign/research/predict/publish all reachable by agent + POST /api/tools/{name}/run bridge. Sessions gate + RBAC + audit on every path.

## Drain order
palette goals+workflows+marketplace (C1,5,6) -> billing tools (A3) -> marketplace find tool (A4) -> doctor (A5) -> use show/switch (A2) -> models/marketplace REST (B2,B4) -> remainder.
