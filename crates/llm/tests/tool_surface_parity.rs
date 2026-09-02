//! The MARC27 path and the OpenAI path must offer the SAME tool surface.
//!
//! Regression cover for the defect these tests were written against: on the
//! MARC27 platform path `chat_with_tools_streaming` sent no `tools` array at
//! all. It rendered a categorised name summary into a system message instead —
//! 4 names per category, no descriptions, no schemas — so on a 135-tool
//! catalog the model was shown ~40 bare names and could only reach the rest
//! through a `find_tools` hop. The token-budget fix that landed earlier bounds
//! the caller's selection, but a budget on a list nobody sends is inert.
//!
//! These tests drive a real (loopback) HTTP server so they assert on the bytes
//! that actually leave the process, not on an internal helper.

use std::time::Duration;

use prism_llm::{ChatMessage, FunctionDef, LlmClient, LlmConfig, ToolDefinition};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Accept exactly one request, return its JSON body, and reply with `sse_body`.
async fn serve_once(listener: TcpListener, sse_body: String) -> serde_json::Value {
    serve_one(&listener, 200, "text/event-stream", &sse_body).await
}

/// Accept `replies.len()` requests in order, returning each request body.
async fn serve_many(listener: TcpListener, replies: Vec<(u16, String)>) -> Vec<serde_json::Value> {
    let mut bodies = Vec::new();
    for (status, payload) in replies {
        bodies.push(serve_one(&listener, status, "text/event-stream", &payload).await);
    }
    bodies
}

async fn serve_one(
    listener: &TcpListener,
    status: u16,
    content_type: &str,
    payload: &str,
) -> serde_json::Value {
    let (mut sock, _) = listener.accept().await.expect("accept");
    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    // Read headers, then exactly Content-Length bytes of body.
    let (head_end, content_len) = loop {
        let n = sock.read(&mut buf).await.expect("read");
        assert!(n > 0, "client closed before sending a request");
        raw.extend_from_slice(&buf[..n]);
        if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&raw[..pos]).to_ascii_lowercase();
            let len = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            break (pos + 4, len);
        }
    };
    while raw.len() < head_end + content_len {
        let n = sock.read(&mut buf).await.expect("read body");
        assert!(n > 0, "client closed mid-body");
        raw.extend_from_slice(&buf[..n]);
    }
    let body: serde_json::Value =
        serde_json::from_slice(&raw[head_end..head_end + content_len]).expect("request is JSON");

    // `Connection: close` is load-bearing: without it the client may pool the
    // socket and reuse it for the next request, while this server is blocked
    // in `accept()` waiting for a NEW connection — a hang, not a failure.
    let resp = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\nConnection: close\r\n\
         Content-Length: {}\r\n\r\n{}",
        payload.len(),
        payload
    );
    sock.write_all(resp.as_bytes()).await.expect("write");
    sock.flush().await.expect("flush");
    body
}

fn tool(name: &str) -> ToolDefinition {
    ToolDefinition {
        tool_type: "function".to_string(),
        function: FunctionDef {
            name: name.to_string(),
            description: format!("does {name}"),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"q": {"type": "string", "description": "query"}},
                "required": ["q"]
            }),
        },
    }
}

/// A selection wide enough that the old summary would have hidden most of it:
/// 20 `compute_*` tools all landed in one category, of which 4 were shown.
fn wide_selection() -> Vec<ToolDefinition> {
    let mut tools: Vec<ToolDefinition> = (0..20).map(|i| tool(&format!("compute_{i}"))).collect();
    tools.push(tool("compute_gpus"));
    tools.push(tool("find_tools"));
    tools
}

fn config(base_url: String) -> LlmConfig {
    LlmConfig {
        base_url,
        model: "test-model".to_string(),
        context_window: Some(131_072),
        max_output_tokens: Some(4096),
        ..Default::default()
    }
}

