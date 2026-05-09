// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! F5 — signed cross-org audit envelopes for PRISM Fabric.
//!
//! When a cross-org event happens (request received, dispatched,
//! denied, completed, failed), the deciding node emits an
//! [`AuditEnvelope`] — a JSON record of the event, signed with the
//! node's Ed25519 key. Storage is append-only JSONL on disk; the
//! envelope's signer pubkey is included in the record so a third
//! party (auditor, regulator, or the other org) can verify after
//! the fact without coordinating with the signer.
//!
//! # Trust model
//!
//! Verification needs three things:
//!
//! 1. The signing node's pubkey embedded in the envelope.
//! 2. A trusted way to know that pubkey was platform-signed at the
//!    time of the event. The platform-signed [`PeerIdentity`] from
//!    `mesh::federation` is the typical source — pin it next to the
//!    envelope when you persist it, or have the auditor re-fetch
//!    via [`mesh::federation_lookup::PlatformPubkeyFetcher`].
//! 3. The platform's root pubkey at the time, to validate the
//!    identity claim's signature.
//!
//! This crate handles step 1. Steps 2 and 3 are mesh's job; the
//! audit layer holds them at arm's length so a leaked audit log
//! doesn't double as a key-rotation oracle.
//!
//! # What this crate deliberately does NOT do
//!
//! - **Distribute** envelopes. The producer writes locally; the
//!   consumer (audit pipeline, regulator) reads from disk or a
//!   replicated log. This crate is "produce + verify."
//! - **Define the action ontology.** `event.action` is a free-form
//!   string; the meaning is whatever the producer chose. Audit is
//!   for the *fact* of the event, not the schema.
//! - **Encrypt.** Envelopes are signed-not-sealed: anyone with the
//!   file can read it. Confidentiality is the storage layer's job.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::fs::OpenOptions;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum AuditError {
    #[error("audit envelope signature is invalid")]
    BadSignature,
    #[error("audit envelope signer pubkey malformed: {0}")]
    MalformedKey(String),
    #[error("audit envelope signature malformed: {0}")]
    MalformedSignature(String),
    #[error("audit envelope serialisation failed: {0}")]
    Serialize(String),
    #[error("audit envelope was signed by {actual}, expected {expected}")]
    SignerMismatch { expected: String, actual: String },
}

// ---------------------------------------------------------------------------
// Event types
// ---------------------------------------------------------------------------

/// Categories of cross-org events worth recording.
///
/// Keep the set small. Each new variant is a new place producers
/// have to remember to emit, and a new shape auditors have to
/// recognise. Add only when a real workflow needs it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// A cross-org request arrived and the receiver verified the
    /// signature + identity (per `mesh::federation::verify_peer`).
    /// Emitted before any policy check.
    RequestReceived,
    /// Local node dispatched a request to a peer org. Emitted by
    /// the *sender* side, mirroring `RequestReceived` on the receiver.
    RequestDispatched,
    /// A request was denied by policy (or the action→role table did
    /// not authorise the peer). Always emit a denial; silence is
    /// indistinguishable from "the request never arrived" in audit.
    PolicyDenied,
    /// The work the request asked for completed successfully.
    WorkCompleted,
    /// The work was attempted and failed (provider error, OOM,
    /// timeout, etc). The reasons go in `decision`.
    WorkFailed,
}

/// What the policy / executor decided.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AuditDecision {
    /// Allowed, optionally with obligations the executor must
    /// fulfill (e.g. `"audit_log"`, `"require_mfa"`).
    Allowed { obligations: Vec<String> },
    /// Denied with explanatory reasons.
    Denied { reasons: Vec<String> },
    /// No decision rendered (e.g. for `WorkCompleted`/`WorkFailed`
    /// where the policy choice happened earlier and is recorded in
    /// the correlation chain).
    NoOpinion,
}

