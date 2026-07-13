// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! End-to-end execution of the BUILTIN `ingest` playbook.
//!
//! Everything inside the workflow engine is REAL: the builtin YAML is
//! loaded through the real loader (skill_workflow lowering), steps are
//! dispatched in order by the real executor, templates are rendered
//! against the live context, declared outputs are bound and flow into
//! downstream steps, and the provenance step writes a durable record
//! into a real Turso store that the test re-opens and queries.
//!
//! In-process fakes stand in ONLY at the engine's external boundaries:
//!   * the PRISM node tool API (`POST /api/tools/{name}/run`) — which in
//!     production fronts the web + the platform knowledge service, and
//!   * the OpenAI-compatible LLM endpoint used by `llm` steps.
//!
//! Both fakes record every request so the test can prove the engine sent
//! each step the REAL outputs of the step before it.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Path, State};
use axum::response::Json;
use axum::routing::post;
use serde_json::{Value, json};

const BUILTIN_INGEST_YAML: &str = include_str!("../../../app/workflows/builtin/ingest.yaml");

const PAGE_CONTENT: &str = "Ti-6Al-4V is an alpha-beta titanium alloy with high strength-to-weight \
     ratio, used in aerospace structures and LPBF additive manufacturing.";
const PAGE_TITLE: &str = "Ti-6Al-4V — Titanium Alloy";

#[derive(Clone, Default)]
struct Recorded {
    tool_calls: Arc<Mutex<Vec<(String, Value)>>>,
    llm_requests: Arc<Mutex<Vec<Value>>>,
}

/// Fake PRISM node: implements `POST /api/tools/{name}/run` with the same
/// response envelope the real node uses (`{"tool": ..., "result": ...}`),
/// recording every request body.
async fn fake_run_tool(
    State(rec): State<Recorded>,
    Path(name): Path<String>,
    Json(body): Json<Value>,
) -> Json<Value> {
    rec.tool_calls
        .lock()
        .unwrap()
        .push((name.clone(), body.clone()));

    let inputs = body.get("inputs").cloned().unwrap_or_else(|| json!({}));
    let action = inputs.get("action").and_then(Value::as_str).unwrap_or("");
    let result = match (name.as_str(), action) {
        ("web", "search") => json!({
            "query": inputs["query"],
            "results": [
                {"title": PAGE_TITLE, "url": "https://example.com/ti64", "snippet": "Ti-6Al-4V alloy overview"},
                {"title": "Grade 5 titanium", "url": "https://example.com/grade5", "snippet": "Grade 5 spec"},
            ],
            "count": 2,
            "source": "fake",
        }),
        ("web", "read") => json!({
            "url": inputs["url"],
            "title": PAGE_TITLE,
            "content": PAGE_CONTENT,
            "source": "fake",
            "content_length": PAGE_CONTENT.len(),
        }),
        ("knowledge_write", "graph_ingest") => json!({
            "entities_created": inputs["entities"].as_array().map(|a| a.len()).unwrap_or(0),
            "relationships_created": inputs["relationships"].as_array().map(|a| a.len()).unwrap_or(0),
        }),
        ("knowledge_write", "embed") => json!({
            "embedding_id": "emb-e2e-001",
            "doc_id": inputs["doc_id"],
        }),
        other => json!({ "error": format!("fake node has no handler for {other:?}") }),
    };
    Json(json!({ "tool": name, "result": result }))
}

/// Fake OpenAI-compatible LLM: answers the extraction step with the JSON
/// contract the engine asked for.
async fn fake_chat_completions(
    State(rec): State<Recorded>,
    Json(body): Json<Value>,
) -> Json<Value> {
    rec.llm_requests.lock().unwrap().push(body);
    let answer = json!({
        "entities": [
            {"name": "Ti-6Al-4V", "entity_type": "MAT", "label": "Ti-6Al-4V"},
            {"name": "Ti", "entity_type": "ELM", "label": "Titanium"},
        ],
        "relationships": [
            {"from_name": "Ti-6Al-4V", "to_name": "Ti", "rel_type": "CONTAINS"},
        ],
    });
    Json(json!({
        "choices": [{ "message": { "content": answer.to_string() } }]
    }))
}

async fn spawn(router: Router) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    port
}

