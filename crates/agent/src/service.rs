// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Headless chat service — the conversational agent loop as an embeddable
//! service, so ANY client (HTTP, MCP, future transports) gets the same
//! agent the TUI chat app gets.
//!
//! This is deliberately a THIN wrapper: construction goes through
//! [`crate::protocol::build_agent_seed`] and every user turn dispatches
//! through [`crate::agent_loop::run_turn`] — the exact same entry points
//! the stdio backend (`prism backend`, spawned by the TUI) uses. The only
//! things that differ per transport are (a) how events reach the client
//! and (b) how tool approvals are answered:
//!
//! - Events stream through an [`tokio::sync::mpsc::UnboundedSender`] of
//!   typed [`ChatEvent`]s (mapped 1:1 from [`AgentEvent`]) instead of
//!   JSON-RPC notifications on stdout.
//! - Approvals are headless: there is no human to prompt, so tools whose
//!   permission profile requires approval are DENIED (skipped, never
//!   executed) unless the request explicitly pre-approved them by name.
//!   Each denial surfaces as an `approval_required` event so the client
//!   can re-send the message with `approve: ["<tool>"]`. There is no
//!   silent auto-approve. Tools the permission baseline auto-approves
//!   (read-only, no approval flag) and the OPA policy gate inside
//!   `run_turn` behave exactly as they do for the TUI.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use base64::Engine as _;
use prism_ingest::LlmConfig;
use prism_ingest::llm::{ChatMessage, LlmClient};
use prism_python_bridge::{ToolServer, ToolServerHandle, ToolServerPool};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;

use crate::agent_loop::{self, ApprovalResponse};
use crate::command_tools::{
    CommandToolPlatformAccess, CommandToolRuntime, PLATFORM_ACCESS_REFUSAL,
};
use crate::hooks::HookRegistry;
use crate::influence::ContextPrimingStatus;
use crate::permissions::ToolPermissionContext;
use crate::protocol::{AgentSeed, build_agent_seed, restore_history_and_transcript_from_messages};
use crate::scratchpad::Scratchpad;
use crate::session::{SessionInfo, SessionStore};
use crate::tool_catalog::ToolCatalog;
use crate::transcript::TranscriptStore;
use crate::types::{AgentConfig, AgentEvent, ContextPrimingRecord};

/// Stable principal prefix used by the standalone HTTP seam when no account
/// session exists. HTTP handlers append the validated transport session token
/// so anonymous callers can resume their own session without sharing access.
pub const ANONYMOUS_LOCAL_USER_ID: &str = "anonymous-local";

/// Bind an anonymous HTTP caller to its validated transport token without
/// persisting the bearer itself in the session-owner map. The token is a
/// server-issued capability, not request JSON or a caller-selected identity.
pub fn anonymous_caller_id(transport_token: &str) -> String {
    let digest = Sha256::digest(transport_token.as_bytes());
    let owner_key = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    format!("{ANONYMOUS_LOCAL_USER_ID}:{owner_key}")
}

// ── Wire types ───────────────────────────────────────────────────────

/// Typed event stream for chat clients. Serialized with a `type` tag so
/// SSE/JSON consumers can switch on it: `thinking`, `answer`,
/// `context_priming`, `tool_call`, `tool_result`, `approval_required`, `done`,
/// `error`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatEvent {
    /// Reasoning tokens (streamed).
    Thinking { text: String },
    /// Response text delta (streamed). The complete answer is repeated in
    /// the final `done` event.
    Answer { text: String },
    /// Per-LLM-request context-selection status. Only the `primed` status
    /// means an influence-ranked definition actually reached the prompt.
    ContextPriming {
        iteration: usize,
        status: ContextPrimingStatus,
    },
    /// A tool call is starting.
    ToolCall {
        tool_name: String,
        call_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        preview: Option<String>,
    },
    /// A tool call finished (including denied/errored calls).
    ToolResult {
        tool_name: String,
        call_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        content: String,
        elapsed_ms: u64,
        is_error: bool,
        evidence_class: String,
        evidence_color: String,
    },
    /// A tool needed human approval and was SKIPPED (headless mode).
    /// Re-send the same message with `approve: ["<tool_name>"]` to run it.
    ApprovalRequired {
        tool_name: String,
        call_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        permission_mode: String,
        hint: String,
    },
    /// Turn finished successfully.
    Done {
        session_id: String,
        answer: String,
        /// Tools that were skipped pending approval this turn.
        approvals_required: Vec<String>,
        /// One truthful, iteration-addressable status for every LLM request
        /// made during the turn.
        context_priming: Vec<ContextPrimingRecord>,
    },
    /// Turn failed.
    Error { message: String },
}

impl ChatEvent {
    /// Stable SSE event name (matches the serde `type` tag).
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Thinking { .. } => "thinking",
            Self::Answer { .. } => "answer",
            Self::ContextPriming { .. } => "context_priming",
            Self::ToolCall { .. } => "tool_call",
            Self::ToolResult { .. } => "tool_result",
            Self::ApprovalRequired { .. } => "approval_required",
            Self::Done { .. } => "done",
            Self::Error { .. } => "error",
        }
    }
}

