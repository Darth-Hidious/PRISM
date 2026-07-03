// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! Cross-org policy intersection — Fabric F2.
//!
//! When a workflow touches resources owned by multiple organizations, every
//! involved org's policy fires independently. The decisions are then folded
//! into a single [`PolicyDecision`] using **strictest-wins** semantics:
//!
//! - **All allow** → allow, with the **union** of obligations from each org
//!   (everyone's `audit_log`, `notify_admin`, etc. requirements stack)
//! - **Any deny** → deny, with **every denying org's reason** surfaced so
//!   the user sees who blocked what (no silent partial denies)
//! - **Empty input** → deny by default (fail-closed) — refuses to grant
//!   access in the absence of policies; better than the alternative
//!   ("nobody had a policy, so it's allowed") which would silently bypass
//!   intent
//!
//! # Why strictest-wins (vs weighted / origin-only)
//!
//! Per the locked decision in `docs/prism_fabric_v1_spec.md`:
//!
//! - **Predictable.** No "trust score" tuning surface to misconfigure.
//! - **Safe-by-default.** A single denial blocks the action.
//! - **Auditable.** The denying party + reason is always visible.
//! - **Matches contracts.** Cross-org agreements typically encode
//!   "if any party objects, no action" — exactly this rule.
//!
//! # What this module does NOT do
//!
//! - Does NOT load policies from peer orgs. That requires platform-side
//!   peer enumeration (Fabric F1 chunk 3) and per-org policy distribution.
//!   This module operates on already-evaluated decisions.
//! - Does NOT score decisions. There is no "weight"; every party's vote
//!   is binary and equal.

use crate::PolicyDecision;

