//! Platform bridge — in-process adapter between forge and the MARC27 platform.
//!
//! The MARC27 platform exposes a wide surface (LLM, knowledge graph,
//! compute broker, marketplace, mesh federation, billing, ...) under
//! `/api/v1/projects/<project_id>/...` with its own JSON + SSE conventions.
//! This module owns ONE slice of that — the LLM translation slice — by
//! running a tiny axum server on `127.0.0.1` that:
//!
//!   1. Exposes a strict OpenAI shape (`GET /v1/models`,
//!      `POST /v1/chat/completions`) so forge's `openai_compatible`
//!      provider can talk to MARC27 unmodified.
//!   2. Translates outgoing requests to MARC27's `/llm/stream` endpoint
//!      (request body shape, SSE delta format) and streams responses back
//!      reshaped into OpenAI delta chunks.
//!   3. Calls into `prism_tool_router` to prune forge's per-turn tool list
//!      to a top-K semantically relevant subset before forwarding —
//!      keeping the request inside MARC27's body limit AND giving the
//!      chat LLM a focused tool set to choose from.
//!
//! Other MARC27 surfaces (compute, knowledge, marketplace, mesh) are
//! reached through PRISM's existing CLI subcommands and the rust/python
//! MCP servers — those tools shell out directly to MARC27 without going
//! through this bridge.
//!
//! Stage 2.2 will extend this module: instead of just pruning tools, the
//! router will *route* — emit a tool_call locally via FunctionGemma when
//! confident, falling through to the chat LLM only for synthesis or
//! ambiguous cases.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{Context, Result};
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::stream::Stream;
use futures_util::StreamExt;
use prism_tool_router::{RoutingDecision, ToolDef, ToolRouter};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

#[derive(Clone)]
struct ProxyState {
    upstream_base: String, // e.g. "https://api.marc27.com/api/v1/projects/<pid>/llm"
    access_token: String,
    http: reqwest::Client,
    /// Optional semantic tool router. When present, requests with a tools[]
    /// array are filtered to top-K=8 most relevant before forwarding to
    /// MARC27. When absent, fallback to the FIFO trim heuristic so the
    /// proxy still works without the router (e.g. model not yet downloaded).
    router: Option<Arc<ToolRouter>>,
}

/// Handle returned from `start`. Drop it (or call `shutdown()`) when the
/// chat session ends to stop the proxy task.
pub struct ProxyHandle {
    pub url: String, // base URL forge should hit (proxy_url + "/v1")
    shutdown: Option<oneshot::Sender<()>>,
}

impl ProxyHandle {
    pub fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

impl Drop for ProxyHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

/// Spin up the proxy on a free localhost port.
///
/// `platform_url` is the MARC27 base (e.g. `https://api.marc27.com`),
/// `project_id` selects which project's LLM endpoints to hit, and
/// `access_token` is the bearer credential. Returns a handle whose `.url`
/// can be plugged into forge's `OPENAI_URL` (the proxy serves the OpenAI
/// surface under `/v1`, so the value is `http://127.0.0.1:<port>/v1`).
pub async fn start(
    platform_url: &str,
    project_id: &str,
    access_token: &str,
    router: Option<Arc<ToolRouter>>,
) -> Result<ProxyHandle> {
    let upstream_base = format!(
        "{}/api/v1/projects/{}/llm",
        platform_url.trim_end_matches('/'),
        project_id
    );

    let state = ProxyState {
        upstream_base,
        access_token: access_token.to_string(),
        http: reqwest::Client::builder()
            // SSE streams can run for minutes — let the upstream control timing.
            .timeout(std::time::Duration::from_secs(600))
            .build()
            .context("build reqwest client")?,
        router,
    };

    let app = Router::new()
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat_completions))
        // Forge sends the full tool schema array on every turn. With ~150
        // tools that's already several hundred KB; conversations can grow
        // megabytes large. Disable axum's default 2MB body limit.
        .layer(DefaultBodyLimit::disable())
        .with_state(Arc::new(state));

    // Bind to port 0 so the OS picks a free port.
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .context("bind localhost proxy")?;
    let local = listener.local_addr().context("local_addr")?;
    let url = format!("http://{}/v1", local);

    let (tx, rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = rx.await;
            })
            .await;
    });

    Ok(ProxyHandle { url, shutdown: Some(tx) })
}

