// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! F4 — capability descriptors + burst routing.
//!
//! Decides *which* node should run a compute request when the local
//! org doesn't have a node that fits, and the request can spill over
//! to a peer org via the cross-org federation path (F1, F2).
//!
//! The pieces:
//!
//! - [`ResourceRequirement`] — what the request needs (RAM floor,
//!   GPU class, optional model preload).
//! - [`matches_requirement`] — pure check: does a single
//!   [`NodeCapabilities`] satisfy a requirement?
//! - [`BurstRouter::route`] — picks the best candidate from
//!   `local` first, falling back to `peers` if local has nothing.
//!   Combines this matcher with [`super::locality`] for ordering.
//!
//! # What this module deliberately does NOT do
//!
//! - **Live load tracking.** v1 filters by static capability only.
//!   Adding a "current load" field to the descriptor and excluding
//!   nodes above a threshold is a v1.5 layer.
//! - **HTTP dispatch.** Once a route is picked, sending the bytes is
//!   F6 (cross-site inference demo) using the cross-org transport.
//! - **Cost / pricing.** `NodeCapabilities` already has
//!   `price_per_hour_usd`; cost-aware routing is its own concern.

use prism_proto::NodeCapabilities;
use serde::{Deserialize, Serialize};

use crate::locality::{
    CandidateLocality, LocalityHint, LocalityScore, LocalityWeights, score_candidate,
};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// What a compute request needs to be satisfiable.
///
/// All fields optional; an empty `ResourceRequirement` matches any
/// node. Fields with constraints ALL have to be satisfied — this is
/// AND, not OR.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResourceRequirement {
    /// Minimum RAM in GB. Hard requirement.
    pub min_ram_gb: Option<u64>,
    /// Minimum total GPU VRAM (sum across GPUs) in GB. Hard requirement.
    pub min_gpu_vram_gb: Option<u32>,
    /// GPU class substring (e.g. `"A100"`, `"H100"`). The candidate
    /// must have at least one GPU whose `gpu_type` contains this
    /// substring (case-insensitive).
    pub gpu_class: Option<String>,
    /// Required pre-loaded model name (substring match, case-insensitive).
    /// Pulling a model on demand is allowed but slow; if the request
    /// names a specific model and it's already loaded somewhere,
    /// prefer that node.
    pub model_required: Option<String>,
    /// Optional locality preferences (passed through to F3 scoring).
    pub locality: Option<LocalityHint>,
}

/// A candidate node visible to the router. Wraps the platform-issued
/// capability descriptor with the org/node identity needed to address
/// it.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub node_id: String,
    pub org_id: String,
    pub capabilities: NodeCapabilities,
    pub locality: CandidateLocality,
}

/// Where the router decided to run the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteDecision {
    /// Run locally on this node.
    Local { node_id: String },
    /// Send to a peer org's node via the cross-org federation path.
    Burst { org_id: String, node_id: String },
    /// No node — local or peer — satisfies the requirement.
    NoCapacity,
}

// ---------------------------------------------------------------------------
// Matching
// ---------------------------------------------------------------------------

/// Returns true iff `caps` satisfies every constraint in `req`.
///
/// Empty constraints pass through; this lets `ResourceRequirement::default()`
/// behave as "anything goes."
pub fn matches_requirement(req: &ResourceRequirement, caps: &NodeCapabilities) -> bool {
    if let Some(min) = req.min_ram_gb
        && caps.ram_gb < min
    {
        return false;
    }

    if let Some(min) = req.min_gpu_vram_gb {
        let total: u32 = caps.gpus.iter().map(|g| g.vram_gb * g.count).sum();
        if total < min {
            return false;
        }
    }

    if let Some(class) = &req.gpu_class {
        let class_lc = class.to_ascii_lowercase();
        let any = caps
            .gpus
            .iter()
            .any(|g| g.gpu_type.to_ascii_lowercase().contains(&class_lc));
        if !any {
            return false;
        }
    }

    if let Some(model) = &req.model_required {
        let model_lc = model.to_ascii_lowercase();
        let has = caps
            .models
            .iter()
            .any(|m| m.name.to_ascii_lowercase().contains(&model_lc));
        if !has {
            return false;
        }
    }

    true
}

