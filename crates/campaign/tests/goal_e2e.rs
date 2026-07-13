//! End-to-end proof of the goal/campaign loop.
//!
//! Everything in these tests is the REAL production path — the campaign
//! engine loop, per-transition Turso provenance writes, JSON checkpointing,
//! pause/resume — except the two external boundaries the engine has:
//!
//! 1. the proposal LLM (`POST /v1/chat/completions`)
//! 2. the node's evaluate_material tool (`POST /api/tools/evaluate_material/run`)
//!
//! which are served by an in-process HTTP fake, injected through the real
//! `CampaignConfig::{llm_base_url, node_base_url}` config knobs.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::post;
use serde_json::{Value, json};

use prism_campaign::{Campaign, CampaignConfig, CampaignGoal, GoalStatus};
use prism_provenance::ProvenanceStore;

#[derive(Clone)]
struct Boundary {
    llm_calls: Arc<AtomicUsize>,
    eval_calls: Arc<AtomicUsize>,
    eval_fails: bool,
}

async fn llm_chat(State(b): State<Boundary>, Json(_body): Json<Value>) -> Json<Value> {
    b.llm_calls.fetch_add(1, Ordering::SeqCst);
    // OpenAI-compatible chat completion carrying a JSON array of
    // compositions, exactly what a real proposal model returns.
    Json(json!({
        "choices": [{
            "message": { "content": "[\"W0.5 Mo0.5\", \"Ta0.6 Nb0.4\"]" }
        }]
    }))
}

async fn evaluate_material(
    State(b): State<Boundary>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let n = b.eval_calls.fetch_add(1, Ordering::SeqCst);
    if b.eval_fails {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "evaluator down" })),
        );
    }
    let composition = body["inputs"]["composition"].as_str().unwrap_or("");
    // Deterministic but call-varying physics so ranking is meaningful.
    (
        StatusCode::OK,
        Json(json!({
            "composition": composition,
            "mixing_entropy": 1.0 + 0.1 * n as f64,
            "density": 10.0 - 0.5 * n as f64,
        })),
    )
}

/// Serve the two external boundaries on an ephemeral port; return the base URL.
async fn spawn_boundary(eval_fails: bool) -> (String, Boundary) {
    let boundary = Boundary {
        llm_calls: Arc::new(AtomicUsize::new(0)),
        eval_calls: Arc::new(AtomicUsize::new(0)),
        eval_fails,
    };
    let app = axum::Router::new()
        .route("/v1/chat/completions", post(llm_chat))
        .route("/api/tools/evaluate_material/run", post(evaluate_material))
        .with_state(boundary.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), boundary)
}

fn test_goal() -> CampaignGoal {
    CampaignGoal {
        description: "Refractory alloy with high mixing entropy".into(),
        elements: vec!["W".into(), "Mo".into(), "Ta".into(), "Nb".into()],
        objective: "maximize mixing entropy".into(),
        constraints: vec![],
        seeds: vec![],
    }
}

fn config(base: &str, dir: &std::path::Path) -> CampaignConfig {
    CampaignConfig {
        max_iterations: 2,
        batch_size: 2,
        checkpoint_every: 1,
        checkpoint_dir: Some(dir.to_path_buf()),
        llm_base_url: Some(format!("{base}/v1")),
        node_base_url: Some(base.to_string()),
        ..Default::default()
    }
}

fn checkpoint_json(dir: &std::path::Path, id: &str) -> Value {
    let text = std::fs::read_to_string(dir.join(format!("{id}.json"))).unwrap();
    serde_json::from_str(&text).unwrap()
}