fn tool_result_event(
    tool_name: String,
    call_id: String,
    summary: Option<String>,
    content: String,
    elapsed_ms: u64,
    is_error: bool,
) -> ChatEvent {
    // This SSE surface's `evidence_class`/`evidence_color` are non-optional
    // strings — external headless consumers key on their presence — so an
    // undeclared class still collapses to indeterminate HERE, deliberately
    // and only here. The interactive path (`build_ui_card_payload`) omits the
    // fields instead, because its receiver renders silence as a muted
    // `[unclassified]`; this wire has no such renderer to hand the
    // distinction to. Widening this contract is an API decision, not a badge
    // fix.
    let evidence = crate::tool_result::tool_result_evidence(&content)
        .unwrap_or(prism_provenance::EvidenceClass::Indeterminate);
    ChatEvent::ToolResult {
        tool_name,
        call_id,
        summary,
        content,
        elapsed_ms,
        is_error,
        evidence_class: evidence.as_str().to_string(),
        evidence_color: evidence.color().to_string(),
    }
}

/// One user turn.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub message: String,
    /// Existing session to continue; `None` creates a new session.
    pub session_id: Option<String>,
    /// Tool names the client pre-approves for THIS turn (headless
    /// equivalent of clicking "allow" in the TUI approval prompt).
    pub approve: Vec<String>,
}

/// Final result of a turn (also emitted as the `done` event).
#[derive(Debug, Clone, Serialize)]
pub struct ChatOutcome {
    pub session_id: String,
    pub answer: String,
    pub approvals_required: Vec<String>,
    pub context_priming: Vec<ContextPrimingRecord>,
}

/// Errors the HTTP layer maps to status codes.
#[derive(Debug)]
pub enum ChatError {
    /// Unknown session id, or a session this user does not own.
    SessionNotFound(String),
    /// The turn itself failed (LLM unreachable, tool server died, …).
    Turn(anyhow::Error),
}

impl std::fmt::Display for ChatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SessionNotFound(sid) => write!(f, "session not found: {sid}"),
            Self::Turn(e) => write!(f, "chat turn failed: {e:#}"),
        }
    }
}

impl std::error::Error for ChatError {}

// ── Service ──────────────────────────────────────────────────────────

struct ChatInner {
    tool_server: ToolServerHandle,
    /// Separate worker for LocalOnly callers. It runs offline with all
    /// inherited platform/provider credentials blanked, so allowing a local
    /// Python tool does not also allow an owner credentialed platform call.
    local_only_tool_server: ToolServerHandle,
    /// Lane pool for delegated (subagent) turns — see [`AgentSeed`]. Handed
    /// to `run_turn` only for VerifiedNodeOwner turns; LocalOnly turns get
    /// `None` (their subagent spawns are refused at the gate anyway, and no
    /// pool means even a gate regression cannot hand them a credentialed
    /// child).
    subagent_lanes: ToolServerPool,
    command_tool_runtime: CommandToolRuntime,
    config: Arc<AgentConfig>,
    hooks: Arc<HookRegistry>,
    permissions: ToolPermissionContext,
    llm_config: LlmConfig,
    policy: Option<prism_policy::PolicyEngine>,
    store: SessionStore,
}

/// The agent loop as a service. One instance per node process; turns are
/// serialized through an async mutex (each turn drives a single stdio
/// tool-server child — same one-turn-at-a-time model as the backend).
/// Within a turn, delegated subagents check out their own children from
/// `ChatInner::subagent_lanes`, so delegation no longer contends for the
/// turn's child; lifting the turn-level serialization itself is the
/// orchestrator fan-out work that builds on those lanes.
pub struct ChatService {
    inner: tokio::sync::Mutex<ChatInner>,
    /// Cloned out of the seed so read paths don't need the turn lock.
    tools: Arc<ToolCatalog>,
    sessions_dir: PathBuf,
    /// session_id → owning user_id, persisted next to the session files so
    /// ownership survives node restarts.
    owners: std::sync::Mutex<BTreeMap<String, String>>,
    owners_path: PathBuf,
}

impl ChatService {
    /// Spawn the Python tool server and build the shared agent machinery
    /// (same construction path as the TUI backend — `build_agent_seed`).
    ///
    /// `sessions_dir: None` uses the default `~/.prism/sessions` — the same
    /// store the TUI uses.
    pub async fn spawn(
        llm_config: LlmConfig,
        tool_server_config: ToolServer,
        sessions_dir: Option<PathBuf>,
    ) -> Result<Self> {
        let AgentSeed {
            tool_server,
            subagent_lanes,
            command_tool_runtime,
            tools,
            config,
            hooks,
            permissions,
        } = build_agent_seed(&tool_server_config, &llm_config).await?;
        let local_only_tool_server = local_only_tool_server_config(&tool_server_config)
            .spawn_with_clean_environment()
            .await
            .context("failed to spawn LocalOnly Python tool server")?;

        // Same policy bootstrap as run_server: built-in + discovered
        // OPA/Rego policies; absence is a warning, not an error.
        let policy = match prism_policy::PolicyEngine::with_discovery(None) {
            Ok(pe) => {
                tracing::info!(policies = pe.policy_count(), "OPA policy engine loaded");
                Some(pe)
            }
            Err(e) => {
                tracing::warn!(error = %e, "OPA policy engine failed to load — running without policies");
                None
            }
        };

        let mut store = SessionStore::new(sessions_dir);
        store.set_project_cwd(Some(&tool_server_config.project_root));
        let sessions_dir = store.dir().to_path_buf();
        let owners_path = sessions_dir.join("http_chat_owners.json");
        let owners = load_owners(&owners_path);

        Ok(Self {
            inner: tokio::sync::Mutex::new(ChatInner {
                tool_server,
                local_only_tool_server,
                subagent_lanes,
                command_tool_runtime,
                config,
                hooks,
                permissions,
                llm_config,
                policy,
                store,
            }),
            tools,
            sessions_dir,
            owners: std::sync::Mutex::new(owners),
            owners_path,
        })
    }

