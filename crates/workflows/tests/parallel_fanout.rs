// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::post;
use prism_workflows::{
    ParallelExecutionPolicy, WorkflowExecutionOptions, WorkflowSpec, WorkflowStep,
    execute_workflow, execute_workflow_with_parallel_policy,
};
use serde_json::{Value, json};
use tempfile::tempdir;
use tokio::sync::Notify;

#[derive(Clone, Default)]
struct FanoutProbe {
    in_flight: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    completed: Arc<Mutex<Vec<String>>>,
    entered: Arc<Notify>,
    finished: Arc<Notify>,
    release_gates: Arc<HashMap<String, Arc<Notify>>>,
    delays_ms: Arc<HashMap<String, u64>>,
    failing: Arc<HashSet<String>>,
}

impl FanoutProbe {
    fn with_plan(delays_ms: HashMap<String, u64>, failing: HashSet<String>) -> Self {
        let release_gates = delays_ms
            .keys()
            .chain(failing.iter())
            .map(|id| (id.clone(), Arc::new(Notify::new())))
            .collect();
        Self {
            release_gates: Arc::new(release_gates),
            delays_ms: Arc::new(delays_ms),
            failing: Arc::new(failing),
            ..Self::default()
        }
    }

    async fn wait_for_in_flight(&self, target: usize) {
        loop {
            let entered = self.entered.notified();
            if self.in_flight.load(Ordering::SeqCst) >= target {
                return;
            }
            entered.await;
        }
    }

    async fn wait_for_finished(&self, target: usize) {
        loop {
            let finished = self.finished.notified();
            if self.completed.lock().unwrap().len() >= target {
                return;
            }
            finished.await;
        }
    }

    fn release(&self, name: &str) {
        self.release_gates.get(name).unwrap().notify_one();
    }

    fn release_all<'a>(&self, names: impl IntoIterator<Item = &'a str>) {
        for name in names {
            self.release(name);
        }
    }
}

async fn run_probe_tool(
    State(probe): State<FanoutProbe>,
    Path(name): Path<String>,
    Json(_body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let in_flight = probe.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
    probe.peak.fetch_max(in_flight, Ordering::SeqCst);
    probe.entered.notify_one();

    if let Some(gate) = probe.release_gates.get(&name) {
        gate.notified().await;
    }

    let delay_ms = probe.delays_ms.get(&name).copied().unwrap_or(25);
    tokio::time::sleep(Duration::from_millis(delay_ms)).await;

    probe.completed.lock().unwrap().push(name.clone());
    probe.in_flight.fetch_sub(1, Ordering::SeqCst);
    probe.finished.notify_one();

    if probe.failing.contains(&name) {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "error": format!("could not parse {name}") })),
        )
    } else {
        (
            StatusCode::OK,
            Json(json!({ "tool": name, "result": { "ok": true } })),
        )
    }
}

async fn spawn_probe(probe: FanoutProbe) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let router = Router::new()
        .route("/api/tools/{name}/run", post(run_probe_tool))
        .with_state(probe);
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    port
}

fn parallel_tool_workflow(ids: &[&str]) -> WorkflowSpec {
    let sub_steps = ids
        .iter()
        .map(|id| WorkflowStep {
            id: (*id).to_string(),
            action: "tool".to_string(),
            config: BTreeMap::from([("name".to_string(), json!(id))]),
        })
        .collect::<Vec<_>>();
    WorkflowSpec {
        name: "parallel-probe".to_string(),
        description: "parallel fan-out probe".to_string(),
        command_name: "parallel-probe".to_string(),
        source_path: "inline:parallel-probe".to_string(),
        default_mode: "execute".to_string(),
        arguments: Vec::new(),
        steps: vec![WorkflowStep {
            id: "fanout".to_string(),
            action: "parallel".to_string(),
            config: BTreeMap::from([("steps".to_string(), json!(sub_steps))]),
        }],
        raw: Value::Null,
    }
}

fn parallel_policy(max_in_flight: usize) -> ParallelExecutionPolicy {
    ParallelExecutionPolicy {
        max_in_flight: NonZeroUsize::new(max_in_flight).unwrap(),
    }
}

