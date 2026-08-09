// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! The pass-through path costs NOTHING — measured, not argued.
//!
//! Runs each session below twice against a recording stub LLM: once with the
//! pre-flight reprompter enabled (the default) and once with `PRISM_REPROMPT=0`,
//! then asserts the two runs put **byte-identical** traffic on the wire. Same
//! request count, same request bodies, therefore the same prompt tokens and the
//! same number of round-trips. Any prompt injected on the pass-through path, and
//! any extra classifier call, breaks this immediately.
//!
//! It used to exercise ONE fixed materials-science string, which is how a
//! defect that taxed terse SOFTWARE requests ("fix my code", "make it faster")
//! survived review: PRISM is a coding-capable agent, those escalate to
//! `Intent::Other`, and `Other` proceeds silently — so the user never saw a
//! question, they only paid for the round-trip. The matrix now covers terse
//! materials, terse software and anaphora, plus a PAID control that proves the
//! feature is still armed rather than switched off.
//!
//! This test owns its own binary on purpose: it mutates a process-global env
//! var, which is only sound when nothing else in the process is running.
//! Do not add a second `#[test]` to this file.
//!
//! Requires `python3` on PATH; skips (with a note) when absent.

/// Pre-main store isolation — every integration binary must declare this.
mod common;

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

/// Sessions that must be free. Each is a list of user turns run in ONE session,
/// so the anaphora case has real prior context behind it.
const FREE_SESSIONS: &[(&str, &[&str])] = &[
    ("terse materials", &[EXPERT_QUERY]),
    (
        "terse materials, no verb",
        &["yield strength of Ti-6Al-4V at 400 C"],
    ),
    ("terse software — fix", &["fix my code"]),
    ("terse software — improve", &["improve my code"]),
    ("terse software — performance", &["make it faster"]),
    (
        "terse software — perf, possessive",
        &["make my code faster"],
    ),
    (
        "anaphora after a real turn",
        &[EXPERT_QUERY, "now make it stronger"],
    ),
];

/// The controls. These are SUPPOSED to cost a classifier call: a non-expert
/// naming no material and no direction is exactly what the feature exists for.
/// Without them, "make everything free" would pass this file.
///
/// The second one is the blast-radius guard. Narrowing the software case by
/// deleting `faster` from `BARE_COMPARATIVES` also stopped "make my process
/// faster" escalating — a real regression, invisible to `FREE_SESSIONS` because
/// a wrongly-free session and a rightly-free session look identical there.
const PAID_SESSIONS: &[(&str, &[&str])] = &[
    ("materials vagueness", &["Make my alloy better"]),
    ("process vagueness — speed", &["make my process faster"]),
];

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

/// True when this request carries the pre-flight classifier's system prompt —
/// i.e. the user paid for the reprompter on this turn.
fn is_classifier_request(body: &serde_json::Value) -> bool {
    body["messages"].as_array().is_some_and(|msgs| {
        msgs.iter().any(|m| {
            m["content"]
                .as_str()
                .is_some_and(|c| c.starts_with("Classify the user's request"))
        })
    })
}