#[tokio::test(flavor = "multi_thread")]
async fn builtin_ingest_playbook_executes_end_to_end() {
    let rec = Recorded::default();

    let node_port = spawn(
        Router::new()
            .route("/api/tools/{name}/run", post(fake_run_tool))
            .with_state(rec.clone()),
    )
    .await;
    let llm_port = spawn(
        Router::new()
            .route("/v1/chat/completions", post(fake_chat_completions))
            .with_state(rec.clone()),
    )
    .await;

    // Real temp Turso db for the provenance step.
    let tmp = tempfile::tempdir().unwrap();
    let prov_db = tmp.path().join("provenance.db");

    // Load the REAL builtin playbook through the real loader.
    let spec = prism_workflows::load_workflow_from_str(BUILTIN_INGEST_YAML, "builtin:ingest.yaml")
        .expect("builtin ingest.yaml must load");

    let source = "https://example.com/ti64";
    let mut values = BTreeMap::new();
    values.insert("source".to_string(), source.to_string());
    values.insert("node_port".to_string(), node_port.to_string());
    values.insert(
        "llm_base_url".to_string(),
        format!("http://127.0.0.1:{llm_port}/v1"),
    );
    values.insert("provenance_db".to_string(), prov_db.display().to_string());

    let result = prism_workflows::execute_workflow(&spec, &values, true)
        .await
        .expect("ingest playbook must execute end to end");

    // ── Every step ran, in order, for real ─────────────────────────────
    println!("\n=== builtin ingest — per-step results (execute mode) ===");
    for step in &result.steps {
        println!(
            "step={:<18} action={:<10} status={:<9} {}",
            step.id, step.action, step.status, step.summary
        );
    }
    println!("mode={}", result.mode);

    assert_eq!(result.mode, "execute");
    assert_eq!(
        result
            .steps
            .iter()
            .map(|s| (s.id.as_str(), s.status.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("discover", "completed"),
            ("fetch", "completed"),
            ("extract", "completed"),
            ("store_graph", "completed"),
            ("store_embeddings", "completed"),
            ("record_provenance", "completed"),
        ]
    );

    // ── Data flowed between steps ───────────────────────────────────────
    let calls = rec.tool_calls.lock().unwrap().clone();
    assert_eq!(
        calls
            .iter()
            .map(|(name, body)| format!(
                "{name}.{}",
                body["inputs"]["action"].as_str().unwrap_or("?")
            ))
            .collect::<Vec<_>>(),
        vec![
            "web.search",
            "web.read",
            "knowledge_write.graph_ingest",
            "knowledge_write.embed",
        ]
    );

    // discover received the workflow argument.
    assert_eq!(calls[0].1["inputs"]["query"], source);
    // fetch received the source URL.
    assert_eq!(calls[1].1["inputs"]["url"], source);

    // extract (llm step) received the REAL page content fetched by `fetch`.
    let llm_reqs = rec.llm_requests.lock().unwrap().clone();
    assert_eq!(llm_reqs.len(), 1);
    let user_msg = llm_reqs[0]["messages"][1]["content"].as_str().unwrap();
    assert!(
        user_msg.contains("alpha-beta titanium alloy"),
        "extract prompt must carry the fetched content, got: {user_msg}"
    );
    assert!(user_msg.contains(PAGE_TITLE));

    // store_graph received EXACTLY the entities/relationships the LLM
    // extracted (extract → store_graph flow), with approval.
    let graph_body = &calls[2].1;
    assert_eq!(graph_body["approve"], true);
    assert_eq!(
        graph_body["inputs"]["entities"][0]["name"], "Ti-6Al-4V",
        "entities must flow extract → store_graph"
    );
    assert_eq!(
        graph_body["inputs"]["relationships"][0]["rel_type"],
        "CONTAINS"
    );

    // store_embeddings received the fetched content and the source doc id.
    let embed_body = &calls[3].1;
    assert_eq!(embed_body["approve"], true);
    assert_eq!(embed_body["inputs"]["content"], PAGE_CONTENT);
    assert_eq!(embed_body["inputs"]["doc_id"], source);

    // ── Final context carries the bound outputs ─────────────────────────
    assert_eq!(result.context.get("content").unwrap(), PAGE_CONTENT);
    assert_eq!(result.context.get("title").unwrap(), PAGE_TITLE);
    assert_eq!(result.context.get("entities_created").unwrap(), 2);
    assert_eq!(result.context.get("relationships_created").unwrap(), 1);
    assert_eq!(result.context.get("embedding_id").unwrap(), "emb-e2e-001");
    let record_id = result
        .context
        .get("provenance_record_id")
        .and_then(Value::as_str)
        .expect("provenance record id must be bound into context")
        .to_string();

    println!("\n=== final context (selected) ===");
    for key in [
        "content",
        "title",
        "entities_created",
        "relationships_created",
        "embedding_id",
        "provenance_record_id",
    ] {
        println!("{key} = {}", result.context.get(key).unwrap());
    }

    // ── The provenance write was REAL and durable ───────────────────────
    let store = prism_provenance::ProvenanceStore::open(&prov_db)
        .await
        .expect("provenance db must exist after the run");
    let records = store
        .query_by_session("workflow:ingest")
        .await
        .expect("query provenance by session");
    assert_eq!(records.len(), 1, "exactly one provenance record");
    let rec0 = &records[0];
    assert_eq!(rec0.id, record_id);
    assert_eq!(rec0.input_json["source_url"], source);
    assert_eq!(rec0.input_json["entities"][0]["name"], "Ti-6Al-4V");
    assert!((rec0.confidence - 0.85).abs() < 1e-9);
    println!(
        "\n=== durable provenance record ===\nid={} session={} confidence={} source_url={}",
        rec0.id, rec0.session_id, rec0.confidence, rec0.input_json["source_url"]
    );
}
