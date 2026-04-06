# Frontend Bridge And UI Emitter

PRISM's TUI is not the agent runtime. It is a renderer for the backend event
stream emitted by the Rust agent backend over JSON-RPC stdio.

That distinction matters because it lets PRISM support multiple frontends
without duplicating the agent loop:

- Bun/Ink TUI
- future VSX-style IDE surface
- future desktop shell
- test harnesses that only inspect emitted events

## Runtime Split

The current flow is:

1. frontend spawns the backend process through
   [`frontend/src/bridge/client.ts`](/Users/siddharthakovid/Downloads/PRISM/frontend/src/bridge/client.ts)
2. frontend sends JSON-RPC requests such as `init`, `input.message`,
   `input.command`, and `input.prompt_response`
3. backend emits `ui.*` notifications from
   [`crates/agent/src/protocol.rs`](/Users/siddharthakovid/Downloads/PRISM/crates/agent/src/protocol.rs)
4. frontend folds those low-level events into transcript turns in
   [`frontend/src/app.tsx`](/Users/siddharthakovid/Downloads/PRISM/frontend/src/app.tsx)
5. presentational components render the folded state

The important boundary is step 3. The frontend should not re-implement agent
logic. It should render the protocol faithfully.

## UI Event Contract

The source of truth for the frontend-visible payloads is the generated bridge
types in
[`frontend/src/bridge/types.ts`](/Users/siddharthakovid/Downloads/PRISM/frontend/src/bridge/types.ts).

The core event families are:

- `ui.text.delta` / `ui.text.flush`
  Assistant text streaming.
- `ui.tool.start`
  Tool execution has started.
- `ui.card`
  Structured tool or plan output.
- `ui.prompt`
  Interactive approval / prompt request.
- `ui.status`
  Session/runtime snapshot.
- `ui.session.list`
  Structured saved-session listing.
- `ui.model.list`
  Structured model-picker payload.
- `ui.view`
  Rich command screen payload with optional tabs.
- `ui.turn.complete`
  Marks the boundary for finalizing the current transcript turn.

## Why This Supports VSX

Because the frontend already consumes a protocol instead of direct Rust
internals, a VSX-based surface can reuse the same backend contract.

The minimum VSX adapter would need to:

- spawn the same backend command
- send the same JSON-RPC requests
- consume the same `ui.*` notifications
- map those notifications into editor views instead of Ink components

That means the VSX layer should be treated as another renderer, not another
agent implementation.

## Command Screens

Slash-command and native command screens should prefer `ui.view` over dumping
plain text into the transcript. `ui.view` is the stable abstraction for:

- settings/config screens
- permissions screens
- workflow/deploy/discourse/model screens
- future IDE side panels

When a command also needs transcript continuity, the frontend should keep a
compact breadcrumb or preview in the turn while showing the richer `ui.view`
surface separately. The current TUI already does this in
[`frontend/src/app.tsx`](/Users/siddharthakovid/Downloads/PRISM/frontend/src/app.tsx).

## Agent Awareness

The model is not aware of tools because the frontend exists. Tool awareness is
built in the runtime layer through:

- tool schemas and descriptions from
  [`crates/agent/src/command_tools.rs`](/Users/siddharthakovid/Downloads/PRISM/crates/agent/src/command_tools.rs)
- the merged tool catalog in
  [`crates/agent/src/tool_catalog.rs`](/Users/siddharthakovid/Downloads/PRISM/crates/agent/src/tool_catalog.rs)
- prompt strategy in
  [`crates/agent/src/prompts.rs`](/Users/siddharthakovid/Downloads/PRISM/crates/agent/src/prompts.rs)
- mode-aware prompt shaping in
  [`crates/agent/src/protocol.rs`](/Users/siddharthakovid/Downloads/PRISM/crates/agent/src/protocol.rs)

So the stack is:

- runtime decides what tools exist and how they are described
- prompt layer teaches the model how to choose among them
- protocol emits what happened
- frontend renders it

## Design Rule

If a future frontend feature requires new agent behavior, add it to the
backend/protocol first. If it only changes presentation, keep it in the
frontend.
