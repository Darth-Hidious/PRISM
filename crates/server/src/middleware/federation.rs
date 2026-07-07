//! Cross-org federation middleware (Bug #33 / F1c4).
//!
//! Sits in the request pipeline alongside [`super::auth::auth_layer`].
//! Decision tree:
//!
//! - If the incoming request has an `X-PRISM-Federation` header, it is
//!   treated as a cross-org peer call. The middleware decodes the
//!   [`CrossOrgRequest`] envelope from the header, looks up the
//!   required role for `request.action` via
//!   [`ActionRoleTable::defaults`], pulls the cached MARC27 platform
//!   pubkey from `~/.prism/federation/platform_pubkey.bin`, and calls
//!   [`prism_mesh::federation::verify_peer`]. On success, the verified
//!   [`PeerIdentity`] is inserted into request extensions so handlers
//!   can authorize off it.
//!
//! - If no `X-PRISM-Federation` header is present, the middleware is a
//!   no-op and `auth_layer` (or whatever else is in the chain) handles
//!   normal user-session auth.
//!
//! Fail modes:
//!
//! - Header present but undecodable → **400 Bad Request**.
//! - Cached platform pubkey missing → **503 Service Unavailable**
//!   (the node hasn't seen the platform yet — `prism login` populates
//!   this cache via the F1c3 fetcher).
//! - Signature / expiry / role check fails → **401 Unauthorized** or
//!   **403 Forbidden**.
//!
//! All the underlying primitives — [`verify_peer`], `PeerIdentity`,
//! `ActionRoleTable`, `PlatformPubkeyFetcher` — already exist and are
//! unit-tested. This module is purely the wiring that calls them on
//! every cross-org request, which has been listed as F1c4 / Bug #33
//! in the SHIPPED log and the CHANGELOG known-issues since v2.7.0.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::SystemTime;

use axum::{
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::Engine;
use ed25519_dalek::VerifyingKey;
use prism_audit::{AuditDecision, EmitSpec, EventKind};
use prism_mesh::federation::{CrossOrgRequest, PeerIdentity, verify_peer};
use prism_mesh::federation_lookup::{ActionRoleTable, PlatformPubkeyFetcher};
use serde::Serialize;

use crate::NodeState;

/// Header that carries a base64-encoded JSON `CrossOrgRequest` envelope.
const FEDERATION_HEADER: &str = "X-PRISM-Federation";

#[derive(Serialize)]
struct ErrorBody {
    error: &'static str,
    message: String,
}

fn error(status: StatusCode, code: &'static str, message: impl Into<String>) -> Response {
    (
        status,
        axum::Json(ErrorBody {
            error: code,
            message: message.into(),
        }),
    )
        .into_response()
}

/// Process-wide cache of the action→role table. We use the bundled
/// defaults; site-operator overrides via TOML are out of scope for this
/// PR (still tracked separately).
fn action_roles() -> &'static ActionRoleTable {
    static TABLE: OnceLock<ActionRoleTable> = OnceLock::new();
    TABLE.get_or_init(ActionRoleTable::defaults)
}

/// Read the cached MARC27 platform pubkey directly from disk. The
/// fetcher's full lazy refresh path needs a `PlatformClient`, which
/// would require plumbing into `NodeState`. For Bug #33 wiring we just
/// need to *verify*, not refresh — so we read the cache and hand back
/// a parsed [`VerifyingKey`]. If the cache is missing or corrupt we
/// surface a 503 and let the user run `prism login` to populate it.
fn cached_platform_pubkey() -> Option<VerifyingKey> {
    let home = std::env::var_os("HOME")?;
    let cache_path: PathBuf = PlatformPubkeyFetcher::default_cache_path(&PathBuf::from(home));
    let bytes = std::fs::read(&cache_path).ok()?;
    if bytes.len() != 32 {
        tracing::warn!(
            cache = %cache_path.display(),
            len = bytes.len(),
            "platform pubkey cache has wrong length"
        );
        return None;
    }
    let arr: [u8; 32] = bytes.as_slice().try_into().expect("len-checked above");
    VerifyingKey::from_bytes(&arr).ok()
}

/// Pull the federation envelope out of headers. Returns `None` when
/// the header is absent (normal user-auth path); `Err` when present but
/// garbled.
fn decode_envelope(headers: &HeaderMap) -> Result<Option<CrossOrgRequest>, String> {
    let Some(raw) = headers.get(FEDERATION_HEADER) else {
        return Ok(None);
    };
    let header_str = raw
        .to_str()
        .map_err(|_| "X-PRISM-Federation header is not valid UTF-8".to_string())?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(header_str.trim())
        .map_err(|e| format!("X-PRISM-Federation is not valid base64: {e}"))?;
    let envelope: CrossOrgRequest = serde_json::from_slice(&decoded)
        .map_err(|e| format!("X-PRISM-Federation envelope is not valid JSON: {e}"))?;
    Ok(Some(envelope))
}

