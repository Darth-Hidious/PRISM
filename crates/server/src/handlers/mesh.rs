//! Mesh discovery and federation handlers.

use axum::extract::State;
use axum::response::Json;
use serde::Serialize;
use std::sync::Arc;

use crate::NodeState;

#[derive(Serialize)]
pub struct MeshNode {
    pub id: String,
    pub name: String,
    pub address: String,
    pub port: u16,
    pub last_seen: String,
    pub capabilities: Vec<String>,
}

#[derive(Serialize)]
pub struct MeshStatus {
    pub online: bool,
    pub node_id: Option<String>,
    pub peer_count: usize,
    pub peers: Vec<MeshNode>,
}

#[derive(Serialize)]
pub struct SubscriptionInfo {
    pub dataset_name: String,
    pub publisher_node: String,
    pub subscribed_at: String,
}

#[derive(Serialize)]
pub struct PublishedInfo {
    pub name: String,
    pub schema_version: String,
    pub subscriber_count: usize,
}

#[derive(Serialize)]
pub struct SubscriptionsResponse {
    pub published: Vec<PublishedInfo>,
    pub subscribed: Vec<SubscriptionInfo>,
}

/// GET /api/mesh/nodes — list known mesh peers and mesh status.
pub async fn list_nodes(State(state): State<Arc<NodeState>>) -> Json<MeshStatus> {
    let mesh = state.mesh.read().unwrap_or_else(|e| e.into_inner());
    let online = mesh.node_id().is_some();
    let node_id = mesh.node_id().map(|id| id.to_string());
    let peers: Vec<MeshNode> = mesh
        .peers()
        .into_iter()
        .map(|p| MeshNode {
            id: p.node_id.to_string(),
            name: p.name,
            address: p.address,
            port: p.port,
            last_seen: p.last_seen.to_rfc3339(),
            capabilities: p.capabilities,
        })
        .collect();
    let peer_count = peers.len();

    Json(MeshStatus {
        online,
        node_id,
        peer_count,
        peers,
    })
}

/// POST /api/mesh/publish — publish a dataset for other nodes to subscribe to.
pub async fn publish_dataset(
    State(state): State<Arc<NodeState>>,
    Json(body): Json<PublishRequest>,
) -> Result<Json<PublishResponse>, (axum::http::StatusCode, String)> {
    use prism_mesh::subscription::PublishedDataset;

    let mut subs = state.subscriptions.write().unwrap_or_else(|e| e.into_inner());
    subs.publish(PublishedDataset {
        name: body.name.clone(),
        schema_version: body.schema_version.clone(),
        subscribers: vec![],
    });

    // TODO: announce via Kafka when producer is available on NodeState

    Ok(Json(PublishResponse {
        name: body.name,
        status: "published".to_string(),
    }))
}

/// POST /api/mesh/subscribe — subscribe to a dataset on a remote node.
pub async fn subscribe_dataset(
    State(state): State<Arc<NodeState>>,
    Json(body): Json<SubscribeRequest>,
) -> Result<Json<SubscribeResponse>, (axum::http::StatusCode, String)> {
    use prism_mesh::subscription::Subscription;

    let publisher = uuid::Uuid::parse_str(&body.publisher_node).map_err(|e| {
        (
            axum::http::StatusCode::BAD_REQUEST,
            format!("invalid publisher node ID: {e}"),
        )
    })?;

    let mut subs = state.subscriptions.write().unwrap_or_else(|e| e.into_inner());
    subs.subscribe(Subscription {
        dataset_name: body.dataset_name.clone(),
        publisher_node: publisher,
        subscribed_at: chrono::Utc::now(),
    });

    Ok(Json(SubscribeResponse {
        dataset_name: body.dataset_name,
        publisher_node: body.publisher_node,
        status: "subscribed".to_string(),
    }))
}

/// DELETE /api/mesh/subscribe — unsubscribe from a remote dataset.
pub async fn unsubscribe_dataset(
    State(state): State<Arc<NodeState>>,
    Json(body): Json<UnsubscribeRequest>,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, String)> {
    let publisher = uuid::Uuid::parse_str(&body.publisher_node).map_err(|e| {
        (
            axum::http::StatusCode::BAD_REQUEST,
            format!("invalid publisher node ID: {e}"),
        )
    })?;

    let mut subs = state.subscriptions.write().unwrap_or_else(|e| e.into_inner());
    subs.unsubscribe(&body.dataset_name, publisher);

    Ok(Json(serde_json::json!({
        "dataset_name": body.dataset_name,
        "status": "unsubscribed",
    })))
}

#[derive(serde::Deserialize)]
pub struct PublishRequest {
    pub name: String,
    #[serde(default = "default_schema_version")]
    pub schema_version: String,
}

fn default_schema_version() -> String {
    "1.0".to_string()
}

#[derive(Serialize)]
pub struct PublishResponse {
    pub name: String,
    pub status: String,
}

#[derive(serde::Deserialize)]
pub struct SubscribeRequest {
    pub dataset_name: String,
    pub publisher_node: String,
}

#[derive(Serialize)]
pub struct SubscribeResponse {
    pub dataset_name: String,
    pub publisher_node: String,
    pub status: String,
}

#[derive(serde::Deserialize)]
pub struct UnsubscribeRequest {
    pub dataset_name: String,
    pub publisher_node: String,
}

/// GET /api/mesh/subscriptions — list published datasets and active subscriptions.
pub async fn list_subscriptions(State(state): State<Arc<NodeState>>) -> Json<SubscriptionsResponse> {
    let subs = state.subscriptions.read().unwrap_or_else(|e| e.into_inner());

    let published = subs
        .published()
        .iter()
        .map(|d| PublishedInfo {
            name: d.name.clone(),
            schema_version: d.schema_version.clone(),
            subscriber_count: d.subscribers.len(),
        })
        .collect();

    let subscribed = subs
        .subscriptions()
        .iter()
        .map(|s| SubscriptionInfo {
            dataset_name: s.dataset_name.clone(),
            publisher_node: s.publisher_node.to_string(),
            subscribed_at: s.subscribed_at.to_rfc3339(),
        })
        .collect();

    Json(SubscriptionsResponse {
        published,
        subscribed,
    })
}
