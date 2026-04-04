//! Query execution handlers.
//!
//! Supports four query modes:
//! - `nl` — Natural language → LLM translates to Cypher → executes against Neo4j
//! - `cypher` — Direct Cypher execution (power users)
//! - `graph` — Entity neighbor traversal by name
//! - `semantic` — Vector similarity search via Qdrant

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use axum::Extension;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::middleware::AuthenticatedUser;
use crate::NodeState;

#[derive(Deserialize)]
pub struct QueryRequest {
    pub query: String,
    /// "nl" (default), "cypher", "graph", "semantic", or "federated".
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// When true, also fan out the query to mesh peers and merge results.
    #[serde(default)]
    pub federated: bool,
}

fn default_mode() -> String {
    "nl".into()
}
fn default_limit() -> usize {
    10
}

#[derive(Serialize)]
pub struct QueryResponse {
    pub results: Vec<serde_json::Value>,
    pub count: u64,
    pub mode: String,
    /// For "nl" mode: the generated Cypher and explanation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub translation: Option<TranslationInfo>,
}

#[derive(Serialize)]
pub struct TranslationInfo {
    pub cypher: String,
    pub explanation: String,
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
        "nl" => handle_nl_query(&state, &body, user_id).await,
        "cypher" => handle_cypher_query(&state, &body, user_id).await,
        "graph" | "neighbors" => handle_graph_query(&state, &body, user_id).await,
        "semantic" => handle_semantic_query(&state, &body, user_id).await,
        "federated" => handle_federated_query(&state, &body, user_id).await,
        other => Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: format!(
                    "Unknown query mode: '{other}'. Use 'nl', 'cypher', 'graph', 'semantic', or 'federated'."
                ),
            }),
        )),
    }?;

    // If federated=true on any mode, also query peers and merge results
    if body.federated && body.mode != "federated" {
        if let Some(peer_results) = query_mesh_peers(&state, &body.query).await {
            result.results.extend(peer_results);
            result.count = result.results.len() as u64;
        }
    }

    Ok(result)
}

/// Natural language query: LLM translates to Cypher, then executes.
async fn handle_nl_query(
    state: &NodeState,
    body: &QueryRequest,
    user_id: &str,
) -> Result<Json<QueryResponse>, (StatusCode, Json<ErrorResponse>)> {
    let Some(ref neo4j_config) = state.neo4j else {
        return Err(service_unavailable("Neo4j not configured."));
    };
    let Some(ref llm_config) = state.llm else {
        return Err(service_unavailable(
            "LLM not configured — cannot translate natural language queries. Use mode='cypher' for direct Cypher.",
        ));
    };

    use prism_ingest::graph::Neo4jGraphStore;
    use prism_ingest::nl_query::NlQueryTranslator;

    let store = Neo4jGraphStore::new(neo4j_config.clone());
    let translator = NlQueryTranslator::new(llm_config.clone());

    // Get graph schema for the LLM
    let schema = store.schema().await.map_err(|e| {
        tracing::error!(error = %e, "failed to fetch graph schema");
        internal_error("Failed to read graph schema.")
    })?;

    // Translate NL → Cypher
    let translation = translator
        .translate(&body.query, &schema, body.limit)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, query = %body.query, "NL→Cypher translation failed");
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ErrorResponse {
                    error: format!("Could not translate query: {e}"),
                }),
            )
        })?;

    tracing::info!(
        cypher = %translation.cypher,
        explanation = %translation.explanation,
        "NL query translated"
    );

    // Execute the generated Cypher
    let results = store
        .query_cypher(&translation.cypher, None)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, cypher = %translation.cypher, "generated Cypher failed");
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ErrorResponse {
                    error: format!(
                        "Generated Cypher failed to execute: {e}. Try rephrasing your question."
                    ),
                }),
            )
        })?;

    let count = results.len() as u64;

    state.audit_and_broadcast(&prism_core::audit::AuditEntry {
        id: 0,
        timestamp: chrono::Utc::now(),
        user_id: user_id.to_string(),
        action: prism_core::audit::AuditAction::DataQuery,
        target: "nl".into(),
        detail: Some(format!("results={count}, cypher={}", translation.cypher)),
        outcome: prism_core::audit::AuditOutcome::Success,
    });

    Ok(Json(QueryResponse {
        results,
        count,
        mode: "nl".into(),
        translation: Some(TranslationInfo {
            cypher: translation.cypher,
            explanation: translation.explanation,
        }),
    }))
}

