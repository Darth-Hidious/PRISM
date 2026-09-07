//! Audit 2026-09-07 (blocker): context-overflow recovery compacted the
//! assembled request — whose first element is the one leading system
//! message — instead of the history it was built from. The retry went out
//! with no system prompt, task block or memory, the user saw only
//! "[context full — compacting and retrying]", and the untouched history
//! overflowed again on the next step.
#[path = "support/agent_run_harness.rs"]
mod agent_run_harness;
mod common;

use std::sync::{Arc, Mutex};

use prism_agent::agent_loop;
use prism_agent::protocol::AgentSeed;
use prism_agent::transcript::{TranscriptEntry, TranscriptStore};
use prism_agent::types::AgentEvent;
use prism_ingest::llm::LlmClient;
use prism_llm::ChatMessage;

/// One row per request the stub received: the first message's role and text.
type Requests = Arc<Mutex<Vec<(String, String)>>>;

/// Rejects the FIRST request the way Anthropic rejects an oversized prompt
/// and answers every later one, so the loop's one-shot recovery is exercised
/// exactly once.
async fn start_overflowing_stub() -> (String, Requests) {
    use axum::routing::post;
    let requests: Requests = Arc::new(Mutex::new(Vec::new()));
    let sink = requests.clone();
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<serde_json::Value>| {
            let sink = sink.clone();
            async move {
                let first = body["messages"]
                    .as_array()
                    .and_then(|m| m.first())
                    .map(|m| {
                        (
                            m["role"].as_str().unwrap_or("").to_string(),
                            m["content"].as_str().unwrap_or("").to_string(),
                        )
                    })
                    .unwrap_or_default();
                let seen = {
                    let mut log = sink.lock().expect("request log");
                    log.push(first);
                    log.len()
                };
                if seen == 1 {
                    return axum::response::Response::builder()
                        .status(400)
                        .header("content-type", "application/json")
                        .body(axum::body::Body::from(
                            r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 216654 tokens > 200000 maximum"}}"#,
                        ))
                        .expect("stub rejection");
                }
                let chunk = serde_json::json!({
                    "choices": [{ "delta": { "content": "RECOVERED" } }],
                    "usage": { "prompt_tokens": 10, "completion_tokens": 1, "total_tokens": 11 }
                });
                axum::response::Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(axum::body::Body::from(format!(
                        "data: {chunk}\n\ndata: [DONE]\n\n"
                    )))
                    .expect("stub answer")
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub llm");
    let addr = listener.local_addr().expect("stub llm addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/v1"), requests)
}

#[tokio::test]
async fn the_retry_after_an_overflow_still_carries_the_system_prompt() {
    let project = agent_run_harness::stub_project().expect("stub project");
    let (base_url, requests) = start_overflowing_stub().await;
    let llm_config = agent_run_harness::llm_config(base_url);
    let AgentSeed {
        mut tool_server,
        subagent_lanes: _,
        command_tool_runtime,
        tools,
        config,
        hooks,
        permissions,
    } = agent_run_harness::stub_seed(&project, &llm_config)
        .await
        .expect("backend seed");
    prism_agent::hooks::set_provenance_ctx("overflow-recovery", agent_run_harness::TEST_MODEL);
    let llm = LlmClient::new(llm_config);

    // Enough prior turns that compaction has something to fold.
    let mut history = Vec::new();
    let mut transcript = TranscriptStore::new(None);
    for i in 0..8 {
        let (role, text) = if i % 2 == 0 {
            ("user", format!("question {i}"))
        } else {
            ("assistant", format!("answer {i}"))
        };
        history.push(ChatMessage {
            role: role.to_string(),
            content: Some(text.clone()),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        });
        transcript.append(TranscriptEntry::new(role, text));
    }
    let mut scratchpad = prism_agent::scratchpad::Scratchpad::new();
    let mut answer = String::new();
    agent_loop::run_turn(
        &llm,
        &mut tool_server,
        &command_tool_runtime,
        &mut history,
        tools.as_ref(),
        config.as_ref(),
        "and one more question",
        None,
        &mut transcript,
        hooks.as_ref(),
        &permissions,
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
        None,
    )
    .await
    .expect("the turn recovers from one overflow");
    assert_eq!(answer, "RECOVERED");

    let requests = requests.lock().expect("request log");
    assert_eq!(requests.len(), 2, "one overflow, one retry: {requests:?}");
    let (first_role, first_content) = &requests[0];
    assert_eq!(first_role, "system");
    let (retry_role, retry_content) = &requests[1];
    assert_eq!(
        retry_role, "system",
        "the retry lost its preamble: its first message is a {retry_role:?} message"
    );
    assert_eq!(
        retry_content, first_content,
        "the retry must carry the same leading system prompt"
    );
    // The history itself was folded, so the NEXT step does not overflow too:
    // summary marker + the last six + the recovered answer.
    assert!(
        history.len() <= 8,
        "history was not compacted: {} messages remain",
        history.len()
    );
}