/// The fact of a cross-org event, free of any signature material.
///
/// Signing happens around the canonical bytes of this struct;
/// changes to its layout are wire-breaking, so add fields at the
/// end and use `#[serde(default)]` for back-compat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEvent {
    /// UUID for this event. New per emission, even for follow-on
    /// events about the same request.
    pub event_id: Uuid,
    /// Wall-clock at emission time (signer's clock).
    pub timestamp: DateTime<Utc>,
    /// What kind of event this is.
    pub kind: EventKind,
    /// The node that emitted this event.
    pub source_node_id: String,
    /// The org that emitted this event.
    pub source_org_id: String,
    /// The peer node, if applicable. Absent for events with no
    /// remote counterpart (rare in practice — almost all cross-org
    /// audit events have both sides).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_node_id: Option<String>,
    /// The peer org, if applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_org_id: Option<String>,
    /// Free-form action verb (e.g. `"inference.submit"`,
    /// `"dataset.read"`). Mirrors `CrossOrgRequest.action`.
    pub action: String,
    /// Resource handle (e.g. `"node://munich-01/gpu-0"`).
    pub resource: String,
    /// Decision rendered, if any.
    pub decision: AuditDecision,
    /// Optional UUID linking this event to an originating
    /// `CrossOrgRequest.request_id`. Lets auditors stitch a chain
    /// (`RequestReceived` → `PolicyDenied` OR `WorkCompleted`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation: Option<Uuid>,
    /// Free-form extra context the producer wants in the record
    /// (e.g. cost estimate, request hash). Opaque to the verifier.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub extra: serde_json::Value,
}

