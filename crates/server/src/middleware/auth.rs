//! Session validation middleware for the PRISM node HTTP API.
//!
//! Extracts a session token from (in priority order):
//!   1. `Authorization: Bearer <token>` header
//!   2. `X-Session-Token` header
//!   3. `?token=` query parameter
//!
//! Validates the token against the [`SessionManager`] SQLite database.
//! If the session is valid and not expired, inserts [`AuthenticatedUser`]
//! into request extensions.

use std::sync::Arc;

use axum::{
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde::Serialize;

use crate::NodeState;

/// Newtype inserted into request extensions after successful token extraction.
#[derive(Debug, Clone)]
pub struct SessionToken(pub String);

/// Stable principal used when the node has no session database. This is an
/// honest local capability identity, not a user-selected or token-selected id.
pub const ANONYMOUS_LOCAL_USER_ID: &str = "anonymous-local";
/// Internal marker written only after a platform token has been verified.
/// Legacy/raw session rows are therefore never upgraded into authenticated
/// identities by merely existing in the session database.
pub const VERIFIED_SESSION_PREFIX: &str = "authenticated:";

/// How the node established the caller identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallerIdentity {
    /// The request is local, but no authenticated account identity is known.
    AnonymousLocal,
    /// The session database validated this account identity.
    Authenticated { user_id: String },
}

/// Newtype inserted into request extensions representing the caller.
#[derive(Debug, Clone)]
pub struct AuthenticatedUser {
    /// Stable principal used by local ownership and audit scoping. For an
    /// anonymous-local caller this is always [`ANONYMOUS_LOCAL_USER_ID`].
    pub user_id: String,
    identity: CallerIdentity,
}

impl AuthenticatedUser {
    pub fn anonymous_local() -> Self {
        Self {
            user_id: ANONYMOUS_LOCAL_USER_ID.to_string(),
            identity: CallerIdentity::AnonymousLocal,
        }
    }

    pub(crate) fn from_session_user_id(session_user_id: String) -> Self {
        let Some(user_id) = session_user_id.strip_prefix(VERIFIED_SESSION_PREFIX) else {
            return Self::anonymous_local();
        };
        if user_id.is_empty() || user_id == ANONYMOUS_LOCAL_USER_ID {
            return Self::anonymous_local();
        }
        Self {
            identity: CallerIdentity::Authenticated {
                user_id: user_id.to_string(),
            },
            user_id: user_id.to_string(),
        }
    }

    /// True only when a session database established the account identity.
    pub fn is_authenticated(&self) -> bool {
        matches!(self.identity, CallerIdentity::Authenticated { .. })
    }

    pub fn is_anonymous_local(&self) -> bool {
        matches!(self.identity, CallerIdentity::AnonymousLocal)
    }

    /// Actor classification derived by the server, never from request JSON.
    pub fn provenance_actor(&self) -> prism_provenance::Actor {
        if self.is_anonymous_local() {
            prism_provenance::Actor::AnonymousLocal
        } else {
            prism_provenance::Actor::AuthenticatedUser
        }
    }
}

#[derive(Serialize)]
struct ErrorBody {
    error: &'static str,
    message: &'static str,
}

