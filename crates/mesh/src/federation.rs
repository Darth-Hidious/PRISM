// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! Cross-org federation primitives for PRISM Fabric v1.
//!
//! This module owns the **identity + trust** half of mesh federation —
//! the part that decides whether a request from a node in *another*
//! org should be honored. The actual cross-org *transport* (sending
//! the bytes between nodes) reuses the existing mesh subscription
//! channel; the *query* fanout is in [`crate::federated_query`].
//!
//! # Trust model: transitive root-CA via MARC27 platform
//!
//! Every PRISM node is logged in to MARC27 via `prism login`. The
//! login flow returns a token whose claims include `org_id`,
//! `project_id`, `roles[]`, and a platform-signed `node_pubkey` (the
//! node's Ed25519 public key, signed by the platform's root key at
//! first node-up).
//!
//! When node A in org X wants to ask node B in org Y to do something:
//!
//! 1. A signs a [`CrossOrgRequest`] with its own node private key.
//! 2. A includes its [`PeerIdentity`] (the platform-signed claim of who
//!    A is) in the request envelope.
//! 3. B receives the request and calls [`verify_peer`]:
//!     - Verify the platform signature on A's identity claims using
//!       the cached platform root pubkey
//!     - Verify the request signature using A's node pubkey from the
//!       (now-trusted) identity claims
//!     - Check `valid_until` is in the future
//!     - Check `roles` contains the role required for `action`
//! 4. If all four pass, B trusts A and proceeds.
//!
//! No pairwise MoUs, no manual `prism federation trust`. The platform
//! is the single trust anchor; trust is transitive through it.
//!
//! # What this module deliberately does NOT do
//!
//! - **Transport.** Bytes move via `crate::subscription` and the
//!   existing mesh WebSocket channel. This module is data shapes +
//!   verify logic only.
//! - **Policy evaluation.** Once a peer is verified, the cross-org
//!   policy intersection lives in `crates/policy/intersect.rs`.
//! - **Audit.** Signed cross-org audit envelopes live in their own
//!   `crates/audit/` crate (F5). This module emits the verified
//!   identity that audit then signs over.
//!
//! # Design choices
//!
//! - **Ed25519 over RSA/ECDSA.** Already used elsewhere in PRISM
//!   (`crates/node/crypto.rs`); keys are 32 bytes, signatures 64
//!   bytes; constant-time verification.
//! - **Platform pubkey is fetched once at boot, not per-request.**
//!   Cached in the verifier. Rotation handled by re-login.
//! - **`valid_until` is required.** A platform-signed claim with no
//!   expiry would never expire — bad. Tokens are short-lived
//!   (~hours), with refresh via `prism login --refresh` (out of
//!   scope here).
//! - **Stable `PeerIdentity` serialization.** We sign the bytes of
//!   `serde_json::to_vec_pretty(&identity)`. JSON canonical form is
//!   not strictly stable across serde versions, but the platform and
//!   the verifier both use the same library, and we pin via the
//!   workspace lockfile. If this becomes a portability issue,
//!   migrate to a deterministic encoding (CBOR-RFC8949 or borsh)
//!   in v1.5.

use std::time::SystemTime;

use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Wire-stable identifier for an organization, project, or node.
///
/// MARC27 platform issues these as opaque UUIDs. We don't parse the
/// UUID string ourselves — it's identity, not data — but we do
/// require well-formed UTF-8.
pub type OrgId = String;
pub type ProjectId = String;
pub type NodeId = String;

/// A platform-issued role claim. Examples: `"compute:invoke"`,
/// `"data:read"`, `"workflow:execute"`. Free-form strings; the
/// matching is done by exact equality in the action → role table.
pub type Role = String;

/// Errors returned by [`verify_peer`].
#[derive(Debug, Error)]
pub enum TrustError {
    #[error("platform signature invalid for peer identity")]
    BadPlatformSignature,
    #[error("request signature invalid for source node key")]
    BadRequestSignature,
    #[error("peer identity expired at {0}")]
    Expired(DateTime<Utc>),
    #[error("peer lacks required role for action `{action}`: needed `{needed}`")]
    MissingRole { action: String, needed: String },
    #[error("peer node pubkey malformed: {0}")]
    MalformedKey(String),
    #[error("peer signature malformed: {0}")]
    MalformedSignature(String),
    #[error("peer identity serialization failed: {0}")]
    SerializeFailed(String),
}