/// Run `messages` as consecutive turns of ONE session; return the wire traffic
/// they generated, the final answer, and how long they took.
async fn session_traffic(
    project: &Path,
    python: &Path,
    messages: &[&str],
) -> (Vec<serde_json::Value>, String, std::time::Duration) {
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
    for message in messages {
        agent_loop::run_turn(
            &llm,
            &mut seed.tool_server,
            &seed.command_tool_runtime,
            &mut history,
            seed.tools.as_ref(),
            seed.config.as_ref(),
            message,
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
    }
    let elapsed = started.elapsed();
    let requests = log.lock().expect("request log").clone();
    (requests, answer, elapsed)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_pass_through_path_is_byte_identical_with_and_without_the_reprompter() {
    let Some(python) = find_python() else {
        eprintln!("SKIP: python3 not on PATH");
        return;
    };
    let project = tempfile::tempdir().expect("tempdir");
    write_stub_project(project.path());
    let chars = |reqs: &[serde_json::Value]| -> usize {
        reqs.iter().map(|r| r["messages"].to_string().len()).sum()
    };

    // Warm-up: the first turn in the process pays for lazy statics, the model
    // registry and the tool catalog. Measuring it would report start-up cost as
    // if it were the reprompter's.
    // SAFETY: this file contains exactly one test, so nothing else in the
    // process can observe the env mid-change.
    unsafe { std::env::remove_var("PRISM_REPROMPT") };
    let _ = session_traffic(project.path(), &python, &[EXPERT_QUERY]).await;

    // Reprompter ON (the shipped default), every case.
    let mut on = Vec::new();
    for (label, messages) in FREE_SESSIONS {
        on.push(session_traffic(project.path(), &python, messages).await);
        let (_, answer, _) = on.last().expect("just pushed");
        assert_eq!(
            answer, "AGENT_ANSWER",
            "[{label}] must reach the model, not be interrogated"
        );
    }
    let mut paid_on = Vec::new();
    for (_, messages) in PAID_SESSIONS {
        paid_on.push(session_traffic(project.path(), &python, messages).await);
    }

    // Reprompter OFF — the pre-feature baseline.
    unsafe { std::env::set_var("PRISM_REPROMPT", "0") };
    let mut off = Vec::new();
    for (_, messages) in FREE_SESSIONS {
        off.push(session_traffic(project.path(), &python, messages).await);
    }
    let mut paid_off = Vec::new();
    for (_, messages) in PAID_SESSIONS {
        paid_off.push(session_traffic(project.path(), &python, messages).await);
    }
    unsafe { std::env::remove_var("PRISM_REPROMPT") };

    for (i, (label, _)) in FREE_SESSIONS.iter().enumerate() {
        let (with_reprompter, _, t_with) = &on[i];
        let (without_reprompter, _, t_without) = &off[i];
        assert!(
            !with_reprompter.iter().any(is_classifier_request),
            "[{label}] the classifier prompt reached the wire"
        );
        assert_eq!(
            with_reprompter.len(),
            without_reprompter.len(),
            "[{label}] the reprompter added a round-trip"
        );
        assert_eq!(
            with_reprompter, without_reprompter,
            "[{label}] the reprompter changed what the turn sends to the model — \
             any diff here is added prompt tokens the user did not ask to pay for"
        );
        // Token delta, computed from the traffic rather than asserted in prose.
        assert_eq!(
            chars(with_reprompter),
            chars(without_reprompter),
            "[{label}] prompt-character delta must be exactly zero"
        );
        eprintln!(
            "[{label}] {} request(s) both ways, {} prompt chars both ways; \
             wall clock {t_with:?} (on) vs {t_without:?} (off)",
            with_reprompter.len(),
            chars(with_reprompter),
        );
    }

    // The controls: these MUST cost, or the parity above was bought by
    // disabling the feature (or by narrowing it too far).
    for (i, (label, _)) in PAID_SESSIONS.iter().enumerate() {
        let (requests_on, answer_on, _) = &paid_on[i];
        let (requests_off, answer_off, _) = &paid_off[i];
        assert!(
            requests_on.iter().any(is_classifier_request),
            "[{label}] the reprompter no longer fires on a case it exists for"
        );
        assert!(
            !requests_off.iter().any(is_classifier_request),
            "[{label}] the kill switch did not switch it off"
        );
        assert_ne!(
            answer_on, answer_off,
            "[{label}] with the reprompter on, the vague request must be answered \
             with a question rather than run as if it were specified"
        );
        assert_eq!(answer_off, "AGENT_ANSWER");
        eprintln!(
            "[{label}] PAID as designed: {} request(s) on vs {} off",
            requests_on.len(),
            requests_off.len(),
        );
    }
}
