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
//! Stage 2.2 (FunctionGemma local routing) was REMOVED — see the long
//! note in `proxy_chat_completions` for the silent-failure root cause.
//! The bridge now does retrieval only; the chat LLM does selection +
//! arg extraction + summary in one round.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use futures_util::stream::Stream;
use prism_tool_router::{ToolDef, ToolRouter};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::{RwLock, oneshot};

use crate::chat_config::ChatTarget;

#[derive(Clone)]
struct ProxyState {
    upstream_base: String, // e.g. "https://api.marc27.com/api/v1/projects/<pid>/llm"
    /// Bearer credential, wrapped for hot-swap. Phase 1(d) of the
    /// auth-fix arc: when an outside actor (`prism login` in another
    /// shell, the inline re-auth path in main.rs::Commands::Tui)
    /// refreshes the on-disk credentials, the fsevents-watcher task
    /// in forge_chat.rs updates this slot — the bridge picks up the
    /// new token on the next request without tearing down. Each
    /// handler reads at the top of the request via `.read().await`
    /// so an in-flight refresh doesn't tear an in-flight request.
    access_token: Arc<RwLock<String>>,
    http: reqwest::Client,
    /// Optional semantic tool router. When present, requests with a tools[]
    /// array are filtered to top-K=8 most relevant before forwarding to
    /// MARC27. When absent, fallback to the FIFO trim heuristic so the
    /// proxy still works without the router (e.g. model not yet downloaded).
    router: Option<Arc<ToolRouter>>,
    /// Live chat-target selection. Wrapped in `Arc<RwLock<…>>` so the
    /// in-chat `/use` slash command (and `prism use`) can hot-swap
    /// where chat turns get routed without tearing down the proxy.
    /// Each handler reads it at the top of the request, so in-flight
    /// streams keep using whatever target they started on; only the
    /// *next* turn picks up the swap.
    ///
    /// For now (this commit) we only honour `ChatTarget::Marc27 { model }`
    /// — when `model` is `Some(x)`, the bridge rewrites the outgoing
    /// `model` field on chat-completion requests so MARC27 serves
    /// the user-picked upstream (e.g. `gpt-5.5`) instead of whatever
    /// forge defaulted to. `Local` and `Provider` URL-resolution land
    /// in the follow-up commit; their requests still flow through
    /// `upstream_base` until then.
    chat_target: Arc<RwLock<ChatTarget>>,
}

/// Handle returned from `start`. Drop it (or call `shutdown()`) when the
/// chat session ends to stop the proxy task.
///
/// Carries an `Arc<RwLock<ChatTarget>>` so the in-chat `/use` slash
/// command can swap chat targets at runtime: it grabs `chat_target()`,
/// writes the new value, and the bridge picks it up on the next turn.
pub struct ProxyHandle {
    pub url: String, // base URL forge should hit (proxy_url + "/v1")
    /// Hot-swap target for the in-chat `/use` slash command. Not yet
    /// wired into the bridge's per-turn read path; retained so the
    /// `/use` handler can populate it without a struct reshape.
    #[allow(dead_code)]
    chat_target: Arc<RwLock<ChatTarget>>,
    /// Hot-swappable Bearer credential. Shares the same RwLock the
    /// bridge ProxyState reads from per-request — external callers
    /// (the fsevents creds-watcher in forge_chat.rs) can update via
    /// `update_access_token` and the bridge picks it up on the next
    /// turn without restart. Phase 1(d) auth-fix.
    access_token: Arc<RwLock<String>>,
    shutdown: Option<oneshot::Sender<()>>,
}

