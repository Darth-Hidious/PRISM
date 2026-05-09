// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! F3 — locality-aware compute placement.
//!
//! When a node needs to dispatch a compute request (run an inference,
//! estimate a workflow's resource cost, allocate a slot on a
//! GPU-bearing peer), it has a list of candidate nodes from
//! [`super::platform_discovery::PlatformDiscovery::discover`] and
//! local mDNS. F3 ranks those candidates so the request lands on a
//! node that's "close enough" by the dimensions that matter:
//! geographic region, availability zone, expected latency class,
//! and data-residency constraints.
//!
//! # What this module deliberately does NOT do
//!
//! - **Live RTT probing.** v1 ranks off declared metadata only.
//!   Adding active probing belongs in a later chunk (F4 capability
//!   descriptors) so we can amortise the cost across multiple
//!   placement decisions.
//! - **Compute capacity tracking.** "Does this node have a free
//!   GPU slot?" is also F4. F3 only does locality.
//! - **Wire-up to the federation request flow.** That's the
//!   integration step in F4/F6. F3 is the scoring primitive.
//!
//! # Why pure functions
//!
//! Locality scoring is the kind of thing where every site operator
//! will eventually want to override the weights. Keeping it as
//! pure functions over plain data means the override mechanism is
//! "the caller passes a different `LocalityWeights`," not "the
//! caller subclasses a singleton."

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Latency expectations a request has from the placement.
///
/// These are coarse classes, not concrete RTT numbers. v1 uses them
/// as a tie-breaker between same-region nodes — e.g. prefer a
/// same-zone node when `SubMs` is requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LatencyClass {
    /// Single-digit-millisecond critical path (e.g. live chat
    /// completion). Same zone strongly preferred.
    SubMs,
    /// Tens of milliseconds (e.g. workflow step). Same region
    /// preferred, cross-zone OK.
    LowLatency,
    /// Hundreds of milliseconds OK (e.g. batch inference). Region
    /// match is a tiebreaker, not a requirement.
    BestEffort,
}

/// What the request *wants* in a placement.
///
/// All fields optional: a request that doesn't care about region
/// passes `region: None`, etc. Hard requirements live in
/// `data_residency_required` — that one excludes candidates rather
/// than just scoring them lower.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LocalityHint {
    /// Preferred region (e.g. `"us-west-2"`).
    pub region: Option<String>,
    /// Preferred availability zone (e.g. `"us-west-2a"`).
    pub zone: Option<String>,
    /// Latency expectations (default: `BestEffort`).
    pub latency_class: Option<LatencyClass>,
    /// **Hard constraint.** If set, candidates whose `data_residency`
    /// does not match this value are excluded entirely (score = 0).
    /// Used for compliance: an EU dataset MUST stay in EU.
    pub data_residency_required: Option<String>,
}

/// Locality metadata a candidate node advertises about itself.
///
/// Sourced from the platform's node registry (or from the candidate
/// node's mDNS TXT record for local-network peers). Missing fields
/// just mean "unknown" — the candidate isn't excluded, but it can't
/// score positively on those dimensions either.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CandidateLocality {
    pub region: Option<String>,
    pub zone: Option<String>,
    /// Where data processed on this node will physically reside.
    /// Used for the residency hard-constraint check.
    pub data_residency: Option<String>,
}

/// Tunable weights for the additive soft-constraint score. Higher
/// → that dimension matters more.
///
/// Defaults are tuned for the common case: zone match > region match
/// > latency-class match. Site operators can override.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct LocalityWeights {
    pub zone_match: u32,
    pub region_match: u32,
    pub latency_class_satisfied: u32,
}

impl Default for LocalityWeights {
    fn default() -> Self {
        Self {
            zone_match: 60,
            region_match: 30,
            latency_class_satisfied: 10,
        }
    }
}

/// Result of scoring one candidate against one hint.
///
/// `score == 0` means the candidate is excluded (hard-constraint
/// violation). Greater is better; ties are broken by the caller's
/// list ordering (stable sort).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalityScore(pub u32);

