//! Session management handlers (login / logout).

use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;

use crate::NodeState;
use crate::middleware::{ANONYMOUS_LOCAL_USER_ID, VERIFIED_SESSION_PREFIX};

#[derive(Deserialize)]
pub struct CreateSessionRequest {
    /// Compatibility hint retained for older clients. It is never used as an
    /// identity; loopback requests without a verifiable platform token become
    /// [`ANONYMOUS_LOCAL_USER_ID`].
    #[serde(default)]
    pub user_id: Option<String>,
    pub display_name: Option<String>,
    pub platform_role: Option<String>,
    /// MARC27 platform token (from `prism login` / the platform device flow).
    /// The node verifies it against the platform and mints the session for the
    /// VERIFIED identity — a caller can never just claim a user_id.
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
    /// Loopback caller without an independently verifiable account. This is
    /// local capability access only; submitted user_id is ignored.
    AnonymousLocal,
    /// Caller with a platform token — verify it, then mint for the verified
    /// identity.
    VerifyPlatformToken,
    /// Remote caller with no token — refused. This is the gate that keeps
    /// the port safe when it leaves localhost.
    Refuse,
}

fn session_gate(is_loopback: bool, platform_token: Option<&str>) -> SessionGate {
    if platform_token.is_some_and(|t| !t.trim().is_empty()) {
        SessionGate::VerifyPlatformToken
    } else if is_loopback {
        SessionGate::AnonymousLocal
    } else {
        SessionGate::Refuse
    }
}

/// POST /api/sessions — create a new session (login).
///
/// Loopback callers keep local capability access without an account, but the
/// submitted `user_id` is ignored. Any caller that needs an authenticated
/// account session must present a MARC27 platform token (device flow via
/// `prism login`); the node verifies it against the platform and mints the
/// session for the VERIFIED identity.
pub async fn create_session(
    State(state): State<Arc<NodeState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<CreateSessionRequest>,
) -> Result<Json<SessionResponse>, (StatusCode, Json<ErrorResponse>)> {
    let verified_user_id: String = match session_gate(
        addr.ip().is_loopback(),
        body.platform_token.as_deref(),
    ) {
        SessionGate::AnonymousLocal => ANONYMOUS_LOCAL_USER_ID.to_string(),
        SessionGate::Refuse => {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(ErrorResponse {
                    error: "Remote session creation requires a `platform_token` \
                            issued by the platform device flow; a bare user_id \
                            never establishes identity."
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
                        error: "This node is not linked to a hosted platform, so it \
                                cannot verify remote identities. Remote sessions are \
                                unavailable until the node owner authenticates."
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
        // Offline auth is a process-lifetime capability gate, not an identity
        // system. The server remembers this unguessable bearer so repeated
        // requests can resume their own chat while caller-chosen strings fail.
        let session_id = state.mint_offline_session_token();
        return Ok(Json(SessionResponse {
            session_id,
            user_id: ANONYMOUS_LOCAL_USER_ID.to_string(),
            expires_at: (chrono::Utc::now() + chrono::Duration::hours(24)).to_rfc3339(),
        }));
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

    let session_user_id = if verified_user_id == ANONYMOUS_LOCAL_USER_ID {
        ANONYMOUS_LOCAL_USER_ID.to_string()
    } else {
        format!("{VERIFIED_SESSION_PREFIX}{verified_user_id}")
    };

    let session = mgr
        .create_session(
            &session_user_id,
            body.display_name.as_deref(),
            // A submitted platform role is also only metadata. Do not attach
            // caller-asserted authority to an anonymous-local session.
            if verified_user_id == ANONYMOUS_LOCAL_USER_ID {
                None
            } else {
                body.platform_role.as_deref()
            },
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
        detail: Some(if verified_user_id == ANONYMOUS_LOCAL_USER_ID {
            "anonymous-local session; submitted identity ignored".into()
        } else {
            format!("platform-verified remote session from {addr}")
        }),
        outcome: prism_core::audit::AuditOutcome::Success,
    });

    Ok(Json(SessionResponse {
        session_id: session.id,
        // Return the verified external identity, not the internal marker.
        user_id: verified_user_id,
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
    use super::{CreateSessionRequest, SessionGate, create_session, session_gate};
    use crate::NodeState;
    use crate::middleware::ANONYMOUS_LOCAL_USER_ID;
    use axum::Json;

    #[test]
    fn loopback_without_token_is_anonymous_local() {
        assert_eq!(session_gate(true, None), SessionGate::AnonymousLocal);
    }

    #[test]
    fn a_platform_token_is_verified_even_from_loopback() {
        assert_eq!(
            session_gate(true, Some("tok")),
            SessionGate::VerifyPlatformToken
        );
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

    #[tokio::test]
    async fn offline_loopback_mints_a_fresh_anonymous_capability() {
        use axum::extract::{ConnectInfo, State};
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};

        let state = std::sync::Arc::new(NodeState::new("offline-node".into()));
        let request = || {
            create_session(
                State(state.clone()),
                ConnectInfo(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1234)),
                Json(CreateSessionRequest {
                    user_id: Some("caller-selected-user".into()),
                    display_name: None,
                    platform_role: Some("admin".into()),
                    platform_token: None,
                }),
            )
        };
        let first = match request().await {
            Ok(Json(response)) => response,
            Err((status, Json(error))) => {
                panic!("offline session mint failed ({status}): {}", error.error)
            }
        };
        let second = match request().await {
            Ok(Json(response)) => response,
            Err((status, Json(error))) => {
                panic!("offline session mint failed ({status}): {}", error.error)
            }
        };
        assert_ne!(first.session_id, second.session_id);
        assert!(state.is_valid_offline_session_token(&first.session_id));
        assert!(state.is_valid_offline_session_token(&second.session_id));
        assert!(!state.is_valid_offline_session_token("caller-chosen-token"));
        assert_eq!(first.user_id, ANONYMOUS_LOCAL_USER_ID);
        assert_eq!(second.user_id, ANONYMOUS_LOCAL_USER_ID);
    }

    #[tokio::test]
    async fn submitted_user_id_does_not_become_session_identity() {
        use axum::extract::{ConnectInfo, State};
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};

        let mut node = NodeState::new("test-node".into());
        let db = tempfile::NamedTempFile::new().unwrap();
        node.session_db_path = Some(db.path().to_path_buf());
        let state = std::sync::Arc::new(node);
        let response = create_session(
            State(state),
            ConnectInfo(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1234)),
            Json(CreateSessionRequest {
                user_id: Some("victim@example.com".into()),
                display_name: Some("local".into()),
                platform_role: Some("owner".into()),
                platform_token: None,
            }),
        )
        .await;
        let response = match response {
            Ok(Json(response)) => response,
            Err((_status, Json(error))) => panic!("session creation failed: {}", error.error),
        };

        assert_eq!(response.user_id, ANONYMOUS_LOCAL_USER_ID);
        let manager =
            prism_core::session::SessionManager::new(db.path(), chrono::Duration::hours(24))
                .unwrap();
        let stored = manager
            .validate_session(&response.session_id)
            .unwrap()
            .unwrap();
        assert_eq!(stored.user_id, ANONYMOUS_LOCAL_USER_ID);
    }
}
