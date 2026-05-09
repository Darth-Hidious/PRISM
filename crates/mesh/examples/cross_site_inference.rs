// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! F6 — cross-site inference demo (PRISM Fabric v1 done bar).
//!
//! Wires every Fabric layer end-to-end in one process so a reader can
//! follow the path from "tokyo asks munich to run inference" to "tokyo
//! has a signed proof that munich did the work":
//!
//! ```text
//! ┌────────────── org-tokyo ──────────────┐    ┌──────────── org-munich ─────────────┐
//! │                                       │    │                                     │
//! │  1. build CrossOrgRequest             │    │  3. verify_peer (F1)                │
//! │  2. emit AuditEnvelope:Dispatched ────┼───►│  4. action→role check (F1c3)        │
//! │                            (F5)       │    │  5. policy intersection (F2)        │
//! │                                       │    │  6. emit AuditEnvelope:Received     │
//! │                                       │    │  7. burst route (F3+F4)             │
//! │                                       │    │  8. simulate work                   │
//! │  10. verify chain of envelopes  ◄─────┼────┤  9. emit AuditEnvelope:Completed    │
//! │                                       │    │                                     │
//! └───────────────────────────────────────┘    └─────────────────────────────────────┘
//! ```
//!
//! Run:
//!   cargo run --example cross_site_inference -p prism-mesh
//!
//! What's "real" vs "simulated":
//!
//! - **Real**: identity signing, request signing, action→role gating,
//!   policy intersection, locality scoring, burst-router decision,
//!   audit envelope signing+verification on both sides.
//! - **Simulated**: the inference itself (returns a fixed string),
//!   the platform pubkey (we mint our own keypair instead of fetching
//!   from `api.marc27.com`), and the transport (both orgs run in the
//!   same process; in production this hop crosses the federation
//!   transport in `mesh::subscription`).
//!
//! That split is on purpose. F6 is the v1 done bar for the trust /
//! routing / audit stack — not a production inference platform. F6
//! v1.5 is the same example split across two processes; F6 v2 across
//! two machines via the real federation transport.