// ---------------------------------------------------------------------------
// Scoring
// ---------------------------------------------------------------------------

/// Score a single candidate against a hint.
///
/// 1. **Hard constraints** (`data_residency_required`) — violation
///    returns `LocalityScore(0)` which excludes the candidate. A
///    candidate that doesn't *declare* a residency cannot satisfy
///    a residency requirement (we don't trust silence as compliance).
/// 2. **Soft constraints** — additive sum of the per-dimension
///    weights for each match.
pub fn score_candidate(
    hint: &LocalityHint,
    candidate: &CandidateLocality,
    weights: &LocalityWeights,
) -> LocalityScore {
    // 1. Residency hard constraint
    if let Some(required) = &hint.data_residency_required {
        match &candidate.data_residency {
            Some(declared) if declared == required => { /* ok, fall through */ }
            _ => return LocalityScore(0),
        }
    }

    // 2. Soft scoring
    let mut score: u32 = 0;

    if let (Some(want), Some(have)) = (&hint.region, &candidate.region)
        && want == have
    {
        score = score.saturating_add(weights.region_match);
    }
    if let (Some(want), Some(have)) = (&hint.zone, &candidate.zone)
        && want == have
    {
        score = score.saturating_add(weights.zone_match);
    }

    // Latency-class scoring is implicit: if the request wants SubMs
    // and the candidate is in the same zone, the zone match already
    // covers it. We add a small bonus when the candidate's region
    // is at least known (i.e. it can reason about latency at all).
    // BestEffort requests get the bonus unconditionally — they're
    // satisfied by anything reachable.
    if hint.latency_class.is_some()
        && (matches!(hint.latency_class, Some(LatencyClass::BestEffort))
            || candidate.region.is_some())
    {
        score = score.saturating_add(weights.latency_class_satisfied);
    }

    LocalityScore(score)
}

