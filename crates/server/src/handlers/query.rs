//! Query execution handlers.
//!
//! Supports three query modes, all served by the bundled Turso store:
//! - `graph` — Entity neighbor traversal by name
//! - `semantic` — Vector similarity search over locally-embedded entities
//! - `federated` — Fan out to mesh peers and merge results
//!
//! The `nl` and `cypher` modes were removed with the Neo4j retirement —
//! they translated to / executed Cypher and have no Turso equivalent.

use axum::Extension;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::NodeState;
use crate::middleware::AuthenticatedUser;

#[derive(Deserialize)]
pub struct QueryRequest {
    pub query: String,
    /// "graph" (default), "semantic", or "federated".
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// When true, also fan out the query to mesh peers and merge results.
    #[serde(default)]
    pub federated: bool,
}

fn default_mode() -> String {
    "graph".into()
}
fn default_limit() -> usize {
    10
}

#[derive(Serialize)]
pub struct QueryResponse {
    pub results: Vec<serde_json::Value>,
    pub count: u64,
    pub mode: String,
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

/// POST /api/query — execute a query against the knowledge graph.
pub async fn execute_query(
    State(state): State<Arc<NodeState>>,
    user: Option<Extension<AuthenticatedUser>>,
    Json(body): Json<QueryRequest>,
) -> Result<Json<QueryResponse>, (StatusCode, Json<ErrorResponse>)> {
    // Input validation
    if body.query.len() > 10_000 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Query too long (max 10,000 characters).".into(),
            }),
        ));
    }
    if body.limit > 1000 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Limit too high (max 1,000).".into(),
            }),
        ));
    }

    let user_id = user
        .as_ref()
        .map(|u| u.user_id.as_str())
        .unwrap_or("anonymous");

    let mut result = match body.mode.as_str() {
        "graph" | "neighbors" => handle_graph_query(&state, &body, user_id).await,
        "semantic" => handle_semantic_query(&state, &body, user_id).await,
        "federated" => handle_federated_query(&state, &body, user_id).await,
        other => Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: format!(
                    "Unknown query mode: '{other}'. Use 'graph', 'semantic', or 'federated'."
                ),
            }),
        )),
    }?;

    // If federated=true on any mode, also query peers and merge results
    if body.federated
        && body.mode != "federated"
        && let Some(peer_results) = query_mesh_peers(&state, &body.query).await
    {
        result.results.extend(peer_results);
        result.count = result.results.len() as u64;
    }

    Ok(result)
}

// ─── Local Turso reads (same pattern as the CLI's local-first query) ────

/// Tenant under which `prism ingest` writes locally-ingested ontology
/// into the bundled Turso store.
const LOCAL_ONTOLOGY_TENANT: &str = "local";

/// Default path of the bundled Turso provenance store — the same location
/// the agent loop and local ingest use (`~/.prism/provenance.db`).
fn default_provenance_db_path() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".prism/provenance.db"))
        .unwrap_or_else(|| PathBuf::from("provenance.db"))
}

