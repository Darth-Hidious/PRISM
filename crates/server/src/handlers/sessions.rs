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
    /// Access token issued by the identity provider selected when this node
    /// was configured. The node verifies it through that exact adapter and
    /// mints the session for the verified identity; a caller can never just
    /// claim a `user_id`.
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

/// Apply claims that affect PRISM authorization only after provider identity
/// verification has completed. MARC27 roles continue to arrive through its
/// organization-membership reconciliation; Supabase carries a signed role
/// claim on each access token and must update or revoke that subject here.
fn sync_verified_provider_role(
    state: &NodeState,
    identity: &prism_client::auth::VerifiedIdentity,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    match identity.provider {
        prism_client::auth::IdentityProviderAdapter::Marc27 => Ok(()),
        // Supabase and Mirdyne share this arm because they are the SAME shape
        // of provider: an issuer-scoped JWT carrying a role claim. They stay
        // distinct identities regardless — `provider_scope` is the verified
        // issuer, and the principal is derived from it, so merging the code
        // never merges the accounts.
        prism_client::auth::IdentityProviderAdapter::Supabase
        | prism_client::auth::IdentityProviderAdapter::Mirdyne => {
            let Some(rbac_db_path) = state.rbac_db_path.as_deref() else {
                return Err((
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(ErrorResponse {
                        error: "Supabase role verification is unavailable because this node's \
                                authorization store is not configured."
                            .into(),
                    }),
                ));
            };
            let Some(project_scope) = identity.provider_scope.as_deref() else {
                tracing::error!(
                    provider = identity.provider.as_str(),
                    "verified identity is missing its provider scope"
                );
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse {
                        error: "Verified identity could not be mapped into PRISM authorization."
                            .into(),
                    }),
                ));
            };
            let engine = prism_core::rbac::RbacEngine::new(rbac_db_path).map_err(|error| {
                tracing::error!(error = %error, "failed to open RBAC database for verified identity");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse {
                        error: "Verified identity could not be mapped into PRISM authorization."
                            .into(),
                    }),
                )
            })?;
            let role_sync = prism_node::provider_roles::sync_supabase_login_role(
                &engine,
                project_scope,
                &identity.subject_id,
                identity.role_claim.as_deref().unwrap_or(""),
            )
            .map_err(|error| {
                tracing::error!(error = %error, "failed to synchronize verified provider role");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse {
                        error: "Verified identity could not be mapped into PRISM authorization."
                            .into(),
                    }),
                )
            })?;
            if role_sync.principal_id != identity.principal_id {
                tracing::error!(
                    provider = identity.provider.as_str(),
                    "provider role mapping changed the verified principal"
                );
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse {
                        error: "Verified identity could not be mapped into PRISM authorization."
                            .into(),
                    }),
                ));
            }
            Ok(())
        }
    }
}

