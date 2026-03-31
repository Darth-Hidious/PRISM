use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::api::PlatformClient;

/// A freshly registered node, including its authentication token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredNode {
    pub node_id: String,
    pub token: String,
    pub websocket_url: String,
}

/// Summary of a registered node (as returned by the list endpoint).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeSummary {
    pub node_id: String,
    pub name: String,
    pub status: String,
    #[serde(default)]
    pub last_seen: Option<String>,
}

/// Client for the MARC27 node-registry REST endpoints.
///
/// **Note:** The live API currently handles node lifecycle (register, heartbeat,
/// deregister) over WebSocket only. These REST methods are forward-looking —
/// the corresponding endpoints are planned but not yet deployed on marc27-core.
/// See `docs/marc27-api-discrepancies.md` for details.
#[derive(Debug)]
pub struct NodeRegistryClient<'a> {
    platform: &'a PlatformClient,
}

impl<'a> NodeRegistryClient<'a> {
    pub fn new(platform: &'a PlatformClient) -> Self {
        Self { platform }
    }

    /// Register a new compute node with the platform.
    pub async fn register_node(
        &self,
        name: &str,
        capabilities: &serde_json::Value,
    ) -> Result<RegisteredNode> {
        debug!(%name, "registering node");
        self.platform
            .post(
                "/nodes/register",
                &serde_json::json!({
                    "name": name,
                    "capabilities": capabilities,
                }),
            )
            .await
            .context("failed to register node")
    }

    /// List registered nodes, optionally filtered by organisation.
    pub async fn list_nodes(&self, org_id: Option<&str>) -> Result<Vec<NodeSummary>> {
        let path = match org_id {
            Some(id) => format!("/nodes?org_id={id}"),
            None => "/nodes".to_string(),
        };
        debug!(%path, "listing nodes");
        self.platform.get(&path).await
    }

    /// Deregister (remove) a node by its ID.
    pub async fn deregister_node(&self, node_id: &str) -> Result<()> {
        let path = format!("/nodes/{node_id}");
        debug!(%path, "deregistering node");
        self.platform.delete(&path).await
    }

    /// Send a heartbeat for a registered node.
    pub async fn heartbeat(&self, node_id: &str, status: &str, active_jobs: u32) -> Result<()> {
        let path = format!("/nodes/{node_id}/heartbeat");
        debug!(%path, %status, active_jobs, "heartbeat");
        let _: serde_json::Value = self
            .platform
            .post(
                &path,
                &serde_json::json!({
                    "status": status,
                    "active_jobs": active_jobs,
                }),
            )
            .await
            .context("heartbeat failed")?;
        Ok(())
    }
}
