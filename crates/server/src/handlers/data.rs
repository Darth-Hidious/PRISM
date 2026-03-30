//! Data ingestion and retrieval handlers.

use axum::extract::State;
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
                name: format!("Knowledge Graph ({} nodes, {} rels)", stats.node_count, stats.relationship_count),
                kind: "graph".into(),
            }]);
        }
    }
    Json(vec![])
}

/// POST /api/data/ingest — queue a data ingestion job.
pub async fn ingest(Json(body): Json<IngestRequest>) -> Json<IngestResponse> {
    // Ingest is a long-running operation — for now return acknowledgment.
    // The full pipeline is invoked via `prism ingest` CLI.
    let source = body.source.unwrap_or_else(|| "(none)".into());
    Json(IngestResponse {
        status: "accepted",
        message: format!("Ingest queued for source: {source}. Use `prism ingest <file>` for full pipeline."),
    })
}
