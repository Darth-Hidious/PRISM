// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! PRISM calls PRISM: a live agent turn must not hold the provenance store
//! open across a child process that needs it.
//!
//! Measured 2026-08-21 on a live run: the agent searched 113 papers, chose 8,
//! and started ingesting. 113 entities landed (transient in-process writes),
//! then EVERY `papers_ingest` child died on:
//!
//!   Error: failed to open Turso database
//!     Locking error: Failed locking file '~/.prism/provenance.db-wal'.
//!     File is locked by another process
//!
//! because `run_turn` held the run-ledger's `Arc<ProvenanceStore>` (and the
//! heartbeat task held a clone) for the whole turn, and libsql/turso WAL takes
//! an exclusive OS file lock for the lifetime of an open handle — exclusive
//! across PROCESSES, which is exactly what a `CommandExecution::Cli` child is.
//!
//! This test drives the REAL `run_turn` (production run-ledger start,
//! production heartbeat, production command-tool dispatch, production
//! subprocess spawn) and re-invokes THIS TEST BINARY as the child in place of
//! the `prism` executable. The child performs what `prism papers claims
//! --store` performs at the store boundary: it opens the SAME store file from
//! a SECOND PROCESS via `ProvenanceStore::open` and persists one fact into
//! `emmo_edge` / `prov_assertion`. Two handles in one process cannot detect
//! this bug — the file lock only bites across processes.
//!
//! RED under the regression this pins: hold a `ProvenanceStore` in
//! `run_turn` across tool dispatch (what `start_root_agent_run` +
//! `AgentRunHeartbeat` did before `RunLedger`) and the child's open fails,
//! no fact lands, and this test fails on every assertion below.

/// Pre-main store isolation — every integration binary must declare this.
mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use prism_agent::agent_loop;
use prism_agent::protocol::build_agent_seed;
use prism_agent::types::AgentEvent;
use prism_ingest::LlmConfig;
use prism_ingest::llm::LlmClient;
use prism_python_bridge::ToolServer;

const TEST_MODEL: &str = "claude-haiku-4-5";
const SESSION_ID: &str = "store-lock-release-turn";

/// The fact the child process persists — what a real `papers claims --store`
/// child persists for an extracted claim, minus the LLM that produced it.
const CHILD_SUBJECT: &str = "LockSeamAlloy";
const CHILD_PREDICATE: &str = "hasProperty";
const CHILD_OBJECT: &str = "LockSeamYieldStrength";

// ── Child mode ────────────────────────────────────────────────────────
//
// `execute_cli_command` spawns `<current_exe> --project-root <dir> --python
// <bin> papers claims --pmc … --store`. The parent test points `current_exe`
// at THIS binary, so that invocation lands here. libtest never receives a
// `--project-root` flag, so the marker cannot misfire on a normal test run.

/// Runs before `main`, before libtest can choke on the CLI-shaped arguments.
// SAFETY (ctor): pre-main, single-threaded; reads argv/env and either returns
// (normal test run) or does the child's store write and exits.
#[ctor::ctor(unsafe)]
fn store_child_mode() {
    let args: Vec<String> = std::env::args().collect();
    if !args.iter().any(|a| a == "--project-root") {
        return;
    }
    let code = match run_store_child(&args) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("store child failed: {error:#}");
            1
        }
    };
    std::process::exit(code);
}

