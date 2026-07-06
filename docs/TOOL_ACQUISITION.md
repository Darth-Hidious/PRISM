# Acquiring New Tools — Agent Playbook

> For the PRISM agent (and humans). How to discover, install, connect, and
> verify a new tool WITHOUT leaving the session. The system prompt points
> here; read this file with `read_file` when you need the full procedure.

## 0. Check what you already have
- `find_tools <need>` — semantic search over the LOCAL capability registry
  (built-in command-tools + installed Python tools + skills).
- `list_tools` / `GET /api/tools` — the flat catalog.
Don't install what's already loaded.

## 1. Discover on the marketplace
- `marketplace_search {"query": "..."}` — browse published resources
  (tools, workflows, models, datasets).
- `marketplace_info {"name": "<slug>"}` — details, pricing, license, hosting.
- Models are their own lane: `models_search` / `models_info`, and you RUN
  them with `predict` (one call: deploy-or-reuse → infer → auto-stop) —
  models are never "installed" locally unless the human does it themselves.

## 2. Install
- `marketplace_install {"name": "<slug>"}` → writes
  `~/.prism/tools/<slug>.py` (Python tool) or
  `~/.prism/workflows/<slug>.yaml` (workflow).
- Install REFUSES to overwrite an existing local file — that is deliberate
  (local edits are never silently clobbered). Ask the human to remove the
  file if they want the upstream version.
- Workflows are discoverable immediately (`workflow list` — discovery scans
  `~/.prism/workflows/` + `<project>/.prism/workflows/` on every call).

## 3. Connect (how a tool becomes callable)
- **Python tools** are loaded when the tool server starts (node boot / chat
  service spawn). A newly installed tool is NOT hot-loaded into the current
  session — tell the human: "installed; it loads on the next node restart"
  and verify afterwards with `list_tools`. Do not pretend it is callable
  before it appears there.
- **Workflows** need no restart; run via `workflow_run` or
  `POST /api/workflows/{name}/run`.
- **Anti-spoof rule:** an installed tool can never shadow a built-in
  command-tool's name — the Rust built-in always wins.

## 4. Verify (prove, don't claim)
1. `list_tools` → the new name appears.
2. Invoke once with a cheap real input; check the result is real (vary the
   input if output looks canned).
3. Every invocation (and refusal) writes an audit row to provenance.db —
   that trail is the proof it ran.

## 5. Permissions & billing
- Tools declare `permission_mode` + `requires_approval` in their spec.
  Approval-gated tools (spending, code-exec, ingestion) need the human's
  explicit approval — from chat that's the approval prompt; over HTTP it's
  `"approve": true`; the remote relay can NEVER approve.
- Billable operations (predict, compute_submit, goal_start) — check the cost
  first when a budget matters; set `budget_max_usd`/`budget_usd` caps.

## 6. Publishing your own tool (the reverse path)
- Write the tool (Python, one file, self-describing schema — copy the shape
  of an existing tool in `~/.prism/tools/` or `app/tools/`).
- Prove it locally first (run it, check the audit row).
- `publish` / `publish_artifact` pushes it to the marketplace so other nodes
  can `marketplace_install` it. The same honesty rules apply: no stubs, no
  canned outputs, honest errors when unconfigured.

## Invariants
- Nothing hardcoded: new tools/models are discovered via the registry and
  marketplace, never from a baked-in list.
- If a tool can't run (missing key, no backend), it must SAY so — an empty
  result is a defect, not a fallback (see docs/AUDIT_BACKLOG.md history).
