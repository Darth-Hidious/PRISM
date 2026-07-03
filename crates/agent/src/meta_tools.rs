//! Native "meta-tools" — durable-memory and tool-discovery tools that operate
//! on the agent's own state (the Turso provenance store, the tool catalog)
//! rather than on the outside world. They are intercepted in the agent loop
//! before the command-tool / Python tool-server dispatch.
//!
//! Increment 3 ships `recall`: the model's window onto durable memory. Every
//! tool call's full input+output is persisted to Turso (see
//! `agent_loop::record_tool_provenance`); when a result is too large to keep
//! inline, or was dropped by compaction, `recall` pulls it back — by record
//! id (exact) or by query (semantic + keyword search within the session).
//! This replaces the old `peek_result` pointer, which referenced a
//! write-only in-memory map and a tool that never existed.

use anyhow::Result;
use serde_json::{Value, json};

use prism_embed::EmbedBackend;
use prism_provenance::ProvenanceStore;

use crate::permissions::PermissionMode;
use crate::tool_catalog::{LoadedTool, ToolCatalog};

/// Tool names handled natively by the meta-tool layer.
const META_TOOLS: &[&str] = &["recall", "find_tools"];

/// How many matches `recall(query)` returns by default.
const DEFAULT_RECALL_LIMIT: usize = 5;
/// Cap (chars) on a single recalled output echoed back to the model.
const RECALL_OUTPUT_CHARS: usize = 8_000;
/// Per-match preview length (chars) in a keyword search.
const RECALL_PREVIEW_CHARS: usize = 240;
/// Minimum cosine similarity for a semantic match. Measured on the native
/// BGE model: related sentences score ~0.8, unrelated ~0.3 — below this
/// floor a weak semantic hit must not displace an exact keyword match.
const SEMANTIC_SCORE_FLOOR: f32 = 0.4;
/// How many tools `find_tools(query)` returns by default.
const DEFAULT_FIND_TOOLS_LIMIT: usize = 8;
/// Per-match tool-description length (chars) in a discovery result.
const FIND_TOOLS_DESC_CHARS: usize = 400;

/// True if `tool_name` is handled by the native meta-tool layer.
#[must_use]
pub fn is_meta_tool(tool_name: &str) -> bool {
    META_TOOLS.contains(&tool_name)
}

/// Catalog entries for the meta-tools, so the model is offered them and
/// `validate_prepared_tool_calls_are_known` accepts them. Merged into the
/// catalog alongside the command tools.
#[must_use]
pub fn definitions() -> Vec<LoadedTool> {
    vec![
        LoadedTool {
            name: "recall".to_string(),
            description: "Retrieve earlier tool results from durable memory. Pass \
                `id` to fetch one specific result (e.g. the id printed when a large \
                result was truncated), or `query` to search this session's past \
                tool calls semantically (by meaning, when the local embedding \
                model is available) plus by keyword. Use this instead of \
                re-running a tool whose output you already produced but no \
                longer have in context."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "Exact provenance record id to fetch in full."
                    },
                    "query": {
                        "type": "string",
                        "description": "Search this session's past tool calls by meaning and keyword."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max matches for a keyword search (default 5)."
                    }
                }
            }),
            requires_approval: false,
            permission_mode: PermissionMode::ReadOnly,
            source: Some("builtin".to_string()),
            source_detail: Some("durable-memory".to_string()),
        },
        LoadedTool {
            name: "find_tools".to_string(),
            description: "Search the full tool catalog for tools relevant to a task \
                and make them available to call. Use this when you need a capability \
                that isn't already among your offered tools: describe what you want \
                to do (e.g. 'deploy a model', 'query the materials graph') and then \
                call a returned tool by name."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Describe the capability you need."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max tools to return (default 8)."
                    }
                },
                "required": ["query"]
            }),
            requires_approval: false,
            permission_mode: PermissionMode::ReadOnly,
            source: Some("builtin".to_string()),
            source_detail: Some("tool-discovery".to_string()),
        },
    ]
}

