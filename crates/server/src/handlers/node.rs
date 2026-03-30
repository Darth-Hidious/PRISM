use axum::extract::State;
use axum::response::Json;
use serde::Serialize;
use std::sync::Arc;

use crate::NodeState;

#[derive(Serialize)]
pub struct NodeInfo {
    pub name: String,
    pub version: &'static str,
    pub status: &'static str,
    pub services: Vec<ServiceInfo>,
    pub uptime_secs: u64,
}

#[derive(Serialize)]
pub struct ServiceInfo {
    pub name: String,
    pub status: String,
    pub port: u16,
}

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
}

pub async fn get_node_info(State(state): State<Arc<NodeState>>) -> Json<NodeInfo> {
    let uptime = state.started_at.elapsed().as_secs();
    let services = state
        .services
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|s| ServiceInfo {
            name: s.name.clone(),
            status: if s.healthy { "healthy" } else { "starting" }.to_string(),
            port: s.port,
        })
        .collect();

    Json(NodeInfo {
        name: state.node_name.clone(),
        version: env!("CARGO_PKG_VERSION"),
        status: "running",
        services,
        uptime_secs: uptime,
    })
}

pub async fn health_check() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}