/// Fold per-org policy decisions into a single cross-org decision using
/// strictest-wins semantics.
///
/// Returns `Ok(PolicyDecision)` always — the input vec is never empty in
/// well-formed callers, but if it is we fail-closed with a deny.
pub fn intersect_decisions(decisions: &[PolicyDecision]) -> PolicyDecision {
    if decisions.is_empty() {
        return PolicyDecision::deny(
            "no policies evaluated for cross-org request (fail-closed)",
            vec!["empty decision set".to_string()],
        );
    }

    let denials: Vec<&PolicyDecision> = decisions.iter().filter(|d| !d.allowed).collect();

    if !denials.is_empty() {
        // Surface every denying party's reason. Order matches input order
        // so the caller can correlate with their per-org evaluation list.
        let combined_reason = denials
            .iter()
            .map(|d| d.reason.as_str())
            .collect::<Vec<_>>()
            .join(" ; ");

        let mut violations: Vec<String> = denials
            .iter()
            .flat_map(|d| d.violations.iter().cloned())
            .collect();
        violations.sort();
        violations.dedup();

        let mut decision = PolicyDecision::deny(
            format!(
                "cross-org denied by {} of {} parties: {combined_reason}",
                denials.len(),
                decisions.len()
            ),
            violations,
        );

        // Even on deny we union obligations from the *allowing* parties.
        // Some obligations (e.g. `audit_log`) should still fire for the
        // attempted action that was blocked — auditing the denial event
        // matters as much as auditing the success.
        let mut obligations: Vec<String> = decisions
            .iter()
            .filter(|d| d.allowed)
            .flat_map(|d| d.obligations.iter().cloned())
            .collect();
        obligations.sort();
        obligations.dedup();
        decision.obligations = obligations;

        return decision;
    }

    // Unanimous allow — union obligations across all parties.
    let mut obligations: Vec<String> = decisions
        .iter()
        .flat_map(|d| d.obligations.iter().cloned())
        .collect();
    obligations.sort();
    obligations.dedup();

    let mut decision = PolicyDecision::allow(format!(
        "cross-org allow ({} parties unanimous)",
        decisions.len()
    ));
    decision.obligations = obligations;
    decision
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allow(reason: &str, obligations: &[&str]) -> PolicyDecision {
        let mut d = PolicyDecision::allow(reason);
        d.obligations = obligations.iter().map(|s| s.to_string()).collect();
        d
    }

    fn deny(reason: &str, violations: &[&str]) -> PolicyDecision {
        PolicyDecision::deny(reason, violations.iter().map(|s| s.to_string()).collect())
    }

    // ── Single-party cases ────────────────────────────────────────────

    #[test]
    fn empty_decisions_fails_closed() {
        let r = intersect_decisions(&[]);
        assert!(!r.allowed, "empty input must NOT allow (fail-closed)");
        assert!(r.reason.contains("no policies"));
    }

    #[test]
    fn single_allow_passes_through() {
        let r = intersect_decisions(&[allow("only party ok", &["audit_log"])]);
        assert!(r.allowed);
        assert_eq!(r.obligations, vec!["audit_log"]);
    }

    #[test]
    fn single_deny_passes_through_with_count() {
        let r = intersect_decisions(&[deny("budget exceeded", &["over_budget"])]);
        assert!(!r.allowed);
        assert!(r.reason.contains("1 of 1"));
        assert!(r.reason.contains("budget exceeded"));
        assert_eq!(r.violations, vec!["over_budget"]);
    }

    // ── Two-party cases ───────────────────────────────────────────────

    #[test]
    fn allow_intersect_allow_yields_allow_with_union_obligations() {
        let r = intersect_decisions(&[
            allow("orgA ok", &["audit_log", "notify_admin"]),
            allow("orgB ok", &["audit_log", "encrypt_at_rest"]),
        ]);
        assert!(r.allowed);
        // Union dedup: audit_log appears once
        assert_eq!(
            r.obligations,
            vec![
                "audit_log".to_string(),
                "encrypt_at_rest".to_string(),
                "notify_admin".to_string(),
            ]
        );
    }

    #[test]
    fn allow_intersect_deny_yields_deny() {
        let r = intersect_decisions(&[
            allow("orgA ok", &["audit_log"]),
            deny("orgB blocks", &["pii_violation"]),
        ]);
        assert!(!r.allowed);
        assert!(r.reason.contains("1 of 2 parties"));
        assert!(r.reason.contains("orgB blocks"));
        assert_eq!(r.violations, vec!["pii_violation"]);
        // The allowing party's obligation still fires
        // (audit the denial event itself).
        assert_eq!(r.obligations, vec!["audit_log"]);
    }

    #[test]
    fn deny_intersect_deny_surfaces_both_reasons() {
        let r = intersect_decisions(&[
            deny("orgA: budget", &["over_budget"]),
            deny("orgB: data sovereignty", &["data_export_blocked"]),
        ]);
        assert!(!r.allowed);
        assert!(r.reason.contains("2 of 2 parties"));
        assert!(r.reason.contains("orgA: budget"));
        assert!(r.reason.contains("orgB: data sovereignty"));
        // Violations dedup + sorted
        assert_eq!(
            r.violations,
            vec!["data_export_blocked".to_string(), "over_budget".to_string()]
        );
    }

    // ── Three-party case (the spec's canonical fixture) ───────────────

    #[test]
    fn three_orgs_two_allow_one_deny() {
        // Tokyo (data) allows but requires audit. Munich (compute)
        // allows but requires `notify_admin`. San Diego (initiator)
        // denies because the user's project budget is exhausted.
        // Expected: deny with SD's reason + audit/notify obligations
        // still attached so the failed attempt is logged.
        let r = intersect_decisions(&[
            allow("Tokyo CFD: contract X allows access", &["audit_log"]),
            allow("Munich SLURM: capacity available", &["notify_admin"]),
            deny(
                "San Diego: project budget $0 remaining",
                &["budget_exhausted"],
            ),
        ]);
        assert!(!r.allowed);
        assert!(r.reason.contains("1 of 3 parties"));
        assert!(r.reason.contains("San Diego"));
        assert_eq!(r.violations, vec!["budget_exhausted"]);
        // Obligations from the two allowing parties still fire.
        assert_eq!(
            r.obligations,
            vec!["audit_log".to_string(), "notify_admin".to_string()]
        );
    }

    #[test]
    fn three_orgs_all_allow_unions_obligations() {
        let r = intersect_decisions(&[
            allow("A", &["audit_log", "notify_admin"]),
            allow("B", &["audit_log"]),
            allow("C", &["encrypt_at_rest", "notify_admin"]),
        ]);
        assert!(r.allowed);
        assert!(r.reason.contains("3 parties unanimous"));
        assert_eq!(
            r.obligations,
            vec![
                "audit_log".to_string(),
                "encrypt_at_rest".to_string(),
                "notify_admin".to_string(),
            ]
        );
    }

    // ── Property: ordering doesn't change the outcome ─────────────────

    #[test]
    fn ordering_does_not_change_allow_outcome() {
        let a = allow("A", &["x"]);
        let b = allow("B", &["y"]);
        let c = allow("C", &["z"]);

        let r1 = intersect_decisions(&[a.clone(), b.clone(), c.clone()]);
        let r2 = intersect_decisions(&[c, b, a]);

        assert!(r1.allowed && r2.allowed);
        assert_eq!(r1.obligations, r2.obligations); // sorted, so identical
    }

    #[test]
    fn ordering_does_not_change_deny_outcome() {
        let a = allow("A", &[]);
        let b = deny("B blocks", &["v1"]);
        let c = deny("C blocks", &["v2"]);

        let r1 = intersect_decisions(&[a.clone(), b.clone(), c.clone()]);
        let r2 = intersect_decisions(&[c, b, a]);

        assert!(!r1.allowed && !r2.allowed);
        assert_eq!(r1.violations, r2.violations); // sorted+dedup
    }
}
