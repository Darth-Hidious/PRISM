//! Platform-based node discovery via platform.marc27.com.
//!
//! Uses [`prism_client::node_registry::NodeRegistryClient`] for all operations.

use anyhow::{Context, Result};
use chrono::Utc;
use uuid::Uuid;

use prism_client::api::PlatformClient;
use prism_client::node_registry::NodeRegistryClient;

use crate::PeerNode;

/// Handles node registration and discovery through the MARC27 platform API.
pub struct PlatformDiscovery<'a> {
    registry: NodeRegistryClient<'a>,
}

impl<'a> PlatformDiscovery<'a> {
    /// Create a new platform discovery client from an authenticated `PlatformClient`.
    pub fn new(platform: &'a PlatformClient) -> Self {
        Self {
            registry: NodeRegistryClient::new(platform),
        }
    }

    /// Register this node with the platform for cross-org discovery.
    pub async fn register(
        &self,
        _node_id: Uuid,
        name: &str,
        capabilities: &[String],
    ) -> Result<String> {
        let caps = serde_json::json!({ "capabilities": capabilities });
        let registered = self
            .registry
            .register_node(name, &caps)
            .await
            .context("platform registration failed")?;
        tracing::info!(
            node_id = %registered.node_id,
            %name,
            "registered with MARC27 platform"
        );
        Ok(registered.node_id)
    }

    /// Discover peer nodes registered on the platform.
    pub async fn discover(&self, org_id: Option<&str>) -> Result<Vec<PeerNode>> {
        let summaries = self
            .registry
            .list_nodes(org_id)
            .await
            .context("failed to list platform nodes")?;

        let peers = summaries
            .into_iter()
            .filter_map(|s| {
                // Platform nodes may not have full address info yet.
                // Use the node_id as a UUID if parseable, otherwise skip.
                let node_id = Uuid::parse_str(&s.node_id).ok()?;
                Some(PeerNode {
                    node_id,
                    name: s.name,
                    address: String::new(), // filled by direct connection later
                    port: 0,
                    last_seen: s
                        .last_seen
                        .and_then(|ts| chrono::DateTime::parse_from_rfc3339(&ts).ok())
                        .map(|dt| dt.with_timezone(&Utc))
                        .unwrap_or_else(Utc::now),
                    capabilities: Vec::new(),
                })
            })
            .collect();

        Ok(peers)
    }

    /// Deregister this node from the platform.
    pub async fn deregister(&self, node_id: &str) -> Result<()> {
        self.registry
            .deregister_node(node_id)
            .await
            .context("platform deregistration failed")
    }
}
