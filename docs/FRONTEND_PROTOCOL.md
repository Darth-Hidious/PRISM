# PRISM Frontend Protocol Reference

> For LLM agents and developers building PRISM frontends (TUI, dashboard, VSX extension, desktop app).
> All frontends talk to the same Rust backend via JSON-RPC 2.0 over stdio.

## Architecture

```
Frontend (any)              Backend (Rust)              Services
─────────────              ──────────────              ────────
Ink TUI ──┐                                            Neo4j
Dashboard ─┤── JSON-RPC ──→ prism backend ──→ Agent ──→ Qdrant
VSX ext ──┤    (stdio)      (protocol.rs)    Loop     → Kafka
Desktop ──┘                      │                     → MARC27 API
                                 ├──→ Tool Server (Python, 106 tools)
                                 ├──→ LLM (llama.cpp / Ollama / OpenAI / MARC27)
                                 └──→ OPA Policy Engine
```

## Starting the Backend

```bash
prism backend --project-root /path/to/project --python python3
```

The backend reads JSON-RPC from stdin, writes JSON-RPC to stdout. Stderr is for tracing logs only.

---

## Requests (Frontend → Backend)

### `init`

Sent once on startup. Returns `ui.welcome` notification.

```json
{"jsonrpc":"2.0","method":"init","id":1,"params":{"auto_approve":false,"resume":""}}
```

### `input.message`

Send a user message to the agent. Triggers a full TAOR turn (Think-Act-Observe-Repeat).

```json
{"jsonrpc":"2.0","method":"input.message","id":2,"params":{"text":"find titanium alloys with high yield strength"}}
```

Response: `{"result":{"status":"ok"}}` — then stream of `ui.text.delta`, `ui.tool.start`, `ui.card`, `ui.cost`, `ui.turn.complete`.

### `input.command`

Execute a slash command. Does NOT trigger an LLM turn.

```json
{"jsonrpc":"2.0","method":"input.command","id":3,"params":{"command":"/tools","silent":false}}
```

Response: `{"result":{"status":"ok"}}` — then `ui.view` or `ui.text.delta` + `ui.turn.complete`.

### `approval.respond`

Respond to a `ui.prompt` approval request.

```json
{"jsonrpc":"2.0","method":"approval.respond","id":4,"params":{"response":"y"}}
```

Values: `"y"` (allow once), `"n"` (deny), `"a"` (allow all for session).

---

## Notifications (Backend → Frontend)

### `ui.welcome`

Sent once after `init`. Use to populate the welcome screen.

```typescript
interface UiWelcome {
  version: string;          // "2.6.1"
  tool_count: number;       // 106
  session_id: string;       // "20260407_103841_abc123"
  resumed?: boolean;        // true if resuming a previous session
  resumed_messages?: number; // count of restored messages
}
```

### `ui.status`

Sent after every turn completes. Drives the status bar.

```typescript
interface UiStatus {
  auto_approve: boolean;    // whether auto-approve is on
  message_count: number;    // total messages in session
  has_plan: boolean;        // whether a plan is active
  session_mode: string;     // "chat" | "plan"
  plan_status?: string;     // "none" | "active" | "paused"
  model?: string;           // "gemma3:4b-it-qat"
  project_root?: string;    // "/Users/.../PRISM"
}
```

### `ui.text.delta`

Streaming text from the LLM or command output. Append to the current turn's text buffer.

```typescript
interface UiTextDelta {
  text: string;  // incremental chunk, may be partial word
}
```

### `ui.text.flush`

Signals end of text streaming. The next event will be a tool call or turn complete.

```typescript
interface UiTextFlush {
  text: string;  // always ""
}
```

### `ui.tool.start`

Agent is calling a tool. Show a spinner/activity indicator.

```typescript
interface UiToolStart {
  tool_name: string;    // "search_materials"
  call_id: string;      // unique ID for this call
  verb: string;         // "Running search_materials"
  preview?: string;     // truncated args preview
}
```

### `ui.card`

Tool call finished. Show result or error card.

```typescript
interface UiCard {
  card_type: string;     // "results" | "error"
  tool_name: string;     // "search_materials"
  elapsed_ms: number;    // 1234
  content: string;       // human-readable summary
  data: object;          // raw structured result
}
```

### `ui.prompt`

Asks the user for approval before executing a tool. Frontend must show choices and send `approval.respond`.