/// Execute a meta-tool. Returns the tool's result value; the caller wraps it
/// as `{ "result": ... }` to match the command-tool convention.
pub async fn execute_meta_tool(
    tool_name: &str,
    args: &Value,
    store: Option<&ProvenanceStore>,
    session_id: &str,
    catalog: &ToolCatalog,
) -> Result<Value> {
    match tool_name {
        "recall" => recall(args, store, session_id).await,
        "find_tools" => Ok(find_tools(args, catalog)),
        other => anyhow::bail!("unknown meta-tool '{other}'"),
    }
}

/// Discovery over the full catalog. Returns matching tools (name + clipped
/// description); the agent loop adds them to the model's working set so the
/// model can then call them by name.
fn find_tools(args: &Value, catalog: &ToolCatalog) -> Value {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if query.is_empty() {
        return json!({ "error": "find_tools requires a `query`" });
    }
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_FIND_TOOLS_LIMIT);

    let matches: Vec<Value> = catalog
        .search(query, limit)
        .into_iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": clip_str(&tool.description, FIND_TOOLS_DESC_CHARS),
            })
        })
        .collect();

    json!({
        "query": query,
        "count": matches.len(),
        "matches": matches,
        "hint": "these tools are now available — call one by name to use it",
    })
}

async fn recall(args: &Value, store: Option<&ProvenanceStore>, session_id: &str) -> Result<Value> {
    // Only an already-initialized backend: recall must never stall a turn on
    // model init. The background provenance tasks warm it up on first write,
    // so in practice it's ready long before the model asks to recall.
    let backend = crate::embeddings::backend_if_ready();
    recall_with_backend(args, store, session_id, backend.as_deref()).await
}

async fn recall_with_backend(
    args: &Value,
    store: Option<&ProvenanceStore>,
    session_id: &str,
    backend: Option<&dyn EmbedBackend>,
) -> Result<Value> {
    let Some(store) = store else {
        return Ok(json!({ "error": "durable memory is unavailable in this session" }));
    };

    // Exact id lookup wins when present.
    if let Some(id) = args
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        // `query_chain` starts at `id` and walks parents; the record itself
        // is included, so find it in the returned chain.
        let chain = store.query_chain(id).await?;
        return Ok(match chain.into_iter().find(|r| r.id == id) {
            Some(rec) => json!({
                "id": rec.id,
                "tool_name": rec.tool_name,
                "input": rec.input_json,
                "output": clip_value(rec.output_json),
            }),
            None => json!({ "error": format!("no record with id '{id}'") }),
        });
    }

    let query = args
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if query.is_empty() {
        return Ok(json!({ "error": "recall requires either `id` or `query`" }));
    }
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_RECALL_LIMIT);

    let mut matches = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // Semantic pass first — matches by meaning, no shared substring needed.
    // Best-effort: any failure just leaves the keyword pass to fill in.
    if let Some(backend) = backend {
        match backend.embed(std::slice::from_ref(&query)).await {
            Ok(vectors) => {
                if let Some(query_vec) = vectors.first() {
                    match store
                        .semantic_search(query_vec, Some(session_id), limit)
                        .await
                    {
                        Ok(hits) => {
                            for (rec, score) in hits {
                                if score < SEMANTIC_SCORE_FLOOR {
                                    break; // hits are sorted — the rest are weaker
                                }
                                let output_str = rec
                                    .output_json
                                    .as_ref()
                                    .map(ToString::to_string)
                                    .unwrap_or_default();
                                seen.insert(rec.id.clone());
                                matches.push(json!({
                                    "id": rec.id,
                                    "tool_name": rec.tool_name,
                                    "preview": clip_str(&output_str, RECALL_PREVIEW_CHARS),
                                    "score": format!("{score:.3}"),
                                }));
                            }
                        }
                        Err(e) => tracing::debug!("semantic recall failed: {e:#}"),
                    }
                }
            }
            Err(e) => tracing::debug!("recall query embedding failed: {e:#}"),
        }
    }

    // Newest-first keyword scan over this session's persisted tool calls —
    // fills remaining slots; the only pass when no embed backend is ready.
    let needle = query.to_lowercase();
    let records = store.query_by_session(session_id).await?;
    for rec in records.into_iter().rev() {
        if matches.len() >= limit {
            break;
        }
        if seen.contains(&rec.id) {
            continue; // already surfaced by the semantic pass
        }
        let output_str = rec
            .output_json
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        let haystack = format!(
            "{} {} {}",
            rec.tool_name.as_deref().unwrap_or(""),
            rec.input_json,
            output_str
        )
        .to_lowercase();
        if haystack.contains(&needle) {
            matches.push(json!({
                "id": rec.id,
                "tool_name": rec.tool_name,
                "preview": clip_str(&output_str, RECALL_PREVIEW_CHARS),
            }));
        }
    }
    matches.truncate(limit);

    Ok(json!({
        "query": query,
        "count": matches.len(),
        "matches": matches,
        "hint": "call recall with a returned id to get that result's full output",
    }))
}