/// Happy path: a submitted goal really executes its steps, persists every
/// progress transition to the store, and stores a real terminal result.
#[tokio::test]
async fn goal_executes_steps_persists_trail_and_result() {
    let (base, boundary) = spawn_boundary(false).await;
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("provenance.db");
    let store = ProvenanceStore::open(&db).await.unwrap();

    let id = "goal-e2e-happy";
    let mut campaign =
        Campaign::new(test_goal(), config(&base, tmp.path()), id.into()).with_provenance(store);
    let result = campaign.run().await.expect("goal must run to completion");
    drop(campaign);

    // The steps really ran at the boundary: one LLM proposal per iteration,
    // batch_size evaluations per iteration.
    assert_eq!(boundary.llm_calls.load(Ordering::SeqCst), 2);
    assert_eq!(boundary.eval_calls.load(Ordering::SeqCst), 4);

    // Real terminal result.
    assert_eq!(result.state.status, GoalStatus::Completed);
    assert_eq!(result.state.completion_reason, "iteration_limit");
    assert_eq!(result.state.total_evaluated(), 4);
    assert!(!result.winners.is_empty());
    assert!(result.summary.contains("Best:"));
    assert!(
        !result.provenance.is_empty(),
        "result must carry the provenance trail"
    );

    // Persisted trail — reopened fresh from disk to prove durability.
    let store = ProvenanceStore::open(&db).await.unwrap();
    let trail = store.query_by_session(id).await.unwrap();
    println!("--- persisted progress trail ({id}) ---");
    for r in &trail {
        println!(
            "{}  {}  {}",
            r.timestamp,
            r.tool_name.as_deref().unwrap_or("-"),
            serde_json::to_string(&r.input_json).unwrap()
        );
    }
    let events: Vec<&str> = trail
        .iter()
        .filter_map(|r| r.tool_name.as_deref())
        .collect();
    let count = |name: &str| events.iter().filter(|e| **e == name).count();
    assert_eq!(count("campaign.submitted"), 1);
    assert_eq!(count("campaign.status.running"), 1);
    assert_eq!(count("campaign.propose"), 2);
    assert_eq!(count("campaign.evaluate"), 4);
    assert_eq!(count("campaign.iteration"), 2);
    assert_eq!(count("campaign.status.completed"), 1);
    assert_eq!(count("campaign.status.failed"), 0);
    // Order: submitted → running → … → completed last.
    assert_eq!(events.first(), Some(&"campaign.submitted"));
    assert_eq!(events.get(1), Some(&"campaign.status.running"));
    assert_eq!(events.last(), Some(&"campaign.status.completed"));

    // The terminal transition record stores the real result.
    let completed = trail
        .iter()
        .find(|r| r.tool_name.as_deref() == Some("campaign.status.completed"))
        .unwrap();
    assert_eq!(completed.input_json["reason"], "iteration_limit");
    assert_eq!(completed.input_json["candidates"], 4);
    assert!(
        !completed.input_json["winners"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let stored_summary = completed.input_json["summary"].as_str().unwrap();
    assert!(stored_summary.contains("Best:"));
    assert!(stored_summary.contains("completed (iteration_limit)"));
    println!("--- terminal result ---");
    println!("{}", completed.input_json["summary"].as_str().unwrap());

    // Checkpoint on disk agrees with the store.
    let cp = checkpoint_json(tmp.path(), id);
    assert_eq!(cp["status"], "completed");
    assert_eq!(cp["completed"], true);
    assert_eq!(cp["candidates"].as_array().unwrap().len(), 4);
}

/// Honesty: when the evaluator is down, every step fails — the goal must
/// end Failed with the error persisted, never "completed".
#[tokio::test]
async fn goal_must_not_complete_when_steps_cannot_run() {
    let (base, boundary) = spawn_boundary(true).await;
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("provenance.db");
    let store = ProvenanceStore::open(&db).await.unwrap();

    let id = "goal-e2e-failing";
    let mut campaign =
        Campaign::new(test_goal(), config(&base, tmp.path()), id.into()).with_provenance(store);
    let err = campaign
        .run()
        .await
        .expect_err("all evaluations failed — the goal must NOT complete");
    assert!(
        err.to_string().contains("evaluations failed"),
        "err: {err:#}"
    );

    // The steps were really attempted at the boundary.
    assert!(boundary.eval_calls.load(Ordering::SeqCst) > 0);

    assert_eq!(campaign.state().status, GoalStatus::Failed);
    assert!(!campaign.state().completed);
    assert!(campaign.state().completion_reason.starts_with("failed:"));
    drop(campaign);

    // Trail shows the failure, and no fake completion.
    let store = ProvenanceStore::open(&db).await.unwrap();
    let trail = store.query_by_session(id).await.unwrap();
    let events: Vec<&str> = trail
        .iter()
        .filter_map(|r| r.tool_name.as_deref())
        .collect();
    assert!(events.contains(&"campaign.status.failed"));
    assert!(!events.contains(&"campaign.status.completed"));
    let failed = trail
        .iter()
        .find(|r| r.tool_name.as_deref() == Some("campaign.status.failed"))
        .unwrap();
    assert!(
        failed.input_json["error"]
            .as_str()
            .unwrap()
            .contains("evaluations failed")
    );

    let cp = checkpoint_json(tmp.path(), id);
    assert_eq!(cp["status"], "failed");
    assert_eq!(cp["completed"], false);
}

/// Approval gate: the goal pauses (persisted transition), then a resume from
/// the checkpoint — exactly what the detached worker does — drives it through
/// the gate to real completion.
#[tokio::test]
async fn goal_pauses_at_gate_and_resumes_to_completion() {
    let (base, boundary) = spawn_boundary(false).await;
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("provenance.db");

    let id = "goal-e2e-gated";
    let mut cfg = config(&base, tmp.path());
    cfg.approval_gate_at = vec![1];

    let store = ProvenanceStore::open(&db).await.unwrap();
    let mut campaign = Campaign::new(test_goal(), cfg, id.into()).with_provenance(store);
    let paused = campaign.run().await.expect("run until the gate");
    assert_eq!(paused.state.status, GoalStatus::Paused);
    assert_eq!(paused.state.current_iteration, 1);
    drop(campaign);

    // Resume from the checkpoint like `prism campaign continue` does.
    let cp_path = tmp.path().join(format!("{id}.json"));
    let store = ProvenanceStore::open(&db).await.unwrap();
    let mut resumed = Campaign::from_checkpoint(&cp_path)
        .unwrap()
        .with_provenance(store);
    assert_eq!(resumed.state().status, GoalStatus::Paused);
    let result = resumed.resume().await.expect("resume to completion");
    assert_eq!(result.state.status, GoalStatus::Completed);
    assert_eq!(result.state.total_evaluated(), 4);
    drop(resumed);

    // Both iterations really executed across the pause.
    assert_eq!(boundary.llm_calls.load(Ordering::SeqCst), 2);
    assert_eq!(boundary.eval_calls.load(Ordering::SeqCst), 4);

    // One coherent trail across both processes-worth of work.
    let store = ProvenanceStore::open(&db).await.unwrap();
    let trail = store.query_by_session(id).await.unwrap();
    let events: Vec<&str> = trail
        .iter()
        .filter_map(|r| r.tool_name.as_deref())
        .collect();
    println!("--- gated trail ({id}) ---");
    for e in &events {
        println!("{e}");
    }
    let count = |name: &str| events.iter().filter(|e| **e == name).count();
    assert_eq!(count("campaign.submitted"), 1);
    assert_eq!(count("campaign.status.running"), 2); // initial + resume
    assert_eq!(count("campaign.status.paused"), 1);
    assert_eq!(count("campaign.iteration"), 2);
    assert_eq!(count("campaign.status.completed"), 1);
    assert_eq!(events.last(), Some(&"campaign.status.completed"));
}