fn node_values(port: u16) -> BTreeMap<String, String> {
    BTreeMap::from([("node_port".to_string(), port.to_string())])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_step_never_exceeds_the_caller_limit() {
    const LIMIT: usize = 3;
    let ids = (0..12).map(|n| format!("pdf-{n}")).collect::<Vec<_>>();
    let delays = ids
        .iter()
        .map(|id| (id.clone(), 75))
        .collect::<HashMap<_, _>>();
    let probe = FanoutProbe::with_plan(delays, HashSet::new());
    let port = spawn_probe(probe.clone()).await;
    let id_refs = ids.iter().map(String::as_str).collect::<Vec<_>>();
    let spec = parallel_tool_workflow(&id_refs);
    let values = node_values(port);
    let options = WorkflowExecutionOptions::default();
    let policy = parallel_policy(LIMIT);

    let run = tokio::spawn(async move {
        execute_workflow_with_parallel_policy(
            &spec, &values, true, None, None, None, &options, &policy,
        )
        .await
    });

    tokio::time::timeout(Duration::from_secs(2), probe.wait_for_in_flight(LIMIT))
        .await
        .expect("the configured number of branches should overlap");
    assert!(
        tokio::time::timeout(
            Duration::from_millis(500),
            probe.wait_for_in_flight(LIMIT + 1)
        )
        .await
        .is_err(),
        "a branch beyond the configured limit started"
    );
    assert_eq!(probe.peak.load(Ordering::SeqCst), LIMIT);

    probe.release_all(id_refs.iter().copied());
    let result = run
        .await
        .expect("workflow task should not panic")
        .expect("bounded fan-out should complete");

    assert_eq!(result.steps[0].status, "completed");
    assert_eq!(probe.completed.lock().unwrap().len(), ids.len());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_step_uses_the_declared_laptop_default() {
    let limit = ParallelExecutionPolicy::default().max_in_flight.get();
    assert_eq!(
        limit, 8,
        "changing the environment claim must be deliberate"
    );

    let ids = (0..=limit)
        .map(|n| format!("default-pdf-{n}"))
        .collect::<Vec<_>>();
    let delays = ids
        .iter()
        .map(|id| (id.clone(), 10))
        .collect::<HashMap<_, _>>();
    let probe = FanoutProbe::with_plan(delays, HashSet::new());
    let port = spawn_probe(probe.clone()).await;
    let id_refs = ids.iter().map(String::as_str).collect::<Vec<_>>();
    let spec = parallel_tool_workflow(&id_refs);
    let values = node_values(port);

    let run = tokio::spawn(async move { execute_workflow(&spec, &values, true).await });

    tokio::time::timeout(Duration::from_secs(2), probe.wait_for_in_flight(limit))
        .await
        .expect("the default number of branches should overlap");
    assert!(
        tokio::time::timeout(
            Duration::from_millis(500),
            probe.wait_for_in_flight(limit + 1)
        )
        .await
        .is_err(),
        "the default runner started a ninth branch"
    );
    assert_eq!(probe.peak.load(Ordering::SeqCst), limit);

    probe.release_all(id_refs.iter().copied());
    let result = run
        .await
        .expect("workflow task should not panic")
        .expect("default fan-out should complete");
    assert_eq!(result.steps[0].status, "completed");
    assert_eq!(probe.completed.lock().unwrap().len(), ids.len());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_step_keeps_successes_reports_failures_and_orders_by_declaration() {
    let ids = ["slow", "broken", "fast", "middle"];
    let probe = FanoutProbe::with_plan(
        HashMap::from([
            ("slow".to_string(), 100),
            ("broken".to_string(), 60),
            ("fast".to_string(), 5),
            ("middle".to_string(), 25),
        ]),
        HashSet::from(["broken".to_string()]),
    );
    let port = spawn_probe(probe.clone()).await;
    let mut spec = parallel_tool_workflow(&ids);
    spec.steps[0].config.insert("retries".to_string(), json!(2));
    spec.steps[0]
        .config
        .insert("retry_delay_secs".to_string(), json!(0));
    spec.steps.push(WorkflowStep {
        id: "after-fanout".to_string(),
        action: "message".to_string(),
        config: BTreeMap::from([("text".to_string(), json!("partial result surfaced"))]),
    });
    let values = node_values(port);
    let options = WorkflowExecutionOptions::default();
    let policy = parallel_policy(ids.len());

    let run = tokio::spawn(async move {
        execute_workflow_with_parallel_policy(
            &spec, &values, true, None, None, None, &options, &policy,
        )
        .await
    });

    tokio::time::timeout(Duration::from_secs(2), probe.wait_for_in_flight(ids.len()))
        .await
        .expect("all ordering probes should enter");
    for (finished, id) in ["fast", "middle", "broken", "slow"].iter().enumerate() {
        probe.release(id);
        tokio::time::timeout(
            Duration::from_secs(2),
            probe.wait_for_finished(finished + 1),
        )
        .await
        .expect("released probe should finish");
    }

    let result = tokio::time::timeout(Duration::from_secs(2), run)
        .await
        .expect("a partial fan-out must not enter the outer retry loop")
        .expect("workflow task should not panic")
        .expect("one bad document is a partial result, not a whole-run failure");

    let completion_order = probe.completed.lock().unwrap().clone();
    assert_eq!(
        completion_order,
        ["fast", "middle", "broken", "slow"],
        "test setup must force a non-declaration completion order"
    );

    let fanout = &result.steps[0];
    assert_eq!(fanout.status, "partial");
    let outcomes = fanout.data["sub_steps"].as_array().unwrap();
    assert_eq!(outcomes.len(), ids.len());
    assert_eq!(
        outcomes
            .iter()
            .map(|outcome| outcome["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ids
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| outcome["status"] == "completed")
            .count(),
        ids.len() - 1
    );

    let failed = outcomes
        .iter()
        .find(|outcome| outcome["id"] == "broken")
        .unwrap();
    assert_eq!(failed["status"], "failed");
    let reason = failed["data"]["error"].as_str().unwrap();
    assert!(reason.contains("HTTP 422"), "failure reason: {reason}");
    assert!(failed["summary"].as_str().unwrap().contains("broken"));

    let failures = fanout.data["failures"].as_array().unwrap();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0]["id"], "broken");
    assert!(failures[0]["reason"].as_str().unwrap().contains("HTTP 422"));
    assert!(fanout.summary.contains("broken"));
    assert!(fanout.summary.contains("HTTP 422"));
    assert_eq!(result.context["fanout"]["completed"], ids.len() - 1);
    assert_eq!(result.context["fanout"]["failed"], 1);
    assert_eq!(result.context["fanout"]["steps"], json!(ids));
    assert_eq!(result.steps[1].id, "after-fanout");
    assert_eq!(result.steps[1].status, "completed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_step_retries_and_errors_when_every_branch_fails() {
    let ids = ["broken-a", "broken-b"];
    let probe = FanoutProbe::with_plan(
        HashMap::from([("broken-a".to_string(), 5), ("broken-b".to_string(), 5)]),
        ids.into_iter().map(str::to_string).collect(),
    );
    let port = spawn_probe(probe.clone()).await;
    let mut spec = parallel_tool_workflow(&ids);
    spec.steps[0].config.insert("retries".to_string(), json!(1));
    spec.steps[0]
        .config
        .insert("retry_delay_secs".to_string(), json!(0));
    let values = node_values(port);
    let options = WorkflowExecutionOptions::default();
    let policy = parallel_policy(ids.len());

    let run = tokio::spawn(async move {
        execute_workflow_with_parallel_policy(
            &spec, &values, true, None, None, None, &options, &policy,
        )
        .await
    });

    for attempt in 1..=2 {
        tokio::time::timeout(Duration::from_secs(2), probe.wait_for_in_flight(ids.len()))
            .await
            .unwrap_or_else(|_| panic!("all branches should enter attempt {attempt}"));
        probe.release_all(ids);
        tokio::time::timeout(
            Duration::from_secs(2),
            probe.wait_for_finished(attempt * ids.len()),
        )
        .await
        .unwrap_or_else(|_| panic!("all branches should finish attempt {attempt}"));
    }

    let error = run
        .await
        .expect("workflow task should not panic")
        .expect_err("an all-failed parallel step must fail the workflow");
    let error = format!("{error:#}");

    assert!(
        error.contains("parallel step 'fanout' failed"),
        "error: {error}"
    );
    assert!(
        error.contains("0 of 2 branches completed; 2 failed"),
        "error: {error}"
    );
    for id in ids {
        assert!(error.contains(id), "error omitted {id}: {error}");
        assert_eq!(
            probe
                .completed
                .lock()
                .unwrap()
                .iter()
                .filter(|completed| completed.as_str() == id)
                .count(),
            2,
            "{id} should run once initially and once on retry"
        );
    }
    assert!(error.contains("HTTP 422"), "error: {error}");
}

#[tokio::test]
async fn parallel_step_preserves_the_all_success_result_contract() {
    let sub_steps = vec![
        WorkflowStep {
            id: "a".to_string(),
            action: "message".to_string(),
            config: BTreeMap::from([("text".to_string(), json!("first"))]),
        },
        WorkflowStep {
            id: "b".to_string(),
            action: "message".to_string(),
            config: BTreeMap::from([("text".to_string(), json!("second"))]),
        },
    ];
    let mut spec = parallel_tool_workflow(&[]);
    spec.steps[0]
        .config
        .insert("steps".to_string(), json!(sub_steps));

    let result = execute_workflow(&spec, &BTreeMap::new(), true)
        .await
        .expect("all-success parallel workflow should complete");
    let fanout = &result.steps[0];

    assert_eq!(fanout.status, "completed");
    assert_eq!(fanout.summary, "parallel 2 steps completed");
    assert_eq!(
        fanout.data,
        json!({
            "sub_steps": [
                {
                    "id": "a",
                    "action": "message",
                    "status": "completed",
                    "summary": "first",
                    "data": { "message": "first" },
                },
                {
                    "id": "b",
                    "action": "message",
                    "status": "completed",
                    "summary": "second",
                    "data": { "message": "second" },
                },
            ],
        })
    );
    assert_eq!(
        result.context["fanout"],
        json!({ "completed": 2, "steps": ["a", "b"] })
    );
}

#[tokio::test]
async fn parallel_policy_above_tokios_limit_is_bounded_by_the_workload() {
    let sub_step = WorkflowStep {
        id: "one".to_string(),
        action: "message".to_string(),
        config: BTreeMap::from([("text".to_string(), json!("one branch"))]),
    };
    let mut spec = parallel_tool_workflow(&[]);
    spec.steps[0]
        .config
        .insert("steps".to_string(), json!([sub_step]));
    let policy = parallel_policy(usize::MAX);

    let result = execute_workflow_with_parallel_policy(
        &spec,
        &BTreeMap::new(),
        true,
        None,
        None,
        None,
        &WorkflowExecutionOptions::default(),
        &policy,
    )
    .await
    .expect("an oversized environment claim must not panic");

    assert_eq!(result.steps[0].status, "completed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nested_workflow_surfaces_a_child_parallel_partial_result() {
    const CHILD_NAME: &str = "prism-fanout-child-6f790ef90bcb";
    const CHILD_LIMIT: usize = 2;
    let ids = [
        "nested-slow",
        "nested-broken",
        "nested-fast",
        "nested-middle",
    ];
    let probe = FanoutProbe::with_plan(
        HashMap::from([
            ("nested-slow".to_string(), 100),
            ("nested-broken".to_string(), 60),
            ("nested-fast".to_string(), 5),
            ("nested-middle".to_string(), 25),
        ]),
        HashSet::from(["nested-broken".to_string()]),
    );
    let port = spawn_probe(probe.clone()).await;
    let project = tempdir().unwrap();
    let workflow_dir = project.path().join(".prism/workflows");
    fs::create_dir_all(&workflow_dir).unwrap();
    fs::write(
        workflow_dir.join("partial-child.yaml"),
        format!(
            r#"
name: {CHILD_NAME}
steps:
  - id: child-fanout
    action: parallel
    steps:
      - id: nested-slow
        action: tool
        name: nested-slow
      - id: nested-broken
        action: tool
        name: nested-broken
      - id: nested-fast
        action: tool
        name: nested-fast
      - id: nested-middle
        action: tool
        name: nested-middle
"#
        ),
    )
    .unwrap();

    let parent = WorkflowSpec {
        name: "partial-parent".to_string(),
        description: "nested partial probe".to_string(),
        command_name: "partial-parent".to_string(),
        source_path: "inline:partial-parent".to_string(),
        default_mode: "execute".to_string(),
        arguments: Vec::new(),
        steps: vec![WorkflowStep {
            id: "child".to_string(),
            action: "workflow".to_string(),
            config: BTreeMap::from([
                ("name".to_string(), json!(CHILD_NAME)),
                (
                    "inputs".to_string(),
                    json!({ "node_port": "{{node_port}}" }),
                ),
            ]),
        }],
        raw: Value::Null,
    };
    let values = BTreeMap::from([
        (
            "project_root".to_string(),
            project.path().display().to_string(),
        ),
        ("node_port".to_string(), port.to_string()),
    ]);
    let options = WorkflowExecutionOptions::default();
    let policy = parallel_policy(CHILD_LIMIT);

    let run = tokio::spawn(async move {
        execute_workflow_with_parallel_policy(
            &parent, &values, true, None, None, None, &options, &policy,
        )
        .await
    });

    tokio::time::timeout(
        Duration::from_secs(2),
        probe.wait_for_in_flight(CHILD_LIMIT),
    )
    .await
    .expect("the child should inherit the caller's concurrency limit");
    assert!(
        tokio::time::timeout(
            Duration::from_millis(500),
            probe.wait_for_in_flight(CHILD_LIMIT + 1)
        )
        .await
        .is_err(),
        "the child workflow exceeded its inherited limit"
    );
    probe.release_all(ids);

    let result = run
        .await
        .expect("workflow task should not panic")
        .expect("the child partial result should be returned");

    assert_eq!(probe.peak.load(Ordering::SeqCst), CHILD_LIMIT);
    assert_eq!(result.steps[0].status, "partial");
    assert_eq!(result.steps[0].data["steps"][0]["status"], "partial");
    assert_eq!(
        result.steps[0].data["steps"][0]["data"]["sub_steps"][0]["id"],
        "nested-slow"
    );
    assert_eq!(
        result.steps[0].data["steps"][0]["data"]["sub_steps"][1]["id"],
        "nested-broken"
    );
    assert_eq!(
        result.steps[0].data["steps"][0]["data"]["sub_steps"][1]["status"],
        "failed"
    );
}