/// Echo a stored output back to the model, preserving JSON structure when it
/// fits and clipping to a string when it would bloat the context.
fn clip_value(v: Option<Value>) -> Value {
    match v {
        None => Value::Null,
        Some(value) => {
            let serialized = value.to_string();
            if serialized.chars().count() <= RECALL_OUTPUT_CHARS {
                value
            } else {
                Value::String(clip_str(&serialized, RECALL_OUTPUT_CHARS))
            }
        }
    }
}

fn clip_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let head: String = s.chars().take(max_chars).collect();
    format!("{head}…[clipped]")
}

#[cfg(test)]
mod tests {
    use super::*;
    use prism_provenance::{ActionType, Actor, new_record};

    async fn seeded_store() -> (ProvenanceStore, String) {
        let store = ProvenanceStore::open(std::path::Path::new(":memory:"))
            .await
            .unwrap();
        let session = "sess-recall";
        let mut r1 = new_record(
            session,
            ActionType::ToolCall,
            Actor::Agent,
            Some("file"),
            None,
            json!({ "path": "alloy.csv" }),
        );
        r1.output_json = Some(json!("titanium aluminide rows: 42"));
        store.record(&r1).await.unwrap();
        let mut r2 = new_record(
            session,
            ActionType::ToolCall,
            Actor::Agent,
            Some("shell"),
            None,
            json!({ "cmd": "ls" }),
        );
        r2.output_json = Some(json!("a\nb"));
        store.record(&r2).await.unwrap();
        (store, r1.id)
    }

    #[test]
    fn is_meta_tool_recognizes_native_tools() {
        assert!(is_meta_tool("recall"));
        assert!(is_meta_tool("find_tools"));
        assert!(!is_meta_tool("file"));
        assert!(!is_meta_tool("peek_result"));
    }

    #[test]
    fn meta_tools_are_read_only_and_no_approval() {
        let defs = definitions();
        assert!(defs.iter().any(|t| t.name == "recall"));
        assert!(defs.iter().any(|t| t.name == "find_tools"));
        for t in &defs {
            assert_eq!(t.permission_mode, PermissionMode::ReadOnly);
            assert!(!t.requires_approval);
        }
    }

    fn catalog_with(names_and_descs: &[(&str, &str)]) -> ToolCatalog {
        let tools: Vec<Value> = names_and_descs
            .iter()
            .map(|(n, d)| {
                json!({
                    "name": n,
                    "description": d,
                    "input_schema": { "type": "object", "properties": {} }
                })
            })
            .collect();
        ToolCatalog::from_tool_server_json(&json!({ "tools": tools }))
    }