    /// Names of every tool in the catalog (introspection/testing).
    pub fn tool_names(&self) -> Vec<String> {
        self.tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>()
    }

    /// Execute one named tool once — deterministically, with no LLM and no
    /// conversation. This is the *same* execution surface the agent loop
    /// uses for a single tool call: command-tool dispatch first (Rust CLI
    /// shellouts + workflow ops), otherwise the Python/MCP tool server. The
    /// result is byte-for-byte what the tool would return mid-chat, minus
    /// the model deciding to call it.
    ///
    /// Two consumers share this executor:
    ///   1. the platform→node tool-call relay (`PlatformMessage::InvokeTool`),
    ///      where a remote principal runs an owner's local tool through the
    ///      node, and
    ///   2. `POST /api/tools/{name}/run`, which workflow `action: tool` steps
    ///      call.
    ///
    /// `caller` is the real principal on whose behalf the tool runs. Node
    /// reachability/visibility is authorized upstream (the platform relay
    /// gate, or the HTTP auth+RBAC stack); this method records the caller for
    /// audit but does not itself re-derive authorization.
    ///
    /// Meta-tools (`recall` / `find_tools`) operate on live agent/session
    /// state and have no meaning as a one-shot relayed call, so they are
    /// rejected honestly rather than returning a fabricated empty result.
    ///
    /// `approve` stands in for the interactive approval a chat turn would
    /// collect: approval-gated tools (e.g. `execute_bash`, `write_skill`) run
    /// only when the caller explicitly passed approval. The platform relay
    /// always passes `false` — a remote principal must never get an
    /// approval-gated tool on the owner's machine with nobody at the keyboard.
    pub async fn invoke_tool(
        &self,
        name: &str,
        args: serde_json::Value,
        caller: Option<&str>,
        approve: bool,
    ) -> Result<serde_json::Value> {
        self.invoke_tool_with_actor_and_platform_access(
            name,
            args,
            caller,
            prism_provenance::Actor::User,
            CommandToolPlatformAccess::LocalOnly,
            approve,
        )
        .await
    }

    /// Execute a one-shot tool with an actor classification derived by the
    /// authenticated transport. The legacy [`Self::invoke_tool`] entry point
    /// remains for non-HTTP callers; server handlers use the explicit platform
    /// access method so a bearer token cannot select the child credential.
    pub async fn invoke_tool_with_actor(
        &self,
        name: &str,
        args: serde_json::Value,
        caller: Option<&str>,
        actor: prism_provenance::Actor,
        approve: bool,
    ) -> Result<serde_json::Value> {
        self.invoke_tool_with_actor_and_platform_access(
            name,
            args,
            caller,
            actor,
            CommandToolPlatformAccess::LocalOnly,
            approve,
        )
        .await
    }

