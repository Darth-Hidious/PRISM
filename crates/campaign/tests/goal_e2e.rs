//! End-to-end proof of the goal/campaign loop.
//!
//! Everything in these tests is the REAL production path — the campaign
//! engine loop, per-transition Turso provenance writes, JSON checkpointing,
//! pause/resume — except the two external boundaries the engine has:
//!
//! 1. the proposal LLM (`POST /v1/chat/completions`)
//! 2. the node's HEA evaluator (`POST /api/tools/hea_descriptors/run`)
//!
//! which are served by an in-process HTTP fake, injected through the real
//! `CampaignConfig::{llm_base_url, node_base_url}` config knobs.

use std::ffi::OsString;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Json;
use axum::routing::post;
use serde_json::{Value, json};

use prism_campaign::{Campaign, CampaignConfig, CampaignGoal, EvidenceClass, GoalStatus};
use prism_provenance::ProvenanceStore;

#[derive(Clone)]
struct Boundary {
    llm_calls: Arc<AtomicUsize>,
    session_calls: Arc<AtomicUsize>,
    eval_calls: Arc<AtomicUsize>,
    auth_headers: Arc<Mutex<Vec<Option<String>>>>,
    eval_fails: bool,
    hea_constraint_scenario: bool,
    llm_response: String,
    /// USD the evaluator reports per candidate. The proposal LLM has no
    /// equivalent — `LlmClient::chat` returns `Result<String>` — which is the
    /// whole point of `ceiling_declares_the_proposal_calls_it_cannot_price`.
    eval_cost_usd: f64,
}

static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct EnvGuard {
    previous_home: Option<OsString>,
}

impl EnvGuard {
    fn set_test_home(home: &std::path::Path) -> Self {
        let previous_home = std::env::var_os("HOME");
        // SAFETY: every test in this integration-test process holds ENV_LOCK.
        unsafe { std::env::set_var("HOME", home) };
        Self { previous_home }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: every test in this integration-test process holds ENV_LOCK.
        unsafe {
            match &self.previous_home {
                Some(home) => std::env::set_var("HOME", home),
                None => std::env::remove_var("HOME"),
            }
        }
    }
}

fn install_test_identity(home: &std::path::Path) -> EnvGuard {
    let guard = EnvGuard::set_test_home(home);
    let paths = prism_runtime::PrismPaths::discover().unwrap();
    paths
        .save_cli_state(&prism_runtime::PrismCliState {
            credentials: Some(prism_runtime::StoredCredentials {
                user_id: Some("campaign-test-user".into()),
                display_name: Some("Campaign Test User".into()),
                ..Default::default()
            }),
            ..Default::default()
        })
        .unwrap();
    guard
}

async fn llm_chat(State(b): State<Boundary>, Json(_body): Json<Value>) -> Json<Value> {
    b.llm_calls.fetch_add(1, Ordering::SeqCst);
    // OpenAI-compatible chat completion carrying a JSON array of
    // compositions, exactly what a real proposal model returns.
    Json(json!({
        "choices": [{
            "message": { "content": b.llm_response }
        }]
    }))
}

async fn create_session(State(b): State<Boundary>, Json(_body): Json<Value>) -> Json<Value> {
    b.session_calls.fetch_add(1, Ordering::SeqCst);
    Json(json!({"session_id": "goal-e2e-node-session"}))
}

async fn evaluate_material(
    State(b): State<Boundary>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let auth = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    b.auth_headers.lock().unwrap().push(auth.clone());
    let n = b.eval_calls.fetch_add(1, Ordering::SeqCst);
    if auth.as_deref() != Some("Bearer goal-e2e-node-session") {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "missing or invalid session token"})),
        );
    }
    if b.eval_fails {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "evaluator down" })),
        );
    }
    let composition = body["inputs"]["composition"].as_str().unwrap_or("");
    // Reproduce the live objective-degeneration defect: nearly pure W has a
    // higher melting point than the valid five-principal-element candidate.
    let mut properties = if b.hea_constraint_scenario && composition.starts_with("W0.995") {
        json!({
            "composition": composition,
            "Tm_estimate_K": 3693.8,
            "delta_S_mix_J_per_molK": 1.63,
            "fractions": [0.995, 0.005],
            "n_elements": 2,
        })
    } else if b.hea_constraint_scenario {
        json!({
            "composition": composition,
            "Tm_estimate_K": 3300.0,
            "delta_S_mix_J_per_molK": 13.38,
            "fractions": [0.2, 0.2, 0.2, 0.2, 0.2],
            "n_elements": 5,
        })
    } else {
        // Deterministic but call-varying physics so ranking is meaningful.
        json!({
            "composition": composition,
            "Tm_estimate_K": 3000.0 + 10.0 * n as f64,
            "delta_S_mix_J_per_molK": 12.0 + 0.5 * n as f64,
            "delta_H_mix_kJ_per_mol": -5.0,
            "omega": 8.0 + 0.1 * n as f64,
            "VEC": 5.5,
            "delta_radius_pct": 2.1,
            "phase_prediction": "solid_solution",
        })
    };
    if b.eval_cost_usd > 0.0 {
        properties["cost_usd"] = json!(b.eval_cost_usd);
    }
    (
        StatusCode::OK,
        Json(json!({
            "tool": "hea_descriptors",
            "result": { "result": properties }
        })),
    )
}

