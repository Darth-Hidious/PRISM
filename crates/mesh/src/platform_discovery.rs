//! Platform-based node discovery via platform.marc27.com.
//!
//! Uses [`prism_client::node_registry::NodeRegistryClient`] for all operations.

use anyhow::{Context, Result};
use chrono::Utc;
use uuid::Uuid;

use prism_client::api::PlatformClient;
use prism_client::node_registry::{NodeDetail, NodeRegistryClient};
use prism_proto::NodeCapabilities;

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

        let mut discovered = Vec::with_capacity(summaries.len());
        for summary in summaries {
            let Some(node_id) = Uuid::parse_str(&summary.node_id).ok() else {
                continue;
            };
            let detail = match self.registry.get_node(&summary.node_id).await {
                Ok(detail) => Some(detail),
                Err(error) => {
                    tracing::debug!(node_id = %summary.node_id, %error, "platform node detail unavailable");
                    None
                }
            };
            let (address, port, capabilities) = detail
                .as_ref()
                .map(detail_to_peer_fields)
                .unwrap_or_else(|| (String::new(), 0, Vec::new()));

            discovered.push(PeerNode {
                node_id,
                name: summary.name,
                address,
                port,
                last_seen: summary
                    .last_seen
                    .and_then(|ts| chrono::DateTime::parse_from_rfc3339(&ts).ok())
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(Utc::now),
                capabilities,
            });
        }

        Ok(discovered)
    }
}

fn detail_to_peer_fields(detail: &NodeDetail) -> (String, u16, Vec<String>) {
    let capabilities = serde_json::from_value::<NodeCapabilities>(detail.profile.clone()).ok();
    let http_endpoint = capabilities.as_ref().and_then(|caps| {
        caps.services
            .iter()
            // Prefer URLs that can plausibly back the node HTTP surface.
            .find_map(|service| service.endpoint.as_ref())
            .and_then(|endpoint| reqwest::Url::parse(endpoint).ok())
    });

    let address = http_endpoint
        .as_ref()
        .and_then(|url| url.host_str().map(ToOwned::to_owned))
        .unwrap_or_default();
    let port = http_endpoint
        .as_ref()
        .and_then(|url| url.port_or_known_default())
        .unwrap_or(0);
    let capability_list = capabilities
        .map(|caps| {
            let mut entries = caps
                .services
                .into_iter()
                .map(|service| format!("service:{}", service.kind))
                .collect::<Vec<_>>();
            entries.extend(
                caps.software
                    .into_iter()
                    .map(|software| format!("software:{software}")),
            );
            entries
        })
        .unwrap_or_default();

    (address, port, capability_list)
}

impl<'a> PlatformDiscovery<'a> {
    /// Deregister this node from the platform.
    pub async fn deregister(&self, node_id: &str) -> Result<()> {
        self.registry
            .deregister_node(node_id)
            .await
            .context("platform deregistration failed")
    }
}