    /// Execute a one-shot tool with an explicit platform-credential boundary.
    /// Approval remains a separate confirmation bit: it never upgrades this
    /// authorization context.
    pub async fn invoke_tool_with_actor_and_platform_access(
        &self,
        name: &str,
        args: serde_json::Value,
        caller: Option<&str>,
        actor: prism_provenance::Actor,
        platform_access: CommandToolPlatformAccess,
        approve: bool,
    ) -> Result<serde_json::Value> {
        // Pre-execution rejections. Computed (not early-returned) so REFUSALS
        // reach the audit trail below — a denied attempt is at least as
        // audit-worthy as a successful run.
        let rejection: Option<String> =
            if matches!(platform_access, CommandToolPlatformAccess::UnverifiedHttp) {
                Some(PLATFORM_ACCESS_REFUSAL.to_string())
            } else if crate::meta_tools::is_meta_tool(name) {
                Some(format!(
                    "'{name}' is a meta-tool that operates on live agent state; \
                 it is not invocable through the single-tool executor"
                ))
            } else if !crate::command_tools::is_command_tool(name)
                && let Err(error) =
                    crate::command_tools::gate_external_tool_execution(name, platform_access)
            {
                Some(error.to_string())
            } else {
                // Approval gate. Catalog lookup covers Python + offered command
                // tools; the command-tool fallback covers specs hidden from the
                // catalog (hidden ≠ unexecutable — see LOCAL_NODE_TOOLS).
                let gated = self
                    .tools
                    .find(name)
                    .map(|tool| tool.requires_approval)
                    .or_else(|| crate::command_tools::command_tool_requires_approval(name));
                if gated == Some(true) && !approve {
                    Some(format!(
                        "'{name}' is approval-gated and cannot run through the \
                     single-tool executor without explicit approval \
                     (pass approve=true from an authenticated local caller; \
                     remote relay callers cannot approve)"
                    ))
                } else {
                    None
                }
            };

        tracing::info!(
            tool = %name,
            caller = caller.unwrap_or("<unspecified>"),
            refused = rejection.is_some(),
            "invoke_tool: single-tool execution"
        );

        let result = if let Some(message) = rejection {
            Err(anyhow::anyhow!(message))
        } else {
            let mut inner = self.inner.lock().await;
            let ChatInner {
                tool_server,
                local_only_tool_server,
                command_tool_runtime,
                policy,
                ..
            } = &mut *inner;

            if crate::command_tools::is_command_tool(name) {
                crate::command_tools::execute_command_tool_with_platform_access(
                    command_tool_runtime,
                    name,
                    &args,
                    policy.as_mut(),
                    platform_access,
                )
                .await
            } else if self
                .tools
                .find(name)
                .is_some_and(|tool| tool.source.as_deref() == Some("mcp"))
            {
                crate::mcp::call_global_tool_with_platform_access(name, &args, platform_access)
                    .await
            } else {
                match crate::command_tools::gate_external_tool_execution(name, platform_access) {
                    Ok(_) => {
                        let worker = match platform_access {
                            CommandToolPlatformAccess::VerifiedNodeOwner => tool_server,
                            CommandToolPlatformAccess::LocalOnly
                            | CommandToolPlatformAccess::UnverifiedHttp => local_only_tool_server,
                        };
                        worker
                            .call_tool(name, args.clone())
                            .await
                            .map_err(Into::into)
                    }
                    Err(error) => Err(error),
                }
            }
        };

        // Durable per-caller audit. This executor runs OUTSIDE the agent loop,
        // so the provenance after-hook never fires for it — without this write
        // a relayed invocation would leave no durable record of who ran what.
        // Failures are logged, never swallowed into the tool result.
        {
            let mut record = prism_provenance::new_record(
                &format!("invoke:{}", caller.unwrap_or("unspecified")),
                prism_provenance::ActionType::ToolCall,
                actor,
                Some(name),
                None,
                args,
            );
            record.output_json = Some(match &result {
                Ok(value) => value.clone(),
                Err(e) => serde_json::json!({ "error": format!("{e:#}") }),
            });
            // VS3: populate the structured status/exit_code columns (not just
            // the tags) so this externally-invoked tool run is answerable by
            // `query_failures` and counted in `stats().error_records`. We reuse
            // the SAME shared classifier the agent-loop gate and the provenance
            // hook use (`crate::hooks::classify_for_provenance`), so a wrapped
            // `{"result":{"success":false,...}}` (the common Python-tool
            // failure shape) is flagged here too — and a hard dispatch Err is
            // an unambiguous error (the classifier reads its `{"error":...}`
            // output as a top-level string error).
            let (status, exit_code) = crate::hooks::classify_for_provenance(
                record
                    .output_json
                    .as_ref()
                    .unwrap_or(&serde_json::Value::Null),
            );
            record.status = status;
            record.exit_code = exit_code;
            record.tags = vec![
                "single-tool-executor".to_string(),
                format!("caller:{}", caller.unwrap_or("unspecified")),
                if result.is_ok() {
                    "outcome:ok".to_string()
                } else {
                    "outcome:error".to_string()
                },
            ];
            let db_path = crate::hooks::provenance_db_path();
            match prism_provenance::ProvenanceStore::open(&db_path).await {
                Ok(store) => {
                    if let Err(e) = store.record(&record).await {
                        tracing::warn!(error = %e, tool = %name, "invoke_tool audit write failed");
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "invoke_tool audit store open failed");
                }
            }
        }

        result
    }

    /// Run one user turn through `agent_loop::run_turn` — the same entry
    /// point the TUI backend dispatches through. Events stream into
    /// `events` as the turn progresses; the final `done`/`error` event is
    /// always sent before this returns.
    pub async fn chat(
        &self,
        request: ChatRequest,
        user_id: &str,
        events: mpsc::UnboundedSender<ChatEvent>,
    ) -> Result<ChatOutcome, ChatError> {
        self.chat_with_platform_access(
            request,
            user_id,
            CommandToolPlatformAccess::LocalOnly,
            events,
        )
        .await
    }