/// Direct Cypher query execution.
async fn handle_cypher_query(
    state: &NodeState,
    body: &QueryRequest,
    user_id: &str,
) -> Result<Json<QueryResponse>, (StatusCode, Json<ErrorResponse>)> {
    let Some(ref neo4j_config) = state.neo4j else {
        return Err(service_unavailable("Neo4j not configured."));
    };

    // Basic safety: reject write operations in the query endpoint
    let upper = body.query.to_uppercase();
    if upper.contains("DELETE")
        || upper.contains("CREATE")
        || upper.contains("MERGE")
        || upper.contains("SET ")
        || upper.contains("REMOVE ")
        || upper.contains("DROP ")
    {
        return Err((
            StatusCode::FORBIDDEN,
            Json(ErrorResponse {
                error: "Write operations not allowed via the query API.".into(),
            }),
        ));
    }

    use prism_ingest::graph::Neo4jGraphStore;
    let store = Neo4jGraphStore::new(neo4j_config.clone());

    let results = store.query_cypher(&body.query, None).await.map_err(|e| {
        tracing::error!(error = %e, "Cypher query failed");
        internal_error("Cypher execution failed.")
    })?;

    let count = results.len() as u64;

    state.audit_and_broadcast(&prism_core::audit::AuditEntry {
        id: 0,
        timestamp: chrono::Utc::now(),
        user_id: user_id.to_string(),
        action: prism_core::audit::AuditAction::DataQuery,
        target: "cypher".into(),
        detail: Some(format!("results={count}")),
        outcome: prism_core::audit::AuditOutcome::Success,
    });

    Ok(Json(QueryResponse {
        results,
        count,
        mode: "cypher".into(),
        translation: None,
    }))
}

/// Entity neighbor traversal by name.
async fn handle_graph_query(
    state: &NodeState,
    body: &QueryRequest,
    user_id: &str,
) -> Result<Json<QueryResponse>, (StatusCode, Json<ErrorResponse>)> {
    let Some(ref neo4j_config) = state.neo4j else {
        return Err(service_unavailable(
            "Neo4j not configured — is the node running with services?",
        ));
    };

    use prism_ingest::graph::{GraphStore, Neo4jGraphStore};
    let store = Neo4jGraphStore::new(neo4j_config.clone());
    let entity_set = store.neighbors(&body.query, 3).await.map_err(|e| {
        tracing::error!(error = %e, "Neo4j neighbor query failed");
        internal_error("Internal server error.")
    })?;

    let results: Vec<serde_json::Value> = entity_set
        .entities
        .iter()
        .map(|e| {
            serde_json::json!({
                "type": e.entity_type,
                "name": e.name,
                "properties": e.properties,
            })
        })
        .collect();

    let count = results.len() as u64;

    state.audit_and_broadcast(&prism_core::audit::AuditEntry {
        id: 0,
        timestamp: chrono::Utc::now(),
        user_id: user_id.to_string(),
        action: prism_core::audit::AuditAction::DataQuery,
        target: "graph".into(),
        detail: Some(format!("results={count}")),
        outcome: prism_core::audit::AuditOutcome::Success,
    });

    Ok(Json(QueryResponse {
        results,
        count,
        mode: "graph".into(),
        translation: None,
    }))
}

/// Vector similarity search via Qdrant.
async fn handle_semantic_query(
    state: &NodeState,
    body: &QueryRequest,
    user_id: &str,
) -> Result<Json<QueryResponse>, (StatusCode, Json<ErrorResponse>)> {
    let Some(ref qdrant_config) = state.qdrant else {
        return Err(service_unavailable(
            "Qdrant not configured — is the node running with services?",
        ));
    };
    let Some(ref llm_config) = state.llm else {
        return Err(service_unavailable(
            "LLM not configured — cannot generate embeddings for semantic search.",
        ));
    };

    use prism_ingest::embeddings::{QdrantVectorStore, VectorStore};
    use prism_ingest::ontology::LlmOntologyConstructor;

    // Generate an embedding for the query text via Ollama
    let constructor = LlmOntologyConstructor::new(llm_config.clone());
    let query_embedding = constructor.embed_text(&body.query).await.map_err(|e| {
        tracing::error!(error = %e, "failed to embed query text");
        internal_error("Failed to generate query embedding.")
    })?;

    let store = QdrantVectorStore::new(qdrant_config.clone());
    let hits = store
        .query(&query_embedding, body.limit)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "Qdrant search failed");
            internal_error("Semantic search failed.")
        })?;

    let results: Vec<serde_json::Value> = hits
        .iter()
        .map(|(id, score)| {
            serde_json::json!({
                "id": id,
                "score": score,
            })
        })
        .collect();

    let count = results.len() as u64;

    state.audit_and_broadcast(&prism_core::audit::AuditEntry {
        id: 0,
        timestamp: chrono::Utc::now(),
        user_id: user_id.to_string(),
        action: prism_core::audit::AuditAction::DataQuery,
        target: "semantic".into(),
        detail: Some(format!("results={count}")),
        outcome: prism_core::audit::AuditOutcome::Success,
    });

    Ok(Json(QueryResponse {
        results,
        count,
        mode: "semantic".into(),
        translation: None,
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
        translation: None,
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

fn service_unavailable(msg: &str) -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(ErrorResponse { error: msg.into() }),
    )
}

fn internal_error(msg: &str) -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse { error: msg.into() }),
    )
}