/// Serve the two external boundaries on an ephemeral port; return the base URL.
async fn spawn_boundary_with_llm(
    eval_fails: bool,
    eval_cost_usd: f64,
    hea_constraint_scenario: bool,
    llm_response: &str,
) -> (String, Boundary) {
    let boundary = Boundary {
        llm_calls: Arc::new(AtomicUsize::new(0)),
        session_calls: Arc::new(AtomicUsize::new(0)),
        eval_calls: Arc::new(AtomicUsize::new(0)),
        auth_headers: Arc::new(Mutex::new(Vec::new())),
        eval_fails,
        hea_constraint_scenario,
        llm_response: llm_response.to_string(),
        eval_cost_usd,
    };
    let app = axum::Router::new()
        .route("/v1/chat/completions", post(llm_chat))
        .route("/api/sessions", post(create_session))
        .route("/api/tools/hea_descriptors/run", post(evaluate_material))
        .with_state(boundary.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), boundary)
}

async fn spawn_boundary(
    eval_fails: bool,
    eval_cost_usd: f64,
    hea_constraint_scenario: bool,
) -> (String, Boundary) {
    spawn_boundary_with_llm(
        eval_fails,
        eval_cost_usd,
        hea_constraint_scenario,
        "[\"W0.5 Mo0.5\", \"Ta0.6 Nb0.4\"]",
    )
    .await
}

async fn spawn_hea_constraint_boundary() -> (String, Boundary) {
    spawn_boundary(false, 0.0, true).await
}

