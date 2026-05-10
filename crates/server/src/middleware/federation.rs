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
use std::sync::OnceLock;
use std::time::SystemTime;

use axum::{
    extract::Request,
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::Engine;
use ed25519_dalek::VerifyingKey;
use prism_mesh::federation::{CrossOrgRequest, PeerIdentity, verify_peer};
use prism_mesh::federation_lookup::{ActionRoleTable, PlatformPubkeyFetcher};
use serde::Serialize;

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

/// Axum middleware that validates an `X-PRISM-Federation` envelope.
///
/// Inserts the verified [`PeerIdentity`] into request extensions on
/// success. Pass-through when no federation header is present.
pub async fn federation_layer(mut req: Request, next: Next) -> Response {
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
        return error(status, code, e.to_string());
    }

    tracing::debug!(
        action = %envelope.action,
        source_org = %envelope.source.org_id,
        source_node = %envelope.source.node_id,
        "Federation request verified"
    );

    // Hand the verified identity to downstream handlers.
    let identity: PeerIdentity = envelope.source.clone();
    req.extensions_mut().insert(identity);
    req.extensions_mut().insert(envelope);
    next.run(req).await
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
}