// ── Routes ───────────────────────────────────────────────────────────

/// `GET /v1/models` — translate MARC27's flat array into OpenAI's
/// `{ "object": "list", "data": [{ id, object, created, owned_by }, …] }`.
async fn list_models(State(state): State<Arc<ProxyState>>) -> Response {
    let upstream = format!("{}/models", state.upstream_base);
    let res = state
        .http
        .get(&upstream)
        .bearer_auth(&state.access_token)
        .send()
        .await;

    let resp = match res {
        Ok(r) => r,
        Err(e) => return error_response(StatusCode::BAD_GATEWAY, &format!("upstream: {e}")),
    };

    let status = resp.status();
    let text = match resp.text().await {
        Ok(t) => t,
        Err(e) => return error_response(StatusCode::BAD_GATEWAY, &format!("read body: {e}")),
    };

    if !status.is_success() {
        return error_response(
            StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
            &text,
        );
    }

    // MARC27 gives us either `[{...}, ...]` or `{"data": [...]}`. Accept both.
    let arr: Vec<Value> = match serde_json::from_str::<Value>(&text) {
        Ok(Value::Array(a)) => a,
        Ok(Value::Object(mut m)) => match m.remove("data") {
            Some(Value::Array(a)) => a,
            _ => return error_response(StatusCode::BAD_GATEWAY, "models: unexpected object shape"),
        },
        Ok(_) => return error_response(StatusCode::BAD_GATEWAY, "models: not array"),
        Err(e) => return error_response(StatusCode::BAD_GATEWAY, &format!("models parse: {e}")),
    };

    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let data: Vec<Value> = arr
        .into_iter()
        .map(|m| {
            let id = m.get("model_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let owned_by = m
                .get("provider")
                .and_then(|v| v.as_str())
                .unwrap_or("marc27")
                .to_string();
            json!({
                "id": id,
                "object": "model",
                "created": now,
                "owned_by": owned_by,
            })
        })
        .collect();

    Json(json!({ "object": "list", "data": data })).into_response()
}

/// MARC27 platform rejects request bodies above 64 KiB ("failed to read
/// request body"). The Stage 2 tool router keeps us comfortably under by
/// returning top-K=8 tools per query; this fallback budget is only used
/// when the router is unavailable (e.g. EmbeddingGemma model not yet
/// downloaded).
const MARC27_BODY_BUDGET: usize = 60_000;

/// FIFO body-budget trim. Reorders so PRISM/forge-priority tools survive,
/// then drops from the end until the serialised request fits under
/// MARC27's body limit. Used only as fallback when the semantic router
/// is unavailable.
fn fifo_trim(req: &mut Value, tools: &mut Vec<Value>) {
    const ALWAYS_KEEP: &[&str] = &["read", "write", "shell", "task", "fetch"];
    let original = tools.len();
    let mut keep_front = Vec::new();
    let mut prism_tools = Vec::new();
    let mut other = Vec::new();
    for t in std::mem::take(tools) {
        let name = t
            .get("function")
            .and_then(|f| f.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if ALWAYS_KEEP.contains(&name.as_str()) {
            keep_front.push(t);
        } else if name.starts_with("mcp_prism_") {
            prism_tools.push(t);
        } else {
            other.push(t);
        }
    }
    let mut kept: Vec<Value> = keep_front;
    kept.extend(prism_tools);
    kept.extend(other);
    let mut size = {
        req["tools"] = Value::Array(kept.clone());
        serde_json::to_vec(req).map(|b| b.len()).unwrap_or(usize::MAX)
    };
    while size > MARC27_BODY_BUDGET && !kept.is_empty() {
        kept.pop();
        req["tools"] = Value::Array(kept.clone());
        size = serde_json::to_vec(req).map(|b| b.len()).unwrap_or(usize::MAX);
    }
    if kept.len() != original {
        eprintln!(
            "[platform_bridge] FIFO fallback trim: {} → {} (body {} / budget {})",
            original,
            kept.len(),
            size,
            MARC27_BODY_BUDGET,
        );
    }
}

/// Iteratively shrink the longest message's `content` until the serialised
/// request fits MARC27's 60 KiB budget. Tools array is already trimmed by
/// the Stage 2.1 retriever — this handles the orthogonal axis: tool results
/// packed back into chat history exceeding the budget. Idempotent: bails
/// after one no-progress pass to avoid infinite loops on pathological input.
fn truncate_oversized_messages(req: &mut Value) {
    fn body_size(req: &Value) -> usize {
        serde_json::to_vec(req).map(|b| b.len()).unwrap_or(usize::MAX)
    }

    let mut size = body_size(req);
    if size <= MARC27_BODY_BUDGET {
        return;
    }

    let mut iterations = 0;
    let mut total_truncated_bytes: usize = 0;
    loop {
        if size <= MARC27_BODY_BUDGET {
            break;
        }
        iterations += 1;
        if iterations > 50 {
            // Safety brake: if we haven't fit after 50 truncations, give up
            // and let MARC27 reject — surfacing the underlying bug is better
            // than silently looping.
            eprintln!(
                "[platform_bridge] truncate_oversized_messages: gave up at \
                 iteration 50 with body {} > {} budget",
                size, MARC27_BODY_BUDGET
            );
            break;
        }

        let messages = match req.get_mut("messages").and_then(|v| v.as_array_mut()) {
            Some(m) if !m.is_empty() => m,
            _ => break,
        };

        // Find the message with the largest `content` string.
        let mut idx: Option<usize> = None;
        let mut max_len: usize = 0;
        for (i, msg) in messages.iter().enumerate() {
            if let Some(c) = msg.get("content").and_then(|v| v.as_str()) {
                if c.len() > max_len {
                    max_len = c.len();
                    idx = Some(i);
                }
            }
        }
        let i = match idx {
            Some(i) => i,
            None => break, // no string contents to trim
        };

        // Truncate this message to ~50% of its current size — aggressive
        // enough to make progress fast, conservative enough to keep N
        // truncations bounded.
        let msg = &mut messages[i];
        let content = msg
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let new_len = (content.len() / 2).max(512);
        if new_len >= content.len() {
            break;
        }
        let truncated_bytes = content.len() - new_len;
        total_truncated_bytes += truncated_bytes;

        // Truncate at a UTF-8 boundary so we don't split a codepoint.
        let mut cut = new_len.min(content.len());
        while cut > 0 && !content.is_char_boundary(cut) {
            cut -= 1;
        }
        let truncated = format!(
            "{}\n\n[…{} bytes truncated for MARC27 64 KiB body budget]",
            &content[..cut],
            truncated_bytes
        );
        if let Value::Object(obj) = msg {
            obj.insert("content".to_string(), Value::String(truncated));
        }

        size = body_size(req);
    }

    if total_truncated_bytes > 0 {
        eprintln!(
            "[platform_bridge] truncated {} bytes across {} iteration(s) to fit {} byte budget",
            total_truncated_bytes, iterations, MARC27_BODY_BUDGET
        );
    }
}

/// `POST /v1/chat/completions` — accept OpenAI request, forward to MARC27's
/// `/stream`, translate SSE deltas into OpenAI delta chunks.
async fn chat_completions(
    State(state): State<Arc<ProxyState>>,
    body: Bytes,
) -> Response {
    tracing::debug!(target: "platform_bridge", body_len = body.len(), "chat/completions hit");
    let mut req: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return error_response(StatusCode::BAD_REQUEST, &format!("invalid JSON body: {e}"));
        }
    };

    // Stage 2.1: semantic top-K tool retrieval.
    //
    // When a router is wired, we extract the user's last message, ask
    // EmbeddingGemma for the top-K=8 tools by cosine similarity over the
    // dynamic tool index (PRISM built-ins + Rust MCP + Python MCP +
    // marketplace user tools), and replace forge's full tools[] with that
    // subset before forwarding. Always-keep tools (the universally-useful
    // forge built-ins) are pinned regardless of similarity score.
    //
    // If no router (model not downloaded yet, etc.), fall back to the
    // pre-Stage-2 FIFO trim so chat still works. The fallback is body-size
    // budgeted; the router path is count-budgeted (top-K) which is a much
    // tighter fit and will keep us comfortably under MARC27's 64 KiB.
    {
        const ALWAYS_KEEP: &[&str] = &["read", "write", "shell", "task", "fetch"];
        const TOP_K: usize = 8;

        let last_user_msg: Option<String> = req
            .get("messages")
            .and_then(|v| v.as_array())
            .and_then(|arr| {
                arr.iter()
                    .rev()
                    .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_str())
                    .map(|s| s.to_string())
            });

        let tools_owned: Option<Vec<Value>> = req
            .get("tools")
            .and_then(|v| v.as_array())
            .map(|a| a.to_vec());

        if let (Some(tools), Some(query), Some(router)) =
            (tools_owned.as_ref(), last_user_msg.as_ref(), state.router.as_ref())
        {
            let original = tools.len();

            // Build the (name, ToolDef) catalog forge sent us so the router
            // indexes anything it hasn't seen before. This handles new
            // marketplace tools transparently — first request that includes
            // them triggers an embed; subsequent ones use the cache.
            let defs: Vec<ToolDef> = tools
                .iter()
                .filter_map(|t| {
                    let f = t.get("function")?;
                    let name = f.get("name")?.as_str()?.to_string();
                    let description =
                        f.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let args_schema = f.get("parameters").cloned().unwrap_or(json!({}));
                    Some(ToolDef { name, description, args_schema })
                })
                .collect();
            let names: Vec<String> = defs.iter().map(|d| d.name.clone()).collect();
            if let Err(e) = router.index_tools(&defs).await {
                tracing::warn!(target: "platform_bridge", error = %e, "tool index update failed");
            }

            match router.search(query, &names, TOP_K).await {
                Ok(top) => {
                    let mut keep_set: std::collections::HashSet<String> =
                        top.into_iter().collect();
                    for k in ALWAYS_KEEP {
                        if names.iter().any(|n| n == k) {
                            keep_set.insert(k.to_string());
                        }
                    }
                    let kept: Vec<Value> = tools
                        .iter()
                        .filter(|t| {
                            t.get("function")
                                .and_then(|f| f.get("name"))
                                .and_then(|v| v.as_str())
                                .map(|n| keep_set.contains(n))
                                .unwrap_or(false)
                        })
                        .cloned()
                        .collect();
                    let kept_count = kept.len();
                    req["tools"] = Value::Array(kept);
                    if kept_count != original {
                        let final_size =
                            serde_json::to_vec(&req).map(|b| b.len()).unwrap_or(0);
                        eprintln!(
                            "[platform_bridge] semantic top-K: {} → {} tools (body {} bytes)",
                            original, kept_count, final_size
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(target: "platform_bridge", error = %e, "router.search failed; FIFO trim fallback");
                    fifo_trim(&mut req, &mut tools_owned.clone().unwrap());
                }
            }
        } else if let Some(mut tools) = tools_owned {
            // No router available — fall back to FIFO trim.
            fifo_trim(&mut req, &mut tools);
        }
    }

    let model = req
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let stream_requested = req.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);

    // Stage 2.2: FunctionGemma routing.
    //
    // Only run on turns whose latest message is from the user — synthesis
    // turns (where the latest message is a tool result or an assistant
    // continuation) should pass through to the chat LLM untouched.
    //
    // FunctionGemma sees ONLY the post-retrieval tools[] (the top-K already
    // chosen above), so the router can't pick a tool the chat LLM wouldn't
    // also have seen. If FunctionGemma emits a parseable call whose name is
    // in our tool list, we synthesise an OpenAI streaming response with that
    // tool_call and return it directly — never touching the chat LLM.
    if let Some(router) = state.router.as_ref() {
        let last_role = req
            .get("messages")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.last())
            .and_then(|m| m.get("role"))
            .and_then(|r| r.as_str())
            .unwrap_or("");
        let user_query: Option<String> = req
            .get("messages")
            .and_then(|v| v.as_array())
            .and_then(|arr| {
                arr.iter()
                    .rev()
                    .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_str())
                    .map(|s| s.to_string())
            });
        let tools_for_routing: Vec<Value> = req
            .get("tools")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        if last_role == "user" && !tools_for_routing.is_empty() {
            if let Some(query) = user_query.as_ref() {
                let decision = router.route(query, &tools_for_routing).await;
                if let RoutingDecision::Invoke(mut call) = decision {
                    // 1. Validate the chosen tool name is actually in the
                    //    list — guards against hallucinated names.
                    let name_known = tools_for_routing.iter().any(|t| {
                        t.get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|v| v.as_str())
                            == Some(call.name.as_str())
                    });
                    if !name_known {
                        eprintln!(
                            "[platform_bridge] FunctionGemma proposed unknown tool {:?}, falling through",
                            call.name
                        );
                    } else {
                        // 2. Schema-validate args against the tool's
                        //    declared parameters. Base FunctionGemma picks
                        //    the right tool but tends to invent extra args
                        //    — strip those, keep schema-declared ones.
                        //    If required fields are still unsatisfied,
                        //    fall through to the chat LLM rather than
                        //    ship a doomed call.
                        let schema_ok = sanitise_args_against_schema(
                            &mut call,
                            &tools_for_routing,
                        );
                        if !schema_ok {
                            eprintln!(
                                "[platform_bridge] FunctionGemma args invalid against schema for {}, falling through",
                                call.name
                            );
                        } else {
                            eprintln!(
                                "[platform_bridge] FunctionGemma routed locally → {} (skipping chat LLM)",
                                call.name
                            );
                            if stream_requested {
                                let stream = synthetic_tool_call_stream(call, model.clone());
                                return Sse::new(stream).into_response();
                            } else {
                                return Json(synthetic_tool_call_full(call, model.clone()))
                                    .into_response();
                            }
                        }
                    }
                }
            }
        }
    }
    // MARC27's chat endpoint only accepts system|user|assistant roles, but
    // OpenAI-style tool dispatch produces messages with role=tool (the tool
    // result) and assistant messages carrying a tool_calls array. Flatten
    // both into MARC27-friendly shapes before forwarding.
    flatten_tool_messages_for_marc27(&mut req);

    // BUG-C guard: MARC27 rejects request bodies above 64 KiB
    // ("failed to read request body"). The Stage 2.1 tool trim handles the
    // tools[] axis; this handles the messages[] axis — large tool results
    // (e.g. models_list returns 35 KB) packed back into chat history can
    // blow the budget on multi-turn conversations. Iteratively truncate the
    // longest message's content until the total request body fits, leaving
    // a "[…N bytes truncated]" marker so the LLM knows context was reduced.
    truncate_oversized_messages(&mut req);

    tracing::debug!(
        target: "platform_bridge",
        %model, stream_requested,
        msgs_len = req.get("messages").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0),
        has_tools = req.get("tools").is_some(),
        body_bytes = serde_json::to_vec(&req).map(|b| b.len()).unwrap_or(0),
        "forwarding to MARC27"
    );

    // MARC27 only speaks streaming. If forge asked for non-streaming we still
    // hit the streaming endpoint upstream and assemble a single completion
    // response from the deltas.
    let upstream = format!("{}/stream", state.upstream_base);

    let res = state
        .http
        .post(&upstream)
        .bearer_auth(&state.access_token)
        .header("Accept", "text/event-stream")
        .json(&req)
        .send()
        .await;

    let resp = match res {
        Ok(r) => r,
        Err(e) => return error_response(StatusCode::BAD_GATEWAY, &format!("upstream: {e}")),
    };

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        eprintln!(
            "\x1b[31m[prism]\x1b[0m MARC27 returned {status}: {}",
            body.lines().next().unwrap_or("")
        );
        return error_response(
            StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
            &body,
        );
    }

    if stream_requested {
        let stream = sse_stream(resp, model);
        Sse::new(stream).into_response()
    } else {
        match collect_full(resp, &model).await {
            Ok(v) => Json(v).into_response(),
            Err(e) => error_response(StatusCode::BAD_GATEWAY, &format!("assemble: {e}")),
        }
    }
}

