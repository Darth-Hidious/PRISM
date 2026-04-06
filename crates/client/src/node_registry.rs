use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::api::PlatformClient;

/// Result of registering a node through the REST fallback.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RegisteredNode {
    #[serde(alias = "id")]
    pub node_id: String,
    pub name: String,
    pub status: String,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub websocket_url: Option<String>,
}

/// Summary of a registered node returned by `GET /nodes`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeSummary {
    #[serde(alias = "id")]
    pub node_id: String,
    pub name: String,
    pub status: String,
    #[serde(default, alias = "last_seen_at")]
    pub last_seen: Option<String>,
    #[serde(default)]
    pub visibility: Option<String>,
    #[serde(default)]
    pub profile: Option<serde_json::Value>,
    #[serde(default)]
    pub price_per_hour_usd: Option<f64>,
}

/// Detailed node record returned by `GET /nodes/{id}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeDetail {
    #[serde(alias = "id")]
    pub node_id: String,
    pub name: String,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub org_id: Option<String>,
    #[serde(default)]
    pub visibility: Option<String>,
    #[serde(default)]
    pub profile: serde_json::Value,
    pub status: String,
    #[serde(default, alias = "last_seen_at")]
    pub last_seen: Option<String>,
    #[serde(default)]
    pub price_per_hour_usd: Option<f64>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

/// Public-key lookup response for node-to-node E2EE.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodePublicKey {
    #[serde(alias = "id")]
    pub node_id: String,
    pub name: String,
    pub public_key: String,
    pub algorithm: String,
}

/// Public-key exchange response for node-to-node E2EE.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KeyExchangeResponse {
    pub target_node_id: String,
    pub target_public_key: String,
    pub algorithm: String,
    pub your_public_key_received: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NodeListResponse {
    #[serde(default)]
    nodes: Vec<NodeSummary>,
    #[serde(default)]
    error: Option<String>,
}

/// Client for the MARC27 node-registry REST endpoints.
///
/// The WebSocket node loop is still the primary registration path for active nodes,
/// but the REST surface is now live and is the source of truth for discovery,
/// public-key exchange, and graceful lifecycle operations.
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
        let response: NodeListResponse = self.platform.get(&path).await?;
        if let Some(error) = response.error {
            return Err(anyhow!("failed to list platform nodes: {error}"));
        }
        Ok(response.nodes)
    }

    /// Fetch a single node record by id.
    pub async fn get_node(&self, node_id: &str) -> Result<NodeDetail> {
        let path = format!("/nodes/{node_id}");
        debug!(%path, "fetching node detail");
        self.platform
            .get(&path)
            .await
            .context("failed to fetch node detail")
    }

    /// Fetch the registered public key for a node.
    pub async fn get_public_key(&self, node_id: &str) -> Result<NodePublicKey> {
        let path = format!("/nodes/{node_id}/public-key");
        debug!(%path, "fetching node public key");
        self.platform
            .get(&path)
            .await
            .context("failed to fetch node public key")
    }

    /// Exchange our public key for the target node's public key.
    pub async fn exchange_key(
        &self,
        node_id: &str,
        public_key: &str,
    ) -> Result<KeyExchangeResponse> {
        let path = format!("/nodes/{node_id}/exchange-key");
        debug!(%path, "exchanging node public key");
        self.platform
            .post(
                &path,
                &serde_json::json!({
                    "public_key": public_key,
                }),
            )
            .await
            .context("failed to exchange node public key")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_list_response_parses_wrapped_nodes() {
        let payload = serde_json::json!({
            "nodes": [
                {
                    "id": "00000000-0000-4000-c000-000000000001",
                    "name": "lab-hpc-01",
                    "status": "online",
                    "last_seen_at": "2026-04-06T02:00:00Z",
                    "visibility": "org",
                    "profile": {"public_key": "abc"},
                    "price_per_hour_usd": 2.5
                }
            ],
            "count": 1
        });

        let response: NodeListResponse = serde_json::from_value(payload).unwrap();
        assert_eq!(response.nodes.len(), 1);
        let node = &response.nodes[0];
        assert_eq!(node.node_id, "00000000-0000-4000-c000-000000000001");
        assert_eq!(node.last_seen.as_deref(), Some("2026-04-06T02:00:00Z"));
        assert_eq!(node.visibility.as_deref(), Some("org"));
    }

    #[test]
    fn registered_node_parses_rest_shape() {
        let payload = serde_json::json!({
            "node_id": "00000000-0000-4000-c000-000000000001",
            "name": "lab-hpc-01",
            "status": "online"
        });

        let node: RegisteredNode = serde_json::from_value(payload).unwrap();
        assert_eq!(node.name, "lab-hpc-01");
        assert_eq!(node.status, "online");
        assert!(node.token.is_none());
        assert!(node.websocket_url.is_none());
    }

    #[test]
    fn key_exchange_response_parses_live_shape() {
        let payload = serde_json::json!({
            "target_node_id": "00000000-0000-4000-c000-000000000002",
            "target_public_key": "Zm9v",
            "algorithm": "x25519",
            "your_public_key_received": "YmFy"
        });

        let response: KeyExchangeResponse = serde_json::from_value(payload).unwrap();
        assert_eq!(response.algorithm, "x25519");
        assert_eq!(response.target_public_key, "Zm9v");
    }
}