fn user(text: &str) -> Vec<ChatMessage> {
    vec![ChatMessage {
        role: "user".to_string(),
        content: Some(text.to_string()),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    }]
}

/// Both backends must receive byte-identical `tools` arrays for one input.
///
/// Divergence between the two request builders is how the MARC27 path went
/// without tool definitions for so long — nothing compared them.
#[tokio::test]
async fn both_paths_send_the_same_tools_array() {
    let tools = wide_selection();

    // MARC27: base_url ending in `/llm` selects the platform `/stream` path.
    let marc_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let marc_port = marc_listener.local_addr().unwrap().port();
    let marc_server = tokio::spawn(serve_once(
        marc_listener,
        "data: {\"delta\":\"ok\",\"done\":true}\n\n".to_string(),
    ));
    let marc_client = LlmClient::new(config(format!("http://127.0.0.1:{marc_port}/llm")));
    let _ = tokio::time::timeout(
        Duration::from_secs(10),
        marc_client.chat_with_tools_streaming(&user("list my gpus"), &tools, |_, _| {}),
    )
    .await
    .expect("marc27 turn timed out");
    let marc_body = marc_server.await.unwrap();

    // OpenAI-compatible.
    let oai_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let oai_port = oai_listener.local_addr().unwrap().port();
    let oai_server = tokio::spawn(serve_once(
        oai_listener,
        "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n".to_string(),
    ));
    let oai_client = LlmClient::new(config(format!("http://127.0.0.1:{oai_port}/v1")));
    let _ = tokio::time::timeout(
        Duration::from_secs(10),
        oai_client.chat_with_tools_streaming(&user("list my gpus"), &tools, |_, _| {}),
    )
    .await
    .expect("openai turn timed out");
    let oai_body = oai_server.await.unwrap();

    let marc_tools = marc_body
        .get("tools")
        .expect("MARC27 request carries no `tools` array — the model is being shown prose");
    let oai_tools = oai_body
        .get("tools")
        .expect("OpenAI request carries no tools");
    assert_eq!(
        marc_tools, oai_tools,
        "the two paths offered different tool surfaces for the same input"
    );

    // Every tool the caller selected is offered, with its schema — not a name.
    let arr = marc_tools.as_array().unwrap();
    assert_eq!(
        arr.len(),
        tools.len(),
        "MARC27 dropped part of the selection"
    );
    let gpus = arr
        .iter()
        .find(|t| t.pointer("/function/name").and_then(|n| n.as_str()) == Some("compute_gpus"))
        .expect("`compute_gpus` was 17th in its category — the old summary hid it behind '… and N more'");
    assert!(
        gpus.pointer("/function/parameters/properties/q").is_some(),
        "tool offered without its parameter schema"
    );
}

/// The MARC27 path must not smuggle a tool inventory back in as prose: the
/// injected system message is guidance only.
#[tokio::test]
async fn marc27_injects_guidance_not_an_inventory() {
    let tools = wide_selection();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(serve_once(
        listener,
        "data: {\"delta\":\"ok\",\"done\":true}\n\n".to_string(),
    ));
    let client = LlmClient::new(config(format!("http://127.0.0.1:{port}/llm")));
    let _ = tokio::time::timeout(
        Duration::from_secs(10),
        client.chat_with_tools_streaming(&user("hi"), &tools, |_, _| {}),
    )
    .await
    .expect("timed out");
    let body = server.await.unwrap();

    let injected: String = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m.get("content").and_then(|c| c.as_str()))
        .collect();
    assert!(
        !injected.contains("tools available across these categories"),
        "the categorised name summary is back in the prompt"
    );
    assert!(
        !injected.contains("... and "),
        "the '… and N more' truncation is back in the prompt"
    );
    // CONTRACT CHANGE (dehardcoding): the domain guidance (PR #114's
    // "where materials data actually lives") moved out of the Rust block
    // into the tools' own descriptions — what must survive in the injected
    // block is the DOMAIN-NEUTRAL discipline, and no domain vocabulary.
    assert!(
        injected.contains("Long-horizon discipline"),
        "the long-horizon discipline was lost"
    );
    assert!(
        !injected.contains("where materials data actually lives"),
        "domain routing doctrine must live in tool descriptions, not a Rust constant"
    );
}

