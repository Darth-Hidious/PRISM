// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.

//! Which branch of the research DAG to expand next.
//!
//! The measurement half already exists: every call names its branch
//! (`provenance_records.agent`) and every fact names the call that bought it
//! (`prov_activity.origin_action_id`), so `ProvenanceStore::branch_yields`
//! answers "what did this branch buy". This module turns that into a
//! recommendation the next decomposition can read.
//!
//! The policy is the Darwin Gödel Machine's (Zhang, Hu, Lu, Lange & Clune,
//! ICLR 2026), which is the right shape for PRISM because DGM improves a
//! system built on FROZEN pretrained models — no weights, no gradients, no
//! backprop. Selection is:
//!
//!   weight ∝ yield  ÷  (1 + times already expanded)
//!
//! Two properties are load-bearing and neither is an implementation detail:
//!
//! 1. **Dividing by expansions favours strong branches that are
//!    UNDEREXPLORED**, rather than re-mining whichever branch happened to pay
//!    first. Without it the loop converges on its first success.
//!
//! 2. **Every branch keeps a NON-ZERO weight.** In DGM's own archive the
//!    lineage of the final best agent passes through two performance DIPS,
//!    and "many paths to innovation traverse lower-performing nodes." A greedy
//!    policy prunes exactly those. This is also why the saturation problem in
//!    PRISM is a diversity failure rather than a scoring one: a branch that
//!    bought nothing this round is not a branch that cannot pay.
//!
//! This is a ranking handed to a model, not a gradient. Nothing here trains
//! anything.

use std::collections::HashMap;

use prism_provenance::BranchYield;

/// Weight floor, so no branch is ever unreachable.
///
/// DGM: "All agents retain a non-zero selection probability, ensuring that any
/// path to improvement remains feasible given sufficient compute." The number
/// is small enough that a productive branch dominates, and large enough that a
/// barren one still surfaces if everything else dries up.
const FLOOR: f64 = 0.05;

/// One branch, scored and explained.
#[derive(Debug, Clone, PartialEq)]
pub struct BranchChoice {
    pub agent: String,
    /// Facts bought per call — the yield, normalised by effort so a branch is
    /// not rewarded merely for being busy.
    pub yield_per_call: f64,
    /// How many times this branch has already been expanded.
    pub expansions: usize,
    /// Selection weight. Always > 0.
    pub weight: f64,
}

/// Rank branches for expansion, richest-and-least-explored first.
///
/// `expansions` counts how often each branch has already been fanned out; an
/// absent entry counts as zero. Yields with no calls score at the floor rather
/// than dividing by zero — a branch that never ran is unexplored, not barren,
/// and the two must not be conflated.
#[must_use]
pub fn rank(yields: &[BranchYield], expansions: &HashMap<String, usize>) -> Vec<BranchChoice> {
    let mut out: Vec<BranchChoice> = yields
        .iter()
        .map(|y| {
            #[allow(clippy::cast_precision_loss)]
            let per_call = if y.calls == 0 {
                0.0
            } else {
                y.facts as f64 / y.calls as f64
            };
            let times = expansions.get(&y.agent).copied().unwrap_or(0);
            #[allow(clippy::cast_precision_loss)]
            let weight = (per_call / (1.0 + times as f64)).max(FLOOR);
            BranchChoice {
                agent: y.agent.clone(),
                yield_per_call: per_call,
                expansions: times,
                weight,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.weight
            .total_cmp(&a.weight)
            .then_with(|| a.agent.cmp(&b.agent))
    });
    out
}

/// The block a decomposition reads before writing the next one.
///
/// Deliberately compact and free of instruction. It states what each branch
/// bought and how often it has been expanded, and says the one thing the
/// numbers do not: that a quiet branch is not a dead one. The caller decides;
/// this is evidence, not a directive.
#[must_use]
pub fn feedback_block(choices: &[BranchChoice]) -> Option<String> {
    if choices.is_empty() {
        return None;
    }
    let mut lines = String::from(
        "[Branch yield so far] facts per call, and how many times each branch was expanded.\n",
    );
    for c in choices {
        lines.push_str(&format!(
            "  {:<16} {:>5.2} facts/call   expanded {}x\n",
            c.agent, c.yield_per_call, c.expansions
        ));
    }
    lines.push_str(
        "Expand what paid and what is under-explored. A branch that bought nothing is not \
         necessarily barren — the best result often arrives through a lean one — so widen \
         before abandoning.",
    );
    Some(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn y(agent: &str, calls: usize, facts: usize) -> BranchYield {
        BranchYield {
            agent: agent.to_string(),
            calls,
            facts,
        }
    }

    /// Yield per call orders the ranking — not raw fact count, which would
    /// reward a branch merely for making more calls.
    #[test]
    fn a_richer_branch_outranks_a_busier_one() {
        let ranked = rank(
            &[y("Wagner", 20, 20), y("Sarabhai", 4, 16)],
            &HashMap::new(),
        );
        assert_eq!(
            ranked[0].agent, "Sarabhai",
            "4 facts/call beats 1: {ranked:?}"
        );
        assert!((ranked[0].yield_per_call - 4.0).abs() < 1e-9);
    }

    /// DGM's exploration term: among equals, the one expanded less often wins.
    /// Without this the loop re-mines whichever branch paid first.
    #[test]
    fn an_underexplored_branch_outranks_an_equally_rich_one() {
        let expansions = HashMap::from([("Wagner".to_string(), 3)]);
        let ranked = rank(&[y("Wagner", 4, 16), y("Bhabha", 4, 16)], &expansions);
        assert_eq!(ranked[0].agent, "Bhabha", "never expanded wins: {ranked:?}");
        assert!(ranked[1].weight < ranked[0].weight);
    }

    /// The non-zero floor. In DGM's archive the best agent's lineage passes
    /// through performance dips, so a barren-looking branch must stay
    /// reachable — pruning it is how the search converges early.
    #[test]
    fn a_branch_that_bought_nothing_is_still_reachable() {
        let ranked = rank(&[y("Rao", 9, 0), y("Raman", 2, 8)], &HashMap::new());
        let barren = ranked.iter().find(|c| c.agent == "Rao").expect("present");
        assert_eq!(barren.yield_per_call, 0.0);
        assert!(barren.weight > 0.0, "never unreachable: {barren:?}");
        assert_eq!(ranked[0].agent, "Raman", "but it does not lead");
    }

    /// A branch that never ran is UNEXPLORED, not barren, and must not divide
    /// by zero on the way to saying so.
    #[test]
    fn a_branch_with_no_calls_does_not_divide_by_zero() {
        let ranked = rank(&[y("Bose", 0, 0)], &HashMap::new());
        assert_eq!(ranked[0].yield_per_call, 0.0);
        assert!(ranked[0].weight.is_finite() && ranked[0].weight > 0.0);
    }

    /// The block states evidence and one caveat; it does not command.
    #[test]
    fn the_feedback_block_reports_and_warns_against_pruning() {
        let ranked = rank(&[y("Sarabhai", 4, 16), y("Wagner", 9, 0)], &HashMap::new());
        let block = feedback_block(&ranked).expect("non-empty");
        assert!(block.contains("Sarabhai") && block.contains("Wagner"));
        assert!(block.contains("facts/call") && block.contains("expanded"));
        assert!(
            block.contains("not necessarily barren"),
            "the anti-pruning caveat is the point: {block}"
        );
        assert_eq!(
            feedback_block(&[]),
            None,
            "nothing to say about no branches"
        );
    }
}