    /// Run an HTTP chat turn with the platform credential boundary established
    /// by the server. `approve` is intentionally not part of this decision.
    pub async fn chat_with_platform_access(
        &self,
        request: ChatRequest,
        user_id: &str,
        platform_access: CommandToolPlatformAccess,
        events: mpsc::UnboundedSender<ChatEvent>,
    ) -> Result<ChatOutcome, ChatError> {
        let result = if matches!(platform_access, CommandToolPlatformAccess::UnverifiedHttp) {
            Err(ChatError::Turn(anyhow::anyhow!(PLATFORM_ACCESS_REFUSAL)))
        } else {
            crate::command_tools::with_platform_access(
                platform_access,
                // BOXED, and it has to stay boxed. `chat_inner` is a whole
                // agent turn — session resolution, the tool loop, transcript
                // and scratchpad state — so its generated state machine is
                // megabytes wide. Composed inline, that entire object lives
                // on the caller's stack, and the caller here is an HTTP
                // handler on a tokio worker with a 2 MiB stack
                // (`crates/server/src/handlers/chat.rs`). It overflowed and
                // aborted the process with SIGABRT, which the parity test
                // reproduced. Boxing puts the state machine on the heap; only
                // the frames actually executing use stack.
                Box::pin(self.chat_inner(request, user_id, platform_access, &events)),
            )
            .await
        };
        if let Err(ref e) = result {
            let _ = events.send(ChatEvent::Error {
                message: e.to_string(),
            });
        }
        result
    }

    async fn chat_inner(
        &self,
        request: ChatRequest,
        user_id: &str,
        platform_access: CommandToolPlatformAccess,
        events: &mpsc::UnboundedSender<ChatEvent>,
    ) -> Result<ChatOutcome, ChatError> {
        let mut inner = self.inner.lock().await;

        // ── Session resolution ────────────────────────────────────
        // Continuing a session reloads its history from disk (the same
        // restore path `prism backend` uses for resume) — the service
        // holds no in-memory conversation state between turns.
        let mut history: Vec<ChatMessage> = Vec::new();
        let mut transcript = TranscriptStore::new(None);
        let mut scratchpad = Scratchpad::new();

        let session_id = match &request.session_id {
            Some(sid) => {
                // Anonymous HTTP callers are scoped by the server to their
                // validated transport token. Unknown AND not-owned collapse
                // to the same error so the API doesn't leak session ids.
                if !self.user_owns(sid, user_id) {
                    return Err(ChatError::SessionNotFound(sid.clone()));
                }
                let (sid, messages) = inner
                    .store
                    .resume_session(sid)
                    .ok_or_else(|| ChatError::SessionNotFound(sid.clone()))?;
                restore_history_and_transcript_from_messages(
                    &mut history,
                    &mut transcript,
                    &mut scratchpad,
                    &messages,
                );
                sid
            }
            None => {
                let model = inner.llm_config.model.clone();
                let sid = inner.store.new_session(&model);
                self.record_owner(&sid, user_id);
                sid
            }
        };

        // Provenance ledger context — same seeding the backend does on
        // init/resume so tool provenance rows carry the real session id.
        crate::hooks::set_provenance_ctx(&session_id, &inner.llm_config.model);

        inner
            .store
            .append_message("user", &request.message, "", "", None);

        // ── Turn machinery ────────────────────────────────────────
        let llm = LlmClient::new(inner.llm_config.clone());
        let turn_config = {
            let mut c = inner.config.as_ref().clone();
            // Headless invariant: NEVER auto-approve everything. Gated
            // tools run only when named in `request.approve`.
            c.auto_approve = false;
            c
        };
        let approved: BTreeSet<String> = request.approve.iter().cloned().collect();

        // Approval channel: run_turn emits ToolApprovalRequest then awaits
        // one ApprovalResponse. Our emit callback answers synchronously
        // (try_send on a capacity-1 channel), so the loop never blocks.
        let (approval_tx, approval_rx) = mpsc::channel::<ApprovalResponse>(1);
        let approval_rx = Arc::new(tokio::sync::Mutex::new(approval_rx));

        let mut answer = String::new();
        let mut approvals_required: Vec<String> = Vec::new();
        let mut context_priming: Vec<ContextPrimingRecord> = Vec::new();

        // Split borrows: run_turn needs &mut tool_server while the emit
        // callback appends to the session store.
        let ChatInner {
            tool_server,
            local_only_tool_server,
            subagent_lanes,
            command_tool_runtime,
            hooks,
            permissions,
            policy,
            store,
            ..
        } = &mut *inner;
        let selected_tool_server = match platform_access {
            CommandToolPlatformAccess::VerifiedNodeOwner => tool_server,
            CommandToolPlatformAccess::LocalOnly => local_only_tool_server,
            CommandToolPlatformAccess::UnverifiedHttp => {
                unreachable!("UnverifiedHttp is refused before chat_inner")
            }
        };
        // Subagent lanes are owner-only: the pool's children carry the normal
        // (credentialed) environment, so a LocalOnly turn gets no pool —
        // belt to the spawn gate's braces (spawn_subagent already refuses
        // LocalOnly callers before any lane is touched).
        let subagent_lanes = match platform_access {
            CommandToolPlatformAccess::VerifiedNodeOwner => Some(&*subagent_lanes),
            CommandToolPlatformAccess::LocalOnly => None,
            CommandToolPlatformAccess::UnverifiedHttp => {
                unreachable!("UnverifiedHttp is refused before chat_inner")
            }
        };

        // Live catalog if the agent has published one, so a `reload_mcp`
        // in an earlier turn is visible in this one.
        let tools = crate::tool_catalog::live_or(&self.tools);
        let mut assistant = crate::session::AssistantRecorder::default();
        let mut emit = |event: AgentEvent| match event {
            // This SSE surface carries no per-agent field yet, so a delegated
            // agent's activity is not reported on it. Widening that wire is an
            // API decision; inventing a field here would not be one.
            AgentEvent::AgentActivity { .. } => {}
            AgentEvent::ThinkingDelta { text } => {
                let _ = events.send(ChatEvent::Thinking { text });
            }
            AgentEvent::TextDelta { text } => {
                // Accumulate as well as forward: the durable record must match
                // what the caller was streamed. `TurnComplete.text` alone
                // carries only the LAST model message, so a turn ending after
                // tool calls with no final content persisted nothing at all
                // while the caller had already read a full answer.
                assistant.delta(&text);
                let _ = events.send(ChatEvent::Answer { text });
            }
            AgentEvent::TextFlush => {
                if let Some(block) = assistant.flush() {
                    store.append_message("assistant", &block, "", "", None);
                }
            }
            AgentEvent::ContextPriming { iteration, status } => {
                context_priming.push(ContextPrimingRecord {
                    iteration,
                    status: status.clone(),
                });
                let _ = events.send(ChatEvent::ContextPriming { iteration, status });
            }
            AgentEvent::ToolCallStart {
                tool_name,
                call_id,
                preview,
            } => {
                let _ = events.send(ChatEvent::ToolCall {
                    tool_name,
                    call_id,
                    preview,
                });
            }
            AgentEvent::ToolCallResult {
                raw_result: _,
                call_id,
                tool_name,
                content,
                summary,
                elapsed_ms,
                is_error,
                ..
            } => {
                // Same persistence the backend applies in spawn_agent_turn.
                store.append_message("tool", &content, &tool_name, &call_id, None);
                let _ = events.send(tool_result_event(
                    tool_name, call_id, summary, content, elapsed_ms, is_error,
                ));
            }
            AgentEvent::ToolApprovalRequest {
                tool_name,
                call_id,
                tool_description,
                permission_mode,
                ..
            } => {
                let decision = approval_decision(&approved, &tool_name);
                if matches!(decision, ApprovalResponse::Deny) {
                    approvals_required.push(tool_name.clone());
                    let _ = events.send(ChatEvent::ApprovalRequired {
                        tool_name: tool_name.clone(),
                        call_id,
                        description: tool_description,
                        permission_mode,
                        hint:
                            "re-send the message with approve: [\"<tool_name>\"] to run this tool"
                                .to_string(),
                    });
                }
                let _ = approval_tx.try_send(decision);
            }
            AgentEvent::TurnComplete { text, .. } => {
                if let Some(text) = text
                    && !text.is_empty()
                {
                    // Non-streaming paths (refusal, budget cutoff, clarifying
                    // question) carry their whole answer here and nowhere
                    // else. Skip only what the flush already stored.
                    if let Some(final_text) = assistant.complete(Some(&text)) {
                        store.append_message("assistant", &final_text, "", "", None);
                    }
                    answer = text;
                }
            }
        };

        agent_loop::run_turn(
            &llm,
            selected_tool_server,
            command_tool_runtime,
            &mut history,
            tools.as_ref(),
            &turn_config,
            &request.message,
            None, // task-driven research context (chat path — no task)
            &mut transcript,
            hooks.as_ref(),
            permissions,
            None,
            &mut scratchpad,
            &mut emit,
            Some(approval_rx),
            policy.as_mut(),
            subagent_lanes,
        )
        .await
        .map_err(ChatError::Turn)?;

        let outcome = ChatOutcome {
            session_id,
            answer,
            approvals_required,
            context_priming,
        };
        let _ = events.send(ChatEvent::Done {
            session_id: outcome.session_id.clone(),
            answer: outcome.answer.clone(),
            approvals_required: outcome.approvals_required.clone(),
            context_priming: outcome.context_priming.clone(),
        });
        Ok(outcome)
    }

