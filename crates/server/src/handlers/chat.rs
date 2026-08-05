// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Conversational agent endpoints — the agent loop as an HTTP service.
//!
//! `POST /api/chat` runs the SAME agent loop the TUI backend runs (see
//! `prism_agent::service::ChatService` — construction and turn dispatch are
//! shared with `prism backend`, only the transport differs). Any HTTP or
//! MCP client gets full chat-app parity: same tool catalog, same policy
//! gates, same session persistence.
//!
//! - Default response is an SSE stream of typed events (`thinking`,
//!   `answer`, `tool_call`, `tool_result`, `approval_required`, `done`,
//!   `error`).
//! - `?stream=false` returns a single JSON body with the final answer.
//! - Tool approvals are headless: gated tools are skipped with an
//!   `approval_required` event unless named in the request's `approve`
//!   list. No silent auto-approve.

use std::convert::Infallible;
use std::sync::Arc;

use axum::Extension;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::NodeState;
use crate::handlers::deployments::command_tool_platform_access;
use crate::middleware::{AuthenticatedUser, SessionToken};
use prism_agent::service::{ChatError, ChatEvent, ChatRequest, ChatService, anonymous_caller_id};

#[derive(Deserialize)]
pub struct ChatBody {
    pub message: String,
    #[serde(default)]
    pub session_id: Option<String>,
    /// Tool names pre-approved for this turn (headless approval).
    #[serde(default)]
    pub approve: Vec<String>,
}

#[derive(Deserialize)]
pub struct ChatParams {
    /// `true` (default): SSE event stream. `false`: single JSON response.
    #[serde(default = "default_stream")]
    pub stream: bool,
}

fn default_stream() -> bool {
    true
}

fn error_json(status: StatusCode, error: &str, message: String) -> Response {
    (
        status,
        Json(serde_json::json!({ "error": error, "message": message })),
    )
        .into_response()
}

fn service_unavailable() -> Response {
    error_json(
        StatusCode::SERVICE_UNAVAILABLE,
        "chat_unavailable",
        "Chat service is not running on this node — it needs a configured LLM \
         ([chat] in ~/.prism/config.toml or [indexer] in prism.toml) and a \
         Python tool environment. Check the node logs for the reason."
            .to_string(),
    )
}

fn chat_service(state: &NodeState) -> Option<Arc<ChatService>> {
    state.chat.get().cloned()
}

fn chat_owner(user: &AuthenticatedUser, token: &SessionToken) -> String {
    if user.is_anonymous_local() {
        // auth_layer has validated either a durable session row or an
        // unguessable, unexpired standalone capability. Hash it into a stable
        // owner key without copying the bearer into chat ownership metadata.
        anonymous_caller_id(&token.0)
    } else {
        user.user_id.clone()
    }
}

/// `POST /api/chat` — run one agent turn.
pub async fn chat(
    State(state): State<Arc<NodeState>>,
    Extension(user): Extension<AuthenticatedUser>,
    Extension(token): Extension<SessionToken>,
    Query(params): Query<ChatParams>,
    Json(body): Json<ChatBody>,
) -> Response {
    let Some(service) = chat_service(&state) else {
        return service_unavailable();
    };
    if body.message.trim().is_empty() {
        return error_json(
            StatusCode::BAD_REQUEST,
            "empty_message",
            "message must not be empty".to_string(),
        );
    }

    let request = ChatRequest {
        message: body.message,
        session_id: body.session_id,
        approve: body.approve,
    };
    let platform_access = command_tool_platform_access(&state, &user).await;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<ChatEvent>();

    if params.stream {
        // The service always terminates the stream with a `done` or
        // `error` event, so clients never hang on failures.
        let owner = chat_owner(&user, &token);
        tokio::spawn(async move {
            let _ = service
                .chat_with_platform_access(request, &owner, platform_access, tx)
                .await;
        });
        let stream = UnboundedReceiverStream::new(rx).map(|event| {
            Ok::<_, Infallible>(
                Event::default()
                    .event(event.kind())
                    .json_data(&event)
                    .unwrap_or_else(|e| {
                        Event::default().event("error").data(format!(
                            "{{\"type\":\"error\",\"message\":\"serialize: {e}\"}}"
                        ))
                    }),
            )
        });
        Sse::new(stream)
            .keep_alive(KeepAlive::default())
            .into_response()
    } else {
        // Non-streaming: drain events into the void, return the outcome.
        drop(rx);
        let owner = chat_owner(&user, &token);
        match service
            .chat_with_platform_access(request, &owner, platform_access, tx)
            .await
        {
            Ok(outcome) => Json(serde_json::json!({
                "session_id": outcome.session_id,
                "answer": outcome.answer,
                "approvals_required": outcome.approvals_required,
            }))
            .into_response(),
            Err(ChatError::SessionNotFound(sid)) => error_json(
                StatusCode::NOT_FOUND,
                "session_not_found",
                format!("no such chat session: {sid}"),
            ),
            Err(ChatError::Turn(e)) => error_json(
                StatusCode::INTERNAL_SERVER_ERROR,
                "chat_turn_failed",
                format!("{e:#}"),
            ),
        }
    }
}

