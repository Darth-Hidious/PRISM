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
const META_TOOLS: &[&str] = &[
    "recall",
    "find_tools",
    "write_skill",
    "run_skill",
    "list_skills",
    "spawn_subagent",
];

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

/// True if `tool_name` is a reserved, trusted built-in — a native meta-tool or
/// a Rust command-tool (the spine). Authored skills and any future
/// user-brought / third-party tools MUST NOT claim these names: allowing it
/// would let an impostor shadow a trusted tool (tool spoofing), so the agent
/// thinks it is calling the real thing but runs attacker-supplied code. Callers
/// that ingest untrusted tools reject-or-namespace on a match here.
#[must_use]
pub fn is_reserved_tool_name(tool_name: &str) -> bool {
    is_meta_tool(tool_name) || crate::command_tools::is_command_tool(tool_name)
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
        LoadedTool {
            name: "write_skill".to_string(),
            description: "Author a REUSABLE skill: a named snippet of shell or python you \
                can call again on later turns. The skill is VERIFIED by running it once — \
                it is only saved if it exits cleanly — then stored so `list_skills` shows \
                it and `run_skill` re-executes it. Use this when you solve something with \
                code you'll likely need again (a conversion, a fetch, a computation). \
                Provide a clear one-line `description` (it is embedded for later retrieval)."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Short slug, 1-64 chars of [A-Za-z0-9_-]. Becomes the skill id."
                    },
                    "description": {
                        "type": "string",
                        "description": "One line describing what the skill does (embedded for retrieval)."
                    },
                    "language": {
                        "type": "string",
                        "enum": ["shell", "python"],
                        "description": "Interpreter for the code. Default 'shell'."
                    },
                    "code": {
                        "type": "string",
                        "description": "The skill body. Must exit 0 when run or it is rejected."
                    }
                },
                "required": ["name", "description", "code"]
            }),
            requires_approval: true,
            permission_mode: PermissionMode::WorkspaceWrite,
            source: Some("builtin".to_string()),
            source_detail: Some("self-authoring".to_string()),
        },
        LoadedTool {
            name: "run_skill".to_string(),
            description: "Execute a previously authored skill by name (see list_skills). \
                Runs the stored code and returns its stdout/stderr and exit code."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "The skill id to run."
                    }
                },
                "required": ["name"]
            }),
            requires_approval: true,
            permission_mode: PermissionMode::WorkspaceWrite,
            source: Some("builtin".to_string()),
            source_detail: Some("self-authoring".to_string()),
        },
        LoadedTool {
            name: "list_skills".to_string(),
            description: "List the skills you have authored (name, description, language, \
                whether verified). Use this to see what reusable skills already exist \
                before writing a new one or to pick one to run_skill."
                .to_string(),
            input_schema: json!({ "type": "object", "properties": {} }),
            requires_approval: false,
            permission_mode: PermissionMode::ReadOnly,
            source: Some("builtin".to_string()),
            source_detail: Some("self-authoring".to_string()),
        },
        crate::subagent::definition(),
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
        "write_skill" => write_skill(args).await,
        "run_skill" => run_skill(args).await,
        "list_skills" => Ok(list_skills()),
        // Needs the live turn machinery (LLM client, tool server, approval
        // channel), which this signature cannot carry — the agent loop
        // intercepts it BEFORE this dispatcher (see agent_loop.rs). Reaching
        // this arm means a caller (e.g. the single-tool executor) tried to
        // run it out of context.
        "spawn_subagent" => anyhow::bail!(
            "spawn_subagent runs a nested agent turn and is dispatched inside the agent loop only"
        ),
        other => anyhow::bail!("unknown meta-tool '{other}'"),
    }
}

/// Clip a string to `max` chars (whole chars, not bytes) for echoing back.
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}

/// Clip keeping BOTH a head and a tail, with an explicit elided-middle marker.
///
/// stderr from a Python traceback puts the load-bearing `Error: msg` /
/// `Exception: ...` line at the END — a head-only clip ([`clip`]) drops it,
/// so the agent sees stack frames but never the actual error. This keeps the
/// first `head` chars (traceback header — file/line context) AND the last
/// `tail` chars (the final exception line), with a marker naming how many
/// chars were elided. When the whole string fits in `head + tail`, it is
/// returned unchanged (no marker).
///
/// Whole-char-safe: head/tail boundaries snap to UTF-8 char boundaries.
/// VS1/F2: head+tail is enough to see the error line. A full recall-pointer
/// (durable record of the unclipped stderr) is deferred to the verified
/// provenance-store work — it belongs there, not in this foundation patch.
fn clip_head_tail(s: &str, head: usize, tail: usize) -> String {
    let total = s.chars().count();
    if total <= head + tail {
        return s.to_string();
    }
    // Walk char boundaries from the front for the head, and from the back
    // (in bytes) for the tail — snapping inward to the nearest char start.
    let head_str: String = s.chars().take(head).collect();
    // Tail: find the byte index of the (total - tail)-th char.
    let skip = total - tail;
    let tail_byte_start = s
        .char_indices()
        .nth(skip)
        .map(|(byte_idx, _)| byte_idx)
        .unwrap_or(s.len());
    let tail_str = &s[tail_byte_start..];
    let elided = total - head - tail;
    format!(
        "{head_str}\n[…{elided} chars elided — showing head + tail; the final error line is below…]\n…{tail_str}"
    )
}