/// What the real `prism papers claims --store` child does at the store
/// boundary: open the store named by `PRISM_PROVENANCE_DB` (inherited from
/// the parent process) and write one fact through the production write path.
fn run_store_child(args: &[String]) -> Result<()> {
    // Bind this stand-in to the production invocation shape: the agent must
    // really have dispatched `papers claims … --store`. If the papers_ingest
    // arg builder changes shape, fail loudly instead of testing nothing.
    anyhow::ensure!(
        args.iter().any(|a| a == "claims") && args.iter().any(|a| a == "--store"),
        "expected the agent's `papers claims … --store` invocation, got: {args:?}"
    );
    let db_path = std::env::var_os("PRISM_PROVENANCE_DB")
        .map(PathBuf::from)
        .context("the child must inherit PRISM_PROVENANCE_DB from the parent test process")?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build child runtime")?;
    runtime.block_on(async {
        // The REAL open path — including its file-lock behavior. While the
        // parent holds the store, this is the call that dies.
        let store = prism_provenance::ProvenanceStore::open(&db_path)
            .await
            .context("child open of the shared provenance store")?;
        let now = chrono::Utc::now().to_rfc3339();
        let prov = prism_provenance::LocalProvenance {
            activity_id: format!("store-lock-release-{}", std::process::id()),
            agent_id: "store-lock-release-child".to_string(),
            agent_kind: "SoftwareAgent".to_string(),
            source_entity_id: "test://store-lock-release/paper".to_string(),
            source_kind: "Document".to_string(),
            tenant: prism_provenance::LOCAL_TENANT.to_string(),
            started_at: now.clone(),
            ended_at: now,
            locality: "local".to_string(),
            origin_source_id: None,
        };
        let fact = prism_provenance::MaterialFact {
            subject: CHILD_SUBJECT.to_string(),
            predicate: CHILD_PREDICATE.to_string(),
            object: CHILD_OBJECT.to_string(),
            value: Some(1035.0),
            unit: None,
            conditions: Vec::new(),
            confidence: Some(0.9),
            kind: None,
            evidence_class: Default::default(),
            verification: None,
            verification_reason: None,
        };
        store
            .write_fact(&fact, &prov)
            .await
            .context("child write into emmo_edge / prov_assertion")
    })?;
    // A JSON line on stdout, like the real CLI: the envelope the agent builds
    // around this child reports `success` from the exit status.
    println!("{{\"stored\": 1}}");
    Ok(())
}

// ── Parent-side harness ───────────────────────────────────────────────
// Deliberately mirrors tests/support/agent_run_harness.rs, which pins the
// ledger contract with a no-tool-call stub. This test needs a STATEFUL stub
// (first reply calls papers_ingest, second reply finishes) and a swapped
// child executable, which that harness does not parameterize.

const STUB_TOOL_SERVER_PY: &str = r#"
import json, sys
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    if req.get("method") == "list_tools":
        resp = {"tools": []}
    else:
        resp = {"error": "unknown method"}
    sys.stdout.write(json.dumps(resp) + "\n")
    sys.stdout.flush()
"#;

fn require_python() -> Result<PathBuf> {
    let output = std::process::Command::new("python3")
        .arg("--version")
        .output()
        .context("python3 is required for the real agent-turn harness")?;
    anyhow::ensure!(output.status.success(), "python3 --version failed");
    Ok(PathBuf::from("python3"))
}

fn write_stub_project(dir: &Path) -> Result<()> {
    let app = dir.join("app");
    std::fs::create_dir_all(&app)?;
    std::fs::write(app.join("__init__.py"), "")?;
    std::fs::write(app.join("tool_server.py"), STUB_TOOL_SERVER_PY)?;
    Ok(())
}