/// Query the bundled Turso store for locally-ingested ontology entities,
/// each paired with the origin locator this node can honestly report for
/// it (`None` when no stored assertion mentions the entity).
///
/// Never errors: any failure (store unopenable, query error) degrades to
/// `None`, which the handler renders as an empty result set. `None` is
/// also returned when the store is fine but nothing matched (fresh
/// install, unknown term).
async fn local_graph_lookup(
    db_path: &Path,
    text: &str,
    limit: usize,
) -> Option<Vec<(prism_provenance::GraphNode, Option<String>)>> {
    let store = match prism_provenance::ProvenanceStore::open(db_path).await {
        Ok(store) => store,
        Err(e) => {
            tracing::debug!("local graph store open failed: {e:#}");
            return None;
        }
    };
    let limit = limit.max(1) as i64;

    // Exact/canonical entity name → 1-hop neighborhood (the Turso
    // counterpart of Neo4j `neighbors`).
    let mut nodes = match store
        .get_neighbors(text, None, LOCAL_ONTOLOGY_TENANT, limit)
        .await
    {
        Ok(traversal) => traversal.nodes,
        Err(e) => {
            tracing::debug!("local graph neighbor read failed: {e:#}");
            Vec::new()
        }
    };

    // No exact center → substring search over entity names.
    if nodes.is_empty() {
        nodes = match store.graph_search(text, LOCAL_ONTOLOGY_TENANT, limit).await {
            Ok(nodes) => nodes,
            Err(e) => {
                tracing::debug!("local graph search failed: {e:#}");
                Vec::new()
            }
        };
    }

    if nodes.is_empty() {
        return None;
    }

    // Attach the origin each entity can honestly be attributed to, so a
    // mesh peer syncing these rows can key corroboration on the ORIGINAL
    // source instead of collapsing every relay to `mesh:unattributed`.
    // A read failure degrades to an unattributed row, never an error.
    let mut out = Vec::with_capacity(nodes.len());
    for node in nodes {
        let origin = store
            .entity_origin(&node.name, LOCAL_ONTOLOGY_TENANT)
            .await
            .unwrap_or_else(|e| {
                tracing::debug!("entity origin read failed: {e:#}");
                None
            });
        out.push((node, origin));
    }
    Some(out)
}

/// Semantic entity search over the bundled Turso store, ranked by Turso's
/// native `vector_distance_cos()`, using the offline `prism-embed` backend
/// for the query vector (no Qdrant, no cloud) — the vectors that local
/// ingest writes via `embed_entities_best_effort`.
///
/// # Honesty contract
///
/// `Ok(vec![])` means **nothing is embedded locally yet**, and nothing
/// else. Anything that makes the index unusable — an unopenable store, a
/// missing embedding backend, a dimension mismatch — is an `Err` whose
/// message names the problem, so a broken index can never be served as
/// "no matches". The store is counted BEFORE the backend is built, so a
/// fresh install never pays the embedding-model init just to return
/// nothing.
async fn local_semantic_lookup(
    db_path: &Path,
    text: &str,
    limit: usize,
) -> anyhow::Result<Vec<(String, f32)>> {
    use anyhow::Context as _;

    // No store file at all ⇒ nothing was ever ingested. That is an empty
    // index, not a broken one, so it must not raise the alarm a fresh
    // install would otherwise trip on (opening a path under a missing
    // `~/.prism` fails outright).
    if !db_path.exists() {
        return Ok(Vec::new());
    }
    let store = prism_provenance::ProvenanceStore::open(db_path)
        .await
        .with_context(|| {
            format!(
                "local semantic store {} could not be opened",
                db_path.display()
            )
        })?;
    let embedded = store
        .entity_embedding_count(LOCAL_ONTOLOGY_TENANT)
        .await
        .context("local semantic index could not be counted")?;
    if embedded == 0 {
        return Ok(Vec::new()); // nothing ingested yet — a real empty answer
    }

    // First ever native init may download the model — blocking pool.
    let backend = tokio::task::spawn_blocking(prism_embed::from_config)
        .await
        .context("embedding backend initialization panicked")?
        .context(
            "no embedding backend available, so the query cannot be embedded — set \
             PRISM_EMBED_BACKEND=native (the default) or =openai with \
             PRISM_EMBED_ENDPOINT_URL",
        )?;
    let query_vec = backend
        .embed(std::slice::from_ref(&text.to_string()))
        .await
        .context("embedding the query failed")?
        .into_iter()
        .next()
        .context("embedding backend returned no vector for the query")?;

    store
        .semantic_search_entities(&query_vec, LOCAL_ONTOLOGY_TENANT, limit)
        .await
}