impl AuditEvent {
    /// Bytes the signer signs. Derived from a canonical JSON view
    /// of every field. Excludes any signature material — callers
    /// must not pre-populate sigs into `extra`.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, AuditError> {
        // Build a deterministic JSON value. We don't rely on serde's
        // map-iteration order — instead we hand-build a canonical
        // shape that won't shift when serde versions change.
        let canonical = serde_json::json!({
            "event_id": self.event_id,
            "timestamp": self.timestamp.to_rfc3339(),
            "kind": self.kind,
            "source_node_id": self.source_node_id,
            "source_org_id": self.source_org_id,
            "target_node_id": self.target_node_id,
            "target_org_id": self.target_org_id,
            "action": self.action,
            "resource": self.resource,
            "decision": self.decision,
            "correlation": self.correlation,
            "extra": self.extra,
        });
        serde_json::to_vec(&canonical).map_err(|e| AuditError::Serialize(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Envelope
// ---------------------------------------------------------------------------

/// A signed audit record. Append-only — once written, never edited.
///
/// `signer_pubkey_hex` is included so an auditor doesn't need to
/// coordinate with the signer to verify; they only need a
/// platform-signed [`PeerIdentity`] (or equivalent attestation)
/// proving this pubkey was issued to the claimed `source_node_id`
/// at signing time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEnvelope {
    pub event: AuditEvent,
    /// The node that signed this envelope. Should equal
    /// `event.source_node_id`; mismatch is a bug or an attempt at
    /// forgery — [`Self::verify`] flags it.
    pub signer_node_id: String,
    /// Signer's Ed25519 public key, hex-encoded (32 bytes).
    pub signer_pubkey_hex: String,
    /// Signature over [`AuditEvent::signing_bytes`], hex-encoded
    /// (64 bytes).
    pub signature_hex: String,
}

impl AuditEnvelope {
    /// Sign an event. The caller chooses the `signer_node_id` —
    /// usually their own node id from `PeerIdentity.node_id`.
    pub fn sign(
        event: AuditEvent,
        signer_node_id: impl Into<String>,
        signing_key: &SigningKey,
    ) -> Result<Self, AuditError> {
        let bytes = event.signing_bytes()?;
        let sig = signing_key.sign(&bytes);
        Ok(Self {
            event,
            signer_node_id: signer_node_id.into(),
            signer_pubkey_hex: hex::encode(signing_key.verifying_key().to_bytes()),
            signature_hex: hex::encode(sig.to_bytes()),
        })
    }

    /// Verify the envelope's signature using the embedded pubkey.
    ///
    /// **Important:** this does NOT verify *who owns* the pubkey —
    /// that's the platform-signed identity claim's job. Use this
    /// alongside `mesh::federation::verify_peer` (or equivalent
    /// attestation) to establish the chain:
    ///
    ///   1. Platform signed identity X claiming pubkey K
    ///   2. Pubkey K signed this envelope
    ///   3. Therefore identity X attested to this event
    pub fn verify_signature(&self) -> Result<(), AuditError> {
        let pk_bytes = hex::decode(&self.signer_pubkey_hex)
            .map_err(|e| AuditError::MalformedKey(format!("hex: {e}")))?;
        let pk_arr: [u8; 32] = pk_bytes.as_slice().try_into().map_err(|_| {
            AuditError::MalformedKey(format!("expected 32 bytes, got {}", pk_bytes.len()))
        })?;
        let pubkey = VerifyingKey::from_bytes(&pk_arr)
            .map_err(|e| AuditError::MalformedKey(format!("ed25519: {e}")))?;

        let sig_bytes = hex::decode(&self.signature_hex)
            .map_err(|e| AuditError::MalformedSignature(format!("hex: {e}")))?;
        let sig_arr: [u8; 64] = sig_bytes.as_slice().try_into().map_err(|_| {
            AuditError::MalformedSignature(format!("expected 64 bytes, got {}", sig_bytes.len()))
        })?;
        let sig = Signature::from_bytes(&sig_arr);

        let bytes = self.event.signing_bytes()?;
        pubkey
            .verify(&bytes, &sig)
            .map_err(|_| AuditError::BadSignature)?;

        if self.signer_node_id != self.event.source_node_id {
            return Err(AuditError::SignerMismatch {
                expected: self.event.source_node_id.clone(),
                actual: self.signer_node_id.clone(),
            });
        }
        Ok(())
    }

    /// Verify against a *specific* expected pubkey (e.g. the one
    /// pulled from a platform-signed [`PeerIdentity`]). Use this
    /// when you already know who signed the envelope and want to
    /// confirm they really did.
    pub fn verify_with(&self, expected: &VerifyingKey) -> Result<(), AuditError> {
        // Embedded pubkey must match the claimed one.
        let claimed = hex::decode(&self.signer_pubkey_hex)
            .map_err(|e| AuditError::MalformedKey(format!("hex: {e}")))?;
        if claimed.as_slice() != expected.to_bytes() {
            return Err(AuditError::BadSignature);
        }
        self.verify_signature()
    }
}

// ---------------------------------------------------------------------------
// Append-only on-disk log
// ---------------------------------------------------------------------------

/// Append-only JSONL audit log on disk.
///
/// One envelope per line, JSON-serialised. The producer appends;
/// the consumer reads sequentially. Robust to mid-write crashes
/// because each line is a complete envelope — partial trailing
/// lines are simply skipped on read.
pub struct AuditLog {
    path: PathBuf,
}

impl AuditLog {
    /// Wrap an existing file path. The parent directory must exist;
    /// create it explicitly if needed (we don't auto-create to
    /// avoid masking misconfiguration).
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// File path the log writes to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one envelope as a JSONL line.
    ///
    /// Acquires no lock — concurrent appenders MUST coordinate
    /// externally (single tokio task per file is the simplest
    /// pattern). We do flush after each write so a crash leaves a
    /// well-formed file at the line boundary.
    pub async fn append(&self, envelope: &AuditEnvelope) -> Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await
            .with_context(|| format!("opening audit log {}", self.path.display()))?;
        let mut line =
            serde_json::to_string(envelope).with_context(|| "serialising audit envelope")?;
        line.push('\n');
        file.write_all(line.as_bytes()).await?;
        file.flush().await?;
        Ok(())
    }

    /// Read every well-formed envelope in the log. Skips trailing
    /// partial lines (a crash mid-write leaves these). Returns
    /// envelopes in append order.
    pub async fn read_all(&self) -> Result<Vec<AuditEnvelope>> {
        let file = match OpenOptions::new().read(true).open(&self.path).await {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => {
                return Err(anyhow::Error::new(e)
                    .context(format!("opening audit log {}", self.path.display())));
            }
        };
        let mut out = Vec::new();
        let mut lines = BufReader::new(file).lines();
        while let Some(line) = lines.next_line().await? {
            if line.is_empty() {
                continue;
            }
            // Skip lines that don't deserialise — typically a partial
            // tail from a crash. Log a warning so the operator knows.
            match serde_json::from_str::<AuditEnvelope>(&line) {
                Ok(env) => out.push(env),
                Err(e) => {
                    tracing::warn!(
                        path = %self.path.display(),
                        error = %e,
                        "skipping malformed audit line"
                    );
                }
            }
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::OsRng;
    use tempfile::TempDir;

    fn fresh_event() -> AuditEvent {
        AuditEvent {
            event_id: Uuid::new_v4(),
            timestamp: Utc::now(),
            kind: EventKind::RequestReceived,
            source_node_id: "node-tokyo-01".into(),
            source_org_id: "org-tokyo".into(),
            target_node_id: Some("node-munich-01".into()),
            target_org_id: Some("org-munich".into()),
            action: "inference.submit".into(),
            resource: "node://munich-01/gpu-0".into(),
            decision: AuditDecision::Allowed {
                obligations: vec!["audit_log".into()],
            },
            correlation: Some(Uuid::new_v4()),
            extra: serde_json::json!({"model": "llama-3-70b"}),
        }
    }

    // ── Sign / verify ──────────────────────────────────────────────

    #[test]
    fn sign_and_verify_roundtrip() {
        let key = SigningKey::generate(&mut OsRng);
        let event = fresh_event();
        let env = AuditEnvelope::sign(event.clone(), "node-tokyo-01", &key).unwrap();
        env.verify_signature().unwrap();

        // Embedded pubkey should match the signing key's public.
        assert_eq!(
            env.signer_pubkey_hex,
            hex::encode(key.verifying_key().to_bytes())
        );
    }

    #[test]
    fn verify_with_expected_pubkey_passes_for_correct_key() {
        let key = SigningKey::generate(&mut OsRng);
        let env = AuditEnvelope::sign(fresh_event(), "node-tokyo-01", &key).unwrap();
        env.verify_with(&key.verifying_key()).unwrap();
    }

    #[test]
    fn verify_with_expected_pubkey_fails_for_wrong_key() {
        let key = SigningKey::generate(&mut OsRng);
        let other = SigningKey::generate(&mut OsRng);
        let env = AuditEnvelope::sign(fresh_event(), "node-tokyo-01", &key).unwrap();
        let r = env.verify_with(&other.verifying_key());
        assert!(matches!(r, Err(AuditError::BadSignature)));
    }

    #[test]
    fn tampered_event_fails_verification() {
        let key = SigningKey::generate(&mut OsRng);
        let mut env = AuditEnvelope::sign(fresh_event(), "node-tokyo-01", &key).unwrap();
        // Flip the action verb after signing.
        env.event.action = "dataset.delete".into();
        let r = env.verify_signature();
        assert!(matches!(r, Err(AuditError::BadSignature)));
    }

    #[test]
    fn signer_mismatch_is_caught_even_with_valid_signature() {
        let key = SigningKey::generate(&mut OsRng);
        // Sign as "node-tokyo-01" but the event itself says
        // "node-tokyo-01" — so this is the happy path. Now tamper:
        let mut env = AuditEnvelope::sign(fresh_event(), "node-tokyo-01", &key).unwrap();
        env.signer_node_id = "node-attacker-01".into();
        // Note: we can't re-sign here; the signature is still over
        // the original event bytes, so it would still verify
        // crypto-wise — but the signer-identity check catches it.
        let r = env.verify_signature();
        assert!(matches!(r, Err(AuditError::SignerMismatch { .. })));
    }

    #[test]
    fn malformed_pubkey_hex_returns_specific_error() {
        let key = SigningKey::generate(&mut OsRng);
        let mut env = AuditEnvelope::sign(fresh_event(), "node-tokyo-01", &key).unwrap();
        env.signer_pubkey_hex = "not-hex".into();
        assert!(matches!(
            env.verify_signature(),
            Err(AuditError::MalformedKey(_))
        ));
    }

    #[test]
    fn malformed_signature_hex_returns_specific_error() {
        let key = SigningKey::generate(&mut OsRng);
        let mut env = AuditEnvelope::sign(fresh_event(), "node-tokyo-01", &key).unwrap();
        env.signature_hex = "0011".into(); // valid hex, wrong length
        assert!(matches!(
            env.verify_signature(),
            Err(AuditError::MalformedSignature(_))
        ));
    }

    // ── On-disk log ────────────────────────────────────────────────

    #[tokio::test]
    async fn append_and_read_back_in_order() {
        let tmp = TempDir::new().unwrap();
        let log = AuditLog::new(tmp.path().join("audit.jsonl"));

        let key = SigningKey::generate(&mut OsRng);
        let mut envs = Vec::new();
        for _ in 0..3 {
            let e = AuditEnvelope::sign(fresh_event(), "node-tokyo-01", &key).unwrap();
            log.append(&e).await.unwrap();
            envs.push(e);
        }

        let read_back = log.read_all().await.unwrap();
        assert_eq!(read_back.len(), 3);
        for (a, b) in envs.iter().zip(read_back.iter()) {
            assert_eq!(a, b);
            b.verify_signature().unwrap();
        }
    }

    #[tokio::test]
    async fn read_all_on_missing_file_is_empty_not_error() {
        let tmp = TempDir::new().unwrap();
        let log = AuditLog::new(tmp.path().join("never_written.jsonl"));
        let read = log.read_all().await.unwrap();
        assert!(read.is_empty());
    }

    #[tokio::test]
    async fn malformed_trailing_line_is_skipped_not_fatal() {
        // Simulate a crash mid-append: well-formed line followed by
        // a partial junk line. Reader should return the good one.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("audit.jsonl");

        let key = SigningKey::generate(&mut OsRng);
        let log = AuditLog::new(&path);
        let env = AuditEnvelope::sign(fresh_event(), "node-tokyo-01", &key).unwrap();
        log.append(&env).await.unwrap();

        // Append junk that won't deserialise.
        let mut f = OpenOptions::new().append(true).open(&path).await.unwrap();
        f.write_all(b"{\"event\": \"truncated").await.unwrap();
        f.flush().await.unwrap();

        let read = log.read_all().await.unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0], env);
    }

    #[tokio::test]
    async fn jsonl_lines_each_round_trip_independently() {
        // Each line stands alone — a downstream consumer should be
        // able to grep one envelope out of the file and verify it.
        let tmp = TempDir::new().unwrap();
        let log = AuditLog::new(tmp.path().join("audit.jsonl"));
        let key = SigningKey::generate(&mut OsRng);
        let env = AuditEnvelope::sign(fresh_event(), "node-tokyo-01", &key).unwrap();
        log.append(&env).await.unwrap();

        let raw = tokio::fs::read_to_string(log.path()).await.unwrap();
        let line = raw.trim_end();
        assert!(!line.contains('\n'));
        let parsed: AuditEnvelope = serde_json::from_str(line).unwrap();
        parsed.verify_signature().unwrap();
        assert_eq!(parsed, env);
    }

    // ── Decision serde ─────────────────────────────────────────────

    #[test]
    fn decision_allowed_serialises_with_obligations() {
        let d = AuditDecision::Allowed {
            obligations: vec!["audit_log".into(), "require_mfa".into()],
        };
        let s = serde_json::to_string(&d).unwrap();
        assert!(s.contains("\"outcome\":\"allowed\""));
        assert!(s.contains("audit_log"));
    }

    #[test]
    fn decision_denied_serialises_with_reasons() {
        let d = AuditDecision::Denied {
            reasons: vec!["role missing".into()],
        };
        let s = serde_json::to_string(&d).unwrap();
        assert!(s.contains("\"outcome\":\"denied\""));
        assert!(s.contains("role missing"));
    }

    #[test]
    fn decision_no_opinion_serialises() {
        let d = AuditDecision::NoOpinion;
        let s = serde_json::to_string(&d).unwrap();
        assert!(s.contains("\"outcome\":\"no_opinion\""));
    }
}
