// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
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

use anyhow::Result;
use prism_ingest::LlmConfig;
use prism_ingest::llm::{ChatMessage, LlmClient};
use prism_python_bridge::{ToolServer, ToolServerHandle};
use serde::Serialize;
use tokio::sync::mpsc;

use crate::agent_loop::{self, ApprovalResponse};
use crate::command_tools::CommandToolRuntime;
use crate::hooks::HookRegistry;
use crate::permissions::ToolPermissionContext;
use crate::protocol::{AgentSeed, build_agent_seed, restore_history_and_transcript_from_messages};
use crate::scratchpad::Scratchpad;
use crate::session::{SessionInfo, SessionStore};
use crate::tool_catalog::ToolCatalog;
use crate::transcript::TranscriptStore;
use crate::types::{AgentConfig, AgentEvent};

// ── Wire types ───────────────────────────────────────────────────────

/// Typed event stream for chat clients. Serialized with a `type` tag so
/// SSE/JSON consumers can switch on it: `thinking`, `answer`, `tool_call`,
/// `tool_result`, `approval_required`, `done`, `error`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatEvent {
    /// Reasoning tokens (streamed).
    Thinking { text: String },
    /// Response text delta (streamed). The complete answer is repeated in
    /// the final `done` event.
    Answer { text: String },
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
            Self::ToolCall { .. } => "tool_call",
            Self::ToolResult { .. } => "tool_result",
            Self::ApprovalRequired { .. } => "approval_required",
            Self::Done { .. } => "done",
            Self::Error { .. } => "error",
        }
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
    command_tool_runtime: CommandToolRuntime,
    config: Arc<AgentConfig>,
    hooks: Arc<HookRegistry>,
    permissions: ToolPermissionContext,
    llm_config: LlmConfig,
    policy: Option<prism_policy::PolicyEngine>,
    store: SessionStore,
}