// ---------------------------------------------------------------------------
// Burst routing
// ---------------------------------------------------------------------------

/// Picks the best candidate to satisfy a request, preferring local
/// org over peer orgs.
///
/// The decision rule:
///
/// 1. Filter `local` candidates by [`matches_requirement`]. If any
///    pass, sort by locality score and return the best as
///    `RouteDecision::Local`.
/// 2. Otherwise, filter `peers` the same way. If any pass, return
///    the best as `RouteDecision::Burst`.
/// 3. Otherwise, [`RouteDecision::NoCapacity`].
///
/// "Local" vs "peer" is the caller's split — typically by
/// `org_id == self_org`.
#[derive(Default)]
pub struct BurstRouter {
    weights: LocalityWeights,
}

impl BurstRouter {
    pub fn new(weights: LocalityWeights) -> Self {
        Self { weights }
    }

    pub fn route(
        &self,
        req: &ResourceRequirement,
        local: &[Candidate],
        peers: &[Candidate],
    ) -> RouteDecision {
        if let Some(best) = self.best_match(req, local) {
            return RouteDecision::Local {
                node_id: best.node_id.clone(),
            };
        }
        if let Some(best) = self.best_match(req, peers) {
            return RouteDecision::Burst {
                org_id: best.org_id.clone(),
                node_id: best.node_id.clone(),
            };
        }
        RouteDecision::NoCapacity
    }

    /// Best capacity-matching candidate, ranked by locality score.
    /// Returns `None` if no candidate satisfies the requirement.
    fn best_match<'a>(
        &self,
        req: &ResourceRequirement,
        pool: &'a [Candidate],
    ) -> Option<&'a Candidate> {
        let hint = req.locality.clone().unwrap_or_default();

        // Filter to capacity-satisfying, residency-passing candidates,
        // then score by locality. Residency is a HARD constraint and
        // is checked here explicitly — relying on `score_candidate`'s
        // zero return would incorrectly drop residency-passing
        // candidates that simply have no positive locality match.
        let mut scored: Vec<(&Candidate, LocalityScore)> = pool
            .iter()
            .filter(|c| matches_requirement(req, &c.capabilities))
            .filter(|c| passes_residency(&hint, &c.locality))
            .map(|c| {
                let s = score_candidate(&hint, &c.locality, &self.weights);
                (c, s)
            })
            .collect();

        // Stable descending sort by score; ties keep input order so
        // caller-supplied tiebreakers (round-robin pinning) work.
        scored.sort_by_key(|(_, s)| std::cmp::Reverse(s.0));
        scored.first().map(|(c, _)| *c)
    }
}

