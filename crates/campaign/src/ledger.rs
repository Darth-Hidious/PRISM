//! The campaign ledger — one read model over the three durable stores.
//!
//! PRISM's unit of work is a campaign, and a campaign's state lives in three
//! places today: the checkpoint file (what was proposed and accepted), the
//! persistent compute-job tracker (what was submitted), and the Turso
//! provenance store (what was established and what failed). No agent joining
//! mid-flight can see all three with one question. This module is that one
//! question: **what is done, what is pending, what was refuted**.
//!
//! It is deliberately a READ MODEL, not a fourth store: it persists nothing
//! of its own. Every answer is recomputed from the three sources at call
//! time, so it can never drift from them and it survives a process restart
//! exactly as well as they do.
//!
//! Honesty rules (the reason this exists):
//! - A submitted-but-unfinished job is **pending**, never done.
//! - An evidence class is copied verbatim from its source — never greener.
//! - When one of the three stores cannot answer part of the question, the
//!   ledger says so in `gaps` instead of returning a confident partial.
//!   A ledger that quietly omits is worse than one that reports a gap,
//!   because the next agent will trust it.

use chrono::Utc;
use prism_compute::job::{JobRecord, JobTracker, TrackedStatus};
use prism_provenance::ProvenanceStore;
use serde::{Deserialize, Serialize};

use crate::{CampaignState, GoalStatus};

/// How many provenance failure records the ledger enumerates per campaign.
/// `query_failures` clamps to 1000 regardless; this keeps the model-facing
/// payload readable while staying far above any real campaign's failure count.
const FAILURE_RECORD_LIMIT: usize = 100;

/// What was established — completed work, carrying its evidence class
/// verbatim (never upgraded past its source).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DoneEntry {
    /// A candidate that was evaluated, admitted to the ranking, and therefore
    /// established by the campaign. The evidence class is the checkpoint's own
    /// — an indeterminate (RED) candidate stays indeterminate here.
    EstablishedCandidate {
        candidate: String,
        reward: f64,
        evidence_class: String,
        evidence_color: String,
        iteration: usize,
        source: String,
        /// Where this claim is backed — facts without provenance are not done.
        provenance: String,
    },
    /// An attributed compute job that actually reached terminal success.
    /// A job that is merely submitted never appears here.
    CompletedJob {
        job_id: String,
        name: String,
        backend: String,
        completed_at: String,
        provenance: String,
    },
}

/// What was submitted or planned and is not yet resolved.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PendingEntry {
    /// Submitted and waiting to run. NEVER done.
    JobQueued {
        job_id: String,
        name: String,
        backend: String,
        submitted_at: String,
        provenance: String,
    },
    /// Submitted and still running. NEVER done.
    JobRunning {
        job_id: String,
        name: String,
        backend: String,
        progress: f64,
        submitted_at: String,
        provenance: String,
    },
    /// The goal is not terminal, so iterations remain planned work.
    PlannedIterations {
        remaining: usize,
        of: usize,
        status: String,
    },
}

/// What was tried and did not work — retained WITH its reason, so the next
/// round does not propose it again. This is the memory a campaign loop
/// otherwise loses.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RefutedEntry {
    /// A candidate excluded by a hard domain constraint (or an invalid
    /// proposal stopped before evaluation). The reasons are the point.
    RejectedCandidate {
        candidate: String,
        reasons: Vec<String>,
        iteration: usize,
        /// False when the identity never reached the evaluator.
        evaluated: bool,
        provenance: String,
    },
    /// An attributed compute job that failed or was cancelled before it could
    /// establish anything.
    FailedJob {
        job_id: String,
        name: String,
        backend: String,
        /// The recorded error, or `"cancelled before completion"`.
        error: String,
        provenance: String,
    },
    /// A provenance record flagged `status = "error"` in this campaign's
    /// session — an action the campaign ran that failed.
    FailedAction {
        record_id: String,
        tool_name: Option<String>,
        error: Option<String>,
        exit_code: Option<i64>,
        timestamp: String,
        provenance: String,
    },
}

