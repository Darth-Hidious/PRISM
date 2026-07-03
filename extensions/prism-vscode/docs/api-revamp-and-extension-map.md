# PRISM VS Code Extension And MARC27 API Revamp Map

Date: 2026-05-24
Status: implementation scaffold plus API-boundary decision record

## Decision

Build PRISM VS Code support as a first-party extension, not a VSCodium fork as
the first move.

Reason: PRISM needs to meet scientists and developers inside their daily work:
Python, notebooks, terminal jobs, Git, SSH, language servers, logs, YAML
workflows, and simulation input files. A fork can still happen later for a
fully branded "PRISM Desktop", but the extension proves the workflow with less
maintenance and keeps the existing VS Code ecosystem intact.

## Current Repo Reality

PRISM already says all frontends should speak the same backend protocol:

- `docs/FRONTEND_PROTOCOL.md`: JSON-RPC 2.0 over stdio to `prism backend`.
- `crates/forge_main/src/vscode.rs`: inherited Forge code detects VS Code and
  installs one extension id.
- `crates/forge_main/src/cli.rs`: `vscode install-extension` subcommand.
- `crates/forge_main/src/ui.rs`: UI handler for explicit extension install.
- `docs/plans/2026-04-04-frontend-design.md`: older VSCodium/fork plan.
- `docs/prism_ide_design_2026.md`: later correction that VS Code ecosystem
  matters and an extension should come before a fork.

This scaffold adds the missing first-party extension under:

- `extensions/prism-vscode/package.json`
- `extensions/prism-vscode/src/extension.ts`
- `extensions/prism-vscode/src/backend/*`
- `extensions/prism-vscode/src/marc27/*`
- `extensions/prism-vscode/src/views/*`

## MARC27 API Boundary

MARC27 already has the right central discovery primitive:

- External repo: `marc27-core/crates/api/src/router.rs`
- Router root: `/api/v1`
- Discovery endpoint: `GET /api/v1/agent/capabilities`
- Error help points clients back to capabilities and GraphQL.

The extension treats that endpoint as the contract of record. It should not
hardcode scattered MARC27 endpoints across views. All MARC27 calls belong under
`src/marc27/`.

## Extension Surfaces

### Agent

Primary pane. Speaks to local PRISM backend through JSON-RPC over stdio.

Responsibilities:

- Start/stop `prism backend`.
- Send `input.message`.
- Send `input.command` for slash commands.
- Render `ui.text.delta`, `ui.tool.start`, `ui.card`, `ui.prompt`, `ui.cost`,
  `ui.view`, `ui.permissions`, and `ui.turn.complete`.
- Send `approval.respond` from explicit buttons.

### Context

Workspace and trust surface.

Responsibilities:

- Show backend state.
- Show workspace root.
- Show whether a MARC27 API key exists in SecretStorage.
- Expose start/stop/refresh/auth commands.

### Models

Model discovery and LLM limit visibility.

Responsibilities:

- Use MARC27 capabilities to discover model endpoints.
- Show provider/model availability once API v2 exposes structured limits.
- Surface per-model context windows, max output tokens, price, and allowed use.
- Keep "select model" separate from "spend money".

### Workflows

PRISM and MARC27 workflow launcher.

Responsibilities:

- List workflow endpoints from capabilities.
- Later: render workflow specs, parameters, dry-run, and approval summaries.
- Later: run `prism workflow` through the local backend when local context is
  required.

### Jobs

Compute/job operations.

Responsibilities:

- List job and compute endpoints from capabilities.
- Later: show job queue, stdout/stderr, artifacts, and cancellation.
- Later: enforce visible max budget before submit.

### Billing

Read-only first.

Responsibilities:

- Show packages and balance only when MARC27 billing endpoints are hardened.
- Never initiate top-up in the extension until Stripe webhook idempotency,
  reconciliation, and error paths are verified.
- Surface 402/429 errors as user-understandable budget/quota states.

## API Revamp Contract

The extension will stay stable if the MARC27 API revamp gives it these things:

1. `GET /api/v1/agent/capabilities`
   - Versioned schema.
   - Endpoint groups.
   - Auth requirements.
   - Rate/quota/credit metadata.
   - Deprecation metadata.
   - Example request bodies as JSON, not stringified JSON.

2. Consistent response envelope for new endpoints.
   - Success: `{ "data": ..., "meta": ... }`
   - Error: `{ "error": { "code": "...", "message": "...", "details": ... }, "help": ... }`
   - Pagination: `{ "next_cursor": "...", "limit": 50 }`

3. Version negotiation.
   - Current: `/api/v1`
   - Revamp candidates: `/api/v1` with capability schema version, or `/api/v2`
     behind `prism.experimentalMarc27ApiV2`.
   - Extension setting already includes `prism.experimentalMarc27ApiV2`.

4. Streaming events.
   - Use SSE or WebSocket for long jobs, discourse, research, and LLM streaming.
   - Event names should be typed and stable.
   - Include correlation IDs for UI cards and logs.

5. Idempotency.
   - Required for billing top-ups, workflow starts, compute submits, and publish
     operations.
   - Client can send `Idempotency-Key`.
   - Server returns existing operation on retry.

6. Explicit LLM limits everywhere.
   - Context window.
   - Max output tokens.
   - Max tool calls.
   - Max cost per turn/session/project.
   - Rate limit and quota remaining.
   - Provider/model policy labels.

7. Credential discipline.
   - API keys only through environment, CLI login, OS keychain, or VS Code
     SecretStorage.
   - Never through checked-in files, generated docs, or transfer briefs.

## LLM Limit UX

The extension should make limits visible before a request starts:

- Composer footer: selected model, context window, projected input tokens.
- Agent status: session spend and remaining session/project budget.
- Tool cards: max tool calls and approval policy.
- Error translation:
  - 402 -> insufficient credits.
  - 429 -> quota/rate limit.
  - validation error -> likely schema/version mismatch.

The goal is not just safety. It is demo trust: ESA reviewers should see PRISM
knows where the boundaries are.

## What Not To Build Into The Extension

- No direct Stripe checkout until billing is fully hardened.
- No raw secret entry into workspace settings.
- No per-view hardcoded endpoint URLs outside `src/marc27`.
- No new agent protocol that bypasses `docs/FRONTEND_PROTOCOL.md`.
- No fork-only features before proving the extension path.

## Next Implementation Slices

1. Render streaming agent events as proper cards instead of raw JSON.
2. Generate TypeScript API types from the capabilities response once schema
   versioning lands.
3. Add model and budget panels backed by real MARC27 limit metadata.
4. Add workspace-aware file context packs for selected files, open tabs, and Git
   diffs.
5. Add job and workflow webviews with dry-run, approval, and idempotency keys.
6. Package a `.vsix` and wire the Rust installer to use `marc27.prism-vscode`.
