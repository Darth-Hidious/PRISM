//! mDNS-based local network node discovery using DNS-SD.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use uuid::Uuid;

use crate::PeerNode;

/// The DNS-SD service type for PRISM nodes.
const SERVICE_TYPE: &str = "_prism._tcp.local.";

/// Handles mDNS announcement and discovery on the local network.
pub struct MdnsDiscovery {
    daemon: ServiceDaemon,
    instance_name: Option<String>,
    port: u16,
}

impl MdnsDiscovery {
    /// Create a new mDNS discovery handle.
    pub fn new(_service_name: &str, port: u16) -> Result<Self> {
        let daemon = ServiceDaemon::new()
            .context("failed to create mDNS daemon")?;
        Ok(Self {
            daemon,
            instance_name: None,
            port,
        })
    }

    /// Announce this node on the local network via mDNS.
    pub fn announce(&mut self, node_id: Uuid, name: &str, capabilities: &[String]) -> Result<()> {
        let instance_name = format!("prism-{}", &node_id.to_string()[..8]);
        let mut properties = HashMap::new();
        properties.insert("node_id".to_string(), node_id.to_string());
        properties.insert("name".to_string(), name.to_string());
        properties.insert("caps".to_string(), capabilities.join(","));

        let service = ServiceInfo::new(
            SERVICE_TYPE,
            &instance_name,
            &format!("{instance_name}.local."),
            "",  // auto-detect IP
            self.port,
            properties,
        )
        .context("failed to create mDNS service info")?;

        self.daemon
            .register(service)
            .context("failed to register mDNS service")?;

        self.instance_name = Some(instance_name.clone());
        tracing::info!(%node_id, %name, port = self.port, "mDNS: announced as {instance_name}");
        Ok(())
    }

    /// Scan the local network for other PRISM nodes via mDNS.
    pub fn discover(&self, timeout: Duration) -> Result<Vec<PeerNode>> {
        let receiver = self
            .daemon
            .browse(SERVICE_TYPE)
            .context("failed to start mDNS browse")?;

        let mut peers = Vec::new();
        let deadline = std::time::Instant::now() + timeout;

        while std::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            match receiver.recv_timeout(remaining) {
                Ok(ServiceEvent::ServiceResolved(info)) => {
                    if let Some(peer) = service_info_to_peer(&info) {
                        // Skip self
                        if self.instance_name.as_deref() == Some(info.get_fullname().split('.').next().unwrap_or("")) {
                            continue;
                        }
                        tracing::debug!(peer_name = %peer.name, peer_id = %peer.node_id, "mDNS: discovered peer");
                        peers.push(peer);
                    }
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }

        // Stop browsing
        let _ = self.daemon.stop_browse(SERVICE_TYPE);
        Ok(peers)
    }

    /// Stop mDNS announcements and shut down the daemon.
    pub fn stop(self) -> Result<()> {
        if let Some(ref name) = self.instance_name {
            let fullname = format!("{name}.{SERVICE_TYPE}");
            let _ = self.daemon.unregister(&fullname);
            tracing::info!(service = %fullname, "mDNS: unregistered");
        }
        self.daemon.shutdown().context("failed to shut down mDNS daemon")?;
        Ok(())
    }
}

/// Convert a resolved mDNS service into a `PeerNode`.
fn service_info_to_peer(info: &ServiceInfo) -> Option<PeerNode> {
    let props = info.get_properties();
    let node_id_str = props.get_property_val_str("node_id")?;
    let node_id = Uuid::parse_str(node_id_str).ok()?;
    let name = props
        .get_property_val_str("name")
        .unwrap_or("unknown")
        .to_string();
    let caps_str = props.get_property_val_str("caps").unwrap_or("");
    let capabilities: Vec<String> = caps_str
        .split(',')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();

    let address = info
        .get_addresses()
        .iter()
        .next()
        .map(|a| a.to_string())
        .unwrap_or_else(|| "127.0.0.1".into());

    Some(PeerNode {
        node_id,
        name,
        address,
        port: info.get_port(),
        last_seen: Utc::now(),
        capabilities,
    })
}