/// Stub LLM: the FIRST completion calls `papers_ingest` (the tool whose
/// child died on the held handle), every later completion ends the turn.
async fn start_stub_llm() -> Result<String> {
    use axum::routing::post;
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let calls = calls.clone();
            async move {
                let call = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let chunk = if call == 0 {
                    serde_json::json!({
                        "choices": [{ "delta": { "tool_calls": [{
                            "index": 0,
                            "id": "call_papers_ingest_1",
                            "type": "function",
                            "function": {
                                "name": "papers_ingest",
                                "arguments": "{\"pmc\": \"PMC424242\"}"
                            }
                        }] } }],
                        "usage": { "prompt_tokens": 100, "completion_tokens": 20, "total_tokens": 120 }
                    })
                } else {
                    serde_json::json!({
                        "choices": [{ "delta": { "content": "SEAM_TURN_DONE" } }],
                        "usage": { "prompt_tokens": 100, "completion_tokens": 5, "total_tokens": 105 }
                    })
                };
                axum::response::Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(axum::body::Body::from(format!(
                        "data: {chunk}\n\ndata: [DONE]\n\n"
                    )))
                    .expect("stub SSE response")
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(format!("http://{address}/v1"))
}

/// Remove the isolated store and its WAL sidecars whatever the verdict —
/// this binary owns its pid-named scratch store outright.
struct StoreCleanup(PathBuf);

impl Drop for StoreCleanup {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let mut path = self.0.clone().into_os_string();
            path.push(suffix);
            let _ = std::fs::remove_file(PathBuf::from(path));
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_live_run_ledger_does_not_block_a_prism_child_from_storing_facts() {
    let db_path = prism_agent::hooks::provenance_db_path();
    let _cleanup = StoreCleanup(db_path.clone());

    let python = require_python().expect("python3 available");
    let project = tempfile::tempdir().expect("stub project dir");
    write_stub_project(project.path()).expect("write stub project");
    let base_url = start_stub_llm().await.expect("start stub LLM");
    let llm_config = LlmConfig {
        base_url,
        model: TEST_MODEL.to_string(),
        api_key: None,
        embedding_model: None,
        timeout_secs: 30,
        ..Default::default()
    };
    let mut seed = build_agent_seed(
        &ToolServer {
            python_bin: python,
            project_root: project.path().to_path_buf(),
            env: std::collections::BTreeMap::new(),
        },
        &llm_config,
    )
    .await
    .expect("build agent seed");
    prism_agent::hooks::set_provenance_ctx(SESSION_ID, TEST_MODEL);

    // The seam under test: the production CLI dispatch spawns
    // `current_exe … papers claims --pmc PMC424242 --store` as a CHILD
    // PROCESS. Point it at this test binary, whose pre-main child mode
    // opens the same store from that second process.
    seed.command_tool_runtime.current_exe =
        std::env::current_exe().expect("current test executable");

    // A real (empty) policy engine: the dispatch is fail-closed and refuses
    // every tool call when the engine is absent. Zero loaded policies allow.
    let mut policy = prism_policy::PolicyEngine::new().expect("empty policy engine");

    let llm = LlmClient::new(llm_config);
    let mut history = Vec::new();
    let mut transcript = prism_agent::transcript::TranscriptStore::new(None);
    let mut scratchpad = prism_agent::scratchpad::Scratchpad::new();
    let tool_results: Arc<Mutex<Vec<(String, bool, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = tool_results.clone();
    agent_loop::run_turn(
        &llm,
        &mut seed.tool_server,
        &seed.command_tool_runtime,
        &mut history,
        seed.tools.as_ref(),
        seed.config.as_ref(),
        "Ingest PMC424242 into the knowledge graph.",
        None,
        &mut transcript,
        seed.hooks.as_ref(),
        &seed.permissions,
        None,
        &mut scratchpad,
        &mut move |event| {
            if let AgentEvent::ToolCallResult {
                tool_name,
                is_error,
                content,
                ..
            } = event
            {
                sink.lock()
                    .expect("tool result sink")
                    .push((tool_name, is_error, content));
            }
        },
        None,
        Some(&mut policy),
        None,
    )
    .await
    .expect("the turn must complete");

    // 1. The child process succeeded WHILE the run ledger was live: the
    //    dispatched papers_ingest result is a success envelope, not the
    //    "failed to open Turso database … File is locked" death.
    let results = tool_results.lock().expect("tool result sink").clone();
    let (_, is_error, content) = results
        .iter()
        .find(|(tool, _, _)| tool == "papers_ingest")
        .expect("the turn must dispatch papers_ingest")
        .clone();
    assert!(
        !is_error,
        "the papers_ingest child must succeed while the parent's run ledger \
         is live; it reported an error: {content}"
    );

    // 2. The child's fact LANDED: one row in emmo_edge, one in
    //    prov_assertion, written by a second OS process into the same store.
    let store = prism_provenance::ProvenanceStore::open(&db_path)
        .await
        .expect("parent reopen after the child");
    let neighbors = store
        .get_neighbors(CHILD_SUBJECT, None, prism_provenance::LOCAL_TENANT, 10)
        .await
        .expect("query emmo_edge for the child's fact");
    assert!(
        !neighbors.edges.is_empty(),
        "the child's fact must land in emmo_edge"
    );
    let assertion_id = prism_provenance::conditioned_assertion_id(
        prism_provenance::LOCAL_TENANT,
        CHILD_SUBJECT,
        CHILD_PREDICATE,
        CHILD_OBJECT,
        Some(1035.0),
        None,
        &[],
    )
    .expect("compute the child's assertion id");
    let assertion = store
        .assertion_by_id(&assertion_id)
        .await
        .expect("query prov_assertion for the child's fact");
    assert!(
        assertion.is_some(),
        "the child's fact must land in prov_assertion"
    );

    // 3. Releasing the handle lost no ledger writes: the run row was
    //    started AND finished around the child.
    let runs = store
        .list_agent_runs(&prism_provenance::AgentRunFilter {
            session_id: Some(SESSION_ID.to_string()),
            ..Default::default()
        })
        .await
        .expect("query the run ledger");
    assert_eq!(runs.len(), 1, "the turn must create exactly one run row");
    assert_eq!(
        runs[0].status,
        prism_provenance::AgentRunStatus::Completed,
        "the run row must be closed after the turn"
    );
}
