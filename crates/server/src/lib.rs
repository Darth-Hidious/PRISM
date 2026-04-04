// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! Embedded Axum HTTP/WebSocket server for PRISM nodes.
//!
//! Exposes a REST API and WebSocket endpoint for:
//!
//! - Node status and health monitoring.
//! - Dataset management and ingestion triggers.
//! - Graph and semantic queries.
//! - Tool execution and listing.
//! - Mesh discovery and subscription management.
//! - User management and audit log access.
//!
//! All routes are role-gated via the [`middleware`] layer. The server also hosts
//! the embedded web dashboard SPA (future).

pub mod handlers;
pub mod middleware;
pub mod router;
pub mod ws;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Instant;
use uuid::Uuid;

use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tracing::{error, info};

/// Events broadcast to connected WebSocket clients for live dashboard updates.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", content = "data")]
pub enum WsEvent {
    /// Periodic node status snapshot (uptime, service health).
    NodeStatusUpdate {
        uptime_secs: u64,
        services: Vec<ServiceSnapshot>,
    },
    /// A mesh peer was added or removed.
    MeshPeerChange {
        action: &'static str,
        node_id: String,
        name: String,
    },
    /// A new audit log entry was recorded.
    AuditEntry {
        timestamp: String,
        user: String,
        action: String,
    },
}

/// Snapshot of a service for WebSocket broadcast.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ServiceSnapshot {
    pub name: String,
    pub port: u16,
    pub healthy: bool,
}

/// Shared state for the PRISM node HTTP server.
pub struct NodeState {
    pub node_name: String,
    pub started_at: Instant,
    pub services: Mutex<Vec<ServiceEntry>>,
    /// Neo4j config — set when managed services are running.
    pub neo4j: Option<prism_ingest::Neo4jConfig>,
    /// Qdrant config — set when managed services are running.
    pub qdrant: Option<prism_ingest::QdrantConfig>,
    /// Path to the audit log SQLite database.
    pub audit_db_path: Option<PathBuf>,
    /// Path to the RBAC SQLite database.
    pub rbac_db_path: Option<PathBuf>,
    /// Path to the session SQLite database.
    pub session_db_path: Option<PathBuf>,
    /// In-memory tool registry (populated by scanning tool directories).
    pub tool_registry: RwLock<prism_core::registry::ToolRegistry>,
    /// Mesh handle for peer discovery.
    pub mesh: RwLock<prism_mesh::MeshHandle>,
    /// Subscription manager for pub/sub.
    pub subscriptions: RwLock<prism_mesh::subscription::SubscriptionManager>,
    /// LLM config for NL→Cypher query translation (Ollama or compatible).
    pub llm: Option<prism_ingest::LlmConfig>,
    /// Platform API client (set when node is registered with MARC27 platform).
    pub platform_client: Option<prism_client::PlatformClient>,
    /// Broadcast channel for live WebSocket updates to the dashboard.
    pub ws_broadcast: broadcast::Sender<String>,
    /// Current number of active WebSocket connections (used to enforce concurrency limit).
    pub ws_connections: AtomicUsize,
    /// Kafka producer for publishing mesh messages to other nodes (set once after init).
    pub kafka_producer: OnceLock<Arc<prism_mesh::kafka::MeshKafkaProducer>>,
    /// This node's unique ID on the mesh (set once after mesh init).
    pub node_id: OnceLock<Uuid>,
    /// Federated query client for dispatching queries to mesh peers.
    pub federation: OnceLock<prism_mesh::federation::FederatedQuery>,
}

/// A running service tracked by the server.
pub struct ServiceEntry {
    pub name: String,
    pub port: u16,
    pub healthy: bool,
}