// ── MARC27 message-shape adapter ─────────────────────────────────────

/// MARC27 only accepts `system|user|assistant`. OpenAI-style tool dispatch
/// produces:
///   {"role":"assistant", "content": null, "tool_calls":[{...}]}
///   {"role":"tool", "tool_call_id":"...", "content":"result"}
/// We rewrite those into MARC27-friendly form:
///   - assistant tool_calls → assistant message whose content describes the
///     intent ("Calling X with args Y"); tool_calls field dropped
///   - tool result → user message prefixed with "[tool <name> result]"
/// The chat LLM still sees enough context to synthesise a final answer.
fn flatten_tool_messages_for_marc27(req: &mut Value) {
    let messages = match req.get_mut("messages").and_then(|v| v.as_array_mut()) {
        Some(m) => m,
        None => return,
    };
    // Build a map of tool_call_id → tool_name from any preceding assistant
    // tool_calls so we can label the tool-result message clearly.
    let mut id_to_name: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for msg in messages.iter() {
        if let Some(calls) = msg.get("tool_calls").and_then(|v| v.as_array()) {
            for c in calls {
                let id = c.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let name = c
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("tool")
                    .to_string();
                if !id.is_empty() {
                    id_to_name.insert(id, name);
                }
            }
        }
    }

    for msg in messages.iter_mut() {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
        match role {
            "tool" => {
                let id = msg
                    .get("tool_call_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let tool_name = id_to_name.get(&id).cloned().unwrap_or_else(|| "tool".into());
                let result = msg
                    .get("content")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                let new = json!({
                    "role": "user",
                    "content": format!("[result of {tool_name}]: {result}"),
                });
                *msg = new;
            }
            "assistant" => {
                if msg.get("tool_calls").is_some() {
                    // Rewrite assistant-with-tool_calls into a textual
                    // description so MARC27 doesn't see the unsupported
                    // tool_calls field.
                    let mut summary = String::new();
                    if let Some(calls) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                        for c in calls {
                            let name = c
                                .get("function")
                                .and_then(|f| f.get("name"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("tool");
                            let args = c
                                .get("function")
                                .and_then(|f| f.get("arguments"))
                                .map(|v| match v {
                                    Value::String(s) => s.clone(),
                                    other => other.to_string(),
                                })
                                .unwrap_or_else(|| "{}".to_string());
                            if !summary.is_empty() {
                                summary.push_str("; ");
                            }
                            summary.push_str(&format!("calling {name} with {args}"));
                        }
                    }
                    let existing = msg
                        .get("content")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                        .unwrap_or_default();
                    let content = if existing.is_empty() {
                        summary
                    } else {
                        format!("{existing}\n{summary}")
                    };
                    *msg = json!({"role": "assistant", "content": content});
                }
            }
            _ => {}
        }
    }
}

// ── Schema-aware arg sanitisation (Stage 2.2 quality fix) ────────────

/// Trim `call.arguments` to the keys declared in the matching tool's JSON
/// schema and verify all required fields are present. Returns true when
/// the call is safe to ship to the tool dispatcher; false when it should
/// fall through to the chat LLM.
///
/// This is the runtime arg-accuracy guard that makes the un-fine-tuned
/// FunctionGemma reliable in practice — base model picks correct tool +
/// emits structurally valid call but invents extra args; we drop those.
fn sanitise_args_against_schema(
    call: &mut prism_tool_router::ToolCall,
    tools: &[Value],
) -> bool {
    let parameters = tools
        .iter()
        .find(|t| {
            t.get("function")
                .and_then(|f| f.get("name"))
                .and_then(|v| v.as_str())
                == Some(call.name.as_str())
        })
        .and_then(|t| t.get("function"))
        .and_then(|f| f.get("parameters"));

    let Some(parameters) = parameters else {
        // No schema available — best-effort, accept what the model emitted.
        return true;
    };
    let allowed: std::collections::HashSet<String> = parameters
        .get("properties")
        .and_then(|p| p.as_object())
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    let required: Vec<String> = parameters
        .get("required")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    if let Value::Object(args) = &mut call.arguments {
        // Always retain only allowed keys. Previously gated on
        // `!allowed.is_empty()`, which let hallucinated args survive for
        // tools whose schema is `{"properties": {}}` (e.g. compute_gpus,
        // knowledge_stats, list_corpora) — Pydantic would then reject the
        // call with `unexpected_keyword_argument`. Empty `allowed` is the
        // valid case where the tool takes zero args, so retain() should
        // still drop everything the LLM hallucinated.
        args.retain(|k, _| allowed.contains(k));
        for req in &required {
            if !args.contains_key(req) {
                return false;
            }
        }
        true
    } else {
        // Args weren't an object — schema almost certainly wants one.
        // Wrap in empty object and check required.
        if !required.is_empty() {
            return false;
        }
        call.arguments = Value::Object(Default::default());
        true
    }
}

// ── Synthetic tool-call response (Stage 2.2) ─────────────────────────

/// Build an OpenAI streaming response that delivers a single tool_call as
/// if a chat LLM had emitted it. Used when FunctionGemma routes the query
/// locally — forge consumes this stream, dispatches the tool, and we never
/// pay a chat LLM round-trip for tool selection.
fn synthetic_tool_call_stream(
    call: prism_tool_router::ToolCall,
    model: String,
) -> impl Stream<Item = std::result::Result<Event, std::convert::Infallible>> {
    let id = format!("chatcmpl-{}", uuid_hex8());
    let call_id = format!("call_{}", uuid_hex8());
    let created = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let args_json = serde_json::to_string(&call.arguments).unwrap_or_else(|_| "{}".to_string());

    async_stream::stream! {
        // 1. role-only chunk
        yield Ok(Event::default().data(serde_json::to_string(&json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": { "role": "assistant" },
                "finish_reason": null,
            }],
        })).unwrap()));

        // 2. tool_call header (id, type, name, empty args)
        yield Ok(Event::default().data(serde_json::to_string(&json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": call_id,
                        "type": "function",
                        "function": { "name": call.name, "arguments": "" }
                    }]
                },
                "finish_reason": null,
            }],
        })).unwrap()));

        // 3. tool_call args body
        yield Ok(Event::default().data(serde_json::to_string(&json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "function": { "arguments": args_json }
                    }]
                },
                "finish_reason": null,
            }],
        })).unwrap()));

        // 4. finish
        yield Ok(Event::default().data(serde_json::to_string(&json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "tool_calls",
            }],
        })).unwrap()));

        yield Ok(Event::default().data("[DONE]"));
    }
}