/// The agent loop as a service. One instance per node process; turns are
/// serialized through an async mutex (the underlying Python tool server is
/// a single stdio child — same one-turn-at-a-time model as the backend).
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
            command_tool_runtime,
            tools,
            config,
            hooks,
            permissions,
        } = build_agent_seed(&tool_server_config).await?;

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

        let store = SessionStore::new(sessions_dir);
        let sessions_dir = store.dir().to_path_buf();
        let owners_path = sessions_dir.join("http_chat_owners.json");
        let owners = std::fs::read_to_string(&owners_path)
            .ok()
            .and_then(|text| serde_json::from_str::<BTreeMap<String, String>>(&text).ok())
            .unwrap_or_default();

        Ok(Self {
            inner: tokio::sync::Mutex::new(ChatInner {
                tool_server,
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
        if crate::meta_tools::is_meta_tool(name) {
            anyhow::bail!(
                "'{name}' is a meta-tool that operates on live agent state; \
                 it is not invocable through the single-tool executor"
            );
        }

        // Approval gate. Catalog lookup covers Python + offered command tools;
        // the command-tool fallback covers specs hidden from the catalog
        // (hidden ≠ unexecutable — see LOCAL_NODE_TOOLS).
        let gated = self
            .tools
            .find(name)
            .map(|tool| tool.requires_approval)
            .or_else(|| crate::command_tools::command_tool_requires_approval(name));
        if gated == Some(true) && !approve {
            anyhow::bail!(
                "'{name}' is approval-gated and cannot run through the \
                 single-tool executor without explicit approval \
                 (pass approve=true from an authenticated local caller; \
                 remote relay callers cannot approve)"
            );
        }

        tracing::info!(
            tool = %name,
            caller = caller.unwrap_or("<unspecified>"),
            "invoke_tool: single-tool execution"
        );

        let result = {
            let mut inner = self.inner.lock().await;
            let ChatInner {
                tool_server,
                command_tool_runtime,
                policy,
                ..
            } = &mut *inner;

            if crate::command_tools::is_command_tool(name) {
                crate::command_tools::execute_command_tool(
                    command_tool_runtime,
                    name,
                    &args,
                    policy.as_mut(),
                )
                .await
            } else {
                tool_server
                    .call_tool(name, args.clone())
                    .await
                    .map_err(Into::into)
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
                prism_provenance::Actor::User,
                Some(name),
                None,
                args,
            );
            record.output_json = Some(match &result {
                Ok(value) => value.clone(),
                Err(e) => serde_json::json!({ "error": format!("{e:#}") }),
            });
            record.tags = vec![
                "single-tool-executor".to_string(),
                format!("caller:{}", caller.unwrap_or("unspecified")),
            ];
            let db_path = dirs::home_dir()
                .map(|h| h.join(".prism/provenance.db"))
                .unwrap_or_else(|| std::path::PathBuf::from("provenance.db"));
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
        let result = self.chat_inner(request, user_id, &events).await;
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
                if !self.user_owns(sid, user_id) {
                    // Unknown AND not-owned collapse to the same error so
                    // the API doesn't leak which session ids exist.
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

        // Split borrows: run_turn needs &mut tool_server while the emit
        // callback appends to the session store.
        let ChatInner {
            tool_server,
            command_tool_runtime,
            hooks,
            permissions,
            policy,
            store,
            ..
        } = &mut *inner;

        let tools = Arc::clone(&self.tools);
        let mut emit = |event: AgentEvent| match event {
            AgentEvent::ThinkingDelta { text } => {
                let _ = events.send(ChatEvent::Thinking { text });
            }
            AgentEvent::TextDelta { text } => {
                let _ = events.send(ChatEvent::Answer { text });
            }
            AgentEvent::TextFlush => {}
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
                let _ = events.send(ChatEvent::ToolResult {
                    tool_name,
                    call_id,
                    summary,
                    content,
                    elapsed_ms,
                    is_error,
                });
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
                    store.append_message("assistant", &text, "", "", None);
                    answer = text;
                }
            }
        };

        agent_loop::run_turn(
            &llm,
            tool_server,
            command_tool_runtime,
            &mut history,
            tools.as_ref(),
            &turn_config,
            &request.message,
            &mut transcript,
            hooks.as_ref(),
            permissions,
            None,
            &mut scratchpad,
            &mut emit,
            Some(approval_rx),
            policy.as_mut(),
        )
        .await
        .map_err(ChatError::Turn)?;

        let outcome = ChatOutcome {
            session_id,
            answer,
            approvals_required,
        };
        let _ = events.send(ChatEvent::Done {
            session_id: outcome.session_id.clone(),
            answer: outcome.answer.clone(),
            approvals_required: outcome.approvals_required.clone(),
        });
        Ok(outcome)
    }

    // ── Session listing/reading (per-user, no turn lock needed) ───

    /// Sessions owned by `user_id`, newest first.
    pub fn list_sessions(&self, user_id: &str) -> Vec<SessionInfo> {
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
        if !self.user_owns(session_id, user_id) {
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
        match serde_json::to_string_pretty(&*owners) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&self.owners_path, json) {
                    tracing::warn!(error = %e, "failed to persist chat session owners");
                }
            }
            Err(e) => tracing::warn!(error = %e, "failed to serialize chat session owners"),
        }
    }
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

/// Build the tool-server env for an embedded chat service the same way the
/// `prism backend` arm does (MCP marker + platform URL + user API keys; the
/// session JWT is deliberately NOT exported — see the backend arm comment).
pub fn default_tool_server_env(api_base: &str) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert("PRISM_ENABLE_MCP".to_string(), "1".to_string());
    env.insert("MARC27_API_URL".to_string(), api_base.to_string());
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
            ChatEvent::ToolCall {
                tool_name: "query".into(),
                call_id: "c1".into(),
                preview: None,
            },
            ChatEvent::ToolResult {
                tool_name: "query".into(),
                call_id: "c1".into(),
                summary: None,
                content: "{}".into(),
                elapsed_ms: 3,
                is_error: false,
            },
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
            },
            ChatEvent::Error {
                message: "boom".into(),
            },
        ];
        assert_eq!(events.len(), 7, "cover every ChatEvent variant");
        let expected = [
            "thinking",
            "answer",
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
}