/// The platform forwards the provider's OpenAI-style `tool_calls` deltas
/// verbatim on its `StreamChunk`. Assemble them like the OpenAI path does —
/// before this change the MARC27 path only ever looked at `delta` text, so a
/// native tool call arrived as an empty turn.
#[tokio::test]
async fn marc27_assembles_native_tool_calls_from_the_stream() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let sse = concat!(
        "data: {\"delta\":\"\",\"done\":false,\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",",
        "\"type\":\"function\",\"function\":{\"name\":\"compute_gpus\",\"arguments\":\"{\\\"q\\\"\"}}]}\n\n",
        "data: {\"delta\":\"\",\"done\":false,\"tool_calls\":[{\"index\":0,",
        "\"function\":{\"arguments\":\":\\\"idle\\\"}\"}}]}\n\n",
        "data: {\"delta\":\"\",\"done\":true,\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5}}\n\n",
    );
    let server = tokio::spawn(serve_once(listener, sse.to_string()));
    let client = LlmClient::new(config(format!("http://127.0.0.1:{port}/llm")));
    let resp = tokio::time::timeout(
        Duration::from_secs(10),
        client.chat_with_tools_streaming(
            &user("which gpus are idle?"),
            &wide_selection(),
            |_, _| {},
        ),
    )
    .await
    .expect("timed out")
    .expect("stream failed");
    let _ = server.await.unwrap();

    let calls = resp
        .message
        .tool_calls
        .expect("native tool_calls were dropped — the MARC27 path only read `delta` text");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].function.name, "compute_gpus");
    assert_eq!(calls[0].id, "call_1");
    assert_eq!(calls[0].function.arguments, r#"{"q":"idle"}"#);
    let usage = resp.usage.expect("usage lost");
    assert_eq!(usage.total_tokens, 15);
}

/// Prose that accompanies a NATIVE tool call must survive. The
/// looks-like-JSON scrubber exists for models that leak a fenced call into
/// their text; native calls arrive on their own channel and cannot, so an
/// answer that merely ends in a code fence must not be deleted.
#[tokio::test]
async fn marc27_keeps_prose_that_accompanies_a_native_tool_call() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let sse = concat!(
        "data: {\"delta\":\"Checking the broker. Equivalent CLI:\\n```\\nprism compute gpus\\n```\",",
        "\"done\":false}\n\n",
        "data: {\"delta\":\"\",\"done\":true,\"tool_calls\":[{\"index\":0,\"id\":\"c1\",",
        "\"function\":{\"name\":\"compute_gpus\",\"arguments\":\"{}\"}}]}\n\n",
    );
    let server = tokio::spawn(serve_once(listener, sse.to_string()));
    let client = LlmClient::new(config(format!("http://127.0.0.1:{port}/llm")));
    let resp = tokio::time::timeout(
        Duration::from_secs(10),
        client.chat_with_tools_streaming(&user("gpus?"), &wide_selection(), |_, _| {}),
    )
    .await
    .expect("timed out")
    .expect("stream failed");
    let _ = server.await.unwrap();

    assert!(
        resp.message
            .content
            .as_deref()
            .is_some_and(|c| c.contains("Checking the broker")),
        "prose alongside a native tool call was scrubbed: {:?}",
        resp.message.content
    );
    assert_eq!(
        resp.message.tool_calls.expect("call lost")[0]
            .function
            .arguments,
        "{}"
    );
}

