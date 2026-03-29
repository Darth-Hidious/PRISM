//! PRISM mesh networking — node discovery, data pub/sub, and federated queries.
//!
//! Two discovery mechanisms:
//!
//! - **mDNS/DNS-SD** ([`mdns`]): Zero-config local-network discovery via `_prism._tcp.local`.
//! - **Platform** ([`platform_discovery`]): Cross-org discovery mediated by platform.marc27.com.
//!
//! Nodes publish datasets via [`subscription`] and subscribe to remote datasets.
//! Federated queries across the mesh are handled by [`federation`].

pub mod federation;
pub mod kafka;
pub mod mdns;
pub mod platform_discovery;
pub mod protocol;
pub mod subscription;
pub mod sync;

use std::sync::{Arc, RwLock};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── Configuration ──────────────────────────────────────────────────

/// How a node discovers peers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DiscoveryMethod {
    /// Local network mDNS broadcast/scan.
    Mdns,
    /// MARC27 platform-mediated discovery.
    Platform { url: String, token: String },
}

/// Configuration for joining the mesh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshConfig {
    pub node_name: String,
    pub publish_port: u16,
    pub discovery: Vec<DiscoveryMethod>,
}

// ── Peer tracking ──────────────────────────────────────────────────

/// A discovered peer node on the mesh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerNode {
    pub node_id: Uuid,
    pub name: String,
    pub address: String,
    pub port: u16,
    pub last_seen: DateTime<Utc>,
    pub capabilities: Vec<String>,
}

// ── Mesh handle ────────────────────────────────────────────────────

/// Runtime handle to the mesh — either online (participating) or offline.
#[derive(Debug, Clone)]
pub enum MeshHandle {
    /// Not connected to any mesh.
    Offline,

    /// Active mesh participant.
    Online {
        node_id: Uuid,
        config: MeshConfig,
        peers: Arc<RwLock<Vec<PeerNode>>>,
    },
}

impl MeshHandle {
    /// The node's unique ID, if online.
    pub fn node_id(&self) -> Option<Uuid> {
        match self {
            MeshHandle::Online { node_id, .. } => Some(*node_id),
            MeshHandle::Offline => None,
        }
    }

    /// Snapshot of currently known peers.
    pub fn peers(&self) -> Vec<PeerNode> {
        match self {
            MeshHandle::Online { peers, .. } => {
                peers.read().expect("peers lock poisoned").clone()
            }
            MeshHandle::Offline => Vec::new(),
        }
    }

    /// Add a peer to the known peer list.
    pub fn add_peer(&self, peer: PeerNode) {
        if let MeshHandle::Online { peers, .. } = self {
            peers.write().expect("peers lock poisoned").push(peer);
        }
    }

    /// Remove a peer by its node ID.
    pub fn remove_peer(&self, target: Uuid) {
        if let MeshHandle::Online { peers, .. } = self {
            peers
                .write()
                .expect("peers lock poisoned")
                .retain(|p| p.node_id != target);
        }
    }

    /// Number of currently known peers.
    pub fn peer_count(&self) -> usize {
        match self {
            MeshHandle::Online { peers, .. } => {
                peers.read().expect("peers lock poisoned").len()
            }
            MeshHandle::Offline => 0,
        }
    }
}

/// Initialize a mesh handle from the given configuration.
///
/// Generates a fresh UUID for this node and returns an `Online` handle
/// with an empty peer list. Discovery must be started separately.
pub fn init_mesh(config: MeshConfig) -> Result<MeshHandle> {
    let node_id = Uuid::new_v4();
    tracing::info!(%node_id, name = %config.node_name, "Mesh node initialized");
    Ok(MeshHandle::Online {
        node_id,
        config,
        peers: Arc::new(RwLock::new(Vec::new())),
    })
}

/// Options for [`start_mesh`].
#[derive(Debug, Clone)]
pub struct MeshStartOptions {
    /// Node name (used in mDNS TXT records).
    pub node_name: String,
    /// Port the node's HTTP API listens on.
    pub publish_port: u16,
    /// Whether to broadcast (announce) this node for discovery.
    /// When `false`, the node passively discovers peers but doesn't advertise itself.
    pub broadcast: bool,
    /// Capabilities to advertise (only used when `broadcast` is true).
    pub capabilities: Vec<String>,
    /// How often to re-scan for peers (seconds).
    pub discovery_interval_secs: u64,
    /// Optional channel to emit peer-change events (for live dashboard updates).
    pub event_tx: Option<tokio::sync::broadcast::Sender<String>>,
}