fn test_goal() -> CampaignGoal {
    CampaignGoal {
        description: "Refractory alloy with high mixing entropy".into(),
        elements: vec!["W".into(), "Mo".into(), "Ta".into(), "Nb".into()],
        objective: "maximize mixing entropy".into(),
        target_property: None,
        target_direction: None,
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

/// A malformed model proposal must be a visible hard rejection, never an
/// evaluator input, ranked candidate, or persisted composition.
#[tokio::test]
async fn non_unit_llm_composition_never_reaches_evaluator_or_checkpoint() {
    let _env_lock = ENV_LOCK.lock().await;
    let test_home = tempfile::tempdir().unwrap();
    let _home = install_test_identity(test_home.path());
    let malformed = "W0.6 Mo0.2 Ta0.4 Nb0.4 V0.4";
    let valid = "W0.2 Mo0.2 Ta0.2 Nb0.2 V0.2";
    let response = serde_json::to_string(&[malformed, valid]).unwrap();
    let (base, boundary) = spawn_boundary_with_llm(false, 0.0, false, &response).await;
    let tmp = tempfile::tempdir().unwrap();
    let id = "goal-e2e-invalid-composition";
    let goal = CampaignGoal {
        description: "Find a refractory alloy with high melting point".into(),
        elements: ["W", "Mo", "Ta", "Nb", "V"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        objective: "maximize melting point".into(),
        target_property: None,
        target_direction: None,
        constraints: vec![],
        seeds: vec![],
    };
    let mut cfg = config(&base, tmp.path());
    cfg.max_iterations = 1;
    let mut campaign = Campaign::new(goal, cfg, id.into());

    let result = campaign
        .run()
        .await
        .expect("valid proposal should complete");

    assert_eq!(boundary.eval_calls.load(Ordering::SeqCst), 1);
    assert_eq!(result.state.candidates.len(), 1);
    assert_eq!(result.state.candidates[0].composition, valid);
    assert_eq!(result.state.rejected_candidates.len(), 1);
    assert_eq!(result.state.total_evaluated(), 1);
    assert_eq!(result.state.total_rejected_before_evaluation(), 1);
    assert!(result.summary.contains("REJECTED"), "{}", result.summary);
    assert!(result.summary.contains(malformed), "{}", result.summary);
    assert!(result.summary.contains("2.000000"), "{}", result.summary);

    let checkpoint = std::fs::read_to_string(tmp.path().join(format!("{id}.json"))).unwrap();
    assert!(!checkpoint.contains(malformed), "{checkpoint}");
    assert!(
        !checkpoint.contains("\"composition\": null"),
        "{checkpoint}"
    );
    let persisted: Value = serde_json::from_str(&checkpoint).unwrap();
    let rejection = &persisted["rejected_candidates"][0];
    assert_eq!(rejection["evaluated"], false);
    assert!(rejection.get("composition").is_none(), "{rejection}");
    assert!(
        rejection["reasons"][0]
            .as_str()
            .unwrap()
            .contains("got 2.000000")
    );
}

/// Regression for objective degeneration: a higher-melting near-pure element
/// must not outrank an actual HEA when the goal explicitly asks for an HEA.
#[tokio::test]
async fn hea_goal_rejects_near_pure_melting_point_exploit() {
    let _env_lock = ENV_LOCK.lock().await;
    let test_home = tempfile::tempdir().unwrap();
    let _home = install_test_identity(test_home.path());
    let (base, _boundary) = spawn_hea_constraint_boundary().await;
    let tmp = tempfile::tempdir().unwrap();

    let goal = CampaignGoal {
        description: "Find a refractory HEA with maximum melting point".into(),
        elements: ["W", "Mo", "Ta", "Nb", "V", "Re", "Hf"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        objective: "maximize melting point".into(),
        target_property: None,
        target_direction: None,
        constraints: vec![],
        seeds: vec![
            "W0.995 Re0.005".into(),
            "W0.2 Mo0.2 Ta0.2 Nb0.2 V0.2".into(),
        ],
    };
    let mut cfg = config(&base, tmp.path());
    cfg.max_iterations = 1;
    let mut campaign = Campaign::new(goal, cfg, "goal-e2e-hea-constraints".into());

    let result = campaign
        .run()
        .await
        .expect("campaign should find a valid HEA");
    let winner = result.state.best().expect("valid HEA should remain");

    assert_eq!(winner.composition, "W0.2 Mo0.2 Ta0.2 Nb0.2 V0.2");
    assert!(result.summary.contains("REJECTED"), "{}", result.summary);
    assert!(
        result.summary.contains("delta_S_mix_J_per_molK=1.6300"),
        "{}",
        result.summary
    );
}

/// Happy path: a submitted goal really executes its steps, persists every
/// progress transition to the store, and stores a real terminal result.
#[tokio::test]
async fn goal_executes_steps_persists_trail_and_result() {
    let _env_lock = ENV_LOCK.lock().await;
    let test_home = tempfile::tempdir().unwrap();
    let _home = install_test_identity(test_home.path());
    let (base, boundary) = spawn_boundary(false, 0.0, false).await;
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("provenance.db");
    let store = ProvenanceStore::open(&db).await.unwrap();

    let id = "goal-e2e-happy";
    let mut campaign =
        Campaign::new(test_goal(), config(&base, tmp.path()), id.into()).with_provenance(store);
    let result = campaign.run().await.expect("goal must run to completion");
    drop(campaign);

    // The steps really ran at the boundary: one LLM proposal per iteration,
    // batch_size authenticated evaluations per iteration.
    assert_eq!(boundary.llm_calls.load(Ordering::SeqCst), 2);
    assert_eq!(boundary.session_calls.load(Ordering::SeqCst), 4);
    assert_eq!(boundary.eval_calls.load(Ordering::SeqCst), 4);
    assert!(
        boundary
            .auth_headers
            .lock()
            .unwrap()
            .iter()
            .all(|header| header.as_deref() == Some("Bearer goal-e2e-node-session"))
    );

    // Real terminal result.
    assert_eq!(result.state.status, GoalStatus::Completed);
    assert_eq!(result.state.completion_reason, "iteration_limit");
    assert_eq!(result.state.total_evaluated(), 4);
    assert!(!result.winners.is_empty());
    assert!(
        result
            .state
            .candidates
            .iter()
            .all(|candidate| candidate.properties["Tm_estimate_K"].is_number())
    );
    assert!(result.summary.contains("Best:"));
    assert!(result.summary.contains("Tm_estimate_K="));
    assert_eq!(result.evidence_class, EvidenceClass::Indeterminate);
    assert!(
        result
            .state
            .candidates
            .iter()
            .all(|candidate| candidate.evidence_class == EvidenceClass::Indeterminate)
    );
    assert!(result.summary.contains("[RED indeterminate]"));
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
    assert_eq!(cp["evidence_class"], "indeterminate");
    assert!(
        cp["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .all(|candidate| candidate["evidence_class"] == "indeterminate")
    );
}

/// Honesty: when the evaluator is down, every step fails — the goal must
/// end Failed with the error persisted, never "completed".
#[tokio::test]
async fn goal_must_not_complete_when_steps_cannot_run() {
    let _env_lock = ENV_LOCK.lock().await;
    let test_home = tempfile::tempdir().unwrap();
    let _home = install_test_identity(test_home.path());
    let (base, boundary) = spawn_boundary(true, 0.0, false).await;
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
    let _env_lock = ENV_LOCK.lock().await;
    let test_home = tempfile::tempdir().unwrap();
    let _home = install_test_identity(test_home.path());
    let (base, boundary) = spawn_boundary(false, 0.0, false).await;
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

/// DEFECT (budget honesty): every iteration past seed exhaustion makes a real
/// completion call through `prism_llm::LlmClient::chat`, whose signature is
/// `-> Result<String>` — no usage, no cost. On a billed backend (the campaign
/// honours `LLM_BASE_URL` / `LLM_API_KEY` / `MARC27_TOKEN`) that is money the
/// `budget_usd` ceiling structurally cannot see, while the evaluator's own
/// `cost_usd` accrues normally. Reporting only the half it can see would make
/// "$0.20 spent of $25.00 ceiling" read as headroom the goal does not have.
///
/// There is no price table this crate could apply — `prism-agent` depends on
/// `prism-campaign`, not the reverse, and the campaign points at whatever
/// `LLM_BASE_URL` names — so the ceiling declares the gap instead of
/// inventing a number, and the iteration cap is the limit that actually
/// stops the loop.
#[tokio::test]
async fn ceiling_declares_the_proposal_calls_it_cannot_price() {
    let _env_lock = ENV_LOCK.lock().await;
    let test_home = tempfile::tempdir().unwrap();
    let _home = install_test_identity(test_home.path());
    let (base, boundary) = spawn_boundary(false, 0.05, false).await;
    let tmp = tempfile::tempdir().unwrap();
    let store = ProvenanceStore::open(&tmp.path().join("provenance.db"))
        .await
        .unwrap();

    let id = "goal-e2e-budget-honesty";
    let mut cfg = config(&base, tmp.path());
    // A ceiling far above anything this run reports, so nothing stops early
    // and the ONLY question is what the ceiling says about what it saw.
    cfg.budget_usd = Some(25.0);
    let mut campaign = Campaign::new(test_goal(), cfg, id.into()).with_provenance(store);
    let result = campaign.run().await.expect("goal runs to completion");
    drop(campaign);

    // The proposal calls really happened at the boundary: 2 iterations.
    assert_eq!(boundary.llm_calls.load(Ordering::SeqCst), 2);
    assert_eq!(result.state.uncosted_llm_calls, 2);

    // The evaluator's spend accrued; the proposal spend could not.
    assert_eq!(result.state.total_cost_usd, 0.2); // 4 evaluations × $0.05

    // The ceiling must not present that as the whole bill.
    let budget = result.state.budget_status();
    assert_eq!(
        budget,
        prism_campaign::BudgetStatus::PartiallyMeasured {
            spent: 0.2,
            ceiling: 25.0,
            uncosted_llm_calls: 2
        }
    );
    let shown = budget.to_string();
    assert!(shown.contains("2 LLM proposal calls"), "{shown}");
    assert!(shown.contains("NOT in that figure"), "{shown}");
    assert!(shown.contains("iteration cap"), "{shown}");
    // The summary the user actually reads carries the same sentence.
    assert!(
        result.summary.contains("NOT in that figure"),
        "{}",
        result.summary
    );

    // And the cap that DOES stop the loop is the iteration cap, not the USD
    // ceiling — which is what the message tells the user to rely on.
    assert_eq!(result.state.completion_reason, "iteration_limit");

    // The count survives a checkpoint round-trip, so a resumed goal does not
    // silently forget the calls it already made.
    let cp = tmp.path().join(format!("{id}.json"));
    let resumed = Campaign::from_checkpoint(&cp).unwrap();
    assert_eq!(resumed.state().uncosted_llm_calls, 2);
}