```typescript
interface UiPrompt {
  prompt_type: string;        // "approval"
  message: string;            // "Allow execute_python?"
  choices: string[];          // ["y", "n", "a", "b"]
  tool_name: string;          // "execute_python"
  tool_args: object;          // the arguments being passed
  tool_description?: string;  // tool's description
  requires_approval: boolean; // true
  permission_mode?: string;   // "workspace-write"
}
```

Choices: `y` = allow once, `n` = deny, `a` = allow all this session, `b` = block all this session.

### `ui.cost`

Token usage for the just-completed LLM call. Always emitted before `ui.turn.complete`.

```typescript
interface UiCost {
  input_tokens: number;   // 1500
  output_tokens: number;  // 200
  turn_cost: number;      // 0.003 (USD)
  session_cost: number;   // 0.018 (cumulative USD)
}
```

### `ui.turn.complete`

Signals the current turn is done. Stop all spinners, re-enable input.

```typescript
interface UiTurnComplete {}  // empty payload
```

### `ui.view`

Tabbed view panel — used by slash commands. Frontend should render as a modal/panel with tab navigation.

```typescript
interface UiView {
  view_type: string;      // "tools" | "settings" | "context" | "models" | "deployments" | "discourse" | "permissions" | "commands" | "model"
  title: string;          // "Hosted Models"
  tone: string;           // "info" | "warning" | "accent"
  tabs: UiViewTab[];      // array of tab objects
  selected_tab: string;   // ID of initially selected tab
  footer?: string;        // footer text
}

interface UiViewTab {
  id: string;             // "summary", "anthropic", "google"
  title: string;          // "Summary", "anthropic (9)"
  body: string;           // tab content (plain text or markdown)
  tone?: string;          // "info" | "warning" | "accent"
}
```

### `ui.permissions`

Detailed permission state. Sent by `/permissions` command.

```typescript
interface UiPermissions {
  mode: string;                    // "chat"
  auto_approved: UiPermissionTool[];
  blocked: UiPermissionTool[];
  approval_required: UiPermissionTool[];
  read_only: UiPermissionTool[];
  workspace_write: UiPermissionTool[];
  full_access: UiPermissionTool[];
  allow_overrides: string[];       // tools with session allow override
  deny_overrides: string[];        // tools with session deny override
  notice?: string;
}

interface UiPermissionTool {
  name: string;              // "execute_bash"
  permission_mode: string;   // "full-access"
  requires_approval: boolean;
  description: string;
  source?: string;           // "builtin" | "prism-command" | "mcp"
  source_detail?: string;
  current_behavior: string;  // "auto-approved" | "approval required" | "blocked"
}
```

### `ui.session.list`

Available sessions for resume. Sent by `/sessions` command.

```typescript
interface UiSessionList {
  sessions: Array<{
    session_id: string;     // "20260407_103841_abc123"
    created_at: number;     // unix timestamp
    turn_count: number;
    model: string;
    size_kb: number;
    is_latest: boolean;
  }>;
}
```

---

## Slash Commands

Commands start with `/` and are dispatched via `input.command`.

### Built-in (handled in protocol.rs)

| Command | Emits | Purpose |
|---------|-------|---------|
| `/tools` | `ui.view` (6 tabs) | Tool catalog — summary, by permission level, approval status |
| `/status` | `ui.view` (3 tabs) | Session runtime, LLM config, usage |
| `/context` | `ui.view` (5 tabs) | Prompt load, API view, work tracking, files, raw dump |
| `/help` | `ui.view` | Available commands reference |
| `/permissions` | `ui.permissions` | Full permission matrix |
| `/model` | `ui.view` | Show current model |
| `/model <id>` | `ui.view` | Switch LLM model for this session |
| `/clear` | — | Clear conversation history |
| `/compact` | — | Compress conversation, keep context |
| `/sessions` | `ui.session.list` | List saved sessions |
| `/session resume <ref>` | — | Resume a previous session |
| `/session fork` | — | Fork current session |
| `/config` | `ui.view` | Show prism.toml config |
| `/config <key> <value>` | `ui.view` | Update config value |
| `/usage` | `ui.view` | Token usage and cost |
| `/memory` | `ui.text.delta` | Not yet implemented |
| `/files` | `ui.text.delta` | Not yet implemented |
| `/tasks` | `ui.text.delta` | Not yet implemented |
| `/setup` | — | Re-run platform login |
| `/login` | — | Device-flow auth |
| `/logout` | — | Clear credentials |

### File operations (handled in protocol.rs)