/// Start mesh networking as a background task.
///
/// - If `broadcast` is true, announces this node via mDNS.
/// - Runs periodic mDNS discovery on `discovery_interval_secs`.
/// - Updates the `MeshHandle`'s peer list automatically.
///
/// Returns a `JoinHandle` that runs until the `CancellationToken` is cancelled.
pub fn start_mesh(
    handle: MeshHandle,
    opts: MeshStartOptions,
    cancel: tokio_util::sync::CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let node_id = match handle.node_id() {
            Some(id) => id,
            None => {
                tracing::warn!("mesh start called on offline handle, exiting");
                return;
            }
        };

        // Set up mDNS
        let mut mdns = match mdns::MdnsDiscovery::new("prism", opts.publish_port) {
            Ok(m) => m,
            Err(e) => {
                tracing::error!(error = %e, "failed to create mDNS daemon, mesh disabled");
                return;
            }
        };

        // Announce only if broadcast is enabled
        if opts.broadcast {
            if let Err(e) = mdns.announce(node_id, &opts.node_name, &opts.capabilities) {
                tracing::warn!(error = %e, "mDNS announce failed (continuing without broadcast)");
            }
        }

        let mut interval =
            tokio::time::interval(std::time::Duration::from_secs(opts.discovery_interval_secs));
        interval.tick().await; // first tick is immediate

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    tracing::info!("mesh shutdown requested");
                    break;
                }
                _ = interval.tick() => {
                    match mdns.discover(std::time::Duration::from_secs(3)) {
                        Ok(discovered) => {
                            for peer in discovered {
                                // Only add if not already known
                                let known = handle.peers();
                                if !known.iter().any(|p| p.node_id == peer.node_id) {
                                    tracing::info!(
                                        peer_name = %peer.name,
                                        peer_id = %peer.node_id,
                                        "new mesh peer discovered"
                                    );
                                    // Emit event for dashboard
                                    if let Some(ref tx) = opts.event_tx {
                                        let event = serde_json::json!({
                                            "type": "MeshPeerChange",
                                            "data": {
                                                "action": "added",
                                                "node_id": peer.node_id.to_string(),
                                                "name": &peer.name,
                                            }
                                        });
                                        let _ = tx.send(event.to_string());
                                    }
                                    handle.add_peer(peer);
                                }
                            }
                        }
                        Err(e) => {
                            tracing::debug!(error = %e, "mDNS discovery scan failed");
                        }
                    }
                }
            }
        }

        // Clean shutdown
        if let Err(e) = mdns.stop() {
            tracing::debug!(error = %e, "mDNS shutdown error");
        }
    })
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn test_config() -> MeshConfig {
        MeshConfig {
            node_name: "test-node".into(),
            publish_port: 9100,
            discovery: vec![DiscoveryMethod::Mdns],
        }
    }

    fn test_peer(name: &str) -> PeerNode {
        PeerNode {
            node_id: Uuid::new_v4(),
            name: name.into(),
            address: "127.0.0.1".into(),
            port: 9100,
            last_seen: Utc::now(),
            capabilities: vec!["compute".into()],
        }
    }

    #[test]
    fn init_mesh_returns_real_uuid() {
        let handle = init_mesh(test_config()).unwrap();
        let id = handle.node_id().expect("should be online");
        // UUID v4 has version nibble = 4
        assert_eq!(id.get_version_num(), 4);
    }

    #[test]
    fn init_mesh_starts_with_no_peers() {
        let handle = init_mesh(test_config()).unwrap();
        assert_eq!(handle.peer_count(), 0);
        assert!(handle.peers().is_empty());
    }

    #[test]
    fn add_and_remove_peers() {
        let handle = init_mesh(test_config()).unwrap();
        let peer_a = test_peer("alpha");
        let peer_b = test_peer("beta");
        let id_a = peer_a.node_id;

        handle.add_peer(peer_a);
        handle.add_peer(peer_b);
        assert_eq!(handle.peer_count(), 2);

        handle.remove_peer(id_a);
        assert_eq!(handle.peer_count(), 1);
        assert_eq!(handle.peers()[0].name, "beta");
    }

    #[test]
    fn offline_handle_returns_none_and_empty() {
        let handle = MeshHandle::Offline;
        assert!(handle.node_id().is_none());
        assert!(handle.peers().is_empty());
        assert_eq!(handle.peer_count(), 0);
    }

    #[test]
    fn subscription_manager_publish_unpublish() {
        use subscription::*;

        let mut mgr = SubscriptionManager::new();
        mgr.publish(PublishedDataset {
            name: "alloy-db".into(),
            schema_version: "1.0".into(),
            subscribers: vec![],
        });
        mgr.publish(PublishedDataset {
            name: "phase-diagrams".into(),
            schema_version: "2.1".into(),
            subscribers: vec![],
        });
        assert_eq!(mgr.published().len(), 2);

        mgr.unpublish("alloy-db");
        assert_eq!(mgr.published().len(), 1);
        assert_eq!(mgr.published()[0].name, "phase-diagrams");
    }

    #[test]
    fn subscription_manager_subscribe_unsubscribe() {
        use subscription::*;

        let mut mgr = SubscriptionManager::new();
        let pub_node = Uuid::new_v4();
        let other_node = Uuid::new_v4();

        mgr.subscribe(Subscription {
            dataset_name: "alloy-db".into(),
            publisher_node: pub_node,
            subscribed_at: Utc::now(),
        });
        mgr.subscribe(Subscription {
            dataset_name: "alloy-db".into(),
            publisher_node: other_node,
            subscribed_at: Utc::now(),
        });
        assert_eq!(mgr.subscriptions().len(), 2);

        mgr.unsubscribe("alloy-db", pub_node);
        assert_eq!(mgr.subscriptions().len(), 1);
        assert_eq!(mgr.subscriptions()[0].publisher_node, other_node);
    }

    // ── New edge-case tests ─────────────────────────────────────────

    #[test]
    fn mesh_config_serde_roundtrip() {
        let cfg = MeshConfig {
            node_name: "roundtrip-node".into(),
            publish_port: 4242,
            discovery: vec![
                DiscoveryMethod::Mdns,
                DiscoveryMethod::Platform {
                    url: "https://platform.marc27.com".into(),
                    token: "tok-abc123".into(),
                },
            ],
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let parsed: MeshConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.node_name, cfg.node_name);
        assert_eq!(parsed.publish_port, cfg.publish_port);
        assert_eq!(parsed.discovery.len(), 2);
    }

    #[test]
    fn discovery_method_platform_serde_roundtrip() {
        let method = DiscoveryMethod::Platform {
            url: "https://example.com".into(),
            token: "secret-token".into(),
        };
        let json = serde_json::to_string(&method).unwrap();
        let parsed: DiscoveryMethod = serde_json::from_str(&json).unwrap();
        match parsed {
            DiscoveryMethod::Platform { url, token } => {
                assert_eq!(url, "https://example.com");
                assert_eq!(token, "secret-token");
            }
            _ => panic!("expected Platform variant"),
        }
    }

    #[test]
    fn peer_node_serde_roundtrip() {
        let peer = test_peer("serde-peer");
        let json = serde_json::to_string(&peer).unwrap();
        let parsed: PeerNode = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.node_id, peer.node_id);
        assert_eq!(parsed.name, peer.name);
        assert_eq!(parsed.address, peer.address);
        assert_eq!(parsed.port, peer.port);
        assert_eq!(parsed.capabilities, peer.capabilities);
    }

    #[test]
    fn init_mesh_generates_unique_ids() {
        let h1 = init_mesh(test_config()).unwrap();
        let h2 = init_mesh(test_config()).unwrap();
        let id1 = h1.node_id().unwrap();
        let id2 = h2.node_id().unwrap();
        assert_ne!(id1, id2, "two init_mesh calls must produce different UUIDs");
    }

    #[test]
    fn add_peer_on_offline_handle_is_noop() {
        let handle = MeshHandle::Offline;
        // Must not panic
        handle.add_peer(test_peer("ghost"));
        assert_eq!(handle.peer_count(), 0);
    }

    #[test]
    fn remove_peer_on_offline_handle_is_noop() {
        let handle = MeshHandle::Offline;
        // Must not panic
        handle.remove_peer(Uuid::new_v4());
        assert_eq!(handle.peer_count(), 0);
    }

    #[test]
    fn remove_peer_nonexistent_id_is_noop() {
        let handle = init_mesh(test_config()).unwrap();
        handle.add_peer(test_peer("alpha"));
        let phantom_id = Uuid::new_v4();
        handle.remove_peer(phantom_id);
        // The real peer is untouched
        assert_eq!(handle.peer_count(), 1);
    }

    #[test]
    fn add_many_peers_count_matches() {
        let handle = init_mesh(test_config()).unwrap();
        for i in 0..100 {
            handle.add_peer(test_peer(&format!("peer-{i}")));
        }
        assert_eq!(handle.peer_count(), 100);
    }

    #[test]
    fn peers_returns_snapshot_not_reference() {
        let handle = init_mesh(test_config()).unwrap();
        handle.add_peer(test_peer("first"));
        let snapshot = handle.peers();
        // Add another peer after taking the snapshot
        handle.add_peer(test_peer("second"));
        // The snapshot must not have grown
        assert_eq!(snapshot.len(), 1);
        // The live count reflects both peers
        assert_eq!(handle.peer_count(), 2);
    }
}
