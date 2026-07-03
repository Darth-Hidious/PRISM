// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
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
use crate::middleware::AuthenticatedUser;
use prism_agent::service::{ChatError, ChatEvent, ChatRequest, ChatService};

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

/// `POST /api/chat` — run one agent turn.
pub async fn chat(
    State(state): State<Arc<NodeState>>,
    Extension(user): Extension<AuthenticatedUser>,
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
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<ChatEvent>();

    if params.stream {
        // The service always terminates the stream with a `done` or
        // `error` event, so clients never hang on failures.
        let user_id = user.user_id.clone();
        tokio::spawn(async move {
            let _ = service.chat(request, &user_id, tx).await;
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
        match service.chat(request, &user.user_id, tx).await {
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
) -> Response {
    let Some(service) = chat_service(&state) else {
        return service_unavailable();
    };
    Json(serde_json::json!({ "sessions": service.list_sessions(&user.user_id) })).into_response()
}

/// `GET /api/chat/sessions/{id}` — read one owned session's messages.
pub async fn get_session(
    State(state): State<Arc<NodeState>>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(id): Path<String>,
) -> Response {
    let Some(service) = chat_service(&state) else {
        return service_unavailable();
    };
    match service.read_session(&id, &user.user_id) {
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