/// `GET /api/chat/sessions` — list the authenticated user's chat sessions.
pub async fn list_sessions(
    State(state): State<Arc<NodeState>>,
    Extension(user): Extension<AuthenticatedUser>,
    Extension(token): Extension<SessionToken>,
) -> Response {
    let Some(service) = chat_service(&state) else {
        return service_unavailable();
    };
    let owner = chat_owner(&user, &token);
    Json(serde_json::json!({ "sessions": service.list_sessions(&owner) })).into_response()
}

/// `GET /api/chat/sessions/{id}` — read one owned session's messages.
pub async fn get_session(
    State(state): State<Arc<NodeState>>,
    Extension(user): Extension<AuthenticatedUser>,
    Extension(token): Extension<SessionToken>,
    Path(id): Path<String>,
) -> Response {
    let Some(service) = chat_service(&state) else {
        return service_unavailable();
    };
    let owner = chat_owner(&user, &token);
    match service.read_session(&id, &owner) {
        Ok(messages) => Json(serde_json::json!({
            "session_id": id,
            "messages": messages,
        }))
        .into_response(),
        Err(_) => error_json(
            StatusCode::NOT_FOUND,
            "session_not_found",
            format!("no such chat session: {id}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{anonymous_caller_id, chat_owner};
    use crate::NodeState;
    use crate::middleware::{AuthenticatedUser, SessionToken};
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode, header};
    use prism_agent::service::{ChatRequest, ChatService};
    use serde_json::json;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use tower::ServiceExt;

    const STUB_TOOL_SERVER: &str = r#"
import json
import sys

for line in sys.stdin:
    request = json.loads(line)
    if request.get("method") == "list_tools":
        response = {"tools": []}
    else:
        response = {"error": "unexpected tool call"}
    sys.stdout.write(json.dumps(response) + "\n")
    sys.stdout.flush()
"#;

    fn find_python() -> Option<PathBuf> {
        std::process::Command::new("python3")
            .arg("--version")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|_| PathBuf::from("python3"))
    }

    fn write_stub_tool_server(project: &Path) {
        let app = project.join("app");
        std::fs::create_dir_all(&app).expect("create stub app");
        std::fs::write(app.join("__init__.py"), "").expect("write package marker");
        std::fs::write(app.join("tool_server.py"), STUB_TOOL_SERVER)
            .expect("write stub tool server");
    }

    async fn start_stub_llm() -> String {
        let response = serde_json::json!({
            "choices": [{ "delta": { "content": "SOLO_OK" } }]
        });
        let body = format!("data: {response}\n\ndata: [DONE]\n\n");
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move || {
                let body = body.clone();
                async move {
                    axum::response::Response::builder()
                        .header("content-type", "text/event-stream")
                        .body(axum::body::Body::from(body))
                        .expect("stub response")
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind stub LLM");
        let address = listener.local_addr().expect("stub LLM address");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{address}/v1")
    }

    fn llm_config(base_url: &str) -> prism_ingest::LlmConfig {
        prism_ingest::LlmConfig {
            base_url: base_url.to_string(),
            model: "stub-model".into(),
            timeout_secs: 30,
            ..Default::default()
        }
    }

    fn tool_server(project: &Path, python: &Path) -> prism_python_bridge::ToolServer {
        prism_python_bridge::ToolServer {
            python_bin: python.to_path_buf(),
            project_root: project.to_path_buf(),
            env: Default::default(),
        }
    }

    #[test]
    fn offline_chat_owner_is_stable_for_own_capability_and_scoped_from_anothers() {
        let state = crate::NodeState::new("offline-test".into());
        let token_a = state.mint_offline_session().expect("mint token a").token;
        let token_b = state.mint_offline_session().expect("mint token b").token;
        let user = AuthenticatedUser::anonymous_local();
        let owner_a = chat_owner(&user, &SessionToken(token_a.clone()));
        let owner_a_again = chat_owner(&user, &SessionToken(token_a.clone()));
        let owner_b = chat_owner(&user, &SessionToken(token_b.clone()));

        assert!(state.is_valid_offline_session_token(&token_a));
        assert!(state.is_valid_offline_session_token(&token_b));
        assert_eq!(owner_a, owner_a_again);
        assert_ne!(owner_a, owner_b);
        assert!(!owner_a.contains(&token_a));
        assert!(!owner_b.contains(&token_b));
        assert_eq!(owner_a, anonymous_caller_id(&token_a));
        assert_eq!(owner_b, anonymous_caller_id(&token_b));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn offline_solo_user_resumes_own_session_across_server_restart() {
        let Some(python) = find_python() else {
            eprintln!("SKIP: python3 not on PATH");
            return;
        };
        let project = tempfile::tempdir().expect("stub project");
        let state_dir = tempfile::tempdir().expect("server state");
        let sessions_dir = state_dir.path().join("chat-sessions");
        write_stub_tool_server(project.path());
        let base_url = start_stub_llm().await;

        let mut first_node = crate::NodeState::new("offline-node".into());
        first_node.audit_db_path = Some(state_dir.path().join("audit.db"));
        let capability = first_node
            .mint_offline_session()
            .expect("mint standalone session");
        let owner = anonymous_caller_id(&capability.token);
        let first_service = ChatService::spawn(
            llm_config(&base_url),
            tool_server(project.path(), &python),
            Some(sessions_dir.clone()),
        )
        .await
        .expect("start first chat service");
        let (first_tx, _first_rx) = tokio::sync::mpsc::unbounded_channel();
        let first_turn = first_service
            .chat(
                ChatRequest {
                    message: "start my solo session".into(),
                    session_id: None,
                    approve: Vec::new(),
                },
                &owner,
                first_tx,
            )
            .await
            .expect("first solo turn");
        drop(first_service);
        drop(first_node);

        let mut restarted_node = crate::NodeState::new("offline-node".into());
        restarted_node.audit_db_path = Some(state_dir.path().join("audit.db"));
        assert!(
            restarted_node.is_valid_offline_session_token(&capability.token),
            "the original bearer must authenticate after restart"
        );
        let restarted_owner = anonymous_caller_id(&capability.token);
        assert_eq!(restarted_owner, owner);
        let restarted_service = ChatService::spawn(
            llm_config(&base_url),
            tool_server(project.path(), &python),
            Some(sessions_dir),
        )
        .await
        .expect("restart chat service");
        let (resume_tx, _resume_rx) = tokio::sync::mpsc::unbounded_channel();
        let resumed = restarted_service
            .chat(
                ChatRequest {
                    message: "resume after restart".into(),
                    session_id: Some(first_turn.session_id.clone()),
                    approve: Vec::new(),
                },
                &restarted_owner,
                resume_tx,
            )
            .await
            .expect("resume solo session after restart");
        assert_eq!(resumed.session_id, first_turn.session_id);
        assert_eq!(resumed.answer, "SOLO_OK");
    }

    // ── Round 7: literal POST /api/chat ────────────────────────────────

    /// `PRISM_SKILLS_DIR` is process-global; restore it on drop so a failed
    /// assertion cannot leak the temp dir into sibling tests.
    struct SkillsEnvGuard;

    impl Drop for SkillsEnvGuard {
        fn drop(&mut self) {
            // SAFETY: test-only process-global cleanup.
            unsafe { std::env::remove_var("PRISM_SKILLS_DIR") };
        }
    }

    /// Stub LLM that calls `run_skill(name=probe)` until a tool result is the
    /// last message, then answers.
    async fn start_skill_probe_llm() -> String {
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(
                |axum::Json(body): axum::Json<serde_json::Value>| async move {
                    let last_is_tool = body["messages"]
                        .as_array()
                        .and_then(|msgs| msgs.last())
                        .map(|m| m["role"] == "tool")
                        .unwrap_or(false);
                    let chunk = if last_is_tool {
                        json!({"choices": [{"delta": {"content": "CHAT_DONE"}}]})
                    } else {
                        json!({"choices": [{"delta": {"tool_calls": [{
                            "index": 0,
                            "id": "call_1",
                            "function": {
                                "name": "run_skill",
                                "arguments": "{\"name\": \"probe\"}"
                            }
                        }]}}]})
                    };
                    let body = format!("data: {chunk}\n\ndata: [DONE]\n\n");
                    axum::response::Response::builder()
                        .header("content-type", "text/event-stream")
                        .body(axum::body::Body::from(body))
                        .expect("stub response")
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind stub LLM");
        let address = listener.local_addr().expect("stub LLM address");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{address}/v1")
    }

    /// Round 7, end to end through the real route: an authenticated non-owner
    /// of a LINKED node resolves to LocalOnly (`command_tool_platform_access`),
    /// and a `/api/chat` turn whose model calls `run_skill` — pre-approved by
    /// the caller itself, the headless equivalent of clicking "allow" — is
    /// refused with the owner-only message. The skill must NOT execute.
    #[tokio::test(flavor = "multi_thread")]
    async fn non_owner_cannot_reach_run_skill_via_api_chat() {
        let Some(python) = find_python() else {
            eprintln!("SKIP: python3 not on PATH");
            return;
        };

        // Isolated skill store + a pre-planted skill whose body writes a
        // marker file if it EVER executes.
        let skills_dir =
            std::env::temp_dir().join(format!("prism-api-chat-skills-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&skills_dir);
        std::fs::create_dir_all(&skills_dir).expect("create temp skills dir");
        // SAFETY: test-only process-global setup; restored by SkillsEnvGuard.
        unsafe { std::env::set_var("PRISM_SKILLS_DIR", &skills_dir) };
        let _skills_guard = SkillsEnvGuard;
        let marker = skills_dir.join("run_marker");
        prism_agent::skills::store(&prism_agent::skills::AuthoredSkill::new(
            "probe",
            "probe skill for the access-gate test",
            "shell",
            &format!("echo PWNED > {}", marker.to_string_lossy()),
            true,
        ))
        .expect("store probe skill");

        // Chat service backed by the stub LLM + stub tool server.
        let project = tempfile::tempdir().expect("create tool project");
        write_stub_tool_server(project.path());
        let sessions_dir = tempfile::tempdir().expect("sessions dir");
        let base_url = start_skill_probe_llm().await;
        let service = ChatService::spawn(
            llm_config(&base_url),
            tool_server(project.path(), &python),
            Some(sessions_dir.path().to_path_buf()),
        )
        .await
        .expect("spawn chat service");

        // A LINKED node (platform client configured) + an authenticated
        // session for a user who is NOT the node owner → LocalOnly.
        let session_db = tempfile::NamedTempFile::new().expect("create session database");
        let manager = prism_core::session::SessionManager::new(
            session_db.path(),
            chrono::Duration::hours(24),
        )
        .expect("open session manager");
        let session = manager
            .create_session("authenticated:non-owner", None, None)
            .expect("create authenticated non-owner session");

        let mut node = NodeState::new("test-node".into());
        node.session_db_path = Some(session_db.path().to_path_buf());
        node.platform_client = Some(prism_client::PlatformClient::new("http://127.0.0.1:1"));
        node.platform_owner_id
            .set("owner-user".into())
            .expect("set platform owner");
        assert!(node.chat.set(Arc::new(service)).is_ok());
        let app = crate::router::build_router(Arc::new(node));

        let request = Request::builder()
            .method("POST")
            .uri("/api/chat")
            .header(header::AUTHORIZATION, format!("Bearer {}", session.id))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "message": "run my probe skill",
                    "approve": ["run_skill"]
                })
                .to_string(),
            ))
            .expect("build request");

        let response = app.oneshot(request).await.expect("run request");
        assert_eq!(response.status(), StatusCode::OK);
        let body = String::from_utf8_lossy(
            &to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("read SSE body"),
        )
        .to_string();

        // The streamed tool_result carries the owner-only refusal.
        assert!(
            body.contains("run_skill"),
            "the turn must show the run_skill attempt: {body}"
        );
        assert!(
            body.contains("owner-only"),
            "the refusal must reach the HTTP client: {body}"
        );
        assert!(body.contains("verified node-owner session"), "{body}");
        // The skill did NOT execute, and the store is untouched.
        assert!(
            !marker.exists(),
            "run_skill must not execute for a non-owner /api/chat caller"
        );
        assert!(prism_agent::skills::load("probe").is_ok());
    }
}