/// Build an [`EmitSpec`] for a cross-org event about `env`. The emitter
/// fills in the source (this node); the peer is the request's `source`.
fn audit_spec(env: &CrossOrgRequest, kind: EventKind, decision: AuditDecision) -> EmitSpec {
    EmitSpec {
        kind,
        target_node_id: Some(env.source.node_id.clone()),
        target_org_id: Some(env.source.org_id.clone()),
        action: env.action.clone(),
        resource: env.resource.clone(),
        decision,
        correlation: Some(env.request_id),
        extra: serde_json::Value::Null,
    }
}

/// A verified request we accepted (before dispatching downstream).
fn received_spec(env: &CrossOrgRequest, required_role: Option<&str>) -> EmitSpec {
    let obligations = required_role
        .map(|r| vec![format!("role:{r}")])
        .unwrap_or_default();
    audit_spec(
        env,
        EventKind::RequestReceived,
        AuditDecision::Allowed { obligations },
    )
}

/// A request rejected at the trust/role gate.
fn denied_spec(env: &CrossOrgRequest, reason: String) -> EmitSpec {
    audit_spec(
        env,
        EventKind::PolicyDenied,
        AuditDecision::Denied {
            reasons: vec![reason],
        },
    )
}

/// The outcome of an accepted request after the downstream handler ran.
fn completion_spec(env: &CrossOrgRequest, status: StatusCode) -> EmitSpec {
    if status.is_success() {
        audit_spec(env, EventKind::WorkCompleted, AuditDecision::NoOpinion)
    } else {
        audit_spec(
            env,
            EventKind::WorkFailed,
            AuditDecision::Denied {
                reasons: vec![format!("downstream status {}", status.as_u16())],
            },
        )
    }
}

