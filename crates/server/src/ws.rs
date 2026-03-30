//! WebSocket handler for real-time dashboard updates.
//!
//! Clients connect with `?token=<auth_token>` and receive [`WsEvent`]s
//! as JSON text frames. The server subscribes to the [`NodeState::ws_broadcast`]
//! channel and forwards events to each connected client.

use std::sync::Arc;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
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
                    return (StatusCode::INTERNAL_SERVER_ERROR, "Session validation failed.")
                        .into_response();
                }
            },
            Err(_) => {
                return (StatusCode::INTERNAL_SERVER_ERROR, "Session service unavailable.")
                    .into_response();
            }
        }
    } else {
        t.clone()
    };

    // Enforce connection concurrency limit
    let current = state.ws_connections.load(std::sync::atomic::Ordering::Relaxed);
    if current >= MAX_WS_CONNECTIONS {
        return (StatusCode::SERVICE_UNAVAILABLE, "Too many WebSocket connections.")
            .into_response();
    }
    state
        .ws_connections
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let rx = state.ws_broadcast.subscribe();
    let ws_state = state.clone();
    ws.max_message_size(MAX_MESSAGE_SIZE)
        .on_upgrade(move |socket| handle_socket(socket, user_id, rx, ws_state))
}

async fn handle_socket(
    mut socket: WebSocket,
    user_id: String,
    mut rx: broadcast::Receiver<String>,
    state: Arc<NodeState>,
) {
    debug!(user_id = %user_id, "WebSocket connection established");

    loop {
        tokio::select! {
            // Forward broadcast events to the client
            result = rx.recv() => {
                match result {
                    Ok(json) => {
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