    #[test]
    fn find_tools_returns_relevant_matches() {
        let catalog = catalog_with(&[
            (
                "deploy_model",
                "Deploy a trained model to a serving endpoint",
            ),
            ("query_graph", "Query the materials knowledge graph"),
            ("send_email", "Send an email to a recipient"),
        ]);
        let out = find_tools(&json!({ "query": "deploy a model" }), &catalog);
        let names: Vec<&str> = out["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"deploy_model"));
        assert!(!names.contains(&"send_email"));
    }

    #[test]
    fn find_tools_requires_query() {
        let catalog = catalog_with(&[("x", "y")]);
        let out = find_tools(&json!({}), &catalog);
        assert!(out["error"].as_str().unwrap().contains("query"));
    }

    #[tokio::test]
    async fn recall_by_id_returns_full_record() {
        let (store, id) = seeded_store().await;
        let out = recall(&json!({ "id": id.clone() }), Some(&store), "sess-recall")
            .await
            .unwrap();
        assert_eq!(out["id"], json!(id));
        assert_eq!(out["tool_name"], json!("file"));
        assert_eq!(out["output"], json!("titanium aluminide rows: 42"));
    }

    #[tokio::test]
    async fn recall_by_query_finds_matches() {
        let (store, _) = seeded_store().await;
        let out = recall(&json!({ "query": "titanium" }), Some(&store), "sess-recall")
            .await
            .unwrap();
        assert_eq!(out["count"], json!(1));
        assert_eq!(out["matches"][0]["tool_name"], json!("file"));
    }

    #[tokio::test]
    async fn recall_requires_id_or_query() {
        let (store, _) = seeded_store().await;
        let out = recall(&json!({}), Some(&store), "sess-recall")
            .await
            .unwrap();
        assert!(out["error"].as_str().unwrap().contains("either"));
    }

    #[tokio::test]
    async fn recall_without_store_is_graceful() {
        let out = recall(&json!({ "query": "x" }), None, "sess-recall")
            .await
            .unwrap();
        assert!(out["error"].as_str().unwrap().contains("unavailable"));
    }

    /// Deterministic embed stub: axis 0 fires on titanium-ish words, axis 1
    /// on everything else — "Ti-6Al-4V" lands next to "titanium" without
    /// sharing a substring, which is exactly what semantic recall adds.
    struct TitaniumAxis;

    #[async_trait::async_trait]
    impl EmbedBackend for TitaniumAxis {
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|t| {
                    let t = t.to_lowercase();
                    if t.contains("titanium") || t.contains("ti-6al") {
                        vec![1.0, 0.0]
                    } else {
                        vec![0.0, 1.0]
                    }
                })
                .collect())
        }
        fn dimensions(&self) -> usize {
            2
        }
        fn id(&self) -> &str {
            "test:titanium-axis"
        }
    }

    #[tokio::test]
    async fn recall_merges_semantic_and_keyword_and_dedupes() {
        let (store, r1_id) = seeded_store().await;
        let backend = TitaniumAxis;

        // r3: semantically titanium-ish but shares no substring with the
        // query — only the semantic pass can surface it.
        let mut r3 = new_record(
            "sess-recall",
            ActionType::ToolCall,
            Actor::Agent,
            Some("gfn_sample"),
            None,
            json!({ "alloy": "Ti-6Al-4V" }),
        );
        r3.output_json = Some(json!("candidate accepted"));
        store.record(&r3).await.unwrap();

        // Embed r1 (keyword AND semantic match → must dedupe) and r3.
        for id in [&r1_id, &r3.id] {
            let recs = store.query_chain(id).await.unwrap();
            let rec = recs.into_iter().find(|r| &r.id == id).unwrap();
            store
                .embed_and_store(id, &prism_provenance::embedding_text(&rec), &backend)
                .await
                .unwrap();
        }

        let out = recall_with_backend(
            &json!({ "query": "titanium" }),
            Some(&store),
            "sess-recall",
            Some(&backend),
        )
        .await
        .unwrap();

        let matches = out["matches"].as_array().unwrap();
        let ids: Vec<&str> = matches.iter().map(|m| m["id"].as_str().unwrap()).collect();
        // Both surfaced, each exactly once (r1 hit both passes).
        assert!(ids.contains(&r1_id.as_str()));
        assert!(ids.contains(&r3.id.as_str()));
        assert_eq!(out["count"], json!(2));
        // Semantic hits come first and carry a score; both here are semantic.
        assert!(matches.iter().all(|m| m["score"].is_string()));
    }

    #[tokio::test]
    async fn recall_keyword_only_when_backend_missing() {
        let (store, _) = seeded_store().await;
        let out = recall_with_backend(
            &json!({ "query": "titanium" }),
            Some(&store),
            "sess-recall",
            None,
        )
        .await
        .unwrap();
        assert_eq!(out["count"], json!(1));
        assert_eq!(out["matches"][0]["tool_name"], json!("file"));
        // Keyword matches carry no semantic score.
        assert!(out["matches"][0]["score"].is_null());
    }
}
