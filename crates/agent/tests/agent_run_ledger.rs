//! Disconnect-sensitive coverage for the public turn-to-ledger write-through.

#[path = "support/agent_run_harness.rs"]
mod agent_run_harness;
mod common;

use agent_run_harness::{
    TEST_EXPECTED_COST_USD, TEST_INPUT_TOKENS, TEST_OUTPUT_TOKENS, run_stub_turn,
};
use prism_provenance::{AgentRunFilter, AgentRunStatus, ProvenanceStore};

#[tokio::test(flavor = "multi_thread")]
async fn completed_real_turn_persists_tokens_and_exact_emitted_cost() {
    let session_id = "agent-run-ledger-real-turn";
    let outcome = run_stub_turn(session_id)
        .await
        .expect("real agent turn must complete");
    assert_eq!(outcome.answer, "LEDGER_TURN_DONE");

    let store = ProvenanceStore::open(&prism_agent::hooks::provenance_db_path())
        .await
        .expect("open isolated run ledger");
    let runs = store
        .list_agent_runs(&AgentRunFilter {
            session_id: Some(session_id.to_string()),
            ..Default::default()
        })
        .await
        .expect("query completed run");
    assert_eq!(runs.len(), 1, "the public turn must create exactly one row");
    let run = &runs[0];
    assert_eq!(run.status, AgentRunStatus::Completed);
    assert_eq!(run.tokens_in, TEST_INPUT_TOKENS);
    assert_eq!(run.tokens_out, TEST_OUTPUT_TOKENS);
    assert!(run.ended_at.is_some());
    assert!(run.cost_usd > 0.0, "the priced model must record spend");
    assert!(
        (run.cost_usd - TEST_EXPECTED_COST_USD).abs() < f64::EPSILON,
        "the ledger must price tokens with the model used by the LLM client"
    );
    assert!(
        (run.cost_usd - outcome.estimated_cost).abs() < f64::EPSILON,
        "the durable cost must be the exact cost emitted by the turn"
    );
}