use anyhow::Result;
use chrono::Utc;
use ed25519_dalek::{Signer, SigningKey};
use prism_audit::{AuditDecision, AuditEnvelope, AuditEvent, AuditLog, EventKind};
use prism_mesh::burst_routing::{BurstRouter, Candidate, ResourceRequirement, RouteDecision};
use prism_mesh::federation::{CrossOrgRequest, PeerIdentity, verify_peer};
use prism_mesh::federation_lookup::ActionRoleTable;
use prism_mesh::locality::{CandidateLocality, LatencyClass, LocalityHint};
use prism_policy::intersect_decisions;
use prism_policy::{PolicyDecision, PolicyEngine, PolicyInput};
use prism_proto::{GpuInfo, ModelInfo, NodeCapabilities};
use rand::rngs::OsRng;
use std::collections::BTreeMap;
use std::time::SystemTime;
use uuid::Uuid;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    println!("╭─ PRISM Fabric v1 — cross-site inference demo ───────────╮");
    println!();

    // ─────────────────────────────────────────────────────────────
    // SETUP: keys + identities (would normally come from `prism login`)
    // ─────────────────────────────────────────────────────────────

    let mut rng = OsRng;
    let platform_key = SigningKey::generate(&mut rng);

    let tokyo_node_key = SigningKey::generate(&mut rng);
    let tokyo_id = mint_identity(&platform_key, &tokyo_node_key, "org-tokyo", "node-tokyo-01");

    let munich_node_key = SigningKey::generate(&mut rng);
    let munich_id = mint_identity(
        &platform_key,
        &munich_node_key,
        "org-munich",
        "node-munich-01",
    );

    println!("  setup: platform key minted, two node identities issued");
    println!(
        "    org-tokyo  → node-tokyo-01   (roles: {:?})",
        tokyo_id.roles
    );
    println!(
        "    org-munich → node-munich-01  (roles: {:?})",
        munich_id.roles
    );
    println!();

    // Two audit logs in tempdirs so we can show both orgs' chain at the end.
    let scratch = tempfile::tempdir()?;
    let tokyo_log = AuditLog::new(scratch.path().join("audit-tokyo.jsonl"));
    let munich_log = AuditLog::new(scratch.path().join("audit-munich.jsonl"));

    // ─────────────────────────────────────────────────────────────
    // STEP 1+2: tokyo builds + signs a CrossOrgRequest, emits dispatch audit
    // ─────────────────────────────────────────────────────────────

    println!("─ tokyo ─────────────────────────────────────────────────");
    let request = CrossOrgRequest::sign(
        &tokyo_node_key,
        tokyo_id.clone(),
        "org-munich".to_string(),
        "inference.submit",
        "node://munich-01/gpu-0",
        serde_json::json!({"model": "llama-3-70b-instruct", "prompt": "hello fabric"}),
    )?;
    println!("  1. signed CrossOrgRequest {}", request.request_id);

    let dispatch = AuditEnvelope::sign(
        AuditEvent {
            event_id: Uuid::new_v4(),
            timestamp: Utc::now(),
            kind: EventKind::RequestDispatched,
            source_node_id: tokyo_id.node_id.clone(),
            source_org_id: tokyo_id.org_id.clone(),
            target_node_id: Some(munich_id.node_id.clone()),
            target_org_id: Some(munich_id.org_id.clone()),
            action: request.action.clone(),
            resource: request.resource.clone(),
            decision: AuditDecision::NoOpinion,
            correlation: Some(request.request_id),
            extra: serde_json::Value::Null,
        },
        &tokyo_id.node_id,
        &tokyo_node_key,
    )?;
    tokyo_log.append(&dispatch).await?;
    println!("  2. audit:RequestDispatched signed + appended to tokyo log");
    println!();

    // ─────────────────────────────────────────────────────────────
    // STEP 3-4: munich verifies identity + role
    // ─────────────────────────────────────────────────────────────

    println!("─ munich ────────────────────────────────────────────────");
    let role_table = ActionRoleTable::defaults();
    let required_role = role_table.required_role(&request.action);
    verify_peer(
        &request,
        &platform_key.verifying_key(),
        required_role,
        SystemTime::now(),
    )
    .map_err(|e| anyhow::anyhow!("verify_peer failed: {e}"))?;
    println!("  3. verify_peer OK (platform sig valid, request sig valid, expiry OK)");
    println!(
        "  4. action→role: {:?} requires {:?}, peer holds {:?}",
        request.action, required_role, request.source.roles,
    );

    // ─────────────────────────────────────────────────────────────
    // STEP 5: policy intersection (tokyo's policy + munich's policy)
    // ─────────────────────────────────────────────────────────────

    // Both orgs' admin principals signed off on the federation
    // agreement out-of-band; per-request the policy engine sees it
    // as an admin-authorised action. (A real deployment would have
    // an `inference.submit` allow-rule on the receiving side; this
    // demo uses the default Rego unchanged to keep moving parts low.)
    let tokyo_decision = run_local_policy(&request, "alice@org-tokyo", "admin")?;
    let munich_decision = run_local_policy(&request, "fabric@org-munich", "admin")?;
    let combined = intersect_decisions(&[tokyo_decision.clone(), munich_decision.clone()]);
    println!(
        "  5. policy intersection: tokyo={} + munich={} → combined={}",
        if tokyo_decision.allowed {
            "allow"
        } else {
            "deny"
        },
        if munich_decision.allowed {
            "allow"
        } else {
            "deny"
        },
        if combined.allowed { "allow" } else { "deny" },
    );
    if !combined.allowed {
        anyhow::bail!("cross-org policy denied: {}", combined.reason);
    }
    println!("     obligations: {:?}", combined.obligations);

    // ─────────────────────────────────────────────────────────────
    // STEP 6: munich emits RequestReceived audit
    // ─────────────────────────────────────────────────────────────

    let received = AuditEnvelope::sign(
        AuditEvent {
            event_id: Uuid::new_v4(),
            timestamp: Utc::now(),
            kind: EventKind::RequestReceived,
            source_node_id: munich_id.node_id.clone(),
            source_org_id: munich_id.org_id.clone(),
            target_node_id: Some(tokyo_id.node_id.clone()),
            target_org_id: Some(tokyo_id.org_id.clone()),
            action: request.action.clone(),
            resource: request.resource.clone(),
            decision: AuditDecision::Allowed {
                obligations: combined.obligations.clone(),
            },
            correlation: Some(request.request_id),
            extra: serde_json::Value::Null,
        },
        &munich_id.node_id,
        &munich_node_key,
    )?;
    munich_log.append(&received).await?;
    println!("  6. audit:RequestReceived signed + appended to munich log");

    // ─────────────────────────────────────────────────────────────
    // STEP 7: burst route — pick a candidate node
    // ─────────────────────────────────────────────────────────────

    let req_caps = ResourceRequirement {
        min_gpu_vram_gb: Some(80),
        gpu_class: Some("A100".into()),
        model_required: Some("llama-3-70b".into()),
        locality: Some(LocalityHint {
            region: Some("eu-central-1".into()),
            latency_class: Some(LatencyClass::LowLatency),
            data_residency_required: Some("EU".into()),
            ..Default::default()
        }),
        ..Default::default()
    };

    // Local-org pool: just the receiving munich node.
    let local_pool = vec![Candidate {
        node_id: munich_id.node_id.clone(),
        org_id: munich_id.org_id.clone(),
        capabilities: gpu_node_caps("A100", 1, 80, &["llama-3-70b-instruct"]),
        locality: CandidateLocality {
            region: Some("eu-central-1".into()),
            zone: Some("eu-central-1a".into()),
            data_residency: Some("EU".into()),
        },
    }];

    match BurstRouter::default().route(&req_caps, &local_pool, &[]) {
        RouteDecision::Local { node_id } => {
            println!("  7. burst router: Local → {node_id}");
        }
        other => anyhow::bail!("expected Local route, got {other:?}"),
    }

    // ─────────────────────────────────────────────────────────────
    // STEP 8-9: simulate work, emit completion audit
    // ─────────────────────────────────────────────────────────────

    println!("  8. running inference (simulated) … ");
    let result = "Hello! This is a mock inference response from llama-3-70b.".to_string();

    let completed = AuditEnvelope::sign(
        AuditEvent {
            event_id: Uuid::new_v4(),
            timestamp: Utc::now(),
            kind: EventKind::WorkCompleted,
            source_node_id: munich_id.node_id.clone(),
            source_org_id: munich_id.org_id.clone(),
            target_node_id: Some(tokyo_id.node_id.clone()),
            target_org_id: Some(tokyo_id.org_id.clone()),
            action: request.action.clone(),
            resource: request.resource.clone(),
            decision: AuditDecision::NoOpinion,
            correlation: Some(request.request_id),
            extra: serde_json::json!({
                "tokens_generated": result.split_whitespace().count(),
                "model": "llama-3-70b-instruct",
            }),
        },
        &munich_id.node_id,
        &munich_node_key,
    )?;
    munich_log.append(&completed).await?;
    println!("  9. audit:WorkCompleted signed + appended to munich log");
    println!();

    // ─────────────────────────────────────────────────────────────
    // STEP 10: tokyo verifies the audit chain it received
    // ─────────────────────────────────────────────────────────────

    println!("─ tokyo ─────────────────────────────────────────────────");
    println!("  10. verifying munich's audit chain:");
    let munich_envelopes = munich_log.read_all().await?;
    let munich_pubkey = munich_id.node_verifying_key().unwrap();
    for env in &munich_envelopes {
        env.verify_with(&munich_pubkey)
            .map_err(|e| anyhow::anyhow!("munich envelope failed verification: {e}"))?;
        println!(
            "      ✓ {:?}  event_id={}  correlated to request_id={}",
            env.event.kind,
            env.event.event_id,
            env.event.correlation.unwrap(),
        );
    }
    println!();

    println!("─ result ────────────────────────────────────────────────");
    println!("  inference output: {result:?}");
    println!();
    println!("  audit chain (tokyo log → munich log):");
    let tokyo_envelopes = tokyo_log.read_all().await?;
    for env in tokyo_envelopes.iter().chain(munich_envelopes.iter()) {
        println!(
            "    [{}] {:>21}  by {:>15} ({})",
            env.event.timestamp.format("%H:%M:%S%.3f"),
            format!("{:?}", env.event.kind),
            env.event.source_node_id,
            env.event.source_org_id,
        );
    }

    println!();
    println!("╰─ done. F6 v1 bar cleared. ───────────────────────────────╯");
    Ok(())
}