    // ── Session listing/reading (per-user, no turn lock needed) ───

    /// Sessions owned by `user_id`, newest first.
    pub fn list_sessions(&self, user_id: &str) -> Vec<SessionInfo> {
        if user_id == ANONYMOUS_LOCAL_USER_ID {
            return Vec::new();
        }
        let owners = self.owners.lock().unwrap_or_else(|e| e.into_inner());
        SessionStore::new(Some(self.sessions_dir.clone()))
            .list_sessions(usize::MAX)
            .into_iter()
            .filter(|info| owners.get(&info.session_id).is_some_and(|o| o == user_id))
            .collect()
    }

    /// Messages of one session, if `user_id` owns it.
    pub fn read_session(
        &self,
        session_id: &str,
        user_id: &str,
    ) -> Result<Vec<serde_json::Value>, ChatError> {
        if user_id == ANONYMOUS_LOCAL_USER_ID || !self.user_owns(session_id, user_id) {
            return Err(ChatError::SessionNotFound(session_id.to_string()));
        }
        SessionStore::new(Some(self.sessions_dir.clone()))
            .load_messages(session_id)
            .ok_or_else(|| ChatError::SessionNotFound(session_id.to_string()))
    }

    fn user_owns(&self, session_id: &str, user_id: &str) -> bool {
        self.owners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session_id)
            .is_some_and(|owner| owner == user_id)
    }

    fn record_owner(&self, session_id: &str, user_id: &str) {
        let mut owners = self.owners.lock().unwrap_or_else(|e| e.into_inner());
        owners.insert(session_id.to_string(), user_id.to_string());
        if let Err(e) = write_owners(&self.owners_path, &owners) {
            tracing::warn!(error = %e, "failed to persist chat session owners");
        }
    }
}

