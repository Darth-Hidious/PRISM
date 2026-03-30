//! Session management handlers (login / logout).

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::NodeState;

#[derive(Deserialize)]
pub struct CreateSessionRequest {
    pub user_id: String,
    pub display_name: Option<String>,
    pub platform_role: Option<String>,
}

#[derive(Serialize)]
pub struct SessionResponse {
    pub session_id: String,
    pub user_id: String,
    pub expires_at: String,
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

/// POST /api/sessions — create a new session (login).
///
/// For now, accepts a user_id directly. In production, this will be
/// gated behind platform token verification (device code flow).
pub async fn create_session(
    State(state): State<Arc<NodeState>>,
    Json(body): Json<CreateSessionRequest>,
) -> Result<Json<SessionResponse>, (StatusCode, Json<ErrorResponse>)> {
    // Input validation
    if body.user_id.is_empty() || body.user_id.len() > 256 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "user_id must be 1-256 characters.".into(),
            }),
        ));
    }

    let Some(ref db_path) = state.session_db_path else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse {
                error: "Session management not configured.".into(),
            }),
        ));
    };

    let mgr =
        prism_core::session::SessionManager::new(db_path, chrono::Duration::hours(24)).map_err(
            |e| {
                tracing::error!(error = %e, "failed to open session database");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse {
                        error: "Internal server error.".into(),
                    }),
                )
            },
        )?;

    let session = mgr
        .create_session(
            &body.user_id,
            body.display_name.as_deref(),
            body.platform_role.as_deref(),
        )
        .map_err(|e| {
            tracing::error!(error = %e, "failed to create session");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Internal server error.".into(),
                }),
            )
        })?;

    // Audit the login
    state.audit_and_broadcast(&prism_core::audit::AuditEntry {
        id: 0,
        timestamp: chrono::Utc::now(),
        user_id: body.user_id.clone(),
        action: prism_core::audit::AuditAction::UserLogin,
        target: "session".into(),
        detail: None,
        outcome: prism_core::audit::AuditOutcome::Success,
    });

    Ok(Json(SessionResponse {
        session_id: session.id,
        user_id: session.user_id,
        expires_at: session.expires_at.to_rfc3339(),
    }))
}

/// DELETE /api/sessions — destroy the current session (logout).
pub async fn destroy_session(
    State(state): State<Arc<NodeState>>,
    token: Option<axum::Extension<crate::middleware::SessionToken>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    let Some(axum::Extension(crate::middleware::SessionToken(session_id))) = token else {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "No session token provided.".into(),
            }),
        ));
    };

    let Some(ref db_path) = state.session_db_path else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse {
                error: "Session management not configured.".into(),
            }),
        ));
    };

    let mgr =
        prism_core::session::SessionManager::new(db_path, chrono::Duration::hours(24)).map_err(
            |e| {
                tracing::error!(error = %e, "failed to open session database");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse {
                        error: "Internal server error.".into(),
                    }),
                )
            },
        )?;

    mgr.destroy_session(&session_id).map_err(|e| {
        tracing::error!(error = %e, "failed to destroy session");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Internal server error.".into(),
            }),
        )
    })?;

    Ok(Json(serde_json::json!({ "status": "ok" })))
}