/// Same as `synthetic_tool_call_stream` but as a single JSON response for
/// callers that asked for `stream:false`.
fn synthetic_tool_call_full(call: prism_tool_router::ToolCall, model: String) -> Value {
    let id = format!("chatcmpl-{}", uuid_hex8());
    let call_id = format!("call_{}", uuid_hex8());
    let created = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let args_json = serde_json::to_string(&call.arguments).unwrap_or_else(|_| "{}".to_string());
    json!({
        "id": id,
        "object": "chat.completion",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": call_id,
                    "type": "function",
                    "function": { "name": call.name, "arguments": args_json }
                }]
            },
            "finish_reason": "tool_calls",
        }],
        "usage": { "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0 },
    })
}

// ── SSE translation ──────────────────────────────────────────────────

fn sse_stream(
    resp: reqwest::Response,
    model: String,
) -> impl Stream<Item = std::result::Result<Event, std::convert::Infallible>> {
    let id = format!("chatcmpl-{}", uuid_hex8());
    let created = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let body = resp.bytes_stream();

    async_stream::stream! {
        // Emit an initial role-only chunk like OpenAI does.
        yield Ok(Event::default().data(serde_json::to_string(&json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": { "role": "assistant" },
                "finish_reason": null,
            }],
        })).unwrap()));

        let mut buf = String::new();
        let mut body = body;
        let mut sent_done = false;
        while let Some(chunk) = body.next().await {
            let bytes = match chunk {
                Ok(b) => b,
                Err(_) => break,
            };
            buf.push_str(&String::from_utf8_lossy(&bytes));

            while let Some(pos) = buf.find("\n\n") {
                let event_block: String = buf.drain(..pos + 2).collect();
                for line in event_block.lines() {
                    let payload = match line.strip_prefix("data: ") {
                        Some(rest) => rest.trim(),
                        None => continue,
                    };
                    if payload.is_empty() || payload == "[DONE]" {
                        continue;
                    }
                    let parsed: Value = match serde_json::from_str(payload) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    let delta_text = parsed
                        .get("delta")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let done = parsed.get("done").and_then(|v| v.as_bool()).unwrap_or(false);

                    if !delta_text.is_empty() {
                        yield Ok(Event::default().data(serde_json::to_string(&json!({
                            "id": id,
                            "object": "chat.completion.chunk",
                            "created": created,
                            "model": model,
                            "choices": [{
                                "index": 0,
                                "delta": { "content": delta_text },
                                "finish_reason": null,
                            }],
                        })).unwrap()));
                    }

                    if done {
                        yield Ok(Event::default().data(serde_json::to_string(&json!({
                            "id": id,
                            "object": "chat.completion.chunk",
                            "created": created,
                            "model": model,
                            "choices": [{
                                "index": 0,
                                "delta": {},
                                "finish_reason": "stop",
                            }],
                        })).unwrap()));
                        yield Ok(Event::default().data("[DONE]"));
                        sent_done = true;
                        break;
                    }
                }
                if sent_done { break; }
            }
            if sent_done { break; }
        }
        if !sent_done {
            yield Ok(Event::default().data("[DONE]"));
        }
    }
}