/// The one-call answer: done / pending / refuted for a campaign, plus the
/// gaps where a store could not answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CampaignLedger {
    pub campaign_id: String,
    pub goal: String,
    pub status: String,
    pub generated_at: String,
    pub done: Vec<DoneEntry>,
    pub pending: Vec<PendingEntry>,
    pub refuted: Vec<RefutedEntry>,
    /// Explicit "cannot answer" notes. Never empty by accident: if a store is
    /// unavailable or cannot attribute its rows to this campaign, that is
    /// reported here instead of being silently omitted.
    pub gaps: Vec<String>,
}

/// The job store has no structural campaign linkage — attribution is the
/// job's name referencing the campaign id (case-insensitive). This is a
/// best-effort convention; the honest fallback when it matches nothing is a
/// stated gap, not a guess.
fn job_is_attributed(record: &JobRecord, campaign_id: &str) -> bool {
    record
        .name
        .to_lowercase()
        .contains(&campaign_id.to_lowercase())
}

fn job_provenance(record: &JobRecord) -> String {
    format!("compute job registry (job {})", record.job_id)
}

/// Build the ledger for one campaign from the three existing stores.
///
/// `state` is the campaign checkpoint (the only mandatory source — without a
/// campaign there is no ledger). `provenance` and `jobs` are optional because
/// either store may be unavailable; each absence becomes a stated gap rather
/// than a silent omission. This function never fails on a store error — it
/// reports the error as a gap and answers what it can.
pub async fn build_campaign_ledger(
    state: &CampaignState,
    provenance: Option<&ProvenanceStore>,
    jobs: Option<&JobTracker>,
) -> CampaignLedger {
    let campaign_id = state.campaign_id.clone();
    let mut ledger = CampaignLedger {
        campaign_id: campaign_id.clone(),
        goal: state.goal.description.clone(),
        status: state.status.as_str().to_string(),
        generated_at: Utc::now().to_rfc3339(),
        done: Vec::new(),
        pending: Vec::new(),
        refuted: Vec::new(),
        gaps: Vec::new(),
    };

    // ── Done: established candidates, evidence class verbatim ─────────────
    for (rank, candidate) in state.candidates.iter().enumerate() {
        ledger.done.push(DoneEntry::EstablishedCandidate {
            candidate: candidate.composition.clone(),
            reward: candidate.reward,
            evidence_class: candidate.evidence_class.as_str().to_string(),
            evidence_color: candidate.evidence_class.color().to_string(),
            iteration: candidate.iteration,
            source: candidate.source.clone(),
            provenance: format!(
                "campaign checkpoint '{}' (rank {} in the candidate ranking)",
                campaign_id,
                rank + 1
            ),
        });
    }

    // ── Refuted: constraint rejections retained with their reasons ────────
    for rejection in &state.rejected_candidates {
        // `display_composition` would show the live raw proposal, but raw
        // proposals are deliberately NOT checkpointed — the durable ledger
        // must only show what survived to disk.
        let candidate = if rejection.evaluated {
            rejection.composition.clone()
        } else {
            "<invalid proposal; raw value not checkpointed>".to_string()
        };
        ledger.refuted.push(RefutedEntry::RejectedCandidate {
            candidate,
            reasons: rejection.reasons.clone(),
            iteration: rejection.iteration,
            evaluated: rejection.evaluated,
            provenance: format!("campaign checkpoint '{campaign_id}'"),
        });
    }

    // ── Compute jobs: pending (queued/running), done (completed),
    //    refuted (failed/cancelled) — only when attributed ────────────────
    match jobs {
        Some(tracker) => {
            let records = tracker.list(false).await;
            let mut unattributed_active = 0usize;
            for record in records {
                if !job_is_attributed(&record, &campaign_id) {
                    if !record.status.is_terminal() {
                        unattributed_active += 1;
                    }
                    continue;
                }
                match &record.status {
                    TrackedStatus::Queued => {
                        ledger.pending.push(PendingEntry::JobQueued {
                            job_id: record.job_id.to_string(),
                            name: record.name.clone(),
                            backend: record.backend.clone(),
                            submitted_at: record.submitted_at.to_rfc3339(),
                            provenance: job_provenance(&record),
                        });
                    }
                    TrackedStatus::Running { progress } => {
                        ledger.pending.push(PendingEntry::JobRunning {
                            job_id: record.job_id.to_string(),
                            name: record.name.clone(),
                            backend: record.backend.clone(),
                            progress: *progress,
                            submitted_at: record.submitted_at.to_rfc3339(),
                            provenance: job_provenance(&record),
                        });
                    }
                    TrackedStatus::Completed { .. } => {
                        ledger.done.push(DoneEntry::CompletedJob {
                            job_id: record.job_id.to_string(),
                            name: record.name.clone(),
                            backend: record.backend.clone(),
                            completed_at: record.updated_at.to_rfc3339(),
                            provenance: job_provenance(&record),
                        });
                    }
                    TrackedStatus::Failed { error } => {
                        ledger.refuted.push(RefutedEntry::FailedJob {
                            job_id: record.job_id.to_string(),
                            name: record.name.clone(),
                            backend: record.backend.clone(),
                            error: error.clone(),
                            provenance: job_provenance(&record),
                        });
                    }
                    TrackedStatus::Cancelled => {
                        ledger.refuted.push(RefutedEntry::FailedJob {
                            job_id: record.job_id.to_string(),
                            name: record.name.clone(),
                            backend: record.backend.clone(),
                            error: "cancelled before completion".to_string(),
                            provenance: job_provenance(&record),
                        });
                    }
                }
            }
            if unattributed_active > 0 {
                ledger.gaps.push(format!(
                    "the compute job registry holds {unattributed_active} active job(s) that \
                     cannot be attributed to this campaign: job records carry no campaign \
                     linkage, and attribution here is only the job name referencing the \
                     campaign id — ask about those jobs by their own ids if they matter"
                ));
            }
        }
        None => {
            ledger.gaps.push(
                "compute job registry unavailable — submitted jobs cannot be enumerated, so \
                 pending/completed/failed jobs are ABSENT from this ledger, not zero"
                    .to_string(),
            );
        }
    }

    // ── Pending: planned iterations while the goal is not terminal ────────
    if !matches!(
        state.status,
        GoalStatus::Completed | GoalStatus::Failed
    ) {
        let remaining = state
            .config
            .max_iterations
            .saturating_sub(state.current_iteration);
        if remaining > 0 {
            ledger.pending.push(PendingEntry::PlannedIterations {
                remaining,
                of: state.config.max_iterations,
                status: state.status.as_str().to_string(),
            });
        }
    }

    // ── Refuted: failed actions recorded in the campaign's provenance ─────
    // Campaign events are recorded with the campaign id as the session id
    // (`Campaign::record_event`), so scoping failures to that session is the
    // campaign's own failure memory.
    match provenance {
        Some(store) => match store
            .query_failures(Some(campaign_id.as_str()), FAILURE_RECORD_LIMIT)
            .await
        {
            Ok(failures) => {
                for record in failures {
                    let error = record
                        .output_json
                        .as_ref()
                        .and_then(|out| {
                            out.get("error")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_string)
                                .or_else(|| {
                                    out.get("stderr")
                                        .and_then(serde_json::Value::as_str)
                                        .and_then(|stderr| {
                                            stderr.lines().find(|line| !line.trim().is_empty())
                                        })
                                        .map(str::to_string)
                                })
                        });
                    ledger.refuted.push(RefutedEntry::FailedAction {
                        record_id: record.id.clone(),
                        tool_name: record.tool_name.clone(),
                        error,
                        exit_code: record.exit_code,
                        timestamp: record.timestamp.clone(),
                        provenance: format!("provenance store, session '{campaign_id}'"),
                    });
                }
            }
            Err(error) => {
                ledger.gaps.push(format!(
                    "provenance store failed to enumerate this campaign's failed actions: \
                     {error:#} — refuted ACTIONS are absent from this ledger, not zero"
                ));
            }
        },
        None => {
            ledger.gaps.push(
                "provenance store unavailable — the campaign's failed actions cannot be \
                 enumerated, so they are ABSENT from this ledger, not zero"
                    .to_string(),
            );
        }
    }

    ledger
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Campaign, CampaignConfig, CampaignGoal};
    use crate::{Candidate, ConstraintRejection};
    use prism_compute::job::{JobTarget, TrackedStatus};
    use prism_provenance::{ActionType, Actor, new_record};
    use uuid::Uuid;

    const CAMPAIGN_ID: &str = "camp_ledger_test";

    fn goal() -> CampaignGoal {
        CampaignGoal {
            description: "refractory alloy for turbine blades".into(),
            elements: vec!["W".into(), "Mo".into(), "Ta".into(), "Nb".into()],
            objective: "maximize creep resistance".into(),
            constraints: vec![],
            seeds: vec![],
        }
    }

    fn config() -> CampaignConfig {
        CampaignConfig {
            max_iterations: 10,
            ..Default::default()
        }
    }

    fn accepted_candidate(composition: &str, reward: f64, iteration: usize) -> Candidate {
        Candidate {
            composition: composition.to_string(),
            properties: serde_json::json!({}),
            reward,
            evidence_class: crate::EvidenceClass::Indeterminate,
            iteration,
            source: "llm".to_string(),
        }
    }

    /// Seed every state type the ledger reads, then drop ALL in-memory
    /// handles. Re-open the three stores from disk exactly as a fresh process
    /// would and prove the one-call answer is still correct. This is the test
    /// the whole read model exists for: state must outlive the process.
    #[tokio::test]
    async fn ledger_survives_a_process_restart() {
        let base =
            std::env::temp_dir().join(format!("prism-ledger-restart-{}", Uuid::new_v4()));
        let campaigns_dir = base.join("campaigns");
        let jobs_dir = base.join("jobs");
        let prov_db = base.join("provenance.db");
        std::fs::create_dir_all(&campaigns_dir).unwrap();
        std::fs::create_dir_all(&jobs_dir).unwrap();

        let running_job_id;
        let done_job_id;
        let failed_action_id;

        // ── Hour one: write state through the real write paths ────────────
        {
            // Checkpoint: one accepted candidate (screening evidence), one
            // refuted candidate with its reason, mid-flight status.
            let mut state = CampaignState::new(CAMPAIGN_ID.to_string(), goal(), config());
            let mut candidate = accepted_candidate("W25Mo25Ta25Nb25", 0.82, 3);
            candidate.evidence_class = crate::EvidenceClass::Screening;
            state.candidates.push(candidate);
            state.rejected_candidates.push(ConstraintRejection {
                composition: "W80Mo10Ta5Nb5".to_string(),
                properties: serde_json::json!({"density": 19.1}),
                iteration: 2,
                reasons: vec!["density 19.1 g/cm3 exceeds the 12 g/cm3 constraint".into()],
                evaluated: true,
                raw_proposal: None,
            });
            state.current_iteration = 4;
            // Paused (e.g. at an approval gate): realistic for an agent
            // joining mid-flight, and — unlike Running — it carries no
            // worker-liveness metadata for `from_checkpoint` to re-normalize,
            // keeping the restart test deterministic.
            state.status = GoalStatus::Paused;
            let checkpoint_path = campaigns_dir.join(format!("{CAMPAIGN_ID}.json"));
            std::fs::write(
                &checkpoint_path,
                serde_json::to_string_pretty(&state).unwrap(),
            )
            .unwrap();

            // Provenance: one failed action in the campaign's own session.
            let store = ProvenanceStore::open(&prov_db).await.unwrap();
            let mut failed = new_record(
                CAMPAIGN_ID,
                ActionType::ToolCall,
                Actor::Agent,
                Some("melt_pool_sim"),
                None,
                serde_json::json!({"composition": "W80Mo10Ta5Nb5"}),
            );
            failed.output_json = Some(serde_json::json!({
                "success": false,
                "exit_code": 1,
                "stderr": "melt-pool model rejected the composition: keyhole porosity"
            }));
            failed.status = Some("error".to_string());
            failed.exit_code = Some(1);
            store.record(&failed).await.unwrap();
            failed_action_id = failed.id.clone();
        }
        {
            // Compute jobs: one running, one completed, one failed — all
            // attributed by naming the campaign in the job name.
            let tracker = JobTracker::persistent(&jobs_dir).unwrap();
            running_job_id = Uuid::new_v4();
            done_job_id = Uuid::new_v4();
            let failed_job_id = Uuid::new_v4();
            tracker
                .register(
                    running_job_id,
                    &format!("dft batch for {CAMPAIGN_ID}"),
                    "img",
                    "local",
                    JobTarget::Local,
                )
                .await
                .unwrap();
            tracker
                .update_status(running_job_id, TrackedStatus::Running { progress: 0.5 })
                .await
                .unwrap();
            tracker
                .register(
                    done_job_id,
                    &format!("descriptor screen {CAMPAIGN_ID}"),
                    "img",
                    "local",
                    JobTarget::Local,
                )
                .await
                .unwrap();
            tracker
                .update_status(done_job_id, TrackedStatus::Completed { duration_secs: 60 })
                .await
                .unwrap();
            tracker
                .register(
                    failed_job_id,
                    &format!("melt-pool run {CAMPAIGN_ID}"),
                    "img",
                    "local",
                    JobTarget::Local,
                )
                .await
                .unwrap();
            tracker
                .update_status(
                    failed_job_id,
                    TrackedStatus::Failed {
                        error: "keyhole porosity".into(),
                    },
                )
                .await
                .unwrap();
        }
        // Everything in-memory is now dropped. Only disk remains.

        // ── Hour six: a fresh process re-opens from disk ──────────────────
        let campaign =
            Campaign::from_checkpoint(&campaigns_dir.join(format!("{CAMPAIGN_ID}.json")))
                .unwrap();
        let store = ProvenanceStore::open(&prov_db).await.unwrap();
        let tracker = JobTracker::persistent(&jobs_dir).unwrap();

        let ledger = build_campaign_ledger(campaign.state(), Some(&store), Some(&tracker)).await;

        // Done: the established candidate — evidence class verbatim, not
        // greener — plus the one job that actually completed.
        assert_eq!(ledger.done.len(), 2, "done: {:#?}", ledger.done);
        let candidate = ledger
            .done
            .iter()
            .find_map(|entry| match entry {
                DoneEntry::EstablishedCandidate { .. } => Some(entry),
                _ => None,
            })
            .expect("established candidate present");
        match candidate {
            DoneEntry::EstablishedCandidate {
                candidate,
                evidence_class,
                evidence_color,
                provenance,
                ..
            } => {
                assert_eq!(candidate, "W25Mo25Ta25Nb25");
                assert_eq!(evidence_class, "screening", "class copied verbatim");
                assert_eq!(evidence_color, "yellow");
                assert!(provenance.contains(CAMPAIGN_ID), "claim is quote-bound");
            }
            _ => unreachable!(),
        }
        assert!(
            ledger.done.iter().any(|entry| matches!(
                entry,
                DoneEntry::CompletedJob { job_id, .. } if *job_id == done_job_id.to_string()
            )),
            "completed job is done"
        );
        assert!(
            !ledger
                .done
                .iter()
                .any(|entry| matches!(entry, DoneEntry::CompletedJob { job_id, .. }
                    if *job_id == running_job_id.to_string())),
            "a running job must never be reported done"
        );

        // Pending: the running job and the planned remaining iterations.
        assert!(
            ledger.pending.iter().any(|entry| matches!(
                entry,
                PendingEntry::JobRunning { job_id, progress, .. }
                    if *job_id == running_job_id.to_string() && (*progress - 0.5).abs() < f64::EPSILON
            )),
            "running job is pending: {:#?}",
            ledger.pending
        );
        assert!(
            ledger.pending.iter().any(|entry| matches!(
                entry,
                PendingEntry::PlannedIterations { remaining: 6, of: 10, status }
                    if status == "paused"
            )),
            "4 of 10 iterations run leaves 6 planned"
        );

        // Refuted: the rejected candidate WITH its reason, the failed job,
        // and the failed provenance action — all three memories retained.
        let rejection = ledger
            .refuted
            .iter()
            .find_map(|entry| match entry {
                RefutedEntry::RejectedCandidate { .. } => Some(entry),
                _ => None,
            })
            .expect("rejected candidate present");
        match rejection {
            RefutedEntry::RejectedCandidate {
                candidate,
                reasons,
                evaluated,
                ..
            } => {
                assert_eq!(candidate, "W80Mo10Ta5Nb5");
                assert!(
                    reasons
                        .iter()
                        .any(|reason| reason.contains("density")),
                    "the reason survives the restart: {reasons:?}"
                );
                assert!(evaluated);
            }
            _ => unreachable!(),
        }
        assert!(
            ledger.refuted.iter().any(|entry| matches!(
                entry,
                RefutedEntry::FailedJob { error, .. } if error.contains("keyhole")
            )),
            "failed attributed job is refuted"
        );
        assert!(
            ledger.refuted.iter().any(|entry| matches!(
                entry,
                RefutedEntry::FailedAction { record_id, error, .. }
                    if *record_id == failed_action_id
                        && error.as_deref() ==
                            Some("melt-pool model rejected the composition: keyhole porosity")
            )),
            "failed provenance action is refuted with its error"
        );

        // Every store answered — no gaps.
        assert!(ledger.gaps.is_empty(), "gaps: {:?}", ledger.gaps);

        std::fs::remove_dir_all(base).unwrap();
    }

    #[tokio::test]
    async fn submitted_but_unfinished_job_reports_pending_never_done() {
        let state = CampaignState::new(CAMPAIGN_ID.to_string(), goal(), config());
        let tracker = JobTracker::new();
        let queued_id = Uuid::new_v4();
        let running_id = Uuid::new_v4();
        tracker
            .register(
                queued_id,
                &format!("sim for {CAMPAIGN_ID}"),
                "img",
                "local",
                JobTarget::Local,
            )
            .await
            .unwrap();
        tracker
            .register(
                running_id,
                &format!("sim two for {CAMPAIGN_ID}"),
                "img",
                "local",
                JobTarget::Local,
            )
            .await
            .unwrap();
        tracker
            .update_status(running_id, TrackedStatus::Running { progress: 0.1 })
            .await
            .unwrap();

        let ledger = build_campaign_ledger(&state, None, Some(&tracker)).await;

        assert!(
            ledger
                .done
                .iter()
                .all(|entry| !matches!(entry, DoneEntry::CompletedJob { .. })),
            "no job is done: {:#?}",
            ledger.done
        );
        let queued = ledger.pending.iter().filter(|entry| matches!(
            entry,
            PendingEntry::JobQueued { job_id, .. } if *job_id == queued_id.to_string()
        ));
        assert_eq!(queued.count(), 1, "queued job is pending");
        let running = ledger.pending.iter().filter(|entry| matches!(
            entry,
            PendingEntry::JobRunning { job_id, .. } if *job_id == running_id.to_string()
        ));
        assert_eq!(running.count(), 1, "running job is pending");
    }

    #[tokio::test]
    async fn refuted_attempt_is_retained_with_reason() {
        let mut state = CampaignState::new(CAMPAIGN_ID.to_string(), goal(), config());
        // An evaluated rejection and a pre-evaluation invalid proposal.
        state.rejected_candidates.push(ConstraintRejection {
            composition: "W90Mo5Ta3Nb2".to_string(),
            properties: serde_json::json!({}),
            iteration: 1,
            reasons: vec![
                "melting point below the 2000 K constraint".into(),
                "configurational entropy below the HEA threshold".into(),
            ],
            evaluated: true,
            raw_proposal: None,
        });
        state.rejected_candidates.push(ConstraintRejection {
            composition: String::new(),
            properties: serde_json::json!({"evaluation_skipped": true}),
            iteration: 1,
            reasons: vec!["composition is empty".into()],
            evaluated: false,
            raw_proposal: None,
        });

        let ledger = build_campaign_ledger(&state, None, None).await;

        let evaluated = ledger.refuted.iter().find_map(|entry| match entry {
            RefutedEntry::RejectedCandidate {
                candidate,
                reasons,
                evaluated,
                ..
            } if *evaluated => Some((candidate, reasons)),
            _ => None,
        });
        let (candidate, reasons) = evaluated.expect("evaluated rejection present");
        assert_eq!(candidate, "W90Mo5Ta3Nb2");
        assert_eq!(reasons.len(), 2, "every reason retained: {reasons:?}");

        // The invalid proposal is retained with its reason, but its raw
        // (non-checkpointed) identity never leaks into the durable view.
        let invalid = ledger.refuted.iter().find_map(|entry| match entry {
            RefutedEntry::RejectedCandidate {
                candidate,
                reasons,
                evaluated,
                ..
            } if !evaluated => Some((candidate, reasons)),
            _ => None,
        });
        let (candidate, reasons) = invalid.expect("pre-evaluation rejection present");
        assert!(candidate.starts_with("<invalid proposal"));
        assert_eq!(reasons, &vec!["composition is empty".to_string()]);
    }

    #[tokio::test]
    async fn unanswerable_store_produces_stated_gap_not_silent_omission() {
        let mut state = CampaignState::new(CAMPAIGN_ID.to_string(), goal(), config());
        state.status = GoalStatus::Completed;

        // Neither optional store can answer: both absences must be SAID.
        let ledger = build_campaign_ledger(&state, None, None).await;
        assert_eq!(ledger.gaps.len(), 2, "gaps: {:?}", ledger.gaps);
        assert!(
            ledger
                .gaps
                .iter()
                .any(|gap| gap.contains("compute job registry unavailable")),
            "job-store absence is stated"
        );
        assert!(
            ledger
                .gaps
                .iter()
                .any(|gap| gap.contains("provenance store unavailable")),
            "provenance absence is stated"
        );

        // A store that CAN open but cannot attribute its rows must say so
        // too: an active job with no campaign reference in its name is
        // neither silently claimed nor silently dropped.
        let tracker = JobTracker::new();
        let orphan_id = Uuid::new_v4();
        tracker
            .register(orphan_id, "someone else's sim", "img", "local", JobTarget::Local)
            .await
            .unwrap();
        let ledger = build_campaign_ledger(&state, None, Some(&tracker)).await;
        assert!(
            ledger
                .pending
                .iter()
                .all(|entry| !matches!(entry, PendingEntry::JobQueued { .. })),
            "an unattributable job is never claimed by this campaign"
        );
        assert!(
            ledger.gaps.iter().any(|gap| gap
                .contains("1 active job(s) that cannot be attributed")),
            "attribution failure is stated: {:?}",
            ledger.gaps
        );
    }

    #[tokio::test]
    async fn evidence_class_is_never_reported_greener_than_its_source() {
        let mut state = CampaignState::new(CAMPAIGN_ID.to_string(), goal(), config());
        state.status = GoalStatus::Completed;
        let mut red = accepted_candidate("W25Mo25Ta25Nb25", 0.4, 0);
        // No evidence in the properties → indeterminate (RED) is the honest
        // class, and the ledger must surface exactly that.
        red.evidence_class = crate::EvidenceClass::Indeterminate;
        let mut orange = accepted_candidate("W20Mo30Ta30Nb20", 0.6, 1);
        orange.evidence_class = crate::EvidenceClass::Research;
        state.candidates = vec![orange, red];

        let ledger = build_campaign_ledger(&state, None, None).await;
        let classes: Vec<(String, String)> = ledger
            .done
            .iter()
            .map(|entry| match entry {
                DoneEntry::EstablishedCandidate {
                    evidence_class,
                    evidence_color,
                    ..
                } => (evidence_class.clone(), evidence_color.clone()),
                _ => unreachable!("only candidates in done here"),
            })
            .collect();
        assert_eq!(
            classes,
            vec![
                ("research".to_string(), "orange".to_string()),
                ("indeterminate".to_string(), "red".to_string()),
            ],
            "classes leave the ledger exactly as stored"
        );
    }

    #[tokio::test]
    async fn terminal_goal_reports_no_planned_iterations() {
        let mut state = CampaignState::new(CAMPAIGN_ID.to_string(), goal(), config());
        state.status = GoalStatus::Completed;
        state.current_iteration = 3; // less than max — but terminal wins
        let ledger = build_campaign_ledger(&state, None, None).await;
        assert!(
            ledger
                .pending
                .iter()
                .all(|entry| !matches!(entry, PendingEntry::PlannedIterations { .. })),
            "a completed goal plans nothing: {:#?}",
            ledger.pending
        );
    }

    #[tokio::test]
    async fn checkpoint_round_trip_keeps_ledger_readable() {
        // `Campaign::from_checkpoint` is the reader every caller (CLI, tool)
        // uses; prove a ledger can be built straight off its output.
        let dir =
            std::env::temp_dir().join(format!("prism-ledger-roundtrip-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut state = CampaignState::new(CAMPAIGN_ID.to_string(), goal(), config());
        state.candidates.push(accepted_candidate("NbMoTaW", 0.5, 0));
        let path = dir.join(format!("{CAMPAIGN_ID}.json"));
        std::fs::write(&path, serde_json::to_string_pretty(&state).unwrap()).unwrap();

        let campaign = Campaign::from_checkpoint(&path).unwrap();
        let ledger = build_campaign_ledger(campaign.state(), None, None).await;
        assert!(matches!(
            ledger.done.first(),
            Some(DoneEntry::EstablishedCandidate { candidate, .. }) if candidate == "NbMoTaW"
        ));
        // Provenance/absent stores show up as stated gaps, not silence.
        assert_eq!(ledger.gaps.len(), 2);

        std::fs::remove_dir_all(dir).unwrap();
    }
}