impl ProxyHandle {
    /// Explicit shutdown — alternative to letting the handle Drop. Both
    /// paths fire the oneshot. Public API kept for callers that want to
    /// shut the proxy down without dropping the handle.
    #[allow(dead_code)]
    pub fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }

    /// Replace the Bearer credential the bridge uses for upstream calls.
    /// Used by the fsevents creds-watcher in forge_chat.rs when an
    /// outside actor (`prism login` in another shell, or the inline
    /// reauth path in Commands::Tui) refreshes the on-disk creds. The
    /// bridge picks up the new token on the next request — no
    /// listener teardown, no chat-session restart.
    #[allow(dead_code)]
    pub async fn update_access_token(&self, new_token: &str) {
        let mut guard = self.access_token.write().await;
        *guard = new_token.to_string();
    }

    /// Hand out the live access-token lock. The fsevents creds-watcher
    /// task in forge_chat.rs holds a clone for the lifetime of the
    /// TUI session and writes through it on state.json changes.
    pub fn access_token_lock(&self) -> Arc<RwLock<String>> {
        self.access_token.clone()
    }

    /// Hand out the live `ChatTarget` lock so callers (the `/use` slash
    /// command, in particular) can hot-swap the chat upstream without
    /// restarting the proxy. The lock is the single source of truth
    /// the bridge reads at the top of every chat-completion handler.
    #[allow(dead_code)]
    pub fn chat_target(&self) -> Arc<RwLock<ChatTarget>> {
        self.chat_target.clone()
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
    initial_chat_target: ChatTarget,
) -> Result<ProxyHandle> {
    let upstream_base = format!(
        "{}/api/v1/projects/{}/llm",
        platform_url.trim_end_matches('/'),
        project_id
    );

    let chat_target = Arc::new(RwLock::new(initial_chat_target));
    // Hot-swap slot for the Bearer credential — shared by reference
    // between ProxyState (axum-side read at request time) and
    // ProxyHandle (external write via update_access_token, used by
    // the fsevents creds-watcher in forge_chat.rs).
    let access_token = Arc::new(RwLock::new(access_token.to_string()));

    let state = ProxyState {
        upstream_base,
        access_token: access_token.clone(),
        http: reqwest::Client::builder()
            // SSE streams can run for minutes — let the upstream control timing.
            .timeout(std::time::Duration::from_secs(600))
            .build()
            .context("build reqwest client")?,
        router,
        chat_target: chat_target.clone(),
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

    Ok(ProxyHandle {
        url,
        chat_target,
        access_token,
        shutdown: Some(tx),
    })
}

// ── Routes ───────────────────────────────────────────────────────────

/// `GET /v1/models` — translate MARC27's flat array into OpenAI's
/// `{ "object": "list", "data": [{ id, object, created, owned_by }, …] }`.
async fn list_models(State(state): State<Arc<ProxyState>>) -> Response {
    let upstream = format!("{}/models", state.upstream_base);
    // Read the hot-swappable token at request time. If the
    // fsevents creds-watcher in forge_chat.rs just wrote a fresh
    // token, this request uses it — no listener teardown needed.
    let token = state.access_token.read().await.clone();
    let res = state.http.get(&upstream).bearer_auth(&token).send().await;

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
            let id = m
                .get("model_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
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
        serde_json::to_vec(req)
            .map(|b| b.len())
            .unwrap_or(usize::MAX)
    };
    while size > MARC27_BODY_BUDGET && !kept.is_empty() {
        kept.pop();
        req["tools"] = Value::Array(kept.clone());
        size = serde_json::to_vec(req)
            .map(|b| b.len())
            .unwrap_or(usize::MAX);
    }
    if kept.len() != original {
        // Bug #30b: was eprintln! → leaked into the chat panel on
        // every turn. Demote to tracing::debug! so operators on
        // RUST_LOG=debug still see it but the user doesn't.
        tracing::debug!(
            target: "platform_bridge",
            original_tools = original,
            kept_tools = kept.len(),
            body_bytes = size,
            budget = MARC27_BODY_BUDGET,
            "FIFO fallback trim"
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
        serde_json::to_vec(req)
            .map(|b| b.len())
            .unwrap_or(usize::MAX)
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
            // Loud warning — this is a real bailout, operators want to know.
            // Still tracing rather than eprintln! so it goes to logs not the
            // user-facing chat panel (Bug #30b).
            tracing::warn!(
                target: "platform_bridge",
                body_bytes = size,
                budget = MARC27_BODY_BUDGET,
                "truncate_oversized_messages: gave up at iteration 50"
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
            if let Some(c) = msg.get("content").and_then(|v| v.as_str())
                && c.len() > max_len
            {
                max_len = c.len();
                idx = Some(i);
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
        // Bug #30b: was eprintln! → leaked into the chat panel on
        // turns where the body had to be truncated. Demote to debug
        // (operators can see it via RUST_LOG=debug).
        tracing::debug!(
            target: "platform_bridge",
            truncated_bytes = total_truncated_bytes,
            iterations = iterations,
            budget = MARC27_BODY_BUDGET,
            "truncated body to fit budget"
        );
    }
}

/// `POST /v1/chat/completions` — accept OpenAI request, forward to MARC27's
/// `/stream`, translate SSE deltas into OpenAI delta chunks.
async fn chat_completions(State(state): State<Arc<ProxyState>>, body: Bytes) -> Response {
    tracing::debug!(target: "platform_bridge", body_len = body.len(), "chat/completions hit");
    let mut req: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return error_response(StatusCode::BAD_REQUEST, &format!("invalid JSON body: {e}"));
        }
    };

    // Honour the user's chat-target preference. Right now this only
    // rewrites the `model` field for `ChatTarget::Marc27 { model: Some(x) }`
    // — everything else still flows to MARC27 over `upstream_base`.
    // Local/Provider URL routing lands in the follow-up commit; for
    // now, picking a model still requires `prism use marc27 --model x`
    // so the user controls *which* model MARC27 serves.
    {
        let target = state.chat_target.read().await.clone();
        if let ChatTarget::Marc27 {
            model: Some(picked),
        } = target
        {
            // Forge always sends a `model` field (the one in
            // .forge.toml's ModelConfig). Override it so MARC27 picks
            // the user's chosen upstream (e.g. `gpt-5.5`).
            if let Some(obj) = req.as_object_mut() {
                obj.insert("model".to_string(), Value::String(picked));
            }
        }
    }

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

        if let (Some(tools), Some(query), Some(router)) = (
            tools_owned.as_ref(),
            last_user_msg.as_ref(),
            state.router.as_ref(),
        ) {
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
                    let description = f
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let args_schema = f.get("parameters").cloned().unwrap_or(json!({}));
                    Some(ToolDef {
                        name,
                        description,
                        args_schema,
                    })
                })
                .collect();
            let names: Vec<String> = defs.iter().map(|d| d.name.clone()).collect();
            if let Err(e) = router.index_tools(&defs).await {
                tracing::warn!(target: "platform_bridge", error = %e, "tool index update failed");
            }

            match router.search(query, &names, TOP_K).await {
                Ok(top) => {
                    let mut keep_set: std::collections::HashSet<String> = top.into_iter().collect();
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
                        let final_size = serde_json::to_vec(&req).map(|b| b.len()).unwrap_or(0);
                        // Bug #30b: was eprintln! → fired EVERY turn,
                        // dumping operational debug info ('semantic
                        // top-K: 149 → 12 tools (body 39859 bytes)')
                        // straight into the user-facing chat panel.
                        // Demote to tracing::debug! — operators on
                        // RUST_LOG=debug still see it.
                        tracing::debug!(
                            target: "platform_bridge",
                            original_tools = original,
                            kept_tools = kept_count,
                            body_bytes = final_size,
                            "semantic top-K trim"
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

    // Stage 2.2 (FunctionGemma local routing) was REMOVED.
    //
    // Why: a 270M local router that picked the tool AND invented args was
    // wrong on both counts. It conflated "which tool" with "what arguments"
    // — two problems with different right answers — and shipped a black-box
    // decision the user couldn't introspect. In production this manifested
    // as a silent-failure bug: when the router picked a Python MCP tool,
    // the synthesised tool_call response routed through the agent loop, the
    // tool returned, and the second LLM round produced empty content, so
    // the user saw a blank screen for materials questions like
    // "what is Inconel 718".
    //
    // The new architecture: Stage 2.1 (semantic top-K retrieval, above)
    // narrows 125 tools → ~13 relevant ones. Those go to the chat LLM
    // (Gemini / GPT-4 / Claude) as standard OpenAI tools. Frontier chat
    // LLMs are state-of-the-art at tool selection AND argument extraction,
    // and they always render a natural-language answer after the tool
    // result — no "skip chat LLM" path means no silent-failure class of
    // bugs. Marketplace-native: a new tool ships → embed its description →
    // it's in the cosine pool. No fine-tune needed.
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

    // Resolve where this chat turn actually goes. The bridge's three
    // first-class targets all reach this point:
    //   - Marc27 → MARC27 platform proxy (existing path, custom SSE)
    //   - Local → user-supplied OpenAI-compatible URL (Ollama, llama.cpp,
    //     vLLM, friend's PRISM node, …). Standard OpenAI shape.
    //   - Provider → direct vendor with the user's OWN key. For
    //     OpenAI-compat vendors (openai, mistral, gemini-openai-shim,
    //     cohere) we resolve to a known URL and pass through. For
    //     Anthropic — which uses a non-OpenAI-shape /v1/messages
    //     endpoint — we surface a clear "not yet supported, use
    //     `prism use marc27 --model claude-…` instead" rather than
    //     silently mis-routing.
    //
    // Marc27 stays on the SSE-reshape path (`sse_stream`); Local /
    // Provider get their response piped through more directly because
    // they already emit OpenAI-shape deltas. We branch *after* the
    // upstream call so the retry logic + visible-failure detector
    // still wrap every path.
    enum UpstreamShape {
        Marc27Sse,
        OpenAiCompat,
    }
    let (upstream, bearer, shape): (String, Option<String>, UpstreamShape) = {
        let target = state.chat_target.read().await.clone();
        match target {
            ChatTarget::Marc27 { .. } => (
                format!("{}/stream", state.upstream_base),
                // Hot-swappable Bearer (Phase 1d): read fresh per request.
                Some(state.access_token.read().await.clone()),
                UpstreamShape::Marc27Sse,
            ),
            ChatTarget::Local { url, api_key, .. } => (
                format!("{}/chat/completions", url.trim_end_matches('/')),
                api_key.filter(|k| !k.is_empty()),
                UpstreamShape::OpenAiCompat,
            ),
            ChatTarget::Provider {
                provider,
                api_key_env,
                ..
            } => {
                let provider_lc = provider.to_ascii_lowercase();
                if provider_lc == "anthropic" {
                    return error_response(
                        StatusCode::NOT_IMPLEMENTED,
                        "Anthropic direct-provider mode is not yet supported by the chat bridge. \
                         Use `prism use marc27 --model claude-sonnet-4` (or another claude-*) to \
                         route through MARC27 instead, which proxies Anthropic with the platform's \
                         own key. Direct Anthropic support will land once we wire the /v1/messages \
                         shape.",
                    );
                }
                let env_name = api_key_env
                    .unwrap_or_else(|| ChatTarget::default_api_key_env(&provider_lc).to_string());
                let key = match std::env::var(&env_name) {
                    Ok(k) if !k.is_empty() => k,
                    _ => {
                        return error_response(
                            StatusCode::UNAUTHORIZED,
                            &format!(
                                "Provider chat target needs `{env_name}` env var. Set it and \
                                 retry, or `prism use reset` to go back to MARC27."
                            ),
                        );
                    }
                };
                let url = match provider_lc.as_str() {
                    "openai" => "https://api.openai.com/v1/chat/completions".to_string(),
                    "mistral" => "https://api.mistral.ai/v1/chat/completions".to_string(),
                    "gemini" | "google" => {
                        "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions"
                            .to_string()
                    }
                    "cohere" => {
                        "https://api.cohere.ai/compatibility/v1/chat/completions".to_string()
                    }
                    other => {
                        return error_response(
                            StatusCode::NOT_IMPLEMENTED,
                            &format!(
                                "Unknown provider `{other}`. Supported direct-provider slugs: \
                                 openai, mistral, gemini, cohere. For others, use \
                                 `prism use local --url <openai-compatible-url>` instead."
                            ),
                        );
                    }
                };
                (url, Some(key), UpstreamShape::OpenAiCompat)
            }
        }
    };

    // Defensive retry on transient 5xx. The marc27-core retry helper lives
    // server-side and currently covers 429/502/503/504, but Cloudflare in
    // front of MARC27 occasionally serves 520-524 (origin connection drops,
    // handshake timeouts, "unknown error") which fall through to PRISM as
    // a hard error. Without a client-side retry, forge would log a tracing
    // event and the TUI would render an empty turn — that's the silent-
    // failure bug we hit on materials questions. Retry the *initial* POST
    // up to 4 times with exponential backoff (500ms → 1s → 2s, capped 4s);
    // once the SSE stream has started, mid-stream failures fall through to
    // forge's own retry layer.
    let resp = {
        const RETRYABLE_STATUS: &[u16] = &[429, 502, 503, 504, 520, 521, 522, 523, 524];
        const MAX_ATTEMPTS: u32 = 4;
        let mut last: Option<reqwest::Response> = None;
        let mut last_err: Option<reqwest::Error> = None;
        // If we detect a *permanent* 429 (plan / quota / billing — i.e. a
        // 429 that no number of retries will clear) we capture the upstream
        // body here, break out of the retry loop, and return a friendly
        // user-facing message at the bottom of this block instead of
        // hammering the upstream four more times.
        let mut permanent_429_body: Option<String> = None;
        for attempt in 1..=MAX_ATTEMPTS {
            let mut request = state
                .http
                .post(&upstream)
                .header("Accept", "text/event-stream")
                .json(&req);
            // Auth header: MARC27 + OpenAI/Mistral/Gemini all use Bearer.
            // Some local servers (Ollama with no auth) want NO header,
            // hence Option<String>. (Anthropic's x-api-key shape
            // diverges and is short-circuited above with NOT_IMPLEMENTED.)
            if let Some(ref token) = bearer {
                request = request.bearer_auth(token);
            }
            match request.send().await {
                Ok(r) => {
                    let s = r.status().as_u16();
                    if r.status().is_success()
                        || !RETRYABLE_STATUS.contains(&s)
                        || attempt == MAX_ATTEMPTS
                    {
                        last = Some(r);
                        break;
                    }
                    // Permanent 429 short-circuit. MARC27 returns 429 in
                    // two distinct flavours: (a) genuine rate-limit /
                    // upstream congestion (retry helps), and (b) plan-
                    // level quota / billing exceeded (retry never helps,
                    // and burning four attempts just spams the user with
                    // duplicate error lines from concurrent chat +
                    // metadata requests). For (b) we read the body once,
                    // stash it for the friendly message below, and exit
                    // the retry loop early.
                    if s == 429 {
                        let body = r.text().await.unwrap_or_default();
                        let body_lower = body.to_lowercase();
                        let is_permanent = body_lower.contains("quota_exceeded")
                            || body_lower.contains("plan_exceeded")
                            || body_lower.contains("monthly llm token quota")
                            || body_lower.contains("billing");
                        if is_permanent {
                            permanent_429_body = Some(body);
                            break;
                        }
                        // Transient 429 — fall through to the existing
                        // retry-log + backoff path. Body was consumed,
                        // but since we're going to retry we don't need
                        // to surface this response.
                        eprintln!(
                            "\x1b[33m[prism]\x1b[0m MARC27 returned 429 (transient), retrying (attempt {attempt}/{MAX_ATTEMPTS})"
                        );
                    } else {
                        eprintln!(
                            "\x1b[33m[prism]\x1b[0m MARC27 returned {s}, retrying (attempt {attempt}/{MAX_ATTEMPTS})"
                        );
                    }
                    let backoff_ms = 500u64 * (1 << (attempt - 1)).min(8);
                    tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                }
                Err(e) => {
                    last_err = Some(e);
                    if attempt == MAX_ATTEMPTS {
                        break;
                    }
                    let backoff_ms = 500u64 * (1 << (attempt - 1)).min(8);
                    tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                }
            }
        }
        // Permanent 429 → return a friendly, actionable response instead
        // of letting forge_main's chat layer render the raw upstream JSON
        // (which the user can't act on without grepping the docs).
        if let Some(body) = permanent_429_body {
            let body_lower = body.to_lowercase();
            let msg = if body_lower.contains("quota_exceeded")
                || body_lower.contains("monthly llm token quota")
            {
                "MARC27 monthly LLM token quota exceeded. To keep working immediately, switch chat to a local or BYOK target:\n\
                 \n\
                 - Local OpenAI-compatible:  prism use local --url http://127.0.0.1:8080 --model <name>\n\
                 - BYOK provider:            prism use provider <openai|mistral|gemini|cohere> --model <name>\n\
                 - Or upgrade your plan at https://marc27.com/billing\n\
                 \n\
                 Knowledge graph, marketplace, and tool calls keep working regardless of which chat target you pick."
            } else {
                "MARC27 plan/billing limit reached. Visit https://marc27.com/billing or switch routing with `prism use local|provider`."
            };
            return error_response(StatusCode::PAYMENT_REQUIRED, msg);
        }
        match (last, last_err) {
            (Some(r), _) => r,
            (None, Some(e)) => {
                return error_response(StatusCode::BAD_GATEWAY, &format!("upstream: {e}"));
            }
            (None, None) => {
                return error_response(StatusCode::BAD_GATEWAY, "upstream: no response");
            }
        }
    };

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let body_lower = body.to_lowercase();

        // Auth-expired interceptor: convert raw 401 / "unauthorized" /
        // "expired refresh token" into a friendly chat-embedded message
        // instead of leaking a 4xx that breaks the user's flow. The next
        // iteration will attempt silent refresh and inline device-flow
        // pickup; for now, give the user a clear actionable instruction
        // so they're not staring at `{"error":"unauthorized:..."}`.
        let is_auth_error = status == StatusCode::UNAUTHORIZED
            || body_lower.contains("unauthorized")
            || body_lower.contains("expired refresh token")
            || body_lower.contains("invalid token");
        if is_auth_error {
            eprintln!(
                "\x1b[33m[prism]\x1b[0m MARC27 auth expired ({status}) — \
                 converting to in-chat auth prompt"
            );
            let msg = "Your MARC27 session has expired.\n\
                       \n\
                       To continue:\n\
                       \n\
                       1. Open a new terminal\n\
                       2. Run: `prism login`\n\
                       3. Approve in the browser\n\
                       4. Send your message again — the new session loads automatically.\n\
                       \n\
                       (Inline device-flow pickup is on the way — for now, the relogin step is a side trip.)";
            if stream_requested {
                return Sse::new(synthetic_text_stream(msg.to_string(), model.clone()))
                    .into_response();
            } else {
                return Json(synthetic_text_full(msg.to_string(), model.clone())).into_response();
            }
        }

        eprintln!(
            "\x1b[31m[prism]\x1b[0m MARC27 returned {status}: {}",
            body.lines().next().unwrap_or("")
        );

        // Visible-failure converter: surface MARC27 5xx as a real chat
        // message instead of letting forge's tracing-only retry path
        // render a blank turn. This is the silent-failure mitigation —
        // when retries above are exhausted on transient 5xx, the user
        // SEES what happened and what to do, not an empty screen.
        if status.is_server_error() {
            let msg = format!(
                "MARC27 platform briefly unavailable (HTTP {status}).\n\n\
                 Retried {} times before giving up. This is usually a transient \
                 Cloudflare or origin issue — try the same message again in a \
                 moment.\n\n\
                 If it keeps happening, check status: https://status.marc27.com",
                4u32
            );
            if stream_requested {
                return Sse::new(synthetic_text_stream(msg, model.clone())).into_response();
            } else {
                return Json(synthetic_text_full(msg, model.clone())).into_response();
            }
        }

        return error_response(
            StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
            &body,
        );
    }

    match shape {
        UpstreamShape::Marc27Sse => {
            // MARC27's custom JSON+SSE → OpenAI delta reshape (existing
            // path; handles tool_calls forwarding + silent-failure
            // detection + retry-exhausted message).
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
        UpstreamShape::OpenAiCompat => {
            // Local + direct-provider OpenAI-compat upstreams already
            // emit the OpenAI delta format forge expects. Pipe the
            // bytes through unchanged — no reshape, no tool_calls
            // injection, no silent-failure detector (those exist to
            // patch MARC27's response normalizer; OpenAI-compat
            // sources don't need them).
            if stream_requested {
                let body = resp.bytes_stream();
                use axum::body::Body;
                let body = Body::from_stream(body);
                axum::response::Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "text/event-stream")
                    .header("cache-control", "no-cache")
                    .body(body)
                    .unwrap_or_else(|_| {
                        error_response(StatusCode::INTERNAL_SERVER_ERROR, "build response failed")
                    })
            } else {
                // Non-streaming JSON passthrough.
                match resp.bytes().await {
                    Ok(bytes) => axum::response::Response::builder()
                        .status(StatusCode::OK)
                        .header("content-type", "application/json")
                        .body(axum::body::Body::from(bytes))
                        .unwrap_or_else(|_| {
                            error_response(
                                StatusCode::INTERNAL_SERVER_ERROR,
                                "build response failed",
                            )
                        }),
                    Err(e) => {
                        error_response(StatusCode::BAD_GATEWAY, &format!("read upstream body: {e}"))
                    }
                }
            }
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
///
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
                let id = c
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
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
                let tool_name = id_to_name
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| "tool".into());
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
            "assistant" if msg.get("tool_calls").is_some() => {
                {
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

// ── Synthetic plain-text response ────────────────────────────────────

/// Build a streaming OpenAI chat-completion response that delivers a single
/// plain-text assistant message. Used to convert MARC27-side errors (auth
/// expired, etc.) into in-chat messages the user actually reads, instead of
/// leaking 4xx codes through forge.
fn synthetic_text_stream(
    text: String,
    model: String,
) -> impl Stream<Item = std::result::Result<Event, std::convert::Infallible>> {
    let id = format!("chatcmpl-{}", uuid_hex8());
    let created = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    async_stream::stream! {
        // 1. role chunk
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

        // 2. content chunk (single delivery; no need to chunk further)
        yield Ok(Event::default().data(serde_json::to_string(&json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": { "content": text },
                "finish_reason": null,
            }],
        })).unwrap()));

        // 3. finish chunk
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

        // 4. terminator
        yield Ok(Event::default().data("[DONE]"));
    }
}

/// Non-streaming variant — single completion object with the full text.
fn synthetic_text_full(text: String, model: String) -> Value {
    let id = format!("chatcmpl-{}", uuid_hex8());
    let created = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    json!({
        "id": id,
        "object": "chat.completion",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": text,
            },
            "finish_reason": "stop",
        }],
        "usage": {
            "prompt_tokens": 0,
            "completion_tokens": 0,
            "total_tokens": 0,
        },
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
        // Tracks how many real content characters the upstream actually
        // sent to us. Used by the silent-failure detector below to decide
        // whether `completion_tokens > 0` + empty deltas means MARC27
        // dropped tool_calls.
        let mut total_emitted_chars: usize = 0;
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
                    if std::env::var("PRISM_BRIDGE_DUMP").is_ok() {
                        eprintln!(
                            "[platform_bridge:dump] {}",
                            payload.chars().take(500).collect::<String>()
                        );
                    }
                    let delta_text = parsed
                        .get("delta")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let done = parsed.get("done").and_then(|v| v.as_bool()).unwrap_or(false);

                    // Forward upstream tool_calls verbatim. After
                    // marc27-core PR #2, MARC27's StreamChunk carries a
                    // `tool_calls` array each chunk (default-empty for
                    // plain text). Translate that into the OpenAI
                    // streaming shape forge expects:
                    //   delta: { tool_calls: [...] }
                    if let Some(tc) = parsed
                        .get("tool_calls")
                        .and_then(|v| v.as_array())
                        .filter(|a| !a.is_empty())
                    {
                        yield Ok(Event::default().data(serde_json::to_string(&json!({
                            "id": id,
                            "object": "chat.completion.chunk",
                            "created": created,
                            "model": model,
                            "choices": [{
                                "index": 0,
                                "delta": { "tool_calls": tc },
                                "finish_reason": null,
                            }],
                        })).unwrap()));
                        total_emitted_chars += 1; // count tool_call chunks toward "real output"
                    }

                    // Silent-failure detector. Kept as defence-in-depth
                    // for the case where an upstream model emits content
                    // we still drop. With marc27-core PR #2 deployed, the
                    // tool_calls path above prevents the original signature
                    // (completion_tokens > 0 + every delta empty) from
                    // ever triggering on tool turns. If it ever fires
                    // again, that's a regression worth surfacing.
                    if done {
                        let completion_tokens = parsed
                            .get("usage")
                            .and_then(|u| u.get("completion_tokens"))
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        if completion_tokens > 0 && total_emitted_chars == 0 {
                            let warn = "\n\n\
                                ⚠️  PRISM detected dropped output from the platform.\n\n\
                                The upstream LLM generated content but it arrived blank \
                                — likely a regression in the platform's response \
                                normalizer. Try the same message again, or switch \
                                models with /model.";
                            yield Ok(Event::default().data(serde_json::to_string(&json!({
                                "id": id,
                                "object": "chat.completion.chunk",
                                "created": created,
                                "model": model,
                                "choices": [{
                                    "index": 0,
                                    "delta": { "content": warn },
                                    "finish_reason": null,
                                }],
                            })).unwrap()));
                        }
                    }

                    if !delta_text.is_empty() {
                        total_emitted_chars += delta_text.len();
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