async fn collect_full(resp: reqwest::Response, model: &str) -> Result<Value> {
    let mut body = resp.bytes_stream();
    let mut text = String::new();
    let mut buf = String::new();
    let mut prompt_tokens: u64 = 0;
    let mut completion_tokens: u64 = 0;
    while let Some(chunk) = body.next().await {
        let bytes = chunk.context("stream chunk")?;
        buf.push_str(&String::from_utf8_lossy(&bytes));
        while let Some(pos) = buf.find("\n\n") {
            let event_block: String = buf.drain(..pos + 2).collect();
            for line in event_block.lines() {
                let payload = match line.strip_prefix("data: ") {
                    Some(rest) => rest.trim(),
                    None => continue,
                };
                if payload.is_empty() || payload == "[DONE]" {
                    continue;
                }
                let parsed: Value = match serde_json::from_str(payload) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if let Some(d) = parsed.get("delta").and_then(|v| v.as_str()) {
                    text.push_str(d);
                }
                if let Some(usage) = parsed.get("usage") {
                    if let Some(p) = usage.get("prompt_tokens").and_then(|v| v.as_u64()) {
                        prompt_tokens = prompt_tokens.max(p);
                    }
                    if let Some(c) = usage.get("completion_tokens").and_then(|v| v.as_u64()) {
                        completion_tokens = completion_tokens.max(c);
                    }
                }
            }
        }
    }

    let id = format!("chatcmpl-{}", uuid_hex8());
    let created = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Ok(json!({
        "id": id,
        "object": "chat.completion",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": text },
            "finish_reason": "stop",
        }],
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": prompt_tokens + completion_tokens,
        },
    }))
}

fn uuid_hex8() -> String {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:016x}", (nanos as u64) ^ ((nanos >> 64) as u64))
}

fn error_response(status: StatusCode, body: &str) -> Response {
    let json_body = json!({
        "error": {
            "message": body,
            "type": "platform_bridge_error",
        }
    });
    (status, Json(json_body)).into_response()
}

