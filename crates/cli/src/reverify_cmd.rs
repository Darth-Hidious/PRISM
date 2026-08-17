//! `prism reverify` — re-read a stored assertion's exact cited source.
//!
//! The surface that makes annotate-don't-refuse honest: a weak fact is
//! stored WITH its status precisely so a later reader can re-check it
//! against the source, and this is that re-check. It targets the
//! span-unchecked population (`cited_by_reader` — what the fresh paper
//! path produces) and every other status that carries no rendered
//! judgement; an assertion whose judgement was already rendered is
//! refused, in code, by the re-reader itself.
//!
//! Every verdict — affirmed, denied, uncertain, or `not_ready` (the source
//! moved, the cited lines changed, the witness is legacy) — is appended to
//! the store's `reverify_verdict` ledger and printed with the exact lines
//! it was rendered from. Nothing is silently skipped and nothing rewrites
//! the assertion's verification status: the ledger is the audit axis.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use prism_provenance::{ProvenanceStore, StoredAssertion, VerificationStatus};
use prism_retrieval::reverify::{
    EvidenceReverification, RecordedReverification, RereadOutcome, reverify_and_record,
};

#[derive(Debug, Subcommand)]
pub enum ReverifyCommands {
    /// List stored assertions by verification status — the candidate
    /// population for re-verification. `cited_by_reader` is the
    /// span-unchecked set the fresh paper path produces.
    List {
        /// Exact status spelling, e.g. `cited_by_reader` or
        /// `sample_disagreement` (see `--status` with an invalid value for
        /// the full list).
        #[arg(long)]
        status: String,
        /// Max assertions to list.
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Print the machine shape instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Re-read one assertion's exact cited lines and ask the configured
    /// model whether they support it. Every verdict is recorded in the
    /// reverify ledger; the assertion's own status is never rewritten.
    Run {
        /// Stable assertion id (from `reverify list`).
        #[arg(long)]
        assertion: String,
        /// Override the LLM model that judges the cited lines.
        #[arg(long)]
        model: Option<String>,
        /// Override the LLM base URL.
        #[arg(long)]
        llm_url: Option<String>,
        /// API key for authenticated LLM providers.
        #[arg(long, env = "LLM_API_KEY")]
        api_key: Option<String>,
        /// Print the machine shape instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Show every recorded re-verification verdict for one assertion,
    /// oldest first — the audit trail, with no model call.
    History {
        /// Stable assertion id.
        #[arg(long)]
        assertion: String,
        /// Print the machine shape instead of text.
        #[arg(long)]
        json: bool,
    },
}

/// Where the reverify ledger and assertions live — the same store every
/// ingest path writes (`~/.prism/provenance.db`).
fn store_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    Ok(PathBuf::from(home).join(".prism/provenance.db"))
}

/// Parse a `--status` argument or fail with the full valid list. Status
/// spellings come from [`VerificationStatus::ALL`] — never a hardcoded list.
fn parse_status(raw: &str) -> Result<VerificationStatus> {
    VerificationStatus::parse(raw).with_context(|| {
        format!(
            "unknown verification status {raw:?}; valid spellings: {}",
            VerificationStatus::ALL
                .iter()
                .map(|status| status.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

/// The read scope for the candidate listing: local plus discovered mesh
/// tenants, exactly what `prism query` reads. Honest fallback: local only,
/// with a warning, when discovery fails.
async fn read_tenants(store: &ProvenanceStore) -> Vec<String> {
    store.default_read_tenants().await.unwrap_or_else(|error| {
        eprintln!(
            "  warning: mesh tenant discovery failed — listing LOCAL knowledge only: {error}"
        );
        vec!["local".to_string()]
    })
}

/// One assertion as the listing's machine shape.
fn candidate_json(assertion: &StoredAssertion) -> serde_json::Value {
    serde_json::json!({
        "assertion_id": assertion.id,
        "subject": assertion.subject,
        "predicate": assertion.predicate,
        "object": assertion.object,
        "value": assertion.value,
        "unit": assertion.unit,
        "conditions": assertion.conditions,
        "verification_status": assertion
            .verification_status
            .map(VerificationStatus::as_str),
        "verification_reason": assertion.verification_reason,
        "tenant": assertion.tenant,
    })
}

/// The run's machine shape: the assertion, every witness outcome with the
/// exact lines an assessment was rendered from, and the verdict rows. Rows
/// from EARLIER runs are shown separately (`prior`) so a caller can tell
/// this run's verdicts from the audit trail it joined.
fn run_json(
    recorded: &RecordedReverification,
    prior: &[prism_provenance::ReverifyVerdict],
) -> serde_json::Value {
    let evidence: Vec<serde_json::Value> = recorded
        .reverification
        .evidence
        .iter()
        .map(|item| match item {
            EvidenceReverification::Assessed {
                context,
                affirmation,
            } => serde_json::json!({
                "source_key": context.target().evidence.source_key,
                "outcome": "assessed",
                "verdict": affirmation.verdict.ledger_spelling(),
                "reason": affirmation.reason,
                "cited_lines": context
                    .cited_lines()
                    .iter()
                    .map(|line| serde_json::json!({ "line": line.number, "text": line.text }))
                    .collect::<Vec<_>>(),
            }),
            EvidenceReverification::NotReady(outcome) => {
                let (source_key, outcome_spelling) = witness_of(outcome);
                serde_json::json!({
                    "source_key": source_key,
                    "outcome": outcome_spelling,
                })
            }
        })
        .collect();
    serde_json::json!({
        "assertion": candidate_json(&recorded.reverification.assertion),
        "evidence": evidence,
        "recorded_verdicts": recorded.verdicts,
        "prior_verdicts": prior,
    })
}

/// The source key and a machine spelling for one not-ready outcome.
fn witness_of(outcome: &RereadOutcome) -> (&str, &'static str) {
    match outcome {
        RereadOutcome::Ready(_) => ("", "ready"),
        RereadOutcome::SourceUnavailable { target, .. } => {
            (&target.evidence.source_key, "not_ready")
        }
        RereadOutcome::CitationUnavailable { target, .. } => {
            (&target.evidence.source_key, "not_ready")
        }
        RereadOutcome::SourceChanged { target, .. } => {
            (&target.evidence.source_key, "source_changed")
        }
        RereadOutcome::CitedLinesChanged { target, .. } => {
            (&target.evidence.source_key, "cited_lines_changed")
        }
    }
}

pub(crate) async fn run(command: ReverifyCommands, project_root: &Path) -> Result<()> {
    let store = ProvenanceStore::open(&store_path()?).await?;
    match command {
        ReverifyCommands::List {
            status,
            limit,
            json,
        } => {
            let status = parse_status(&status)?;
            let tenants = read_tenants(&store).await;
            let tenants: Vec<&str> = tenants.iter().map(String::as_str).collect();
            let candidates = store
                .assertions_by_verification(status, &tenants, limit as i64)
                .await?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "status": status.as_str(),
                        "limit": limit,
                        "assertions": candidates
                            .iter()
                            .map(candidate_json)
                            .collect::<Vec<_>>(),
                    }))?
                );
            } else {
                if candidates.is_empty() {
                    println!(
                        "no assertions with status {:?} in the local store — ingest a paper \
                         first (`prism papers ingest`)",
                        status.as_str()
                    );
                    return Ok(());
                }
                println!(
                    "assertions with status {:?} ({} of at most {}):",
                    status.as_str(),
                    candidates.len(),
                    limit
                );
                for assertion in &candidates {
                    println!("  {}", assertion.id);
                    let value = assertion.value.map(|value| {
                        format!(
                            " value={value}{}",
                            assertion
                                .unit
                                .as_deref()
                                .map_or_else(String::new, |unit| format!(" {unit}"))
                        )
                    });
                    println!(
                        "      '{} {} {}'{} — re-check with `prism reverify run --assertion {}`",
                        assertion.subject,
                        assertion.predicate,
                        assertion.object,
                        value.unwrap_or_default(),
                        assertion.id
                    );
                    if let Some(reason) = &assertion.verification_reason {
                        println!("      {reason}");
                    }
                }
            }
        }
        ReverifyCommands::Run {
            assertion,
            model,
            llm_url,
            api_key,
            json,
        } => {
            let llm_config = crate::build_llm_config(
                project_root,
                llm_url.as_deref(),
                model.as_deref(),
                api_key.as_deref(),
            )?;
            let llm = prism_ingest::llm::LlmClient::new(llm_config);
            let decided_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_secs_f64())
                .unwrap_or(0.0);
            let Some(recorded) = reverify_and_record(&store, &llm, &assertion, decided_at).await?
            else {
                bail!(
                    "no stored assertion with id {assertion:?} — find ids with `prism reverify list --status <status>`"
                );
            };
            let history = store.reverify_verdicts(&assertion).await?;
            let prior: Vec<_> = history
                .iter()
                .filter(|v| {
                    !recorded.verdicts.iter().any(|fresh| {
                        fresh.source_key == v.source_key && fresh.decided_at == v.decided_at
                    })
                })
                .cloned()
                .collect();
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&run_json(&recorded, &prior))?
                );
            } else {
                println!(
                    "re-verified {} — verdicts recorded in the reverify ledger:",
                    recorded.reverification.assertion.id
                );
                for verdict in &recorded.verdicts {
                    println!(
                        "  [{}] {} — {}",
                        verdict.verdict, verdict.source_key, verdict.reason
                    );
                    println!("      by {}", verdict.reviewer);
                }
                let prior: Vec<_> = history
                    .iter()
                    .filter(|v| {
                        !recorded.verdicts.iter().any(|fresh| {
                            fresh.source_key == v.source_key && fresh.decided_at == v.decided_at
                        })
                    })
                    .collect();
                if !prior.is_empty() {
                    println!("prior verdicts for this assertion (oldest first):");
                    for verdict in prior {
                        println!(
                            "  [{}] {} — {} (by {})",
                            verdict.verdict, verdict.source_key, verdict.reason, verdict.reviewer
                        );
                    }
                }
            }
        }
        ReverifyCommands::History { assertion, json } => {
            let history = store.reverify_verdicts(&assertion).await?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "assertion_id": assertion,
                        "verdicts": history,
                    }))?
                );
            } else {
                if history.is_empty() {
                    println!(
                        "no recorded reverify verdicts for {assertion:?} — run one with \
                         `prism reverify run --assertion {assertion}`"
                    );
                    return Ok(());
                }
                println!("recorded reverify verdicts for {assertion} (oldest first):");
                for verdict in &history {
                    println!(
                        "  [{}] {} — {}",
                        verdict.verdict, verdict.source_key, verdict.reason
                    );
                    println!("      by {}", verdict.reviewer);
                }
            }
        }
    }
    Ok(())
}