/// Map local Turso graph nodes into the same JSON shape the retired Neo4j
/// path returned (`{type, name, properties}`), keeping the wire format
/// stable for existing clients. The one property a node may carry is
/// `origin_source` — the locator of the source this node's knowledge came
/// from — which mesh peers syncing these rows use to keep corroboration
/// honest (`crates/mesh/src/sync.rs`). A node with no attributable origin
/// keeps the historical empty `properties`, never an invented locator.
fn graph_nodes_to_results(
    nodes: &[(prism_provenance::GraphNode, Option<String>)],
) -> Vec<serde_json::Value> {
    nodes
        .iter()
        .map(|(n, origin)| {
            serde_json::json!({
                "type": n.entity_type,
                "name": n.name,
                "properties": match origin {
                    Some(origin) => serde_json::json!({ "origin_source": origin }),
                    None => serde_json::json!({}),
                },
            })
        })
        .collect()
}

/// Map local Turso semantic hits into the same JSON shape the retired
/// Qdrant path returned (`{id, score}`), keeping the wire format stable.
fn semantic_hits_to_results(hits: &[(String, f32)]) -> Vec<serde_json::Value> {
    hits.iter()
        .map(|(id, score)| {
            serde_json::json!({
                "id": id,
                "score": score,
            })
        })
        .collect()
}

/// Entity neighbor traversal by name, served by the bundled Turso store
/// (~/.prism/provenance.db, tenant "local") — the sole graph backend.
/// A miss (fresh install, unknown term) is an empty result set, not an
/// error.
async fn handle_graph_query(
    state: &NodeState,
    body: &QueryRequest,
    user_id: &str,
) -> Result<Json<QueryResponse>, (StatusCode, Json<ErrorResponse>)> {
    let nodes = local_graph_lookup(&default_provenance_db_path(), &body.query, body.limit)
        .await
        .unwrap_or_default();
    let results = graph_nodes_to_results(&nodes);
    let count = results.len() as u64;

    state.audit_and_broadcast(&prism_core::audit::AuditEntry {
        id: 0,
        timestamp: chrono::Utc::now(),
        user_id: user_id.to_string(),
        action: prism_core::audit::AuditAction::DataQuery,
        target: "graph".into(),
        detail: Some(format!("results={count}, source=turso-local")),
        outcome: prism_core::audit::AuditOutcome::Success,
    });

    Ok(Json(QueryResponse {
        results,
        count,
        mode: "graph".into(),
    }))
}

/// Vector similarity search over the bundled Turso entity vectors written
/// by local ingest, ranked by the native `vector_distance_cos()` (offline
/// prism-embed query embedding — no services needed).
///
/// An empty result set means the local index holds nothing yet. An
/// unusable index — unopenable store, missing embedding backend, dimension
/// mismatch — is a `503` naming the problem and an audited failure, never
/// a silent `200 []`.
async fn handle_semantic_query(
    state: &NodeState,
    body: &QueryRequest,
    user_id: &str,
) -> Result<Json<QueryResponse>, (StatusCode, Json<ErrorResponse>)> {
    let audit = |detail: String, outcome: prism_core::audit::AuditOutcome| {
        state.audit_and_broadcast(&prism_core::audit::AuditEntry {
            id: 0,
            timestamp: chrono::Utc::now(),
            user_id: user_id.to_string(),
            action: prism_core::audit::AuditAction::DataQuery,
            target: "semantic".into(),
            detail: Some(detail),
            outcome,
        });
    };

    let hits =
        match local_semantic_lookup(&default_provenance_db_path(), &body.query, body.limit).await {
            Ok(hits) => hits,
            Err(e) => {
                let error = format!("{e:#}");
                audit(
                    format!("source=turso-local, error={error}"),
                    prism_core::audit::AuditOutcome::Failure,
                );
                return Err((
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(ErrorResponse { error }),
                ));
            }
        };
    let results = semantic_hits_to_results(&hits);
    let count = results.len() as u64;

    audit(
        format!("results={count}, source=turso-local"),
        prism_core::audit::AuditOutcome::Success,
    );

    Ok(Json(QueryResponse {
        results,
        count,
        mode: "semantic".into(),
    }))
}