/// A peer node's claim of who it is, signed by the MARC27 platform.
///
/// Issued at `prism login` (or first node-up) and embedded in every
/// outgoing cross-org request. The receiving node verifies the
/// `platform_signature` field against the well-known MARC27 root
/// pubkey before trusting any other field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerIdentity {
    pub org_id: OrgId,
    pub project_id: Option<ProjectId>,
    pub node_id: NodeId,

    /// Ed25519 public key for this node, 32 bytes (hex-encoded for
    /// JSON portability).
    pub node_pubkey_hex: String,

    /// Platform's Ed25519 signature over the identity claims (every
    /// field above). 64 bytes (hex-encoded).
    pub platform_signature_hex: String,

    pub roles: Vec<Role>,

    /// Wall-clock expiry. Verifier rejects identities past this
    /// instant.
    pub valid_until: DateTime<Utc>,
}

impl PeerIdentity {
    /// Bytes that the platform signs to produce
    /// `platform_signature_hex`. Excludes the signature itself —
    /// otherwise we'd be circular.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, TrustError> {
        // Build a canonical view sans signature
        let claims = serde_json::json!({
            "org_id": self.org_id,
            "project_id": self.project_id,
            "node_id": self.node_id,
            "node_pubkey_hex": self.node_pubkey_hex,
            "roles": self.roles,
            "valid_until": self.valid_until.to_rfc3339(),
        });
        serde_json::to_vec(&claims).map_err(|e| TrustError::SerializeFailed(e.to_string()))
    }

    /// Decode the node pubkey from hex into an Ed25519 verifying key.
    pub fn node_verifying_key(&self) -> Result<VerifyingKey, TrustError> {
        let bytes = hex::decode(&self.node_pubkey_hex)
            .map_err(|e| TrustError::MalformedKey(format!("hex: {e}")))?;
        let arr: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
            TrustError::MalformedKey(format!("expected 32 bytes, got {}", bytes.len()))
        })?;
        VerifyingKey::from_bytes(&arr)
            .map_err(|e| TrustError::MalformedKey(format!("ed25519: {e}")))
    }

    /// Decode the platform signature from hex into an Ed25519 signature.
    pub fn platform_sig(&self) -> Result<Signature, TrustError> {
        let bytes = hex::decode(&self.platform_signature_hex)
            .map_err(|e| TrustError::MalformedSignature(format!("hex: {e}")))?;
        let arr: [u8; 64] = bytes.as_slice().try_into().map_err(|_| {
            TrustError::MalformedSignature(format!("expected 64 bytes, got {}", bytes.len()))
        })?;
        Ok(Signature::from_bytes(&arr))
    }
}

/// One node asking another node (in any org) to do something.
///
/// The receiving node calls [`verify_peer`] before honoring it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CrossOrgRequest {
    /// Server-side request UUID (echoed in audit envelopes).
    pub request_id: Uuid,

    /// The peer making the request.
    pub source: PeerIdentity,

    /// Which org's resource is being touched. May equal `source.org_id`
    /// for same-org requests routed through the federation path.
    pub target_org: OrgId,

    /// Action verb, e.g. `"inference.submit"`, `"compute.estimate"`,
    /// `"dataset.read"`. Used for role matching.
    pub action: String,

    /// Resource handle, e.g. `"node://munich-01/gpu-cluster"` or
    /// `"dataset://tokyo/cfd-2024-q3"`.
    pub resource: String,

    /// Action-specific JSON payload. Opaque to the verifier.
    pub payload: serde_json::Value,

    /// Source node's Ed25519 signature over the canonical bytes of
    /// `request_id || source.node_id || target_org || action ||
    /// resource || payload` (hex-encoded).
    pub request_signature_hex: String,
}

impl CrossOrgRequest {
    /// Bytes the source node signs. Excludes the signature itself.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, TrustError> {
        let canonical = serde_json::json!({
            "request_id": self.request_id,
            "source_node_id": self.source.node_id,
            "target_org": self.target_org,
            "action": self.action,
            "resource": self.resource,
            "payload": self.payload,
        });
        serde_json::to_vec(&canonical).map_err(|e| TrustError::SerializeFailed(e.to_string()))
    }

    pub fn request_sig(&self) -> Result<Signature, TrustError> {
        let bytes = hex::decode(&self.request_signature_hex)
            .map_err(|e| TrustError::MalformedSignature(format!("hex: {e}")))?;
        let arr: [u8; 64] = bytes.as_slice().try_into().map_err(|_| {
            TrustError::MalformedSignature(format!("expected 64 bytes, got {}", bytes.len()))
        })?;
        Ok(Signature::from_bytes(&arr))
    }

    /// Sign + assemble a request locally. Convenience for clients;
    /// the verifier never calls this.
    pub fn sign(
        signing_key: &SigningKey,
        source: PeerIdentity,
        target_org: OrgId,
        action: impl Into<String>,
        resource: impl Into<String>,
        payload: serde_json::Value,
    ) -> Result<Self, TrustError> {
        let action = action.into();
        let resource = resource.into();
        let mut req = CrossOrgRequest {
            request_id: Uuid::new_v4(),
            source,
            target_org,
            action,
            resource,
            payload,
            request_signature_hex: String::new(),
        };
        let sig = signing_key.sign(&req.signing_bytes()?);
        req.request_signature_hex = hex::encode(sig.to_bytes());
        Ok(req)
    }
}