/// Axum middleware that extracts and validates a session token.
///
/// When `session_db_path` is configured, validates the token against the
/// [`SessionManager`]. When not configured (standalone/local mode), accepts
/// the transport token only as a local capability gate and records the caller
/// as [`ANONYMOUS_LOCAL_USER_ID`]. The token is never an identity.
pub async fn auth_layer(
    State(state): State<Arc<NodeState>>,
    mut req: Request,
    next: Next,
) -> Response {
    let token = extract_token(req.headers(), req.uri().query());

    let Some(t) = token.filter(|t| !t.is_empty()) else {
        let body = ErrorBody {
            error: "unauthorized",
            message: "Missing or empty session token. Provide via Authorization: Bearer <token>, X-Session-Token header, or ?token= query parameter.",
        };
        return (StatusCode::UNAUTHORIZED, axum::Json(body)).into_response();
    };

    // Try to validate against SessionManager if configured.
    if let Some(ref db_path) = state.session_db_path {
        match prism_core::session::SessionManager::new(db_path, chrono::Duration::hours(24)) {
            Ok(mgr) => match mgr.validate_session(&t) {
                Ok(Some(session)) => {
                    let user = AuthenticatedUser::from_session_user_id(session.user_id);
                    req.extensions_mut().insert(SessionToken(t));
                    req.extensions_mut().insert(user);
                    return next.run(req).await;
                }
                Ok(None) => {
                    let body = ErrorBody {
                        error: "unauthorized",
                        message: "Session expired or invalid.",
                    };
                    return (StatusCode::UNAUTHORIZED, axum::Json(body)).into_response();
                }
                Err(e) => {
                    tracing::error!(error = %e, "session validation failed");
                    let body = ErrorBody {
                        error: "internal_error",
                        message: "Session validation failed.",
                    };
                    return (StatusCode::INTERNAL_SERVER_ERROR, axum::Json(body)).into_response();
                }
            },
            Err(e) => {
                tracing::error!(error = %e, "failed to open session database");
                let body = ErrorBody {
                    error: "internal_error",
                    message: "Session service unavailable.",
                };
                return (StatusCode::INTERNAL_SERVER_ERROR, axum::Json(body)).into_response();
            }
        }
    }

    // Standalone/local mode: the token proves only that the caller reached the
    // local API seam. It is not an identity and cannot grant platform-owner
    // authority.
    tracing::debug!("session DB not configured, treating caller as anonymous-local");
    let user = AuthenticatedUser::anonymous_local();
    req.extensions_mut().insert(SessionToken(t));
    req.extensions_mut().insert(user);
    next.run(req).await
}

/// Try to pull a token string from the request in priority order.
fn extract_token(headers: &HeaderMap, query: Option<&str>) -> Option<String> {
    // 1. Authorization: Bearer <token>
    if let Some(auth) = headers.get("authorization")
        && let Ok(val) = auth.to_str()
    {
        let val = val.trim();
        if let Some(token) = val.strip_prefix("Bearer ") {
            let token = token.trim();
            if !token.is_empty() {
                return Some(token.to_owned());
            }
        }
    }

    // 2. X-Session-Token header
    if let Some(hdr) = headers.get("x-session-token")
        && let Ok(val) = hdr.to_str()
    {
        let val = val.trim();
        if !val.is_empty() {
            return Some(val.to_owned());
        }
    }

    // 3. ?token=<value> query parameter
    if let Some(qs) = query {
        for pair in qs.split('&') {
            if let Some(val) = pair.strip_prefix("token=") {
                let val = val.trim();
                if !val.is_empty() {
                    return Some(val.to_owned());
                }
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn bearer_token() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("Bearer abc123"));
        assert_eq!(extract_token(&headers, None), Some("abc123".into()));
    }

    #[test]
    fn x_session_token() {
        let mut headers = HeaderMap::new();
        headers.insert("x-session-token", HeaderValue::from_static("sess_xyz"));
        assert_eq!(extract_token(&headers, None), Some("sess_xyz".into()));
    }

    #[test]
    fn query_param() {
        let headers = HeaderMap::new();
        assert_eq!(
            extract_token(&headers, Some("token=qt_999&other=1")),
            Some("qt_999".into())
        );
    }

    #[test]
    fn bearer_takes_priority() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer bearer_tok"),
        );
        headers.insert("x-session-token", HeaderValue::from_static("header_tok"));
        assert_eq!(
            extract_token(&headers, Some("token=query_tok")),
            Some("bearer_tok".into())
        );
    }

    #[test]
    fn none_when_missing() {
        let headers = HeaderMap::new();
        assert_eq!(extract_token(&headers, None), None);
    }

    #[test]
    fn standalone_token_never_becomes_identity() {
        let user = AuthenticatedUser::anonymous_local();
        assert!(user.is_anonymous_local());
        assert!(!user.is_authenticated());
        assert_eq!(user.user_id, ANONYMOUS_LOCAL_USER_ID);
        assert_ne!(user.user_id, "attacker-selected-token");
        assert_eq!(
            user.provenance_actor(),
            prism_provenance::Actor::AnonymousLocal
        );
    }

    #[test]
    fn validated_session_identity_is_distinct_from_anonymous_local() {
        let user =
            AuthenticatedUser::from_session_user_id(format!("{VERIFIED_SESSION_PREFIX}owner-123"));
        assert!(user.is_authenticated());
        assert!(!user.is_anonymous_local());
        assert_eq!(user.user_id, "owner-123");
        assert_eq!(
            user.provenance_actor(),
            prism_provenance::Actor::AuthenticatedUser
        );
    }
}