/// An upstream provider that rejects OpenAI-shaped tool schemas must not fail
/// the turn. Measured live against the platform's direct Anthropic provider:
/// `500 … Anthropic API returned 400 … tools.0: Input tag 'function' … does
/// not match any of the expected tags`. The turn falls back to the text
/// protocol — carrying EVERY selected tool, not a 4-per-category sample.
#[tokio::test]
async fn marc27_falls_back_to_text_tools_when_the_provider_rejects_schemas() {
    let tools = wide_selection();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(serve_many(
        listener,
        vec![
            (
                500,
                "{\"error\":{\"message\":\"internal: Anthropic API returned 400 Bad Request: \
                 tools.0: Input tag 'function' does not match any of the expected tags\"}}"
                    .to_string(),
            ),
            (
                200,
                "data: {\"delta\":\"ok\",\"done\":true}\n\n".to_string(),
            ),
        ],
    ));
    let client = LlmClient::new(config(format!("http://127.0.0.1:{port}/llm")));
    let resp = tokio::time::timeout(
        Duration::from_secs(10),
        client.chat_with_tools_streaming(&user("which gpus are idle?"), &tools, |_, _| {}),
    )
    .await
    .expect("timed out")
    .expect("the turn should have recovered, not failed");
    assert_eq!(resp.message.content.as_deref(), Some("ok"));

    let bodies = server.await.unwrap();
    assert_eq!(bodies.len(), 2, "no fallback attempt was made");
    assert!(
        bodies[0].get("tools").is_some(),
        "first attempt must try native tools"
    );
    assert!(
        bodies[1].get("tools").is_none(),
        "the fallback must not resend the schemas that were just rejected"
    );

    let injected: String = bodies[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m.get("content").and_then(|c| c.as_str()))
        .collect();
    for t in &tools {
        assert!(
            injected.contains(&format!("`{}(", t.function.name)),
            "fallback text omitted `{}` — it must list EVERY selected tool",
            t.function.name
        );
    }
    assert!(
        injected.contains("does compute_gpus"),
        "fallback text omitted descriptions"
    );
    assert!(
        injected.contains("```tool_call"),
        "fallback text omitted the call syntax"
    );
}

/// The fallback is narrow on purpose: a billing or auth failure must surface,
/// not be retried without tools and reported as something else.
#[tokio::test]
async fn marc27_does_not_fall_back_on_a_payment_error() {
    let tools = wide_selection();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(serve_many(
        listener,
        vec![(
            402,
            "{\"error\":{\"code\":\"insufficient_credits\",\"message\":\"top up to continue\"}}"
                .to_string(),
        )],
    ));
    let client = LlmClient::new(config(format!("http://127.0.0.1:{port}/llm")));
    let err = tokio::time::timeout(
        Duration::from_secs(10),
        client.chat_with_tools_streaming(&user("hi"), &tools, |_, _| {}),
    )
    .await
    .expect("timed out")
    .expect_err("a 402 must fail the turn");
    assert!(
        format!("{err:#}").contains("insufficient_credits"),
        "the original error was masked: {err:#}"
    );
    let bodies = server.await.unwrap();
    assert_eq!(bodies.len(), 1, "a 402 must not trigger a second request");
}

/// The fallback needs BOTH a request-shape status AND the tools field named.
/// A credit failure whose body happens to mention tools must still fail —
/// otherwise the second attempt's error replaces the one that said "top up".
#[tokio::test]
async fn marc27_does_not_fall_back_on_a_402_that_mentions_tools() {
    let tools = wide_selection();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(serve_many(
        listener,
        vec![(
            402,
            "{\"error\":{\"code\":\"insufficient_credits\",\"message\":\
             \"your plan does not include \\\"tools\\\". top up to continue\"}}"
                .to_string(),
        )],
    ));
    let client = LlmClient::new(config(format!("http://127.0.0.1:{port}/llm")));
    let err = tokio::time::timeout(
        Duration::from_secs(10),
        client.chat_with_tools_streaming(&user("hi"), &tools, |_, _| {}),
    )
    .await
    .expect("timed out")
    .expect_err("a 402 must fail the turn");
    assert!(
        format!("{err:#}").contains("insufficient_credits"),
        "the billing error was masked: {err:#}"
    );
    let bodies = server.await.unwrap();
    assert_eq!(bodies.len(), 1, "a 402 must not trigger a second request");
}