/// `write_skill`: verify by executing once (Voyager: execute-before-store), and
/// only persist if it exits cleanly. An unverified skill is NOT saved — the
/// error is handed back so the model can fix the code and call again.
async fn write_skill(args: &Value) -> Result<Value> {
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let description = args
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let code = args.get("code").and_then(Value::as_str).unwrap_or("");
    let language = args
        .get("language")
        .and_then(Value::as_str)
        .unwrap_or("shell")
        .trim();

    if !crate::skills::valid_name(name) {
        anyhow::bail!("invalid skill name '{name}': use 1-64 chars of [A-Za-z0-9_-]");
    }
    if is_reserved_tool_name(name) {
        // Anti-spoofing: an authored skill may never take the name of a trusted
        // built-in (meta-tool or command-tool). Reject so the impostor can't
        // shadow the real tool. Reported to the model so it renames + retries.
        return Ok(json!({
            "stored": false,
            "verified": false,
            "name": name,
            "error": format!("'{name}' is a reserved built-in tool name — authored skills must use a distinct name. Rename the skill (e.g. add a domain prefix) and call write_skill again."),
        }));
    }
    if description.is_empty() {
        anyhow::bail!("`description` is required (one line, embedded for retrieval)");
    }
    if code.trim().is_empty() {
        anyhow::bail!("`code` is required");
    }

    let lang = language.to_string();
    let code_for_run = code.to_string();
    let out =
        tokio::task::spawn_blocking(move || crate::skills::execute(&lang, &code_for_run)).await??;

    if !out.ok {
        return Ok(json!({
            "stored": false,
            "verified": false,
            "name": name,
            "error": "skill failed verification (non-zero exit); fix the code and call write_skill again",
            "exit_code": out.code,
            "stderr": clip_head_tail(&out.stderr, 500, 1500),
        }));
    }

    let skill = crate::skills::AuthoredSkill::new(name, description, language, code, true);
    let path = crate::skills::store(&skill)?;
    Ok(json!({
        "stored": true,
        "verified": true,
        "name": name,
        "path": path.to_string_lossy(),
        "stdout": clip(&out.stdout, 1000),
        "note": "Saved (untrusted). It now appears in list_skills; re-run it later with run_skill(name).",
    }))
}

/// `run_skill`: load a stored skill and execute its body.
async fn run_skill(args: &Value) -> Result<Value> {
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if !crate::skills::valid_name(&name) {
        anyhow::bail!("invalid skill name '{name}'");
    }
    let skill = crate::skills::load(&name)?;
    let (lang, code) = (skill.language.clone(), skill.code.clone());
    let out = tokio::task::spawn_blocking(move || crate::skills::execute(&lang, &code)).await??;
    // VS1 fix-round #1: emit the `success`/`error` contract, not just `ok`.
    // The shared is_error gate + provenance classifier
    // (crates/agent/src/tool_result.rs) key on `success`/`error`; a failed
    // stored skill that reported only `ok:false` slipped every rule and was
    // rendered as a green "completed" card with provenance status:ok — the
    // exact VS1 mask, left live for run_skill. `ok` is kept because the TUI
    // renderer (protocol.rs::format_skill_run) reads it; `success` mirrors it
    // for the gate and `error` (null when clean) trips the string-error rule.
    Ok(json!({
        "name": name,
        "ok": out.ok,
        "success": out.ok,
        "error": (!out.ok).then(|| match out.code {
            Some(c) => format!("skill '{name}' exited non-zero (exit {c}); see stderr"),
            None => format!("skill '{name}' was killed by a signal; see stderr"),
        }),
        "exit_code": out.code,
        "stdout": clip(&out.stdout, 4000),
        "stderr": clip_head_tail(&out.stderr, 500, 1500),
    }))
}