/// Whether `cand` passes the hint's residency hard constraint.
///
/// Mirrors the residency check in [`super::locality::score_candidate`]
/// but exposes it as a boolean so the router can distinguish "excluded
/// by residency" from "no positive locality match" — both of which
/// produce a score of 0 in the additive scoring path.
fn passes_residency(hint: &LocalityHint, cand: &CandidateLocality) -> bool {
    match &hint.data_residency_required {
        None => true,
        Some(required) => cand.data_residency.as_deref() == Some(required.as_str()),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use prism_proto::{GpuInfo, ModelInfo};
    use std::collections::BTreeMap;

    fn empty_caps() -> NodeCapabilities {
        NodeCapabilities {
            gpus: vec![],
            cpu_cores: 0,
            ram_gb: 0,
            disk_gb: 0,
            software: vec![],
            container_runtime: None,
            docker: false,
            scheduler: None,
            labels: BTreeMap::new(),
            storage_available_gb: 0,
            datasets: vec![],
            models: vec![],
            services: vec![],
            visibility: "public".into(),
            price_per_hour_usd: None,
            public_key: None,
        }
    }

    fn caps_with(ram_gb: u64, gpus: Vec<GpuInfo>, models: Vec<&str>) -> NodeCapabilities {
        NodeCapabilities {
            ram_gb,
            gpus,
            models: models
                .into_iter()
                .map(|n| ModelInfo {
                    name: n.to_string(),
                    path: format!("/models/{n}"),
                    format: None,
                    size_gb: None,
                })
                .collect(),
            ..empty_caps()
        }
    }

    fn gpu(t: &str, count: u32, vram_gb: u32) -> GpuInfo {
        GpuInfo {
            gpu_type: t.to_string(),
            count,
            vram_gb,
        }
    }

    fn locality(region: Option<&str>, residency: Option<&str>) -> CandidateLocality {
        CandidateLocality {
            region: region.map(str::to_string),
            zone: None,
            data_residency: residency.map(str::to_string),
        }
    }

    fn candidate(
        node_id: &str,
        org_id: &str,
        caps: NodeCapabilities,
        loc: CandidateLocality,
    ) -> Candidate {
        Candidate {
            node_id: node_id.into(),
            org_id: org_id.into(),
            capabilities: caps,
            locality: loc,
        }
    }

    // ── matches_requirement ────────────────────────────────────────

    #[test]
    fn empty_requirement_matches_anything() {
        assert!(matches_requirement(
            &ResourceRequirement::default(),
            &empty_caps(),
        ));
    }

    #[test]
    fn ram_floor_excludes_under_capacity() {
        let req = ResourceRequirement {
            min_ram_gb: Some(64),
            ..Default::default()
        };
        assert!(!matches_requirement(&req, &caps_with(32, vec![], vec![])));
        assert!(matches_requirement(&req, &caps_with(128, vec![], vec![])));
    }

    #[test]
    fn gpu_vram_sums_across_gpus() {
        // 4× 16GB cards = 64GB total
        let caps = caps_with(0, vec![gpu("A100", 4, 16)], vec![]);
        let req = ResourceRequirement {
            min_gpu_vram_gb: Some(64),
            ..Default::default()
        };
        assert!(matches_requirement(&req, &caps));
        let req_too_big = ResourceRequirement {
            min_gpu_vram_gb: Some(128),
            ..Default::default()
        };
        assert!(!matches_requirement(&req_too_big, &caps));
    }

    #[test]
    fn gpu_class_match_is_case_insensitive_substring() {
        let caps = caps_with(0, vec![gpu("NVIDIA A100", 1, 80)], vec![]);
        for class in ["a100", "A100", "NVIDIA"] {
            let req = ResourceRequirement {
                gpu_class: Some(class.into()),
                ..Default::default()
            };
            assert!(
                matches_requirement(&req, &caps),
                "should match '{class}' against 'NVIDIA A100'"
            );
        }
        let no_match = ResourceRequirement {
            gpu_class: Some("H100".into()),
            ..Default::default()
        };
        assert!(!matches_requirement(&no_match, &caps));
    }

    #[test]
    fn model_required_substring_match_case_insensitive() {
        let caps = caps_with(0, vec![], vec!["llama-3-70b-instruct"]);
        let req = ResourceRequirement {
            model_required: Some("Llama-3".into()),
            ..Default::default()
        };
        assert!(matches_requirement(&req, &caps));
        let miss = ResourceRequirement {
            model_required: Some("mistral".into()),
            ..Default::default()
        };
        assert!(!matches_requirement(&miss, &caps));
    }

    // ── BurstRouter::route ─────────────────────────────────────────

    #[test]
    fn route_picks_local_when_local_satisfies() {
        let req = ResourceRequirement {
            min_ram_gb: Some(64),
            ..Default::default()
        };
        let local = vec![candidate(
            "n-local",
            "org-self",
            caps_with(128, vec![], vec![]),
            locality(Some("eu-central-1"), None),
        )];
        let peers = vec![candidate(
            "n-peer",
            "org-other",
            caps_with(256, vec![], vec![]),
            locality(Some("eu-central-1"), None),
        )];
        let r = BurstRouter::default().route(&req, &local, &peers);
        assert_eq!(
            r,
            RouteDecision::Local {
                node_id: "n-local".into()
            }
        );
    }

    #[test]
    fn route_bursts_when_local_lacks_capacity() {
        let req = ResourceRequirement {
            min_gpu_vram_gb: Some(80),
            ..Default::default()
        };
        // Local has CPU only.
        let local = vec![candidate(
            "n-local",
            "org-self",
            caps_with(128, vec![], vec![]),
            locality(Some("eu-central-1"), None),
        )];
        // Peer has a fat GPU.
        let peers = vec![candidate(
            "n-peer",
            "org-other",
            caps_with(64, vec![gpu("A100", 1, 80)], vec![]),
            locality(Some("eu-central-1"), None),
        )];
        let r = BurstRouter::default().route(&req, &local, &peers);
        assert_eq!(
            r,
            RouteDecision::Burst {
                org_id: "org-other".into(),
                node_id: "n-peer".into()
            }
        );
    }

    #[test]
    fn route_returns_no_capacity_when_nobody_satisfies() {
        let req = ResourceRequirement {
            min_ram_gb: Some(1024),
            ..Default::default()
        };
        let local = vec![candidate(
            "n-local",
            "org-self",
            caps_with(64, vec![], vec![]),
            locality(None, None),
        )];
        let peers = vec![candidate(
            "n-peer",
            "org-other",
            caps_with(128, vec![], vec![]),
            locality(None, None),
        )];
        let r = BurstRouter::default().route(&req, &local, &peers);
        assert_eq!(r, RouteDecision::NoCapacity);
    }

    #[test]
    fn route_prefers_better_locality_within_a_pool() {
        // Two local candidates both have capacity; one is in the
        // requested region, the other is far. Better locality wins.
        let hint = LocalityHint {
            region: Some("eu-central-1".into()),
            ..Default::default()
        };
        let req = ResourceRequirement {
            min_ram_gb: Some(64),
            locality: Some(hint),
            ..Default::default()
        };
        let local = vec![
            candidate(
                "n-far",
                "org-self",
                caps_with(128, vec![], vec![]),
                locality(Some("us-west-2"), None),
            ),
            candidate(
                "n-near",
                "org-self",
                caps_with(128, vec![], vec![]),
                locality(Some("eu-central-1"), None),
            ),
        ];
        let r = BurstRouter::default().route(&req, &local, &[]);
        assert_eq!(
            r,
            RouteDecision::Local {
                node_id: "n-near".into()
            }
        );
    }

    #[test]
    fn route_skips_local_residency_violator_and_falls_to_peers() {
        // Local has capacity but violates EU residency; peer satisfies both.
        let hint = LocalityHint {
            data_residency_required: Some("EU".into()),
            ..Default::default()
        };
        let req = ResourceRequirement {
            min_ram_gb: Some(64),
            locality: Some(hint),
            ..Default::default()
        };
        let local = vec![candidate(
            "n-local",
            "org-self",
            caps_with(128, vec![], vec![]),
            locality(Some("us-west-2"), Some("US")), // residency violator
        )];
        let peers = vec![candidate(
            "n-eu",
            "org-other",
            caps_with(128, vec![], vec![]),
            locality(Some("eu-central-1"), Some("EU")),
        )];
        let r = BurstRouter::default().route(&req, &local, &peers);
        assert_eq!(
            r,
            RouteDecision::Burst {
                org_id: "org-other".into(),
                node_id: "n-eu".into()
            }
        );
    }

    #[test]
    fn route_with_no_locality_hint_picks_first_capacity_match() {
        // When no locality hint, all capacity-matching candidates score 0;
        // stable sort preserves input order, so the first qualifies.
        let req = ResourceRequirement {
            min_ram_gb: Some(64),
            ..Default::default()
        };
        let local = vec![
            candidate(
                "n-first",
                "org-self",
                caps_with(128, vec![], vec![]),
                locality(None, None),
            ),
            candidate(
                "n-second",
                "org-self",
                caps_with(256, vec![], vec![]),
                locality(None, None),
            ),
        ];
        let r = BurstRouter::default().route(&req, &local, &[]);
        assert_eq!(
            r,
            RouteDecision::Local {
                node_id: "n-first".into()
            }
        );
    }
}