/// POST /api/sessions — create a new session (login).
///
/// Loopback callers keep local capability access without an account, but the
/// submitted `user_id` is ignored. Any caller that needs an authenticated
/// account session must present an access token from the node's explicitly
/// configured identity provider. Unknown or missing verifier configuration
/// fails closed before a session is written.
pub async fn create_session(
    State(state): State<Arc<NodeState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<CreateSessionRequest>,
) -> Result<Json<SessionResponse>, (StatusCode, Json<ErrorResponse>)> {
    let (verified_user_id, verified_provider) =
        match session_gate(addr.ip().is_loopback(), body.platform_token.as_deref()) {
            SessionGate::AnonymousLocal => (ANONYMOUS_LOCAL_USER_ID.to_string(), None),
            SessionGate::Refuse => {
                return Err((
                    StatusCode::UNAUTHORIZED,
                    Json(ErrorResponse {
                        error: "Remote session creation requires a `platform_token` \
                            issued by the configured identity provider; a bare user_id \
                            never establishes identity."
                            .into(),
                    }),
                ));
            }
            SessionGate::VerifyPlatformToken => {
                let token = body.platform_token.clone().unwrap_or_default();
                // Provider selection is never inferred from a platform URL. A
                // missing verifier cannot silently fall back to MARC27.
                let Some(verifier) = state.identity_verifier.as_ref() else {
                    return Err((
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(ErrorResponse {
                            error: "Identity verification is not configured for this node. \
                                Remote sessions are unavailable until the node owner \
                                configures a recognized identity provider."
                                .into(),
                        }),
                    ));
                };
                match verifier.verify_access_token(&token).await {
                    Ok(identity) => {
                        sync_verified_provider_role(&state, &identity)?;
                        tracing::info!(
                            user_id = %identity.principal_id,
                            provider = identity.provider.as_str(),
                            remote = %addr,
                            "remote session identity-provider-verified"
                        );
                        (identity.principal_id, Some(identity.provider))
                    }
                    Err(_) => {
                        // Do not log the provider error body: an untrusted remote
                        // verifier could reflect the bearer token in its response.
                        tracing::warn!(
                            remote = %addr,
                            provider = verifier.provider().as_str(),
                            "remote session token verification failed"
                        );
                        return Err((
                            StatusCode::UNAUTHORIZED,
                            Json(ErrorResponse {
                                error: "platform_token verification failed — the configured \
                                    identity provider did not accept this token."
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
        // Offline auth is a durable local capability gate, not an identity
        // system. The unguessable bearer survives restart until its persisted
        // expiry and scopes repeated chat requests to the same solo caller.
        let capability = state.mint_offline_session().map_err(|error| {
            tracing::error!(error = %error, "failed to persist standalone session");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to create standalone session.".into(),
                }),
            )
        })?;
        return Ok(Json(SessionResponse {
            session_id: capability.token,
            user_id: ANONYMOUS_LOCAL_USER_ID.to_string(),
            expires_at: capability.expires_at.to_rfc3339(),
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
            format!(
                "{}-verified remote session from {addr}",
                verified_provider
                    .map(prism_client::auth::IdentityProviderAdapter::as_str)
                    .unwrap_or("identity-provider")
            )
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
        state
            .revoke_offline_session_token(&session_id)
            .map_err(|error| {
                tracing::error!(error = %error, "failed to revoke standalone session");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse {
                        error: "Failed to revoke standalone session.".into(),
                    }),
                )
            })?;
        return Ok(Json(serde_json::json!({ "status": "ok" })));
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
    use super::{CreateSessionRequest, SessionGate, create_session, destroy_session, session_gate};
    use crate::NodeState;
    use crate::middleware::ANONYMOUS_LOCAL_USER_ID;
    use axum::Json;
    use axum::Router;
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::get;
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use ed25519_dalek::{Signer, SigningKey};

    const TEST_SUPABASE_ANON_KEY: &str = "test-supabase-anon-key";
    const TEST_SUPABASE_KID: &str = "test-signing-key";

    async fn spawn_supabase_jwks(signing_key: &SigningKey) -> String {
        let jwks = serde_json::json!({
            "keys": [{
                "kty": "OKP",
                "crv": "Ed25519",
                "x": URL_SAFE_NO_PAD.encode(signing_key.verifying_key().as_bytes()),
                "kid": TEST_SUPABASE_KID,
                "alg": "EdDSA",
                "use": "sig"
            }]
        });
        // Bind BEFORE building the router: the discovery document has to name
        // its own issuer, and the issuer is not known until the ephemeral port
        // is assigned.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind Supabase JWKS stub");
        let base = format!("http://{}", listener.local_addr().unwrap());

        // Verification no longer assumes where an issuer keeps its keys — it
        // asks, via OIDC discovery, and refuses if the document's `issuer`
        // does not equal the issuer it was fetched for (OIDC Discovery §4.3).
        // A stub that serves only JWKS therefore fails verification outright,
        // which is what broke these tests: the production change was right and
        // this stub had not caught up.
        let discovery = serde_json::json!({
            "issuer": format!("{base}/auth/v1"),
            "jwks_uri": format!("{base}/auth/v1/.well-known/jwks.json"),
        });

        let app = Router::new()
            .route(
                "/auth/v1/.well-known/openid-configuration",
                get(move || {
                    let discovery = discovery.clone();
                    async move { (StatusCode::OK, Json(discovery)) }
                }),
            )
            .route(
                "/auth/v1/.well-known/jwks.json",
                get(move |headers: HeaderMap| {
                    let jwks = jwks.clone();
                    async move {
                        let authorized =
                            headers.get("apikey").and_then(|value| value.to_str().ok())
                                == Some(TEST_SUPABASE_ANON_KEY);
                        if authorized {
                            (StatusCode::OK, Json(jwks))
                        } else {
                            (
                                StatusCode::UNAUTHORIZED,
                                Json(serde_json::json!({ "error": "missing anon key" })),
                            )
                        }
                    }
                }),
            );
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        base
    }

    fn signed_supabase_token(
        signing_key: &SigningKey,
        issuer: &str,
        subject: &str,
        role: &str,
    ) -> String {
        let header = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&serde_json::json!({
                "alg": "EdDSA",
                "kid": TEST_SUPABASE_KID,
                "typ": "JWT"
            }))
            .expect("serialize JWT header"),
        );
        let payload = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&serde_json::json!({
                "sub": subject,
                "exp": chrono::Utc::now().timestamp() + 3600,
                "iss": issuer,
                "aud": "authenticated",
                "role": role
            }))
            .expect("serialize JWT payload"),
        );
        let signing_input = format!("{header}.{payload}");
        let signature = signing_key.sign(signing_input.as_bytes());
        format!(
            "{signing_input}.{}",
            URL_SAFE_NO_PAD.encode(signature.to_bytes())
        )
    }

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
    async fn token_with_no_configured_identity_verifier_fails_closed() {
        use axum::extract::{ConnectInfo, State};
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};

        let mut node = NodeState::new("unconfigured-node".into());
        let db = tempfile::NamedTempFile::new().unwrap();
        node.session_db_path = Some(db.path().to_path_buf());
        let response = create_session(
            State(std::sync::Arc::new(node)),
            ConnectInfo(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1234)),
            Json(CreateSessionRequest {
                user_id: Some("caller-selected-user".into()),
                display_name: None,
                platform_role: None,
                platform_token: Some("must-not-leak".into()),
            }),
        )
        .await;

        let (status, Json(error)) = match response {
            Err(error) => error,
            Ok(_) => panic!("missing verifier accepted a token"),
        };
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(error.error.contains("not configured"), "{}", error.error);
        assert!(!error.error.contains("must-not-leak"), "{}", error.error);
    }

    #[tokio::test]
    async fn verified_supabase_token_mints_for_project_scoped_principal() {
        use axum::extract::{ConnectInfo, State};
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};

        let signing_key = SigningKey::from_bytes(&[7_u8; 32]);
        let project_url = spawn_supabase_jwks(&signing_key).await;
        let issuer = format!("{project_url}/auth/v1");
        let subject = "supabase-user-123";
        let token = signed_supabase_token(&signing_key, &issuer, subject, "authenticated");
        let expected_principal =
            prism_client::auth::canonical_supabase_principal(&issuer, subject).unwrap();

        let mut node = NodeState::new("supabase-node".into());
        let session_db = tempfile::NamedTempFile::new().unwrap();
        let rbac_db = tempfile::NamedTempFile::new().unwrap();
        node.session_db_path = Some(session_db.path().to_path_buf());
        node.rbac_db_path = Some(rbac_db.path().to_path_buf());
        node.identity_verifier = Some(
            prism_client::auth::IdentityVerifierConfig::new(
                Some(prism_client::auth::SUPABASE_IDENTITY_PROVIDER),
                &project_url,
                Some(TEST_SUPABASE_ANON_KEY),
            )
            .unwrap(),
        );
        let response = create_session(
            State(std::sync::Arc::new(node)),
            ConnectInfo(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1234)),
            Json(CreateSessionRequest {
                user_id: Some("attacker-selected-user".into()),
                display_name: None,
                platform_role: Some("node_admin".into()),
                platform_token: Some(token),
            }),
        )
        .await;
        let Json(response) = match response {
            Ok(response) => response,
            Err((status, Json(error))) => {
                panic!(
                    "verified Supabase token was refused ({status}): {}",
                    error.error
                )
            }
        };

        assert_eq!(response.user_id, expected_principal);
        let stored = prism_core::session::SessionManager::new(
            session_db.path(),
            chrono::Duration::hours(24),
        )
        .unwrap()
        .validate_session(&response.session_id)
        .unwrap()
        .unwrap();
        assert_eq!(
            stored.user_id,
            format!(
                "{}{}",
                crate::middleware::VERIFIED_SESSION_PREFIX,
                expected_principal
            )
        );
        let engine = prism_core::rbac::RbacEngine::new(rbac_db.path()).unwrap();
        assert_eq!(
            engine
                .get_external_role(
                    prism_node::provider_roles::SUPABASE_ROLE_PROVIDER,
                    &expected_principal,
                )
                .unwrap(),
            Some(prism_core::rbac::LocalRole::Viewer)
        );
        assert!(
            !engine
                .check_permission(
                    &expected_principal,
                    prism_core::rbac::Permission::ManageNode,
                )
                .unwrap()
        );
    }

    #[tokio::test]
    async fn unknown_supabase_role_revokes_stale_privilege_before_session_use() {
        use axum::extract::{ConnectInfo, State};
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};

        let signing_key = SigningKey::from_bytes(&[7_u8; 32]);
        let project_url = spawn_supabase_jwks(&signing_key).await;
        let issuer = format!("{project_url}/auth/v1");
        let subject = "supabase-user-with-stale-role";
        let token = signed_supabase_token(&signing_key, &issuer, subject, "service_role");
        let principal = prism_client::auth::canonical_supabase_principal(&issuer, subject).unwrap();

        let session_db = tempfile::NamedTempFile::new().unwrap();
        let rbac_db = tempfile::NamedTempFile::new().unwrap();
        let engine = prism_core::rbac::RbacEngine::new(rbac_db.path()).unwrap();
        engine
            .assign_external_role(
                prism_node::provider_roles::SUPABASE_ROLE_PROVIDER,
                &principal,
                &principal,
                prism_core::rbac::LocalRole::NodeAdmin,
            )
            .unwrap();

        let mut node = NodeState::new("supabase-node".into());
        node.session_db_path = Some(session_db.path().to_path_buf());
        node.rbac_db_path = Some(rbac_db.path().to_path_buf());
        node.identity_verifier = Some(
            prism_client::auth::IdentityVerifierConfig::new(
                Some(prism_client::auth::SUPABASE_IDENTITY_PROVIDER),
                &project_url,
                Some(TEST_SUPABASE_ANON_KEY),
            )
            .unwrap(),
        );
        let response = create_session(
            State(std::sync::Arc::new(node)),
            ConnectInfo(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1234)),
            Json(CreateSessionRequest {
                user_id: None,
                display_name: None,
                platform_role: Some("node_admin".into()),
                platform_token: Some(token),
            }),
        )
        .await;

        let Json(response) = match response {
            Ok(response) => response,
            Err((status, Json(error))) => {
                panic!(
                    "verified token with a non-privileged role was refused ({status}): {}",
                    error.error
                )
            }
        };
        assert_eq!(response.user_id, principal);
        assert_eq!(
            engine
                .get_external_role(
                    prism_node::provider_roles::SUPABASE_ROLE_PROVIDER,
                    &principal,
                )
                .unwrap(),
            None
        );
        assert_eq!(engine.get_role(&principal).unwrap(), None);
        assert!(
            !engine
                .check_permission(&principal, prism_core::rbac::Permission::ManageNode)
                .unwrap()
        );
    }

    #[tokio::test]
    async fn verified_supabase_token_without_rbac_store_fails_closed() {
        use axum::extract::{ConnectInfo, State};
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};

        let signing_key = SigningKey::from_bytes(&[7_u8; 32]);
        let project_url = spawn_supabase_jwks(&signing_key).await;
        let issuer = format!("{project_url}/auth/v1");
        let token =
            signed_supabase_token(&signing_key, &issuer, "supabase-user-123", "authenticated");

        let mut node = NodeState::new("supabase-node".into());
        let session_db = tempfile::NamedTempFile::new().unwrap();
        node.session_db_path = Some(session_db.path().to_path_buf());
        node.identity_verifier = Some(
            prism_client::auth::IdentityVerifierConfig::new(
                Some(prism_client::auth::SUPABASE_IDENTITY_PROVIDER),
                &project_url,
                Some(TEST_SUPABASE_ANON_KEY),
            )
            .unwrap(),
        );
        let response = create_session(
            State(std::sync::Arc::new(node)),
            ConnectInfo(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1234)),
            Json(CreateSessionRequest {
                user_id: None,
                display_name: None,
                platform_role: None,
                platform_token: Some(token),
            }),
        )
        .await;

        let (status, Json(error)) = match response {
            Err(error) => error,
            Ok(_) => panic!("Supabase session minted without an authorization store"),
        };
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            error
                .error
                .contains("authorization store is not configured"),
            "{}",
            error.error
        );
    }

    #[tokio::test]
    async fn wrong_supabase_signature_is_refused_by_real_session_path() {
        use axum::extract::{ConnectInfo, State};
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};

        let advertised_key = SigningKey::from_bytes(&[7_u8; 32]);
        let wrong_key = SigningKey::from_bytes(&[8_u8; 32]);
        let project_url = spawn_supabase_jwks(&advertised_key).await;
        let issuer = format!("{project_url}/auth/v1");
        let subject = "supabase-user-123";
        let token = signed_supabase_token(&wrong_key, &issuer, subject, "authenticated");
        let principal = prism_client::auth::canonical_supabase_principal(&issuer, subject).unwrap();

        let mut node = NodeState::new("supabase-node".into());
        let session_db = tempfile::NamedTempFile::new().unwrap();
        let rbac_db = tempfile::NamedTempFile::new().unwrap();
        let engine = prism_core::rbac::RbacEngine::new(rbac_db.path()).unwrap();
        engine
            .assign_external_role(
                prism_node::provider_roles::SUPABASE_ROLE_PROVIDER,
                &principal,
                &principal,
                prism_core::rbac::LocalRole::NodeAdmin,
            )
            .unwrap();
        node.session_db_path = Some(session_db.path().to_path_buf());
        node.rbac_db_path = Some(rbac_db.path().to_path_buf());
        node.identity_verifier = Some(
            prism_client::auth::IdentityVerifierConfig::new(
                Some(prism_client::auth::SUPABASE_IDENTITY_PROVIDER),
                &project_url,
                Some(TEST_SUPABASE_ANON_KEY),
            )
            .unwrap(),
        );
        let response = create_session(
            State(std::sync::Arc::new(node)),
            ConnectInfo(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1234)),
            Json(CreateSessionRequest {
                user_id: None,
                display_name: None,
                platform_role: None,
                platform_token: Some(token.clone()),
            }),
        )
        .await;

        let (status, Json(error)) = match response {
            Err(error) => error,
            Ok(_) => panic!("wrong signature minted a session"),
        };
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(
            error.error.contains("verification failed"),
            "{}",
            error.error
        );
        assert!(
            !error.error.contains(&token),
            "token leaked: {}",
            error.error
        );
        assert!(
            !error.error.contains(TEST_SUPABASE_ANON_KEY),
            "anon key leaked: {}",
            error.error
        );
        assert_eq!(
            engine
                .get_external_role(
                    prism_node::provider_roles::SUPABASE_ROLE_PROVIDER,
                    &principal,
                )
                .unwrap(),
            Some(prism_core::rbac::LocalRole::NodeAdmin),
            "an unverified token must not mutate role assignments"
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
    async fn offline_logout_revokes_session_capability() {
        use axum::extract::State;

        let state = std::sync::Arc::new(NodeState::new("offline-node".into()));
        let capability = state
            .mint_offline_session()
            .expect("mint standalone session");
        assert!(state.is_valid_offline_session_token(&capability.token));

        if let Err((status, Json(error))) = destroy_session(
            State(state.clone()),
            Some(axum::Extension(crate::middleware::SessionToken(
                capability.token.clone(),
            ))),
        )
        .await
        {
            panic!("standalone logout failed ({status}): {}", error.error);
        }
        assert!(
            !state.is_valid_offline_session_token(&capability.token),
            "logout must revoke the standalone bearer"
        );
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
