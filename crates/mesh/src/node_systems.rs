// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! Node systems manifest — what a mesh node *declares* it brings, and
//! trust-gated discovery over those declarations.
//!
//! Federation slice 1. A node joining the mesh declares the systems
//! (element/domain scope) and tools it offers. Other nodes discover
//! those tools through [`discoverable_tools`], whose output is exactly
//! the `available: &[String]` list `prism_tool_router`'s `search`
//! already consumes.
//!
//! **Authority firewall (ESA-critical invariant).** A manifest is a
//! CLAIM, never a GRANT. Declaration alone NEVER surfaces a tool:
//! [`discoverable_tools`] consults the caller-supplied `is_trusted`
//! predicate FIRST and unconditionally — that predicate is where the
//! existing trust spine (`federation::verify_peer`) plugs in. An
//! untrusted node contributes zero tools no matter what it declares.
//! This module deliberately does not call `verify_peer` itself: trust
//! is injected, so the firewall is testable without crypto and the
//! seam stays explicit. Wiring the manifest into `PeerNode` broadcast,
//! `ToolRouter`, and the `verify_peer` binding are later increments.
//!
//! Scope semantics mirror the elicitation gate
//! (`app/tools/elicitation.py` `ResearchSpec.covers`) so the
//! informed-autonomy gate and federation speak one vocabulary: an empty
//! query scope is covered by any manifest; otherwise the declared
//! systems must overlap the query scope.

use std::collections::HashSet;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::federation::{CrossOrgRequest, verify_peer};

/// What a node declares it brings to the mesh.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeSystemsManifest {
    pub node_id: Uuid,
    /// Declared system/domain scope — element symbols or free-form
    /// domain tags. Same vocabulary as the elicitation gate's
    /// `ResearchSpec.system`.
    pub declared_systems: Vec<String>,
    /// Tool names this node offers.
    pub tool_names: Vec<String>,
    pub declared_at: DateTime<Utc>,
}

impl NodeSystemsManifest {
    /// Build a manifest stamped at the current instant.
    pub fn new(node_id: Uuid, declared_systems: Vec<String>, tool_names: Vec<String>) -> Self {
        Self {
            node_id,
            declared_systems,
            tool_names,
            declared_at: Utc::now(),
        }
    }

    /// Whether this manifest's declared systems cover `query_scope`.
    ///
    /// Mirrors elicitation `ResearchSpec.covers`: an empty query scope
    /// is covered by ANY manifest (a scope-less discovery query sees all
    /// *trusted* nodes); otherwise at least one declared system must
    /// appear in the query scope.
    pub fn covers(&self, query_scope: &[String]) -> bool {
        if query_scope.is_empty() {
            return true;
        }
        let declared: HashSet<&str> = self.declared_systems.iter().map(String::as_str).collect();
        query_scope.iter().any(|q| declared.contains(q.as_str()))
    }
}

/// Trust-gated tool discovery across declared manifests.
///
/// Returns the de-duplicated tool names visible for `query_scope`,
/// drawn ONLY from manifests whose node passes `is_trusted`. The
/// trust check is the authority firewall: it runs first and
/// unconditionally, so an untrusted node is excluded regardless of what
/// it declares. Order is deterministic (manifest order, then tool
/// order; first occurrence wins) so the resulting `available` list is
/// stable for the tool router.
pub fn discoverable_tools(
    query_scope: &[String],
    manifests: &[NodeSystemsManifest],
    is_trusted: impl Fn(&NodeSystemsManifest) -> bool,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for m in manifests {
        // Authority firewall — declaration is not authority.
        if !is_trusted(m) {
            continue;
        }
        if !m.covers(query_scope) {
            continue;
        }
        for t in &m.tool_names {
            if !out.contains(t) {
                out.push(t.clone());
            }
        }
    }
    out
}