/// The session-owner map, from disk.
///
/// This was `read_to_string(..).ok().and_then(parse.ok()).unwrap_or_default()`:
/// a read error and a parse error alike became an EMPTY map with no log. A
/// file truncated by a crash mid-write then made `user_owns` false for every
/// session, `chat_inner` answered `SessionNotFound`, and `list_sessions`
/// filtered them all out — every conversation on disk, intact, reported to
/// the user as not found. A missing file is the first run and is fine. A file
/// that exists and will not parse is set aside under a `.corrupt-<ts>` name so
/// the next `record_owner` cannot overwrite the evidence, and the operator is
/// told which file and why.
fn load_owners(path: &std::path::Path) -> BTreeMap<String, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return BTreeMap::new(),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "chat session owners file could not be read — every session will answer 'not found' until it can");
            return BTreeMap::new();
        }
    };
    match serde_json::from_str::<BTreeMap<String, String>>(&text) {
        Ok(owners) => owners,
        Err(e) => {
            let aside = path.with_extension(format!(
                "json.corrupt-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            ));
            let moved = std::fs::rename(path, &aside).is_ok();
            tracing::warn!(
                path = %path.display(),
                error = %e,
                set_aside = moved,
                "chat session owners file is corrupt; every existing session will answer 'not found' until ownership is restored"
            );
            BTreeMap::new()
        }
    }
}

/// Atomic: write beside, then rename over. A crash mid-`fs::write` left a
/// truncated file, which is exactly what `load_owners` then could not parse.
fn write_owners(path: &std::path::Path, owners: &BTreeMap<String, String>) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(owners)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)
}

/// Headless approval policy: allow only tools the request explicitly
/// pre-approved by name; everything else is denied (skipped) and surfaced
/// as an `approval_required` event. Never auto-approves.
fn approval_decision(approved: &BTreeSet<String>, tool_name: &str) -> ApprovalResponse {
    if approved.contains(tool_name) {
        ApprovalResponse::Allow
    } else {
        ApprovalResponse::Deny
    }
}

fn local_only_tool_server_config(config: &ToolServer) -> ToolServer {
    // Allowlist, not a denylist: combined with `env_clear`, a new credential
    // variable added next month stays out by default. PATH is operational, not
    // authority; the worker needs it for local subprocess-backed tools.
    let mut env = BTreeMap::new();
    if let Some(path) = config
        .env
        .get("PATH")
        .cloned()
        .or_else(|| std::env::var("PATH").ok())
    {
        env.insert("PATH".to_string(), path);
    }
    env.insert("PRISM_OFFLINE".to_string(), "1".to_string());
    ToolServer {
        python_bin: config.python_bin.clone(),
        project_root: config.project_root.clone(),
        env,
    }
}