/// Rank candidates in best-first order, dropping hard-rejects.
///
/// Stable: candidates with equal score keep their input order, which
/// matches caller-determined tiebreakers (e.g. round-robin pinned to
/// caller's last-used index).
pub fn rank_candidates<'a, T>(
    hint: &LocalityHint,
    candidates: &'a [T],
    weights: &LocalityWeights,
    locality_of: impl Fn(&T) -> &CandidateLocality,
) -> Vec<(&'a T, LocalityScore)> {
    let mut scored: Vec<(&T, LocalityScore)> = candidates
        .iter()
        .map(|c| (c, score_candidate(hint, locality_of(c), weights)))
        .filter(|(_, s)| s.0 > 0)
        .collect();
    // Stable sort, descending by score.
    scored.sort_by(|a, b| b.1.0.cmp(&a.1.0));
    scored
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_candidate(
        region: Option<&str>,
        zone: Option<&str>,
        residency: Option<&str>,
    ) -> CandidateLocality {
        CandidateLocality {
            region: region.map(str::to_string),
            zone: zone.map(str::to_string),
            data_residency: residency.map(str::to_string),
        }
    }

    fn mk_hint(
        region: Option<&str>,
        zone: Option<&str>,
        latency: Option<LatencyClass>,
        residency: Option<&str>,
    ) -> LocalityHint {
        LocalityHint {
            region: region.map(str::to_string),
            zone: zone.map(str::to_string),
            latency_class: latency,
            data_residency_required: residency.map(str::to_string),
        }
    }

    // ── Hard constraint: data residency ───────────────────────────

    #[test]
    fn residency_violation_zeroes_score_even_with_perfect_locality() {
        // Hint demands EU residency; candidate is in same region/zone
        // but resides in US. Must score 0.
        let hint = mk_hint(Some("us-west-2"), Some("us-west-2a"), None, Some("EU"));
        let cand = mk_candidate(Some("us-west-2"), Some("us-west-2a"), Some("US"));
        let s = score_candidate(&hint, &cand, &LocalityWeights::default());
        assert_eq!(s, LocalityScore(0));
    }

    #[test]
    fn residency_unknown_fails_required_check() {
        // Silence is not compliance.
        let hint = mk_hint(None, None, None, Some("EU"));
        let cand = mk_candidate(None, None, None);
        let s = score_candidate(&hint, &cand, &LocalityWeights::default());
        assert_eq!(s, LocalityScore(0));
    }

    #[test]
    fn residency_match_passes_through() {
        let hint = mk_hint(Some("eu-central-1"), None, None, Some("EU"));
        let cand = mk_candidate(Some("eu-central-1"), None, Some("EU"));
        let s = score_candidate(&hint, &cand, &LocalityWeights::default());
        assert!(s.0 >= LocalityWeights::default().region_match);
    }

    #[test]
    fn residency_unset_in_hint_means_no_constraint() {
        // No residency required → candidate's residency doesn't matter.
        let hint = mk_hint(Some("us-west-2"), None, None, None);
        let cand = mk_candidate(Some("us-west-2"), None, Some("EU"));
        let s = score_candidate(&hint, &cand, &LocalityWeights::default());
        assert!(s.0 > 0);
    }

    // ── Soft scoring: dimension weights ───────────────────────────

    #[test]
    fn zone_match_outweighs_region_only() {
        // Two candidates: one matches zone (=> region too), one matches only region.
        // Same-zone should rank higher.
        let hint = mk_hint(Some("us-west-2"), Some("us-west-2a"), None, None);
        let same_zone = mk_candidate(Some("us-west-2"), Some("us-west-2a"), None);
        let same_region_other_zone = mk_candidate(Some("us-west-2"), Some("us-west-2b"), None);

        let w = LocalityWeights::default();
        let s_zone = score_candidate(&hint, &same_zone, &w);
        let s_region = score_candidate(&hint, &same_region_other_zone, &w);
        assert!(
            s_zone.0 > s_region.0,
            "expected same-zone {s_zone:?} > region-only {s_region:?}"
        );
    }

    #[test]
    fn region_match_beats_no_match() {
        let hint = mk_hint(Some("us-west-2"), None, None, None);
        let same_region = mk_candidate(Some("us-west-2"), None, None);
        let other_region = mk_candidate(Some("us-east-1"), None, None);

        let w = LocalityWeights::default();
        let s_match = score_candidate(&hint, &same_region, &w);
        let s_miss = score_candidate(&hint, &other_region, &w);
        assert!(s_match.0 > s_miss.0);
    }

    #[test]
    fn unknown_candidate_region_does_not_match() {
        // Hint asks for us-west-2; candidate doesn't declare region.
        // No region match.
        let hint = mk_hint(Some("us-west-2"), None, None, None);
        let cand = mk_candidate(None, None, None);
        let w = LocalityWeights::default();
        let s = score_candidate(&hint, &cand, &w);
        // No region match, no latency class set, no residency check → 0.
        assert_eq!(s, LocalityScore(0));
    }

    #[test]
    fn latency_class_bonus_only_when_class_is_specified() {
        let cand = mk_candidate(Some("us-west-2"), None, None);
        let w = LocalityWeights::default();

        let no_latency = mk_hint(Some("us-west-2"), None, None, None);
        let with_latency = mk_hint(
            Some("us-west-2"),
            None,
            Some(LatencyClass::LowLatency),
            None,
        );
        assert_eq!(
            score_candidate(&with_latency, &cand, &w).0,
            score_candidate(&no_latency, &cand, &w).0 + w.latency_class_satisfied,
        );
    }

    #[test]
    fn besteffort_latency_satisfied_unconditionally() {
        // BestEffort means anything reachable counts.
        let hint = mk_hint(None, None, Some(LatencyClass::BestEffort), None);
        let cand = mk_candidate(None, None, None);
        let s = score_candidate(&hint, &cand, &LocalityWeights::default());
        assert_eq!(s.0, LocalityWeights::default().latency_class_satisfied);
    }

    #[test]
    fn weights_can_be_overridden() {
        // Site operator wants latency to dominate.
        let hint = mk_hint(
            Some("us-west-2"),
            None,
            Some(LatencyClass::LowLatency),
            None,
        );
        let cand = mk_candidate(Some("us-west-2"), None, None);
        let w = LocalityWeights {
            zone_match: 0,
            region_match: 1,
            latency_class_satisfied: 1000,
        };
        let s = score_candidate(&hint, &cand, &w);
        assert_eq!(s.0, 1 + 1000);
    }

    // ── rank_candidates ────────────────────────────────────────────

    #[test]
    fn rank_orders_by_score_descending() {
        let hint = mk_hint(Some("us-west-2"), Some("us-west-2a"), None, None);
        let candidates = vec![
            mk_candidate(Some("us-east-1"), Some("us-east-1a"), None), // no match → 0
            mk_candidate(Some("us-west-2"), Some("us-west-2a"), None), // zone+region
            mk_candidate(Some("us-west-2"), Some("us-west-2b"), None), // region only
        ];
        let ranked = rank_candidates(&hint, &candidates, &LocalityWeights::default(), |c| c);
        assert_eq!(ranked.len(), 2, "the no-match candidate should be filtered");
        // Best-first: same-zone should come first.
        assert_eq!(ranked[0].0.zone.as_deref(), Some("us-west-2a"));
        assert_eq!(ranked[1].0.zone.as_deref(), Some("us-west-2b"));
    }

    #[test]
    fn rank_excludes_residency_violators() {
        // Latency class + region declaration give the EU-but-region-miss
        // candidate a non-zero score, so we can prove the residency
        // violator is the *only* one excluded (not just a side-effect of
        // a zero-score floor).
        let hint = mk_hint(
            Some("eu-central-1"),
            None,
            Some(LatencyClass::BestEffort),
            Some("EU"),
        );
        let candidates = vec![
            mk_candidate(Some("eu-central-1"), None, Some("EU")), // OK
            mk_candidate(Some("eu-central-1"), None, Some("US")), // residency violator
            mk_candidate(Some("eu-west-1"), None, Some("EU")),    // OK, region miss
        ];
        let ranked = rank_candidates(&hint, &candidates, &LocalityWeights::default(), |c| c);
        assert_eq!(
            ranked.len(),
            2,
            "expected both EU candidates to pass; got {ranked:?}"
        );
        for (cand, _) in &ranked {
            assert_eq!(cand.data_residency.as_deref(), Some("EU"));
        }
    }

    #[test]
    fn rank_is_stable_for_equal_scores() {
        // Two candidates that score identically — must keep input order.
        let hint = mk_hint(Some("us-west-2"), None, None, None);
        let candidates = vec![
            mk_candidate(Some("us-west-2"), None, None),
            mk_candidate(Some("us-west-2"), None, None),
        ];
        let ranked = rank_candidates(&hint, &candidates, &LocalityWeights::default(), |c| c);
        // Pointer equality preserves input order.
        assert!(std::ptr::eq(ranked[0].0, &candidates[0]));
        assert!(std::ptr::eq(ranked[1].0, &candidates[1]));
    }

    #[test]
    fn rank_with_custom_locality_extractor() {
        // Caller's candidate type may wrap CandidateLocality. The
        // closure pulls it out so we don't force callers to copy.
        struct Node {
            id: &'static str,
            loc: CandidateLocality,
        }

        let hint = mk_hint(Some("us-west-2"), Some("us-west-2a"), None, None);
        let nodes = vec![
            Node {
                id: "n1",
                loc: mk_candidate(Some("us-east-1"), Some("us-east-1a"), None),
            },
            Node {
                id: "n2",
                loc: mk_candidate(Some("us-west-2"), Some("us-west-2a"), None),
            },
        ];
        let ranked = rank_candidates(&hint, &nodes, &LocalityWeights::default(), |n| &n.loc);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].0.id, "n2");
    }
}