| Command | Emits | Purpose |
|---------|-------|---------|
| `/read <path>` | `ui.text.delta` | Read file contents |
| `/edit <path>` | `ui.text.delta` | Edit a file |
| `/diff <path>` | `ui.text.delta` | Show git diff |
| `/write <path>` | `ui.text.delta` | Write file |
| `/python <code>` | `ui.text.delta` | Execute Python snippet |
| `/bash <cmd>` | `ui.text.delta` | Execute shell command |

### CLI-backed (delegated to `prism <subcommand>`)

| Command | Emits | Delegates to |
|---------|-------|--------------|
| `/models [list\|search\|info]` | `ui.view` (tabs by provider) | `prism models list --json` |
| `/deploy [list\|status\|health]` | `ui.view` | `prism deploy list --json` |
| `/discourse [list\|show\|run\|status\|turns]` | `ui.view` | `prism discourse list --json` |
| `/workflow [list\|show\|run]` | `ui.view` or `ui.text.delta` | `prism workflow list --json` |
| `/run <image> [args]` | `ui.text.delta` | `prism run <image> --json` |
| `/ingest <path> [args]` | `ui.text.delta` | `prism ingest <path> --json` |
| `/research <query>` | `ui.text.delta` | `prism research <query> --json` |
| `/publish <path> [args]` | `ui.text.delta` | `prism publish <path> --json` |

---

## Event Sequence Examples

### Simple text query (no tools)

```
→ input.message { text: "what is Ti-6Al-4V?" }
← ui.text.delta { text: "Ti-6Al-4V is a " }
← ui.text.delta { text: "titanium alloy..." }
← ui.cost { input_tokens: 1500, output_tokens: 80 }
← ui.turn.complete {}
← ui.status { message_count: 2, ... }
```

### Tool call with approval

```
→ input.message { text: "search for nickel superalloys" }
← ui.text.delta { text: "I'll search for..." }
← ui.text.flush { text: "" }
← ui.tool.start { tool_name: "search_materials", verb: "Running search_materials" }
← ui.prompt { prompt_type: "approval", tool_name: "search_materials", ... }
→ approval.respond { response: "y" }
← ui.card { card_type: "results", tool_name: "search_materials", content: "Found 47 materials" }
← ui.text.delta { text: "I found 47 nickel..." }
← ui.cost { input_tokens: 3000, output_tokens: 200 }
← ui.turn.complete {}
← ui.status { ... }
```

### Auto-approved tool (no prompt)

```
→ input.message { text: "search for iron alloys" }
← ui.text.flush {}
← ui.tool.start { tool_name: "search_materials" }
← ui.card { card_type: "results", content: "Found 123 materials" }
← ui.text.delta { text: "Here are 123 iron alloys..." }
← ui.cost { ... }
← ui.turn.complete {}
```

### Slash command

```
→ input.command { command: "/models" }
← ui.view { view_type: "models", title: "Hosted Models", tabs: [...], selected_tab: "summary" }
← ui.turn.complete {}
← ui.status { ... }
```

---

## Key Source Files

| File | Purpose |
|------|---------|
| `crates/agent/src/protocol.rs` | Backend JSON-RPC server — all event emission, slash commands, agent turn orchestration |
| `crates/agent/src/agent_loop.rs` | TAOR agent loop — LLM calls, tool dispatch, OPA policy checks |
| `crates/agent/src/prompts.rs` | System prompt construction for the LLM |
| `crates/agent/src/commands.rs` | Agent-facing command catalog |
| `frontend/src/bridge/types.ts` | TypeScript interfaces for all protocol types |
| `frontend/src/bridge/client.ts` | JSON-RPC client — spawns backend, manages request/response |
| `frontend/src/app.tsx` | Main Ink TUI app — event folding, state management |
| `frontend/src/components/CommandView.tsx` | Tabbed view panel (models, deploy, tools, etc.) |
| `frontend/src/components/TurnCard.tsx` | Chat turn rendering — text, tools, cards, approvals |
| `frontend/src/components/StatusLine.tsx` | Bottom status bar |
| `frontend/src/theme.ts` | Colors and design tokens |
| `crates/server/src/router.rs` | HTTP REST API router (node dashboard, separate from TUI) |
| `crates/cli/src/main.rs` | CLI command definitions and handlers |

---

## REST API (Node Dashboard — localhost:7327)

When `prism node up` runs, it starts an Axum HTTP server. This is separate from the TUI backend.

### Public (no auth)