/// When the fallback ALSO fails, the caller must still see what the provider
/// originally refused — not just the second, less informative failure.
#[tokio::test]
async fn marc27_surfaces_the_original_error_when_the_fallback_also_fails() {
    let tools = wide_selection();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(serve_many(
        listener,
        vec![
            (
                500,
                "{\"error\":{\"message\":\"internal: Anthropic API returned 400: \
                 tools.0: Input tag 'function' does not match\"}}"
                    .to_string(),
            ),
            (
                500,
                "{\"error\":{\"message\":\"upstream connection reset\"}}".to_string(),
            ),
        ],
    ));
    let client = LlmClient::new(config(format!("http://127.0.0.1:{port}/llm")));
    let err = tokio::time::timeout(
        Duration::from_secs(10),
        client.chat_with_tools_streaming(&user("hi"), &tools, |_, _| {}),
    )
    .await
    .expect("timed out")
    .expect_err("both attempts failed");
    let text = format!("{err:#}");
    assert!(
        text.contains("Input tag 'function'"),
        "the original refusal was replaced by the fallback's error: {text}"
    );
    assert!(
        text.contains("upstream connection reset"),
        "the fallback's error was dropped instead of attached: {text}"
    );
    assert_eq!(server.await.unwrap().len(), 2);
}

/// Text-fenced tool calls stay parseable: the platform cannot yet forward
/// tool_calls for every upstream provider (its direct Anthropic provider
/// returns none), and models sometimes narrate a call anyway.
#[tokio::test]
async fn marc27_still_parses_text_fenced_tool_calls_as_a_fallback() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let sse = concat!(
        "data: {\"delta\":\"```tool_call\\n{\\\"name\\\": \\\"compute_gpus\\\", ",
        "\\\"arguments\\\": {\\\"q\\\": \\\"idle\\\"}}\\n```\",\"done\":true}\n\n",
    );
    let server = tokio::spawn(serve_once(listener, sse.to_string()));
    let client = LlmClient::new(config(format!("http://127.0.0.1:{port}/llm")));
    let resp = tokio::time::timeout(
        Duration::from_secs(10),
        client.chat_with_tools_streaming(
            &user("which gpus are idle?"),
            &wide_selection(),
            |_, _| {},
        ),
    )
    .await
    .expect("timed out")
    .expect("stream failed");
    let _ = server.await.unwrap();

    let calls = resp
        .message
        .tool_calls
        .expect("text fallback stopped working");
    assert_eq!(calls[0].function.name, "compute_gpus");
}

/// Without `stream_options.include_usage`, an OpenAI-shaped server streams
/// `"usage": null` on every chunk, and every context mechanism downstream —
/// token-pressure compaction, the budget warning, cost — reads a number that
/// is structurally zero. This asserts on the bytes that leave the process.
#[tokio::test]
async fn the_openai_stream_request_asks_for_usage() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(serve_once(
        listener,
        "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n".to_string(),
    ));
    let client = LlmClient::new(config(format!("http://127.0.0.1:{port}/v1")));
    let _ = tokio::time::timeout(
        Duration::from_secs(10),
        client.chat_with_tools_streaming(&user("hello"), &[], |_, _| {}),
    )
    .await
    .expect("turn timed out");
    let body = server.await.unwrap();
    assert_eq!(
        body.pointer("/stream_options/include_usage"),
        Some(&serde_json::json!(true)),
        "the stream must ask the server to report usage: {body}"
    );
}