/// Axum middleware that validates an `X-PRISM-Federation` envelope.
///
/// Inserts the verified [`PeerIdentity`] into request extensions on
/// success. Pass-through when no federation header is present. When the
/// node has an audit emitter configured, records the cross-org event:
/// `RequestReceived` / `PolicyDenied` on the verify decision, then
/// `WorkCompleted` / `WorkFailed` from the downstream response.
pub async fn federation_layer(
    State(state): State<Arc<NodeState>>,
    mut req: Request,
    next: Next,
) -> Response {
    let envelope = match decode_envelope(req.headers()) {
        Ok(None) => return next.run(req).await, // No federation header — pass through.
        Ok(Some(env)) => env,
        Err(msg) => return error(StatusCode::BAD_REQUEST, "bad_federation_header", msg),
    };

    // Cached platform pubkey is the trust anchor.
    let Some(platform_pubkey) = cached_platform_pubkey() else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "federation_not_initialized",
            "Node has not cached the MARC27 platform pubkey yet. Run `prism login` \
             to fetch it (writes to ~/.prism/federation/platform_pubkey.bin).",
        );
    };

    let required_role = action_roles().required_role(&envelope.action);

    if let Err(e) = verify_peer(
        &envelope,
        &platform_pubkey,
        required_role,
        SystemTime::now(),
    ) {
        // verify_peer's TrustError already classifies; map to HTTP.
        let (status, code) = match &e {
            prism_mesh::federation::TrustError::BadPlatformSignature
            | prism_mesh::federation::TrustError::BadRequestSignature
            | prism_mesh::federation::TrustError::MalformedSignature(_) => {
                (StatusCode::UNAUTHORIZED, "bad_signature")
            }
            prism_mesh::federation::TrustError::Expired(_) => {
                (StatusCode::UNAUTHORIZED, "identity_expired")
            }
            prism_mesh::federation::TrustError::MissingRole { .. } => {
                (StatusCode::FORBIDDEN, "missing_role")
            }
            _ => (StatusCode::UNAUTHORIZED, "federation_rejected"),
        };
        tracing::warn!(
            error = %e,
            action = %envelope.action,
            source_org = %envelope.source.org_id,
            source_node = %envelope.source.node_id,
            "Federation request rejected"
        );
        if let Some(emitter) = &state.federation_audit {
            emitter.emit(denied_spec(&envelope, e.to_string())).await;
        }
        return error(status, code, e.to_string());
    }

    tracing::debug!(
        action = %envelope.action,
        source_org = %envelope.source.org_id,
        source_node = %envelope.source.node_id,
        "Federation request verified"
    );

    if let Some(emitter) = &state.federation_audit {
        emitter.emit(received_spec(&envelope, required_role)).await;
    }

    // Hand the verified identity to downstream handlers.
    let identity: PeerIdentity = envelope.source.clone();
    req.extensions_mut().insert(identity);
    req.extensions_mut().insert(envelope.clone());
    let response = next.run(req).await;

    if let Some(emitter) = &state.federation_audit {
        emitter
            .emit(completion_spec(&envelope, response.status()))
            .await;
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue};

    #[test]
    fn no_header_returns_none() {
        let headers = HeaderMap::new();
        let envelope = decode_envelope(&headers).unwrap();
        assert!(envelope.is_none());
    }

    #[test]
    fn malformed_base64_returns_err() {
        let mut headers = HeaderMap::new();
        headers.insert(FEDERATION_HEADER, HeaderValue::from_static("not-base64!!!"));
        let result = decode_envelope(&headers);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("base64"),
            "expected base64 mention in error: {err}"
        );
    }

    #[test]
    fn malformed_json_after_base64_returns_err() {
        let mut headers = HeaderMap::new();
        // valid base64, but decodes to "not-json"
        let val = base64::engine::general_purpose::STANDARD.encode(b"not-json");
        headers.insert(
            FEDERATION_HEADER,
            HeaderValue::from_str(&val).expect("ascii"),
        );
        let result = decode_envelope(&headers);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("JSON"),
            "expected JSON mention in error: {err}"
        );
    }

    // ── Audit emission on the cross-org dispatch path ──────────────

    use chrono::Utc;
    use ed25519_dalek::SigningKey;
    use prism_audit::AuditEmitter;
    use prism_mesh::federation::PeerIdentity;
    use uuid::Uuid;

    /// A dummy cross-org request (no valid signatures — the spec
    /// builders never verify, they only read fields).
    fn dummy_request() -> CrossOrgRequest {
        CrossOrgRequest {
            request_id: Uuid::new_v4(),
            source: PeerIdentity {
                org_id: "org-tokyo".into(),
                project_id: None,
                node_id: "node-tokyo-01".into(),
                node_pubkey_hex: String::new(),
                platform_signature_hex: String::new(),
                roles: vec!["inference.submit".into()],
                valid_until: Utc::now(),
            },
            target_org: "org-munich".into(),
            action: "inference.submit".into(),
            resource: "node://munich-01/gpu-0".into(),
            payload: serde_json::Value::Null,
            request_signature_hex: String::new(),
        }
    }

    #[test]
    fn received_spec_targets_peer_and_records_role() {
        let req = dummy_request();
        let spec = received_spec(&req, Some("inference.submit"));
        assert_eq!(spec.kind, EventKind::RequestReceived);
        assert_eq!(spec.target_node_id.as_deref(), Some("node-tokyo-01"));
        assert_eq!(spec.target_org_id.as_deref(), Some("org-tokyo"));
        assert_eq!(spec.correlation, Some(req.request_id));
        match spec.decision {
            AuditDecision::Allowed { obligations } => {
                assert_eq!(obligations, vec!["role:inference.submit".to_string()]);
            }
            other => panic!("expected Allowed, got {other:?}"),
        }
    }

    #[test]
    fn denied_spec_is_policy_denied_with_reason() {
        let spec = denied_spec(&dummy_request(), "peer lacks required role".into());
        assert_eq!(spec.kind, EventKind::PolicyDenied);
        match spec.decision {
            AuditDecision::Denied { reasons } => {
                assert_eq!(reasons, vec!["peer lacks required role".to_string()]);
            }
            other => panic!("expected Denied, got {other:?}"),
        }
    }

    #[test]
    fn completion_spec_maps_status_to_outcome() {
        let req = dummy_request();
        assert_eq!(
            completion_spec(&req, StatusCode::OK).kind,
            EventKind::WorkCompleted
        );
        assert_eq!(
            completion_spec(&req, StatusCode::INTERNAL_SERVER_ERROR).kind,
            EventKind::WorkFailed
        );
    }

    #[tokio::test]
    async fn dispatch_path_emits_signed_envelope() {
        // The dispatch path emits through an AuditEmitter; assert an
        // envelope actually lands in the log and verifies.
        let tmp = tempfile::TempDir::new().unwrap();
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let emitter = AuditEmitter::new(
            "node-munich-01",
            "org-munich",
            key,
            tmp.path().join("audit-envelopes.jsonl"),
            true,
        );

        let req = dummy_request();
        emitter
            .emit(received_spec(&req, Some("inference.submit")))
            .await
            .expect("enabled emitter emits");
        emitter
            .emit(completion_spec(&req, StatusCode::OK))
            .await
            .expect("enabled emitter emits");

        let log = prism_audit::AuditLog::new(emitter.log_path());
        let envelopes = log.read_all().await.unwrap();
        assert_eq!(envelopes.len(), 2);
        assert_eq!(envelopes[0].event.kind, EventKind::RequestReceived);
        assert_eq!(envelopes[0].event.source_node_id, "node-munich-01");
        assert_eq!(
            envelopes[0].event.target_node_id.as_deref(),
            Some("node-tokyo-01")
        );
        assert_eq!(envelopes[1].event.kind, EventKind::WorkCompleted);
        for env in &envelopes {
            env.verify_signature().unwrap();
        }
    }

    #[tokio::test]
    async fn disabled_emitter_on_dispatch_path_emits_nothing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let key = SigningKey::from_bytes(&[9u8; 32]);
        let path = tmp.path().join("audit-envelopes.jsonl");
        let emitter = AuditEmitter::new("n", "o", key, &path, false);

        assert!(
            emitter
                .emit(received_spec(&dummy_request(), None))
                .await
                .is_none()
        );
        assert!(!path.exists());
    }
}