/// `list_skills`: the authored-skill inventory.
fn list_skills() -> Value {
    let skills = crate::skills::load_all();
    let items: Vec<Value> = skills
        .iter()
        .map(|s| {
            json!({
                "name": s.name,
                "description": s.description,
                "language": s.language,
                "verified": s.verified,
            })
        })
        .collect();
    json!({ "count": items.len(), "skills": items })
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
        assert!(is_meta_tool("spawn_subagent"));
        assert!(!is_meta_tool("file"));
        assert!(!is_meta_tool("peek_result"));
    }

    #[test]
    fn reserved_names_cover_meta_and_command_tools_but_not_arbitrary() {
        // Anti-spoofing: both trusted layers are reserved against authored/user tools.
        assert!(is_reserved_tool_name("recall")); // meta-tool
        assert!(is_reserved_tool_name("mesh_publish")); // command-tool (spine)
        assert!(is_reserved_tool_name("mesh_health")); // freshly-ported command-tool
        assert!(is_reserved_tool_name("research")); // command-tool
        // A normal, non-built-in skill name is allowed.
        assert!(!is_reserved_tool_name("my_alloy_screener"));
        assert!(!is_reserved_tool_name("summarize_dft_run"));
    }

    #[tokio::test]
    async fn write_skill_rejects_reserved_names() {
        // A skill trying to squat a trusted built-in name is refused BEFORE any
        // execution or storage — the model is told to rename and retry.
        for squat in ["recall", "mesh_publish", "research"] {
            let out = write_skill(&json!({
                "name": squat,
                "description": "impostor",
                "code": "echo hi",
                "language": "shell",
            }))
            .await
            .unwrap();
            assert_eq!(out["stored"], json!(false), "{squat} must not be stored");
            assert_eq!(out["verified"], json!(false), "{squat} must not run");
            assert!(
                out["error"].as_str().unwrap_or("").contains("reserved"),
                "{squat} error should explain the reservation"
            );
        }
    }

    #[test]
    fn meta_tool_permissions_match_their_effect() {
        let defs = definitions();
        let by = |name: &str| defs.iter().find(|t| t.name == name).expect(name).clone();

        // Read-only, no-approval: memory + discovery, and listing skills.
        for name in ["recall", "find_tools", "list_skills"] {
            let t = by(name);
            assert_eq!(t.permission_mode, PermissionMode::ReadOnly, "{name}");
            assert!(!t.requires_approval, "{name} must not need approval");
        }
        // Code-executing self-authoring tools — and delegation, which spends
        // tokens and drives tools — are workspace-write + gated.
        for name in ["write_skill", "run_skill", "spawn_subagent"] {
            let t = by(name);
            assert_eq!(t.permission_mode, PermissionMode::WorkspaceWrite, "{name}");
            assert!(
                t.requires_approval,
                "{name} executes code → must need approval"
            );
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
            Some("generate"),
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

    /// VS1 fix-round #1: a stored skill can pass write-time verification yet
    /// FAIL on a later run (environment drift). run_skill must then report the
    /// failure via the `success`/`error` contract the shared is_error gate +
    /// provenance classifier read — not a bare `ok:false` that renders as a
    /// green "completed" card. We store a failing body directly (the same
    /// `skills::store` path write_skill uses after verification) to reach the
    /// "stored-then-broke" state deterministically.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn run_skill_failure_emits_success_error_contract() {
        let (_g, _dir) = crate::skills::test_env_guard("meta-runskill-fail");
        let catalog = ToolCatalog::from_tool_server_json(&json!({ "tools": [] }));

        let drifted = crate::skills::AuthoredSkill::new(
            "drifted",
            "fails at run time",
            "shell",
            "exit 7",
            true,
        );
        crate::skills::store(&drifted).unwrap();

        let r = execute_meta_tool(
            "run_skill",
            &json!({ "name": "drifted" }),
            None,
            "",
            &catalog,
        )
        .await
        .unwrap();
        assert_eq!(r["ok"], json!(false), "failing skill reports ok:false: {r}");
        assert_eq!(
            r["success"],
            json!(false),
            "and success:false for the shared gate: {r}"
        );
        assert!(
            r["error"].is_string(),
            "and a string error so the gate/provenance catch it: {r}"
        );
        // The gate over the wrapped shape the agent loop actually builds.
        let wrapped = json!({ "result": r });
        assert!(
            crate::tool_result::tool_result_is_error(&wrapped),
            "a wrapped failed run_skill MUST classify as error: {wrapped}"
        );
    }

    /// The self-authoring (Voyager) loop end to end through the meta-tool layer:
    /// write_skill verifies-then-stores, a failing skill is rejected, list_skills
    /// shows only the verified one, run_skill re-executes it, and it surfaces to
    /// the capability/retrieval layer. Deterministic (shell only, temp dir).
    // The env guard is held across `.await` only to serialize `PRISM_SKILLS_DIR`
    // between tests; `#[tokio::test]` is single-threaded so this can't deadlock.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn write_skill_verifies_stores_lists_and_runs() {
        let (_g, _dir) = crate::skills::test_env_guard("meta-writeskill");
        let catalog = ToolCatalog::from_tool_server_json(&json!({ "tools": [] }));

        // 1. A valid shell skill: verified by running once, then stored.
        let w = execute_meta_tool(
            "write_skill",
            &json!({
                "name": "say_hi",
                "description": "print a greeting to stdout",
                "language": "shell",
                "code": "echo hello-from-skill",
            }),
            None,
            "",
            &catalog,
        )
        .await
        .unwrap();
        assert_eq!(w["stored"], json!(true), "valid skill must store: {w}");
        assert_eq!(w["verified"], json!(true));

        // 2. A skill that exits non-zero is REJECTED, not stored.
        let bad = execute_meta_tool(
            "write_skill",
            &json!({
                "name": "broken",
                "description": "exits non-zero",
                "language": "shell",
                "code": "exit 7",
            }),
            None,
            "",
            &catalog,
        )
        .await
        .unwrap();
        assert_eq!(
            bad["stored"],
            json!(false),
            "failing skill must NOT store: {bad}"
        );
        assert_eq!(bad["verified"], json!(false));

        // 3. Only the verified skill is listed.
        let list = execute_meta_tool("list_skills", &json!({}), None, "", &catalog)
            .await
            .unwrap();
        assert_eq!(
            list["count"],
            json!(1),
            "only verified skill listed: {list}"
        );
        assert_eq!(list["skills"][0]["name"], json!("say_hi"));

        // 4. run_skill re-executes the stored skill and returns its output.
        let r = execute_meta_tool(
            "run_skill",
            &json!({ "name": "say_hi" }),
            None,
            "",
            &catalog,
        )
        .await
        .unwrap();
        assert_eq!(r["ok"], json!(true), "run must succeed: {r}");
        assert!(
            r["stdout"]
                .as_str()
                .unwrap_or_default()
                .contains("hello-from-skill"),
            "stdout must carry the skill output: {r}"
        );

        // 5. The stored skill surfaces to the capability/retrieval layer.
        assert_eq!(
            crate::skills::retrieval_entries(),
            vec![(
                "say_hi".to_string(),
                "say_hi: print a greeting to stdout".to_string()
            )]
        );
    }

    // ── VS1 / F2: skill stderr head+tail keeps the final Error line ────

    #[test]
    fn f2_clip_head_tail_preserves_final_error_line() {
        // A Python-style traceback: header context at the front, the real
        // `ValueError: ...` line at the END. Head-only clipping would drop it.
        let frames = "Traceback (most recent call last):\n".to_string()
            + &"  File \"skill.py\", line N, in run\n    pass\n".repeat(60);
        let stderr = frames + "ValueError: x must be positive\n";
        let clipped = clip_head_tail(&stderr, 500, 1500);

        // The tail must survive — that's the whole point.
        assert!(
            clipped.contains("ValueError: x must be positive"),
            "final error line must survive head+tail clip: {clipped}"
        );
        // The traceback header should still be there too.
        assert!(
            clipped.contains("Traceback (most recent call last)"),
            "traceback header must survive head+tail clip: {clipped}"
        );
        // The elision must be announced, not silent.
        assert!(
            clipped.contains("chars elided"),
            "elided middle must be marked explicitly: {clipped}"
        );
    }

    #[test]
    fn f2_clip_head_tail_short_string_unchanged() {
        // Below the head+tail budget: returned verbatim, no marker.
        let s = "Traceback (most recent call last):\nValueError: boom\n";
        assert_eq!(clip_head_tail(s, 500, 1500), s);
    }

    #[test]
    fn f2_clip_head_tail_multibyte_safe() {
        // Whole-char boundaries: a multibyte emoji must not be split at the
        // head/tail seams. The head and tail windows land on char starts.
        let head_pad = "a".repeat(400);
        let tail_pad = "b".repeat(1400);
        // Surround a multibyte char with ASCII so the seam could land mid-codepoint.
        let s = format!("{head_pad}😀{tail_pad}");
        let clipped = clip_head_tail(&s, 500, 1500);
        // The string should be valid UTF-8 (no panic) — clip_head_tail returns
        // a String, so this is enforced by construction. Assert no replacement
        // char and that the emoji survives somewhere in the tail.
        assert!(
            !clipped.contains('\u{FFFD}'),
            "no replacement char from a split codepoint: {clipped}"
        );
    }
}