// ────────────────────────────────────────────────────────────────────
// Helpers
// ────────────────────────────────────────────────────────────────────

/// Mint a `PeerIdentity` for a node by having the platform sign over
/// the identity claim. In production this happens during `prism login`.
fn mint_identity(
    platform_key: &SigningKey,
    node_key: &SigningKey,
    org_id: &str,
    node_id: &str,
) -> PeerIdentity {
    let mut id = PeerIdentity {
        org_id: org_id.into(),
        project_id: Some(format!("proj-{org_id}")),
        node_id: node_id.into(),
        node_pubkey_hex: hex::encode(node_key.verifying_key().to_bytes()),
        platform_signature_hex: String::new(),
        roles: vec!["compute.invoke".to_string(), "data.read".to_string()],
        valid_until: Utc::now() + chrono::Duration::hours(1),
    };
    let bytes = id.signing_bytes().expect("identity serialises");
    let sig = platform_key.sign(&bytes);
    id.platform_signature_hex = hex::encode(sig.to_bytes());
    id
}

/// Run the *local* policy engine for a given principal+role and return
/// that org's standalone decision. The intersection step folds these
/// per-org decisions into one cross-org decision.
fn run_local_policy(
    request: &CrossOrgRequest,
    principal: &str,
    role: &str,
) -> Result<PolicyDecision> {
    let mut engine = PolicyEngine::new()?;
    let decision = engine.evaluate(&PolicyInput {
        action: request.action.clone(),
        principal: principal.into(),
        role: role.into(),
        resource: request.resource.clone(),
        context: request.payload.clone(),
    })?;
    Ok(decision)
}

/// A populated `NodeCapabilities` representing a fat-GPU compute node.
fn gpu_node_caps(gpu_type: &str, count: u32, vram_gb: u32, models: &[&str]) -> NodeCapabilities {
    NodeCapabilities {
        gpus: vec![GpuInfo {
            gpu_type: gpu_type.into(),
            count,
            vram_gb,
        }],
        cpu_cores: 96,
        ram_gb: 512,
        disk_gb: 4096,
        software: vec!["cuda-12.6".into(), "vllm".into()],
        container_runtime: Some("docker".into()),
        docker: true,
        scheduler: None,
        labels: BTreeMap::new(),
        storage_available_gb: 2048,
        datasets: vec![],
        models: models
            .iter()
            .map(|n| ModelInfo {
                name: (*n).into(),
                path: format!("/models/{n}"),
                format: Some("safetensors".into()),
                size_gb: Some(140.0),
            })
            .collect(),
        services: vec![],
        visibility: "private".into(),
        price_per_hour_usd: Some(2.50),
        public_key: None,
    }
}
