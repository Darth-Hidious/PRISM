//! Session management handlers (login / logout).

use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;

use crate::NodeState;

#[derive(Deserialize)]
pub struct CreateSessionRequest {
    #[serde(default)]
    pub user_id: String,
    pub display_name: Option<String>,
    pub platform_role: Option<String>,
    /// MARC27 platform token (from `prism login` / the platform device flow).
    /// REQUIRED for non-loopback callers: the node verifies it against the
    /// platform and mints the session for the VERIFIED identity — a remote
    /// caller can never just claim a user_id.
    #[serde(default)]
    pub platform_token: Option<String>,
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

/// How a session-mint request must authenticate, decided by where the caller
/// connects from and what they presented. Pure so it's unit-testable.
#[derive(Debug, PartialEq, Eq)]
enum SessionGate {
    /// Loopback caller — the same-machine trust that has always existed
    /// (TUI, dashboard, local chat app). Claimed user_id is accepted.
    LocalTrust,
    /// Remote caller with a platform token — verify it, mint for the
    /// verified identity.
    VerifyPlatformToken,
    /// Remote caller with no token — refused. This is the gate that keeps
    /// the port safe when it leaves localhost.
    Refuse,
}

fn session_gate(is_loopback: bool, platform_token: Option<&str>) -> SessionGate {
    if is_loopback {
        SessionGate::LocalTrust
    } else if platform_token.is_some_and(|t| !t.trim().is_empty()) {
        SessionGate::VerifyPlatformToken
    } else {
        SessionGate::Refuse
    }
}

/// POST /api/sessions — create a new session (login).
///
/// Loopback callers keep the same-machine trust that always existed. Any
/// OTHER caller must present a MARC27 platform token (device flow via
/// `prism login`); the node verifies it against the platform and mints the
/// session for the VERIFIED identity — no remote caller can mint a session
/// by merely claiming a user_id.
pub async fn create_session(
    State(state): State<Arc<NodeState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<CreateSessionRequest>,
) -> Result<Json<SessionResponse>, (StatusCode, Json<ErrorResponse>)> {
    let verified_user_id: String = match session_gate(
        addr.ip().is_loopback(),
        body.platform_token.as_deref(),
    ) {
        SessionGate::LocalTrust => body.user_id.clone(),
        SessionGate::Refuse => {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(ErrorResponse {
                    error: "Remote session creation requires a `platform_token` \
                            (obtain one with `prism login` — the platform device \
                            flow); a bare user_id is only trusted from localhost."
                        .into(),
                }),
            ));
        }
        SessionGate::VerifyPlatformToken => {
            let token = body.platform_token.clone().unwrap_or_default();
            // The node's own platform link supplies the API base; without one
            // this node cannot verify anybody — refuse honestly.
            let Some(api_base) = state
                .platform_client
                .as_ref()
                .map(|c| c.base_url().to_string())
            else {
                return Err((
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(ErrorResponse {
                        error: "This node is not linked to a MARC27 platform, so it \
                                cannot verify remote identities. Remote sessions are \
                                unavailable until the node owner runs `prism login`."
                            .into(),
                    }),
                ));
            };
            let verifier = prism_client::PlatformClient::new(&api_base).with_token(&token);
            match verifier.fetch_current_user().await {
                Ok(user) => {
                    tracing::info!(user_id = %user.id, remote = %addr, "remote session platform-verified");
                    user.id
                }
                Err(e) => {
                    tracing::warn!(remote = %addr, error = %e, "remote session token verification failed");
                    return Err((
                        StatusCode::UNAUTHORIZED,
                        Json(ErrorResponse {
                            error: "platform_token verification failed — the platform \
                                    did not recognise this token."
                                .into(),
                        }),
                    ));
                }
            }
        }
    };

    // Input validation
    if verified_user_id.is_empty() || verified_user_id.len() > 256 {
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

    let mgr = prism_core::session::SessionManager::new(db_path, chrono::Duration::hours(24))
        .map_err(|e| {
            tracing::error!(error = %e, "failed to open session database");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Internal server error.".into(),
                }),
            )
        })?;

    let session = mgr
        .create_session(
            &verified_user_id,
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

    // Audit the login (with how the identity was established)
    state.audit_and_broadcast(&prism_core::audit::AuditEntry {
        id: 0,
        timestamp: chrono::Utc::now(),
        user_id: verified_user_id.clone(),
        action: prism_core::audit::AuditAction::UserLogin,
        target: "session".into(),
        detail: Some(if addr.ip().is_loopback() {
            "loopback local trust".into()
        } else {
            format!("platform-verified remote session from {addr}")
        }),
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

    let mgr = prism_core::session::SessionManager::new(db_path, chrono::Duration::hours(24))
        .map_err(|e| {
            tracing::error!(error = %e, "failed to open session database");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Internal server error.".into(),
                }),
            )
        })?;

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

#[cfg(test)]
mod tests {
    use super::{SessionGate, session_gate};

    #[test]
    fn loopback_keeps_local_trust_with_or_without_token() {
        assert_eq!(session_gate(true, None), SessionGate::LocalTrust);
        assert_eq!(session_gate(true, Some("tok")), SessionGate::LocalTrust);
    }

    #[test]
    fn remote_without_token_is_refused() {
        // The gate that keeps the port safe when it leaves localhost: a
        // remote caller can never mint a session by claiming a user_id.
        assert_eq!(session_gate(false, None), SessionGate::Refuse);
        assert_eq!(session_gate(false, Some("")), SessionGate::Refuse);
        assert_eq!(session_gate(false, Some("   ")), SessionGate::Refuse);
    }

    #[test]
    fn remote_with_token_must_verify() {
        assert_eq!(
            session_gate(false, Some("m27_realtoken")),
            SessionGate::VerifyPlatformToken
        );
    }
}