/// The real trust binding — the predicate to pass as `is_trusted` to
/// [`discoverable_tools`]. A manifest is trusted iff it is accompanied
/// by a `CrossOrgRequest` that (a) passes the trust spine
/// [`verify_peer`] and (b) originates from the SAME node the manifest
/// claims.
///
/// **Fail-closed.** No signed request ⇒ not trusted. The live
/// mDNS/Kafka announce path does not yet carry signed identities (the
/// pending "Bug #33" wiring), so over today's live mesh this correctly
/// returns `false` for everyone — federated tools surface only once
/// signed identities flow, never on unauthenticated declaration.
///
/// **Node binding.** A verified request for node A must not vouch for
/// node B's manifest. SEAM (flagged, unresolved): `verify_peer`'s
/// `NodeId` is the platform-minted identity string; the manifest's
/// `node_id` is the mesh `Uuid`. We require string equality of the
/// `Uuid` form. Until the platform↔mesh node-id correlation is settled,
/// production must mint `NodeId == mesh Uuid string` or this denies
/// (fail-closed, which is the safe direction).
pub fn manifest_trusted(
    manifest: &NodeSystemsManifest,
    request: Option<&CrossOrgRequest>,
    platform_root_pubkey: &VerifyingKey,
    required_role: Option<&str>,
    now: SystemTime,
) -> bool {
    let Some(req) = request else {
        return false; // fail-closed: declaration without identity is not authority
    };
    if verify_peer(req, platform_root_pubkey, required_role, now).is_err() {
        return false;
    }
    req.source.node_id == manifest.node_id.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(systems: &[&str], tools: &[&str]) -> NodeSystemsManifest {
        NodeSystemsManifest::new(
            Uuid::new_v4(),
            systems.iter().map(|s| s.to_string()).collect(),
            tools.iter().map(|s| s.to_string()).collect(),
        )
    }

    fn scope(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    // ── Real verify_peer trust binding ───────────────────────────────
    mod trust_binding {
        use super::*;
        use chrono::Duration;
        use ed25519_dalek::{Signer, SigningKey};
        use rand::rngs::OsRng;

        use crate::federation::{CrossOrgRequest, PeerIdentity};

        /// Mint a correctly-signed CrossOrgRequest whose identity claims
        /// node_id == `node`.to_string(). Returns (platform pubkey, req).
        fn signed_for(node: Uuid) -> (VerifyingKey, CrossOrgRequest) {
            let mut rng = OsRng;
            let platform_key = SigningKey::generate(&mut rng);
            let node_key = SigningKey::generate(&mut rng);
            let mut identity = PeerIdentity {
                org_id: "org-a".to_string(),
                project_id: Some("proj-1".to_string()),
                node_id: node.to_string(),
                node_pubkey_hex: hex::encode(node_key.verifying_key().to_bytes()),
                platform_signature_hex: String::new(),
                roles: vec!["compute.invoke".to_string()],
                valid_until: Utc::now() + Duration::hours(1),
            };
            let id_bytes = identity.signing_bytes().unwrap();
            identity.platform_signature_hex = hex::encode(platform_key.sign(&id_bytes).to_bytes());
            let req = CrossOrgRequest::sign(
                &node_key,
                identity,
                "org-b".to_string(),
                "inference.submit",
                "node://b/gpu-0",
                serde_json::json!({"k": "v"}),
            )
            .unwrap();
            (platform_key.verifying_key(), req)
        }

        #[test]
        fn no_signed_request_is_fail_closed() {
            // The live mesh reality today: announces carry no identity.
            let m = manifest(&["Cu"], &["mace_relax"]);
            let pk = SigningKey::generate(&mut OsRng).verifying_key();
            assert!(!manifest_trusted(&m, None, &pk, None, SystemTime::now()));
            let got = discoverable_tools(&scope(&["Cu"]), std::slice::from_ref(&m), |mm| {
                manifest_trusted(mm, None, &pk, None, SystemTime::now())
            });
            assert!(got.is_empty(), "fail-closed must surface nothing");
        }

        #[test]
        fn verified_request_for_same_node_makes_tools_discoverable() {
            let node = Uuid::new_v4();
            let (pk, req) = signed_for(node);
            let m = NodeSystemsManifest::new(node, vec!["Cu".into()], vec!["mace_relax".into()]);
            assert!(manifest_trusted(
                &m,
                Some(&req),
                &pk,
                None,
                SystemTime::now()
            ));
            let got = discoverable_tools(&scope(&["Cu"]), &[m], |mm| {
                manifest_trusted(mm, Some(&req), &pk, None, SystemTime::now())
            });
            assert_eq!(got, vec!["mace_relax".to_string()]);
        }

        #[test]
        fn verified_request_for_other_node_does_not_vouch() {
            // Authority: a valid identity for node A cannot surface
            // node B's declared tools.
            let (pk, req_a) = signed_for(Uuid::new_v4());
            let m_b = NodeSystemsManifest::new(
                Uuid::new_v4(),
                vec!["Cu".into()],
                vec!["mace_relax".into()],
            );
            assert!(!manifest_trusted(
                &m_b,
                Some(&req_a),
                &pk,
                None,
                SystemTime::now()
            ));
        }

        #[test]
        fn tampered_request_is_denied() {
            let node = Uuid::new_v4();
            let (pk, mut req) = signed_for(node);
            req.payload = serde_json::json!({"k": "EVIL"}); // breaks request sig
            let m = NodeSystemsManifest::new(node, vec!["Cu".into()], vec!["t".into()]);
            assert!(!manifest_trusted(
                &m,
                Some(&req),
                &pk,
                None,
                SystemTime::now()
            ));
        }

        #[test]
        fn wrong_platform_root_is_denied() {
            let node = Uuid::new_v4();
            let (_real_pk, req) = signed_for(node);
            let attacker_pk = SigningKey::generate(&mut OsRng).verifying_key();
            let m = NodeSystemsManifest::new(node, vec!["Cu".into()], vec!["t".into()]);
            assert!(!manifest_trusted(
                &m,
                Some(&req),
                &attacker_pk,
                None,
                SystemTime::now()
            ));
        }
    }

    #[test]
    fn untrusted_node_surfaces_zero_tools_even_when_it_declares_a_match() {
        // The whole point: declaration is a claim, not a grant.
        let m = manifest(&["Cu", "Ni"], &["mace_relax", "alloy_discovery"]);
        let got = discoverable_tools(&scope(&["Cu"]), &[m], |_| false);
        assert!(got.is_empty(), "untrusted node leaked tools: {got:?}");
    }

    #[test]
    fn trusted_node_with_overlapping_scope_surfaces_its_tools() {
        let m = manifest(&["Cu", "Ni", "Si"], &["mace_relax"]);
        let got = discoverable_tools(&scope(&["Si"]), &[m], |_| true);
        assert_eq!(got, vec!["mace_relax".to_string()]);
    }

    #[test]
    fn trusted_node_with_disjoint_scope_is_excluded() {
        let m = manifest(&["Fe", "Cr"], &["mace_relax"]);
        let got = discoverable_tools(&scope(&["Cu"]), &[m], |_| true);
        assert!(got.is_empty());
    }

    #[test]
    fn empty_query_scope_sees_all_trusted_nodes_but_still_excludes_untrusted() {
        let trusted = manifest(&["Cu"], &["tool_a"]);
        let untrusted = manifest(&["Cu"], &["tool_b"]);
        let untrusted_id = untrusted.node_id;
        let got = discoverable_tools(&[], &[trusted, untrusted], |m| m.node_id != untrusted_id);
        assert_eq!(got, vec!["tool_a".to_string()]);
    }

    #[test]
    fn tools_are_deduplicated_across_trusted_nodes_order_stable() {
        let a = manifest(&["Cu"], &["shared", "only_a"]);
        let b = manifest(&["Cu"], &["shared", "only_b"]);
        let got = discoverable_tools(&scope(&["Cu"]), &[a, b], |_| true);
        assert_eq!(
            got,
            vec![
                "shared".to_string(),
                "only_a".to_string(),
                "only_b".to_string()
            ]
        );
    }

    #[test]
    fn covers_matches_elicitation_semantics() {
        let m = manifest(&["Cu", "Ni"], &["t"]);
        assert!(m.covers(&[]), "empty scope must be covered by any manifest");
        assert!(m.covers(&scope(&["Ni"])), "overlap must be covered");
        assert!(!m.covers(&scope(&["Fe"])), "disjoint must not be covered");
    }
}