/// Federated query: fan out to all mesh peers and merge results.
async fn handle_federated_query(
    state: &NodeState,
    body: &QueryRequest,
    user_id: &str,
) -> Result<Json<QueryResponse>, (StatusCode, Json<ErrorResponse>)> {
    let peers = {
        let mesh = state.mesh.read().unwrap_or_else(|e| e.into_inner());
        mesh.peers()
    };

    if peers.is_empty() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse {
                error: "No mesh peers available for federated query.".into(),
            }),
        ));
    }

    let federation = state.federation.get().cloned().unwrap_or_default();
    let results = federation
        .query_peers(&peers, &body.query)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "federated query failed");
            internal_error("Federated query execution failed.")
        })?;

    let count = results.len() as u64;

    state.audit_and_broadcast(&prism_core::audit::AuditEntry {
        id: 0,
        timestamp: chrono::Utc::now(),
        user_id: user_id.to_string(),
        action: prism_core::audit::AuditAction::DataQuery,
        target: "federated".into(),
        detail: Some(format!("results={count}, peers={}", peers.len())),
        outcome: prism_core::audit::AuditOutcome::Success,
    });

    Ok(Json(QueryResponse {
        results,
        count,
        mode: "federated".into(),
    }))
}

/// Query mesh peers and return their results (or None if no peers/federation).
async fn query_mesh_peers(state: &NodeState, query: &str) -> Option<Vec<serde_json::Value>> {
    let peers = {
        let mesh = state.mesh.read().unwrap_or_else(|e| e.into_inner());
        mesh.peers()
    };

    if peers.is_empty() {
        return None;
    }

    let federation = state.federation.get().cloned().unwrap_or_default();
    match federation.query_peers(&peers, query).await {
        Ok(results) if !results.is_empty() => {
            tracing::info!(
                peer_count = peers.len(),
                results = results.len(),
                "merged federated peer results"
            );
            Some(results)
        }
        Ok(_) => None,
        Err(e) => {
            tracing::warn!(error = %e, "federated peer query failed — returning local results only");
            None
        }
    }
}

