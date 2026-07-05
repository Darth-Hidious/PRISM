//! Data ingestion and retrieval handlers.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::NodeState;

#[derive(Serialize)]
pub struct DataSource {
    pub id: String,
    pub name: String,
    pub kind: String,
}

#[derive(Deserialize)]
pub struct IngestRequest {
    pub source: Option<String>,
    pub format: Option<String>,
}

#[derive(Serialize)]
pub struct IngestResponse {
    pub status: &'static str,
    pub message: String,
}

/// GET /api/data/sources — list available data sources.
pub async fn list_sources(State(state): State<Arc<NodeState>>) -> Json<Vec<DataSource>> {
    // Report graph stats if Neo4j is available.
    if let Some(ref neo4j_config) = state.neo4j {
        use prism_ingest::graph::{GraphStore, Neo4jGraphStore};
        let store = Neo4jGraphStore::new(neo4j_config.clone());
        if let Ok(stats) = store.stats().await {
            return Json(vec![DataSource {
                id: "neo4j".into(),
                name: format!(
                    "Knowledge Graph ({} nodes, {} rels)",
                    stats.node_count, stats.relationship_count
                ),
                kind: "graph".into(),
            }]);
        }
    }
    Json(vec![])
}

/// POST /api/data/ingest — ingestion is NOT wired on this node endpoint.
///
/// Honest 501: this handler never had a queue or pipeline behind it. The real
/// ingestion path is the `prism ingest <file>` CLI (or the knowledge-service
/// ingest API). Returning a fake "accepted" here made the caller believe work
/// was queued when nothing happened — see AUDIT_BACKLOG 0.3.
pub async fn ingest(Json(body): Json<IngestRequest>) -> (StatusCode, Json<IngestResponse>) {
    let source = body.source.unwrap_or_else(|| "(none)".into());
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(IngestResponse {
            status: "not_implemented",
            message: format!(
                "Ingestion is not available on this endpoint (source: {source}). \
                 Use `prism ingest <file>` or the knowledge-service ingest API."
            ),
        }),
    )
}