/// A usage an earlier chunk reported must survive a trailing chunk whose
/// `usage` is null. The old `.ok()` assignment overwrote it, so a real count
/// became `None` and compaction never fired.
#[tokio::test]
async fn a_reported_usage_survives_a_trailing_null_chunk() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let sse = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}],\"usage\":null}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":7,\"total_tokens\":107}}\n\n",
        "data: {\"choices\":[{\"delta\":{}}],\"usage\":null}\n\n",
        "data: [DONE]\n\n",
    )
    .to_string();
    let server = tokio::spawn(serve_once(listener, sse));
    let client = LlmClient::new(config(format!("http://127.0.0.1:{port}/v1")));
    let response = tokio::time::timeout(
        Duration::from_secs(10),
        client.chat_with_tools_streaming(&user("hello"), &[], |_, _| {}),
    )
    .await
    .expect("turn timed out")
    .expect("stream parses");
    let _ = server.await.unwrap();
    let usage = response
        .usage
        .expect("a usage the server reported must reach the caller");
    assert_eq!(usage.prompt_tokens, 100);
    assert_eq!(usage.completion_tokens, 7);
}

/// Reasoning tokens reach the UI as thinking and must NOT reach
/// `message.content`, which is stored and re-sent as the model's own words.
#[tokio::test]
async fn reasoning_tokens_never_enter_the_message_content() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let sse = concat!(
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"let me think about seals\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"PTFE is the answer.\"}}]}\n\n",
        "data: [DONE]\n\n",
    )
    .to_string();
    let server = tokio::spawn(serve_once(listener, sse));
    let client = LlmClient::new(config(format!("http://127.0.0.1:{port}/v1")));
    let mut thinking = String::new();
    let response = tokio::time::timeout(
        Duration::from_secs(10),
        client.chat_with_tools_streaming(&user("hello"), &[], |delta, is_reasoning| {
            if is_reasoning {
                thinking.push_str(delta);
            }
        }),
    )
    .await
    .expect("turn timed out")
    .expect("stream parses");
    let _ = server.await.unwrap();
    assert_eq!(
        response.message.content.as_deref(),
        Some("PTFE is the answer."),
        "content must be the answer alone"
    );
    assert!(
        thinking.contains("let me think"),
        "reasoning still reaches the UI as thinking"
    );
    assert_eq!(
        response.message.reasoning_content.as_deref(),
        Some("let me think about seals"),
        "reasoning is kept, in its own field"
    );
}

/// What goes back on the wire is the operator's call: by default an
/// assistant turn's reasoning is stripped from the history (some providers
/// reject the field in input); with `replay_reasoning_content` it is sent as
/// its own field, never folded into `content`.
#[tokio::test]
async fn reasoning_is_replayed_only_when_the_operator_asks() {
    let sse = concat!(
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"quote is on line 12\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"noted\"}}]}\n\n",
        "data: [DONE]\n\n",
    )
    .to_string();
    for replay in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(serve_many(
            listener,
            vec![(200, sse.clone()), (200, sse.clone())],
        ));
        let mut cfg = config(format!("http://127.0.0.1:{port}/v1"));
        cfg.replay_reasoning_content = replay;
        let client = LlmClient::new(cfg);
        let mut history = user("where is the quote?");
        let first = client
            .chat_with_tools_streaming(&history, &[], |_, _| {})
            .await
            .expect("first turn");
        history.push(first.message.clone());
        history.extend(user("and the value?"));
        let _second = client
            .chat_with_tools_streaming(&history, &[], |_, _| {})
            .await
            .expect("second turn");
        let bodies = server.await.unwrap();
        let assistant = &bodies[1]["messages"][1];
        assert_eq!(assistant["role"], "assistant", "{assistant}");
        assert_eq!(
            assistant["content"], "noted",
            "content is the answer alone: {assistant}"
        );
        if replay {
            assert_eq!(
                assistant["reasoning_content"], "quote is on line 12",
                "the operator asked for replay: {assistant}"
            );
        } else {
            assert!(
                assistant.get("reasoning_content").is_none(),
                "off by default: {assistant}"
            );
        }
    }
}