| Method | Path | Purpose |
|--------|------|---------|
| GET | `/api/health` | Health check |
| GET | `/healthz` | K8s health |
| POST | `/api/sessions` | Create auth session |
| GET | `/api/mesh/nodes` | List mesh peers |
| GET | `/api/mesh/subscriptions` | Active mesh subscriptions |

### Authenticated

| Method | Path | Permission | Purpose |
|--------|------|------------|---------|
| GET | `/api/v1/node` | ViewDashboard | Node info + capabilities |
| GET | `/api/data/sources` | ViewDashboard | Data sources |
| GET | `/api/tools` | ViewDashboard | Tool catalog |
| POST | `/api/query` | QueryData | Query knowledge graph |
| POST | `/api/mesh/publish` | IngestData | Publish dataset to mesh |
| POST | `/api/mesh/subscribe` | IngestData | Subscribe to peer |
| DELETE | `/api/mesh/subscribe` | IngestData | Unsubscribe |
| POST | `/api/data/ingest` | IngestData | Ingest data files |
| POST | `/api/tools/{name}/run` | ExecuteTools | Execute a tool |
| GET | `/api/users` | ManageUsers | List users |
| POST | `/api/users` | ManageUsers | Create user |
| GET | `/api/audit` | ViewAudit | Audit log |
| DELETE | `/api/sessions` | — | Logout |
| GET | `/ws` | Token | WebSocket upgrade |

### Dashboard SPA

`GET /*` serves the embedded React dashboard from `dashboard/dist/`.

---

## MARC27 Platform API (api.marc27.com/api/v1)

PRISM CLI calls these endpoints. Auth via Bearer token from `prism login`.

### Auth
| POST | `/auth/device/start` | Start device-flow login |
| POST | `/auth/device/poll` | Poll for approval |
| POST | `/auth/refresh` | Refresh access token |

### Knowledge Graph
| GET | `/knowledge/graph/search?q=...` | Entity search |
| GET | `/knowledge/graph/stats` | Graph stats (211K nodes) |
| GET | `/knowledge/embeddings/stats` | Embedding stats |
| POST | `/knowledge/search` | Semantic vector search |
| POST | `/knowledge/research/query` | Research loop |
| POST | `/knowledge/ingest-job` | Submit ingest job |
| GET | `/knowledge/ingest-jobs` | List ingest jobs |
| GET | `/knowledge/catalog` | Data catalog |

### Compute
| POST | `/compute/jobs` | Submit compute job |
| GET | `/compute/jobs/{id}/status` | Job status |
| GET | `/compute/jobs/{id}/results` | Job results |
| POST | `/compute/jobs/{id}/cancel` | Cancel job |
| POST | `/compute/deployments` | Create deployment |
| GET | `/compute/deployments` | List deployments |
| GET | `/compute/deployments/{id}` | Deployment status |
| DELETE | `/compute/deployments/{id}` | Stop deployment |
| GET | `/compute/deployments/{id}/health` | Health check |

### Models
| GET | `/projects/{id}/llm/models` | Hosted LLM catalog |

### Discourse
| POST | `/discourse/specs` | Create discourse spec |
| GET | `/discourse/specs` | List specs |
| GET | `/discourse/specs/{id}` | Get spec |
| POST | `/discourse/run/{id}` | Run discourse |
| GET | `/discourse/{id}` | Instance status |
| GET | `/discourse/{id}/turns` | Instance turns |

### Nodes
| GET | `/nodes/{id}/public-key` | Node E2EE public key |
| POST | `/nodes/{id}/exchange-key` | Key exchange |
| WS | `/nodes/connect` | WebSocket node registration |

### Marketplace
| GET | `/marketplace/search?q=...` | Search tools/workflows |
| GET | `/marketplace/resources` | List all resources |
| GET | `/marketplace/resources/{name}` | Resource detail |
| POST | `/marketplace/resources/{name}/install` | Install resource |

### User/Org
| GET | `/users/me` | Current user profile |
| GET | `/projects` | List projects |
| GET | `/orgs` | List orgs |
| GET | `/orgs/{id}/members` | Org members |
| GET | `/orgs/{id}/keys/llm` | LLM API keys |

### Support
| POST | `/support/tickets` | File bug report |

---

## Totals

- **3** JSON-RPC request methods (message, command, approval)
- **12** JSON-RPC notification types (backend → frontend)
- **27** slash commands
- **20** local REST endpoints + WebSocket + SPA
- **33** MARC27 platform API calls
- **106** agent tools (Python + Rust CLI-backed)
