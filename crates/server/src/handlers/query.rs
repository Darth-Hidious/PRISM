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

/// Query the bundled Turso store for locally-ingested ontology entities.
///
/// Never errors: any failure (store unopenable, query error) degrades to
/// `None`, which the handler renders as an empty result set. `None` is
/// also returned when the store is fine but nothing matched (fresh
/// install, unknown term).
async fn local_graph_lookup(
    db_path: &Path,
    text: &str,
    limit: usize,
) -> Option<Vec<prism_provenance::GraphNode>> {
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

    if nodes.is_empty() { None } else { Some(nodes) }
}

/// Semantic entity search over the bundled Turso store using the offline
/// `prism-embed` backend (no Qdrant, no cloud) — the vectors that local
/// ingest writes via `embed_entities_best_effort`.
///
/// Never errors: an unopenable store, an empty store, an unavailable
/// embedding backend, or zero hits all degrade to `None`, which the
/// handler renders as an empty result set. The store is checked BEFORE
/// the backend is built, so a fresh install never pays the
/// embedding-model init just to return nothing.
async fn local_semantic_lookup(
    db_path: &Path,
    text: &str,
    limit: usize,
) -> Option<Vec<(String, f32)>> {
    let store = match prism_provenance::ProvenanceStore::open(db_path).await {
        Ok(store) => store,
        Err(e) => {
            tracing::debug!("local semantic store open failed: {e:#}");
            return None;
        }
    };
    match store.entity_embedding_count(LOCAL_ONTOLOGY_TENANT).await {
        Ok(0) => return None,
        Ok(_) => {}
        Err(e) => {
            tracing::debug!("local semantic embedding count failed: {e:#}");
            return None;
        }
    }

    // First ever native init may download the model — blocking pool.
    let backend = tokio::task::spawn_blocking(prism_embed::from_config)
        .await
        .ok()
        .flatten()?;
    let query_vec = match backend.embed(std::slice::from_ref(&text.to_string())).await {
        Ok(mut vecs) if !vecs.is_empty() => vecs.remove(0),
        Ok(_) => return None,
        Err(e) => {
            tracing::debug!("local semantic query embedding failed: {e:#}");
            return None;
        }
    };

    match store
        .semantic_search_entities(&query_vec, LOCAL_ONTOLOGY_TENANT, limit)
        .await
    {
        Ok(hits) if !hits.is_empty() => Some(hits),
        Ok(_) => None,
        Err(e) => {
            tracing::debug!("local semantic search failed: {e:#}");
            None
        }
    }
}

/// Map local Turso graph nodes into the same JSON shape the retired Neo4j
/// path returned (`{type, name, properties}`), keeping the wire format
/// stable for existing clients. Local nodes carry no free-form properties,
/// so `properties` is an empty object.
fn graph_nodes_to_results(nodes: &[prism_provenance::GraphNode]) -> Vec<serde_json::Value> {
    nodes
        .iter()
        .map(|n| {
            serde_json::json!({
                "type": n.entity_type,
                "name": n.name,
                "properties": {},
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
/// by local ingest (offline prism-embed query embedding — no services
/// needed). A miss (empty store, unavailable embedding backend, zero hits)
/// is an empty result set, not an error.
async fn handle_semantic_query(
    state: &NodeState,
    body: &QueryRequest,
    user_id: &str,
) -> Result<Json<QueryResponse>, (StatusCode, Json<ErrorResponse>)> {
    let hits = local_semantic_lookup(&default_provenance_db_path(), &body.query, body.limit)
        .await
        .unwrap_or_default();
    let results = semantic_hits_to_results(&hits);
    let count = results.len() as u64;

    state.audit_and_broadcast(&prism_core::audit::AuditEntry {
        id: 0,
        timestamp: chrono::Utc::now(),
        user_id: user_id.to_string(),
        action: prism_core::audit::AuditAction::DataQuery,
        target: "semantic".into(),
        detail: Some(format!("results={count}, source=turso-local")),
        outcome: prism_core::audit::AuditOutcome::Success,
    });

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

    /// Tempfile-backed Turso DB, removed (with WAL sidecars) on drop.
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
        let nodes = vec![prism_provenance::GraphNode {
            name: "Ti-6Al-4V".into(),
            entity_type: "Matter".into(),
            label: "Matter".into(),
            tenant: "local".into(),
        }];
        assert_eq!(
            graph_nodes_to_results(&nodes),
            vec![serde_json::json!({
                "type": "Matter",
                "name": "Ti-6Al-4V",
                "properties": {},
            })]
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
    async fn local_lookups_miss_cleanly_on_empty_or_unopenable_store() {
        let db = TempProvenanceDb::new();

        // Fresh (empty) store: clean miss — the handlers render an empty
        // result set instead of erroring.
        assert!(
            local_graph_lookup(&db.path, "titanium", 10).await.is_none(),
            "empty store must be a clean graph miss"
        );
        // Zero stored embeddings short-circuits BEFORE the embedding
        // backend is built, so this stays model-free.
        assert!(
            local_semantic_lookup(&db.path, "titanium", 10)
                .await
                .is_none(),
            "empty store must be a clean semantic miss"
        );

        // Unopenable path (a directory): also a clean miss, never an error.
        assert!(
            local_graph_lookup(&std::env::temp_dir(), "titanium", 10)
                .await
                .is_none(),
            "graph store open failure must degrade to a miss"
        );
        assert!(
            local_semantic_lookup(&std::env::temp_dir(), "titanium", 10)
                .await
                .is_none(),
            "semantic store open failure must degrade to a miss"
        );
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

        // Exact name → neighbor traversal.
        let nodes = local_graph_lookup(&db.path, "Ti-6Al-4V", 10)
            .await
            .expect("ingested entity must be queryable");
        assert!(nodes.iter().any(|n| n.name == "Ti-6Al-4V"));

        // Substring → graph_search fallback.
        let nodes = local_graph_lookup(&db.path, "6Al", 10)
            .await
            .expect("substring match must be queryable");
        assert!(nodes.iter().any(|n| n.name == "Ti-6Al-4V"));

        // Unknown term → clean miss (handler renders an empty result set).
        assert!(
            local_graph_lookup(&db.path, "no-such-entity-xyz", 10)
                .await
                .is_none()
        );
    }
}