fn internal_error(msg: &str) -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse { error: msg.into() }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tempfile-backed Turso DB, removed (with SQLite journal sidecars) on drop.
    struct TempProvenanceDb {
        path: PathBuf,
    }

    impl TempProvenanceDb {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "prism_server_local_query_{}.db",
                uuid::Uuid::new_v4()
            ));
            Self { path }
        }
    }

    impl Drop for TempProvenanceDb {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let mut p = self.path.clone().into_os_string();
                p.push(suffix);
                let _ = std::fs::remove_file(p);
            }
        }
    }

    #[test]
    fn turso_results_map_to_existing_response_shapes() {
        // Graph: same {type, name, properties} shape as the Neo4j path.
        // An attributable node carries its origin locator in `properties`
        // (additive — older clients ignore it); one without stays `{}`.
        let node = prism_provenance::GraphNode {
            name: "Ti-6Al-4V".into(),
            entity_type: "Matter".into(),
            label: "Matter".into(),
            tenant: "local".into(),
        };
        let nodes = vec![
            (node.clone(), Some("doi:10.1234/abc".to_string())),
            (node, None),
        ];
        assert_eq!(
            graph_nodes_to_results(&nodes),
            vec![
                serde_json::json!({
                    "type": "Matter",
                    "name": "Ti-6Al-4V",
                    "properties": { "origin_source": "doi:10.1234/abc" },
                }),
                serde_json::json!({
                    "type": "Matter",
                    "name": "Ti-6Al-4V",
                    "properties": {},
                }),
            ]
        );

        // Semantic: same {id, score} shape as the Qdrant path.
        let hits = vec![("Ti-6Al-4V".to_string(), 0.87_f32)];
        let results = semantic_hits_to_results(&hits);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["id"], "Ti-6Al-4V");
        let score = results[0]["score"].as_f64().expect("score is a number");
        assert!((score - f64::from(0.87_f32)).abs() < 1e-6, "got: {score}");
    }

    #[tokio::test]
    async fn local_graph_lookup_misses_cleanly_on_empty_or_unopenable_store() {
        let db = TempProvenanceDb::new();
        assert!(
            local_graph_lookup(&db.path, "titanium", 10).await.is_none(),
            "empty store must be a clean graph miss"
        );
        assert!(
            local_graph_lookup(&std::env::temp_dir(), "titanium", 10)
                .await
                .is_none(),
            "graph store open failure must degrade to a miss"
        );
    }

    /// An EMPTY local index and a BROKEN one must not look the same to the
    /// caller: empty is `Ok([])`, broken is an `Err` that names the cause.
    #[tokio::test]
    async fn empty_semantic_index_is_ok_but_unopenable_store_is_a_named_error() {
        let db = TempProvenanceDb::new();

        // Fresh (empty) store: a real empty answer. Zero stored embeddings
        // short-circuits BEFORE the embedding backend is built, so this
        // stays model-free.
        let hits = local_semantic_lookup(&db.path, "titanium", 10)
            .await
            .expect("an empty index is an empty answer, not a failure");
        assert!(hits.is_empty());

        // Unopenable path (a directory): a loud error naming the store,
        // never an empty result set the caller would read as "no matches".
        let err = local_semantic_lookup(&std::env::temp_dir(), "titanium", 10)
            .await
            .expect_err("an unopenable store must not masquerade as no matches");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("could not be opened"),
            "error must name the problem: {msg}"
        );

        // A fresh install has no store file at all (and no `~/.prism` to
        // open one under). "Never ingested" is empty, NOT broken — this
        // must not be the loud error the unopenable case earns.
        let missing = std::env::temp_dir()
            .join(format!("prism_absent_{}", uuid::Uuid::new_v4()))
            .join(".prism/provenance.db");
        let hits = local_semantic_lookup(&missing, "titanium", 10)
            .await
            .expect("a never-created store is an empty index, not a broken one");
        assert!(hits.is_empty());
    }

    #[tokio::test]
    async fn local_graph_lookup_reads_ingested_entities() {
        let db = TempProvenanceDb::new();

        // Write one EMMO fact the way `prism ingest` does (tenant "local").
        let store = prism_provenance::ProvenanceStore::open(&db.path)
            .await
            .expect("open temp store");
        let now = chrono::Utc::now().to_rfc3339();
        let prov = prism_provenance::LocalProvenance {
            activity_id: "act_test".into(),
            agent_id: "prism-ingest".into(),
            agent_kind: "SoftwareAgent".into(),
            source_entity_id: "doc:test".into(),
            source_kind: "Document".into(),
            tenant: LOCAL_ONTOLOGY_TENANT.into(),
            started_at: now.clone(),
            ended_at: now,
            locality: "local".into(),
            origin_source_id: None,
        };
        store.record_activity(&prov).await.expect("record activity");
        store
            .write_fact(
                &prism_provenance::LocalFact {
                    subject: "Ti-6Al-4V".into(),
                    predicate: "hasPart".into(),
                    object: "alpha phase".into(),
                    value: None,
                    unit: None,
                    confidence: Some(0.9),
                    kind: Some("contains".into()),
                },
                &prov,
            )
            .await
            .expect("write fact");

        // Exact name → neighbor traversal, carrying the origin the entity
        // was ingested from (what a syncing mesh peer keys corroboration on).
        let nodes = local_graph_lookup(&db.path, "Ti-6Al-4V", 10)
            .await
            .expect("ingested entity must be queryable");
        assert!(
            nodes
                .iter()
                .any(|(n, origin)| n.name == "Ti-6Al-4V" && origin.as_deref() == Some("doc:test"))
        );

        // Substring → graph_search fallback.
        let nodes = local_graph_lookup(&db.path, "6Al", 10)
            .await
            .expect("substring match must be queryable");
        assert!(nodes.iter().any(|(n, _)| n.name == "Ti-6Al-4V"));

        // Unknown term → clean miss (handler renders an empty result set).
        assert!(
            local_graph_lookup(&db.path, "no-such-entity-xyz", 10)
                .await
                .is_none()
        );
    }
}