impl NodeState {
    pub fn new(node_name: String) -> Self {
        let (ws_broadcast, _) = broadcast::channel(256);
        Self {
            node_name,
            started_at: Instant::now(),
            services: Mutex::new(Vec::new()),
            neo4j: None,
            qdrant: None,
            audit_db_path: None,
            rbac_db_path: None,
            session_db_path: None,
            tool_registry: RwLock::new(prism_core::registry::ToolRegistry::new()),
            mesh: RwLock::new(prism_mesh::MeshHandle::Offline),
            subscriptions: RwLock::new(prism_mesh::subscription::SubscriptionManager::new()),
            llm: None,
            platform_client: None,
            ws_broadcast,
            ws_connections: AtomicUsize::new(0),
            kafka_producer: OnceLock::new(),
            node_id: OnceLock::new(),
            federation: OnceLock::new(),
        }
    }

    /// Broadcast a [`WsEvent`] to all connected WebSocket clients.
    pub fn broadcast(&self, event: &WsEvent) {
        if let Ok(json) = serde_json::to_string(event) {
            // Ignore error — means no active receivers.
            let _ = self.ws_broadcast.send(json);
        }
    }

    /// Write an audit entry AND broadcast it to WebSocket clients.
    pub fn audit_and_broadcast(&self, entry: &prism_core::audit::AuditEntry) {
        // Write to SQLite
        if let Some(ref db_path) = self.audit_db_path {
            if let Ok(log) = prism_core::audit::AuditLog::new(db_path) {
                if let Err(e) = log.log(entry) {
                    tracing::warn!(error = %e, "failed to write audit entry");
                }
            }
        }
        // Broadcast to WebSocket clients
        self.broadcast(&WsEvent::AuditEntry {
            timestamp: entry.timestamp.to_rfc3339(),
            user: entry.user_id.clone(),
            action: format!("{}", entry.action),
        });
    }

    /// Update service list from orchestrator handles.
    pub fn update_services(&self, entries: Vec<ServiceEntry>) {
        *self.services.lock().unwrap_or_else(|e| e.into_inner()) = entries;
    }
}

/// Start the Axum HTTP server on the given port. Returns the actual bound address.
///
/// Also spawns a background task that broadcasts [`WsEvent::NodeStatusUpdate`]
/// every 5 seconds to keep the dashboard live.
pub async fn start_server(
    state: Arc<NodeState>,
    port: u16,
) -> anyhow::Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let app = router::build_router(state.clone());
    let listener = TcpListener::bind(format!("127.0.0.1:{port}")).await?;
    let addr = listener.local_addr()?;
    info!(%addr, "Dashboard server listening");

    // Periodic status broadcaster
    let ticker_state = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            interval.tick().await;
            let uptime_secs = ticker_state.started_at.elapsed().as_secs();
            let services = ticker_state
                .services
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .map(|s| ServiceSnapshot {
                    name: s.name.clone(),
                    port: s.port,
                    healthy: s.healthy,
                })
                .collect();
            ticker_state.broadcast(&WsEvent::NodeStatusUpdate {
                uptime_secs,
                services,
            });
        }
    });

    let handle = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            error!(%e, "Server exited with error");
        }
    });

    Ok((addr, handle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_state_update_services() {
        let state = NodeState::new("test-node".into());
        assert!(state.services.lock().unwrap().is_empty());
        state.update_services(vec![ServiceEntry {
            name: "neo4j".into(),
            port: 7687,
            healthy: true,
        }]);
        assert_eq!(state.services.lock().unwrap().len(), 1);
    }

    #[test]
    fn node_state_default_has_no_backends() {
        let state = NodeState::new("test".into());
        assert!(state.neo4j.is_none());
        assert!(state.qdrant.is_none());
    }

    #[test]
    fn node_state_with_backends() {
        let mut state = NodeState::new("test".into());
        state.neo4j = Some(prism_ingest::Neo4jConfig::default());
        state.qdrant = Some(prism_ingest::QdrantConfig::default());
        assert!(state.neo4j.is_some());
        assert!(state.qdrant.is_some());
    }
}