/// Build the tool-server env for an embedded chat service the same way the
/// `prism backend` arm does (MCP marker + platform URL + user API keys; the
/// session JWT is deliberately NOT exported — see the backend arm comment).
pub fn default_tool_server_env(api_base: Option<&str>) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert("PRISM_ENABLE_MCP".to_string(), "1".to_string());
    if let Some(api_base) = api_base {
        env.insert("PRISM_API_URL".to_string(), api_base.to_string());
        env.insert("MARC27_API_URL".to_string(), api_base.to_string());
    }
    for key in &[
        "MP_API_KEY",
        "LENS_API_TOKEN",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "FIRECRAWL_API_KEY",
    ] {
        if let Ok(val) = std::env::var(key) {
            env.insert((*key).to_string(), val);
        }
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_event_kinds_match_serde_type_tag() {
        let events = [
            ChatEvent::Thinking { text: "hmm".into() },
            ChatEvent::Answer {
                text: "hello".into(),
            },
            ChatEvent::ContextPriming {
                iteration: 0,
                status: ContextPrimingStatus::NotRequested {
                    applied: crate::influence::ToolSelectionMethod::Keyword,
                },
            },
            ChatEvent::ToolCall {
                tool_name: "query".into(),
                call_id: "c1".into(),
                preview: None,
            },
            tool_result_event("query".into(), "c1".into(), None, "{}".into(), 3, false),
            ChatEvent::ApprovalRequired {
                tool_name: "execute_bash".into(),
                call_id: "c2".into(),
                description: None,
                permission_mode: "full-access".into(),
                hint: "hint".into(),
            },
            ChatEvent::Done {
                session_id: "s1".into(),
                answer: "hello".into(),
                approvals_required: vec![],
                context_priming: vec![],
            },
            ChatEvent::Error {
                message: "boom".into(),
            },
        ];
        assert_eq!(events.len(), 8, "cover every ChatEvent variant");
        let expected = [
            "thinking",
            "answer",
            "context_priming",
            "tool_call",
            "tool_result",
            "approval_required",
            "done",
            "error",
        ];
        for (event, expected_kind) in events.iter().zip(expected) {
            assert_eq!(event.kind(), expected_kind);
            let json = serde_json::to_value(event).expect("serialize");
            assert_eq!(
                json.get("type").and_then(|t| t.as_str()),
                Some(expected_kind),
                "serde type tag must match SSE event name"
            );
        }
    }

    #[test]
    fn terminal_context_priming_records_retain_iteration_and_status() {
        let records = vec![
            ContextPrimingRecord {
                iteration: 2,
                status: ContextPrimingStatus::Fallback {
                    requested: crate::influence::ToolSelectionMethod::Influence,
                    applied: crate::influence::ToolSelectionMethod::Cosine,
                    reason: "influence_no_signal".to_string(),
                },
            },
            ContextPrimingRecord {
                iteration: 7,
                status: ContextPrimingStatus::Primed {
                    index_id: "index-v1".to_string(),
                    selected_candidates: vec!["query_platform".to_string()],
                    exact_context_tokens: 42,
                    scorer_input_tokens: 120,
                    scoring_ms: 8,
                    model_sha256: "model-sha".to_string(),
                    template_sha256: "template-sha".to_string(),
                },
            },
        ];
        let outcome = ChatOutcome {
            session_id: "session-1".to_string(),
            answer: "done".to_string(),
            approvals_required: Vec::new(),
            context_priming: records.clone(),
        };
        let done = ChatEvent::Done {
            session_id: "session-1".to_string(),
            answer: "done".to_string(),
            approvals_required: Vec::new(),
            context_priming: records,
        };

        for payload in [
            serde_json::to_value(outcome).expect("serialize outcome"),
            serde_json::to_value(done).expect("serialize done event"),
        ] {
            let records = &payload["context_priming"];
            assert_eq!(records[0]["iteration"], 2);
            assert_eq!(records[0]["status"]["status"], "fallback");
            assert_eq!(records[0]["status"]["requested"], "influence");
            assert_eq!(records[0]["status"]["applied"], "cosine");
            assert_eq!(records[0]["status"]["reason"], "influence_no_signal");
            assert_eq!(records[1]["iteration"], 7);
            assert_eq!(records[1]["status"]["status"], "primed");
            assert_eq!(
                records[1]["status"]["selected_candidates"][0],
                "query_platform"
            );
        }
    }

    #[test]
    fn tool_result_wire_event_carries_evidence_and_defaults_missing_to_indeterminate() {
        let classified = tool_result_event(
            "hea_descriptors".into(),
            "c1".into(),
            None,
            r#"{"value":0.8125,"evidence_class":"screening","evidence_color":"yellow"}"#.into(),
            3,
            false,
        );
        let classified = serde_json::to_value(classified).expect("serialize classified event");
        assert_eq!(classified["evidence_class"], "screening");
        assert_eq!(classified["evidence_color"], "yellow");

        let legacy = tool_result_event(
            "legacy_evaluator".into(),
            "c2".into(),
            None,
            r#"{"value":3455.3}"#.into(),
            3,
            false,
        );
        let legacy = serde_json::to_value(legacy).expect("serialize legacy event");
        assert_eq!(legacy["evidence_class"], "indeterminate");
        assert_eq!(legacy["evidence_color"], "red");
    }

    #[test]
    fn approval_decision_is_deny_by_default() {
        let approved: BTreeSet<String> = ["execute_bash".to_string()].into_iter().collect();
        assert!(matches!(
            approval_decision(&approved, "execute_bash"),
            ApprovalResponse::Allow
        ));
        // Anything not explicitly named is denied — no silent auto-approve.
        assert!(matches!(
            approval_decision(&approved, "delete_everything"),
            ApprovalResponse::Deny
        ));
        let empty = BTreeSet::new();
        assert!(matches!(
            approval_decision(&empty, "execute_bash"),
            ApprovalResponse::Deny
        ));
    }

    /// A corrupt owners file must be set aside and said, never silently read
    /// as "nobody owns anything"; a missing one is the first run.
    #[test]
    fn a_corrupt_owners_file_is_set_aside_not_silently_emptied() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("http_chat_owners.json");
        assert!(super::load_owners(&path).is_empty(), "missing = first run");
        std::fs::write(&path, "{\"s1\": \"alice\", \"s2\": \"bo").unwrap(); // truncated mid-write
        let owners = super::load_owners(&path);
        assert!(owners.is_empty());
        assert!(
            !path.exists(),
            "the corrupt file must be moved aside, not left to be overwritten"
        );
        let aside: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("corrupt-"))
            .collect();
        assert_eq!(
            aside.len(),
            1,
            "the evidence is preserved under a .corrupt-<ts> name"
        );
    }

    /// The write is atomic and round-trips; no temp file is left behind.
    #[test]
    fn owners_are_written_atomically_and_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("http_chat_owners.json");
        let mut owners = BTreeMap::new();
        owners.insert("s1".to_string(), "alice".to_string());
        super::write_owners(&path, &owners).unwrap();
        assert!(
            !path.with_extension("json.tmp").exists(),
            "no temp file left behind"
        );
        assert_eq!(super::load_owners(&path), owners);
    }
}