/// Verify a cross-org request end-to-end.
///
/// The four checks, in order:
///
/// 1. Platform signature on the peer identity (untrusted → trusted).
/// 2. Request signature using the now-trusted node pubkey.
/// 3. Identity expiry (`valid_until` must be in the future).
/// 4. Action requires a role; peer must hold it.
///
/// `required_role` is looked up from a platform-defined or
/// locally-configured action → role table by the caller. Pass `None`
/// to skip the role check (e.g. for actions with no role
/// requirement, like `peer.heartbeat`). We **deliberately don't
/// embed the action→role table here** — that's policy concern, not
/// trust concern.
pub fn verify_peer(
    request: &CrossOrgRequest,
    platform_root_pubkey: &VerifyingKey,
    required_role: Option<&str>,
    now: SystemTime,
) -> Result<(), TrustError> {
    // 1. Platform signature on the identity claims
    let id_bytes = request.source.signing_bytes()?;
    let platform_sig = request.source.platform_sig()?;
    platform_root_pubkey
        .verify(&id_bytes, &platform_sig)
        .map_err(|_| TrustError::BadPlatformSignature)?;

    // 2. Request signature using the (now-trusted) node pubkey
    let node_key = request.source.node_verifying_key()?;
    let req_bytes = request.signing_bytes()?;
    let req_sig = request.request_sig()?;
    node_key
        .verify(&req_bytes, &req_sig)
        .map_err(|_| TrustError::BadRequestSignature)?;

    // 3. Expiry
    let now_chrono: DateTime<Utc> = now.into();
    if now_chrono > request.source.valid_until {
        return Err(TrustError::Expired(request.source.valid_until));
    }

    // 4. Role
    if let Some(required) = required_role
        && !request.source.roles.iter().any(|r| r == required)
    {
        return Err(TrustError::MissingRole {
            action: request.action.clone(),
            needed: required.to_string(),
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;

    /// Test harness: produces (platform_signing_key, peer_signing_key,
    /// PeerIdentity, CrossOrgRequest) all signed correctly.
    fn happy_path() -> (SigningKey, SigningKey, PeerIdentity, CrossOrgRequest) {
        let mut rng = OsRng;
        let platform_key = SigningKey::generate(&mut rng);
        let node_key = SigningKey::generate(&mut rng);

        let mut identity = PeerIdentity {
            org_id: "org-tokyo".to_string(),
            project_id: Some("proj-cfd".to_string()),
            node_id: "node-tokyo-01".to_string(),
            node_pubkey_hex: hex::encode(node_key.verifying_key().to_bytes()),
            platform_signature_hex: String::new(), // filled below
            roles: vec!["inference.submit".to_string(), "data.read".to_string()],
            valid_until: Utc::now() + chrono::Duration::hours(1),
        };

        // Platform signs the identity claims
        let id_bytes = identity.signing_bytes().unwrap();
        let platform_sig = platform_key.sign(&id_bytes);
        identity.platform_signature_hex = hex::encode(platform_sig.to_bytes());

        // Source signs a sample request
        let req = CrossOrgRequest::sign(
            &node_key,
            identity.clone(),
            "org-munich".to_string(),
            "inference.submit",
            "node://munich-01/gpu-0",
            serde_json::json!({"model": "llama-3-70b", "prompt": "hello"}),
        )
        .unwrap();

        (platform_key, node_key, identity, req)
    }

    #[test]
    fn happy_path_verifies() {
        let (platform_key, _node_key, _id, req) = happy_path();
        let result = verify_peer(
            &req,
            &platform_key.verifying_key(),
            Some("inference.submit"),
            SystemTime::now(),
        );
        assert!(result.is_ok(), "expected ok, got: {:?}", result.err());
    }

    #[test]
    fn rejects_bad_platform_signature() {
        let (_platform_key, _node_key, _id, mut req) = happy_path();
        // Tamper with the platform signature
        req.source.platform_signature_hex = hex::encode([0xAAu8; 64]);
        let wrong_pubkey = SigningKey::generate(&mut OsRng).verifying_key();
        // Use the WRONG platform key (so it can't possibly verify
        // even if we hadn't tampered)
        let result = verify_peer(&req, &wrong_pubkey, None, SystemTime::now());
        assert!(matches!(result, Err(TrustError::BadPlatformSignature)));
    }

    #[test]
    fn rejects_wrong_platform_root() {
        let (_platform_key, _node_key, _id, req) = happy_path();
        let attacker_root = SigningKey::generate(&mut OsRng).verifying_key();
        let result = verify_peer(&req, &attacker_root, None, SystemTime::now());
        assert!(matches!(result, Err(TrustError::BadPlatformSignature)));
    }

    #[test]
    fn rejects_bad_request_signature() {
        let (platform_key, _node_key, _id, mut req) = happy_path();
        // Flip the request payload — signature no longer covers it
        req.payload = serde_json::json!({"model": "EVIL", "prompt": "you've been pwned"});
        let result = verify_peer(&req, &platform_key.verifying_key(), None, SystemTime::now());
        assert!(matches!(result, Err(TrustError::BadRequestSignature)));
    }

    #[test]
    fn rejects_expired_identity() {
        let mut rng = OsRng;
        let platform_key = SigningKey::generate(&mut rng);
        let node_key = SigningKey::generate(&mut rng);

        // Identity that expired an hour ago
        let mut identity = PeerIdentity {
            org_id: "org-tokyo".to_string(),
            project_id: None,
            node_id: "node-1".to_string(),
            node_pubkey_hex: hex::encode(node_key.verifying_key().to_bytes()),
            platform_signature_hex: String::new(),
            roles: vec!["inference.submit".to_string()],
            valid_until: Utc::now() - chrono::Duration::hours(1),
        };
        let id_bytes = identity.signing_bytes().unwrap();
        identity.platform_signature_hex = hex::encode(platform_key.sign(&id_bytes).to_bytes());

        let req = CrossOrgRequest::sign(
            &node_key,
            identity,
            "org-munich".to_string(),
            "inference.submit",
            "node://munich-01",
            serde_json::json!({}),
        )
        .unwrap();

        let result = verify_peer(&req, &platform_key.verifying_key(), None, SystemTime::now());
        assert!(matches!(result, Err(TrustError::Expired(_))));
    }

    #[test]
    fn rejects_missing_role() {
        let (platform_key, _node_key, _id, req) = happy_path();
        // Request the role `compute.invoke` which the peer doesn't have
        // (peer only holds inference.submit + data.read)
        let result = verify_peer(
            &req,
            &platform_key.verifying_key(),
            Some("compute.invoke"),
            SystemTime::now(),
        );
        assert!(matches!(result, Err(TrustError::MissingRole { .. })));
    }

    #[test]
    fn skips_role_check_when_none() {
        let (platform_key, _node_key, _id, req) = happy_path();
        // No required_role → skip the role gate, even if peer has nothing
        let result = verify_peer(&req, &platform_key.verifying_key(), None, SystemTime::now());
        assert!(result.is_ok());
    }

    #[test]
    fn rejects_malformed_pubkey() {
        let mut rng = OsRng;
        let platform_key = SigningKey::generate(&mut rng);
        let node_key = SigningKey::generate(&mut rng);

        let mut identity = PeerIdentity {
            org_id: "x".to_string(),
            project_id: None,
            node_id: "n".to_string(),
            // Not 32 bytes worth of hex — malformed
            node_pubkey_hex: "deadbeef".to_string(),
            platform_signature_hex: String::new(),
            roles: vec![],
            valid_until: Utc::now() + chrono::Duration::hours(1),
        };
        let id_bytes = identity.signing_bytes().unwrap();
        identity.platform_signature_hex = hex::encode(platform_key.sign(&id_bytes).to_bytes());

        let req = CrossOrgRequest::sign(
            &node_key,
            identity,
            "y".to_string(),
            "a",
            "r",
            serde_json::json!({}),
        )
        .unwrap();

        let result = verify_peer(&req, &platform_key.verifying_key(), None, SystemTime::now());
        assert!(matches!(result, Err(TrustError::MalformedKey(_))));
    }

    #[test]
    fn signing_bytes_excludes_signature_field() {
        // Sanity check: if the signature field is included in the
        // signed bytes, signing becomes circular and tampering with
        // the signature would change its own input. Verify that the
        // canonical bytes are stable across signature changes.
        let (_p, _n, mut id, _r) = happy_path();
        let bytes_a = id.signing_bytes().unwrap();
        id.platform_signature_hex = hex::encode([0u8; 64]);
        let bytes_b = id.signing_bytes().unwrap();
        assert_eq!(bytes_a, bytes_b);
    }
}
