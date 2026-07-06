//! WebSocket handler for real-time dashboard updates.
//!
//! Clients connect with `?token=<auth_token>` and receive [`WsEvent`]s
//! as JSON text frames. The server subscribes to the [`NodeState::ws_broadcast`]
//! channel and forwards events to each connected client.

use std::sync::Arc;

use axum::{
    extract::{
        Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use tokio::sync::broadcast;
use tracing::{debug, warn};

use crate::NodeState;

/// Maximum WebSocket message size (64 KB).
const MAX_MESSAGE_SIZE: usize = 64 * 1024;

/// Maximum concurrent WebSocket connections per node.
const MAX_WS_CONNECTIONS: usize = 128;

#[derive(Deserialize)]
pub struct WsParams {
    token: Option<String>,
}

/// Handle a WebSocket upgrade request.
///
/// Requires a `?token=` query parameter containing a valid session ID.
/// If `session_db_path` is configured, validates against SessionManager.
/// Otherwise falls back to treating the token as a user_id (localhost mode).
pub async fn ws_upgrade(
    State(state): State<Arc<NodeState>>,
    Query(params): Query<WsParams>,
    ws: WebSocketUpgrade,
) -> Response {
    let Some(ref t) = params.token else {
        return (StatusCode::UNAUTHORIZED, "Missing token query parameter.").into_response();
    };
    if t.is_empty() {
        return (StatusCode::UNAUTHORIZED, "Empty token query parameter.").into_response();
    }

    // Validate session if configured
    let user_id = if let Some(ref db_path) = state.session_db_path {
        match prism_core::session::SessionManager::new(db_path, chrono::Duration::hours(24)) {
            Ok(mgr) => match mgr.validate_session(t) {
                Ok(Some(session)) => session.user_id,
                Ok(None) => {
                    return (StatusCode::UNAUTHORIZED, "Session expired or invalid.")
                        .into_response();
                }
                Err(_) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Session validation failed.",
                    )
                        .into_response();
                }
            },
            Err(_) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Session service unavailable.",
                )
                    .into_response();
            }
        }
    } else {
        t.clone()
    };

    // Enforce the connection concurrency limit ATOMICALLY: reserve the slot
    // with a compare-and-swap. The old load-then-add was a TOCTOU race — two
    // simultaneous upgrades both passed the check and both incremented,
    // letting the live count exceed MAX_WS_CONNECTIONS (audit T3d).
    let reserved = state
        .ws_connections
        .fetch_update(
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
            |current| (current < MAX_WS_CONNECTIONS).then_some(current + 1),
        )
        .is_ok();
    if !reserved {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Too many WebSocket connections.",
        )
            .into_response();
    }

    // Look up the connected user's role to scope audit-event broadcasts.
    // Without this, every WS client (Engineer, Analyst, Viewer) saw every
    // OTHER user's AuditEntry events — leaking who ran what query, when.
    // See Bug #58. Default to non-admin if RBAC isn't configured or the
    // user has no role yet.
    let is_admin = state
        .rbac_db_path
        .as_ref()
        .and_then(|p| prism_core::rbac::RbacEngine::new(p).ok())
        .and_then(|e| e.get_role(&user_id).ok().flatten())
        .map(|role| role == prism_core::rbac::LocalRole::NodeAdmin)
        .unwrap_or(false);

    let rx = state.ws_broadcast.subscribe();
    let ws_state = state.clone();
    ws.max_message_size(MAX_MESSAGE_SIZE)
        .on_upgrade(move |socket| handle_socket(socket, user_id, is_admin, rx, ws_state))
}

async fn handle_socket(
    mut socket: WebSocket,
    user_id: String,
    is_admin: bool,
    mut rx: broadcast::Receiver<String>,
    state: Arc<NodeState>,
) {
    debug!(user_id = %user_id, is_admin, "WebSocket connection established");

    loop {
        tokio::select! {
            // Forward broadcast events to the client
            result = rx.recv() => {
                match result {
                    Ok(json) => {
                        // Filter audit events by user — every non-admin
                        // WS client used to see every other user's
                        // AuditEntry events (Bug #58). Now: NodeAdmin
                        // sees all; non-admins see only their own.
                        // Other event types (NodeStatusUpdate,
                        // MeshPeerChange) are unfiltered — public.
                        if !is_admin && audit_event_for_other_user(&json, &user_id) {
                            continue;
                        }
                        if socket.send(Message::Text(json.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        debug!(skipped = n, "WebSocket client lagged, skipping messages");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            // Handle incoming messages from the client
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Ping(payload))) => {
                        if socket.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Text(text))) => {
                        debug!(len = text.len(), "received WS text message");
                    }
                    Some(Ok(_)) => {} // Binary, Pong — ignore
                    Some(Err(e)) => {
                        warn!("WebSocket receive error: {e}");
                        break;
                    }
                }
            }
        }
    }

    state
        .ws_connections
        .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    debug!(user_id = %user_id, "WebSocket connection closed");
}

/// Returns true when the JSON event is an `AuditEntry` whose `user`
/// field is set to someone other than `connected_user`. Used to
/// scope audit broadcasts to non-admin clients.
///
/// Non-AuditEntry events (NodeStatusUpdate, MeshPeerChange) always
/// return false — they're public. Malformed JSON also returns false
/// (drop nothing extra).
fn audit_event_for_other_user(json: &str, connected_user: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return false;
    };
    if value.get("type").and_then(|v| v.as_str()) != Some("AuditEntry") {
        return false;
    }
    match value.get("user").and_then(|v| v.as_str()) {
        Some(u) => u != connected_user,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_non_audit_events() {
        let event = r#"{"type":"NodeStatusUpdate","uptime_secs":42,"services":[]}"#;
        assert!(!audit_event_for_other_user(event, "alice"));
    }

    #[test]
    fn passes_audit_event_for_self() {
        let event = r#"{"type":"AuditEntry","timestamp":"2026-05-09T00:00:00Z","user":"alice","action":"DataQuery"}"#;
        assert!(!audit_event_for_other_user(event, "alice"));
    }

    #[test]
    fn drops_audit_event_for_other_user() {
        let event = r#"{"type":"AuditEntry","timestamp":"2026-05-09T00:00:00Z","user":"bob","action":"DataQuery"}"#;
        assert!(audit_event_for_other_user(event, "alice"));
    }

    #[test]
    fn passes_malformed_json() {
        // Better to forward than to silently drop — malformed events
        // are a separate bug.
        assert!(!audit_event_for_other_user("not json", "alice"));
    }

    #[test]
    fn passes_audit_event_with_no_user() {
        let event =
            r#"{"type":"AuditEntry","timestamp":"2026-05-09T00:00:00Z","action":"DataQuery"}"#;
        assert!(!audit_event_for_other_user(event, "alice"));
    }
}
