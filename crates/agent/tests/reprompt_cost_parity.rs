// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! The expert path costs NOTHING — measured, not argued.
//!
//! Runs the same well-formed expert query twice against a recording stub LLM:
//! once with the pre-flight reprompter enabled (the default) and once with
//! `PRISM_REPROMPT=0`, then asserts the two runs put **byte-identical** traffic
//! on the wire. Same request count, same request bodies, therefore the same
//! prompt tokens and the same number of round-trips. Any prompt injected on the
//! pass-through path, and any extra classifier call, breaks this immediately.
//!
//! This test owns its own binary on purpose: it mutates a process-global env
//! var, which is only sound when nothing else in the process is running.
//! Do not add a second `#[test]` to this file.
//!
//! Requires `python3` on PATH; skips (with a note) when absent.

use std::path::{Path, PathBuf};

use prism_agent::agent_loop;
use prism_agent::protocol::build_agent_seed;
use prism_agent::types::AgentEvent;
use prism_ingest::LlmConfig;
use prism_ingest::llm::LlmClient;
use prism_python_bridge::ToolServer;

const STUB_TOOL_SERVER_PY: &str = r#"
import sys, json
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    resp = {"tools": []} if req.get("method") == "list_tools" else {"error": "unknown method"}
    sys.stdout.write(json.dumps(resp) + "\n")
    sys.stdout.flush()
"#;

/// The query an expert actually types. Named material, named property, named
/// condition — nothing for a reprompter to legitimately ask about.
const EXPERT_QUERY: &str = "What is the yield strength of Inconel 718 at 650 C?";

type RequestLog = std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>;

fn find_python() -> Option<PathBuf> {
    let out = std::process::Command::new("python3")
        .arg("--version")
        .output()
        .ok()?;
    out.status.success().then(|| PathBuf::from("python3"))
}

fn write_stub_project(dir: &Path) {
    let app = dir.join("app");
    std::fs::create_dir_all(&app).expect("create app dir");
    std::fs::write(app.join("__init__.py"), "").expect("write __init__");
    std::fs::write(app.join("tool_server.py"), STUB_TOOL_SERVER_PY).expect("write stub");
}

fn llm_config(base_url: String) -> LlmConfig {
    LlmConfig {
        base_url,
        model: "stub-model".to_string(),
        api_key: None,
        embedding_model: None,
        timeout_secs: 30,
        ..Default::default()
    }
}

async fn start_stub_llm() -> (String, RequestLog) {
    use axum::routing::post;
    let log: RequestLog = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = log.clone();
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<serde_json::Value>| {
            let sink = sink.clone();
            async move {
                let streaming = body["stream"] == serde_json::Value::Bool(true);
                sink.lock().expect("request log").push(body);
                if streaming {
                    let chunk = serde_json::json!({
                        "choices": [{ "delta": { "content": "AGENT_ANSWER" } }]
                    });
                    return axum::response::Response::builder()
                        .header("content-type", "text/event-stream")
                        .body(axum::body::Body::from(format!(
                            "data: {chunk}\n\ndata: [DONE]\n\n"
                        )))
                        .expect("sse");
                }
                let completion = serde_json::json!({
                    "choices": [{
                        "message": { "role": "assistant", "content": "materials_data" },
                        "finish_reason": "stop"
                    }],
                    "usage": { "prompt_tokens": 150, "completion_tokens": 2, "total_tokens": 152 }
                });
                axum::response::Response::builder()
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(completion.to_string()))
                    .expect("json")
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub llm");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/v1"), log)
}

/// One expert turn; returns the wire traffic it generated and how long it took.
async fn expert_turn(
    project: &Path,
    python: &Path,
) -> (Vec<serde_json::Value>, std::time::Duration) {
    let (base_url, log) = start_stub_llm().await;
    let mut seed = build_agent_seed(
        &ToolServer {
            python_bin: python.to_path_buf(),
            project_root: project.to_path_buf(),
            env: std::collections::BTreeMap::new(),
        },
        &llm_config(base_url.clone()),
    )
    .await
    .expect("agent seed");
    let llm = LlmClient::new(llm_config(base_url));
    let mut history = Vec::new();
    let mut transcript = prism_agent::transcript::TranscriptStore::new(None);
    let mut scratchpad = prism_agent::scratchpad::Scratchpad::new();
    let mut answer = String::new();

    let started = std::time::Instant::now();
    agent_loop::run_turn(
        &llm,
        &mut seed.tool_server,
        &seed.command_tool_runtime,
        &mut history,
        seed.tools.as_ref(),
        seed.config.as_ref(),
        EXPERT_QUERY,
        None,
        &mut transcript,
        seed.hooks.as_ref(),
        &seed.permissions,
        None,
        &mut scratchpad,
        &mut |event| {
            if let AgentEvent::TurnComplete {
                text: Some(text), ..
            } = event
                && !text.is_empty()
            {
                answer = text;
            }
        },
        None,
        None,
    )
    .await
    .expect("turn");
    let elapsed = started.elapsed();

    assert_eq!(answer, "AGENT_ANSWER", "expert query must reach the model");
    let requests = log.lock().expect("request log").clone();
    (requests, elapsed)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_expert_path_is_byte_identical_with_and_without_the_reprompter() {
    let Some(python) = find_python() else {
        eprintln!("SKIP: python3 not on PATH");
        return;
    };
    let project = tempfile::tempdir().expect("tempdir");
    write_stub_project(project.path());

    // Warm-up: the first turn in the process pays for lazy statics, the model
    // registry and the tool catalog. Measuring it would report start-up cost as
    // if it were the reprompter's.
    // SAFETY: this file contains exactly one test, so nothing else in the
    // process can observe the env mid-change.
    unsafe { std::env::remove_var("PRISM_REPROMPT") };
    let _ = expert_turn(project.path(), &python).await;

    // Reprompter ON (the shipped default).
    let (with_reprompter, t_with) = expert_turn(project.path(), &python).await;

    // Reprompter OFF — the pre-feature baseline.
    unsafe { std::env::set_var("PRISM_REPROMPT", "0") };
    let (without_reprompter, t_without) = expert_turn(project.path(), &python).await;
    unsafe { std::env::remove_var("PRISM_REPROMPT") };

    assert_eq!(
        with_reprompter.len(),
        without_reprompter.len(),
        "the reprompter added a round-trip to a well-formed expert query"
    );
    assert_eq!(
        with_reprompter, without_reprompter,
        "the reprompter changed what the expert's turn sends to the model — \
         any diff here is added prompt tokens the expert did not ask to pay for"
    );

    // Token delta, computed from the traffic rather than asserted in prose.
    let chars = |reqs: &[serde_json::Value]| -> usize {
        reqs.iter().map(|r| r["messages"].to_string().len()).sum()
    };
    assert_eq!(
        chars(&with_reprompter),
        chars(&without_reprompter),
        "prompt-character delta on the expert path must be exactly zero"
    );

    eprintln!(
        "expert path: {} request(s) both ways, {} prompt chars both ways; \
         wall clock {t_with:?} (on) vs {t_without:?} (off)",
        with_reprompter.len(),
        chars(&with_reprompter),
    );
}
