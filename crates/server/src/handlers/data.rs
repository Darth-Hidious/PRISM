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
///
/// Always empty for now: the old implementation reported Neo4j graph
/// stats, and Neo4j is retired. Knowledge lives in the bundled Turso
/// store; surfacing its counts here is future work.
pub async fn list_sources(State(_state): State<Arc<NodeState>>) -> Json<Vec<DataSource>> {
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
