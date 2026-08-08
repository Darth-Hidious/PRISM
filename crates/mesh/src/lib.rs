// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! PRISM mesh networking — node discovery, data pub/sub, and federated queries.
//!
//! Discovery mechanism:
//!
//! - **mDNS/DNS-SD** ([`mdns`]): Zero-config local-network discovery via `_prism._tcp.local`.
//!
//! Nodes publish datasets via [`subscription`] and subscribe to remote datasets.
//! Federated queries across the mesh are handled by [`federated_query`].
//!
//! Cross-org **federation primitives** (peer identity, request signing,
//! transitive root-CA trust via the MARC27 platform) live in
//! [`federation`].

pub mod federated_query;
pub mod federation;
pub mod federation_lookup;
pub mod kafka;
pub mod mdns;
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
    /// Optional Kafka broker addresses for mesh pub/sub (e.g., "localhost:9092").
    #[serde(default)]
    pub kafka_brokers: Option<String>,
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
    /// Whether this peer presented an auth token in mDNS TXT records.
    /// Unauthenticated peers are discovered but NOT trusted — their
    /// requests are rejected at the federation layer.
    #[serde(default)]
    pub authenticated: bool,
    /// Short hash of the peer's auth token (for logging/debugging).
    #[serde(default, skip_serializing)]
    pub auth_hash: Option<String>,
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
            MeshHandle::Online { peers, .. } => peers.read().expect("peers lock poisoned").clone(),
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

    /// Shared handle to the peer list (for passing to sync handler, etc.).
    pub fn peers_shared(&self) -> Option<Arc<RwLock<Vec<PeerNode>>>> {
        match self {
            MeshHandle::Online { peers, .. } => Some(Arc::clone(peers)),
            MeshHandle::Offline => None,
        }
    }

    /// Number of currently known peers.
    pub fn peer_count(&self) -> usize {
        match self {
            MeshHandle::Online { peers, .. } => peers.read().expect("peers lock poisoned").len(),
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
    /// Auth token from `prism login`. If `None`, the mesh refuses to
    /// start — you cannot join the mesh without authenticating to
    /// MARC27 first. This is the RBAC gate: the token proves the
    /// user's org/project/roles, which are checked before any peer
    /// interaction is allowed.
    pub auth_token: Option<String>,
}

/// Start mesh networking as a background task.
///
/// - If `broadcast` is true, announces this node via mDNS.
/// - Runs periodic mDNS discovery on `discovery_interval_secs`.
/// - Updates the `MeshHandle`'s peer list automatically.
///
/// Returns a `JoinHandle` that runs until the `CancellationToken` is cancelled.
/// `PRISM_OFFLINE` is process-global; serialize every test in this CRATE that
/// mutates it. One lock, shared — `federated_query.rs` uses this too.
///
/// Two files each declaring their own `static LOCK` for the same variable is
/// the shape that has bitten this codebase five times: the locks do not know
/// about each other, they compile into one test binary, and cargo's runner is
/// multi-threaded, so they serialize nothing across files. Pattern borrowed
/// from `prism-agent`'s `skills::TEST_ENV_LOCK`.
///
/// **Re-exported, not declared.** Being the only lock in THIS binary is not
/// enough: the moment a file here reaches for
/// `prism_runtime::offline::test_support::env_lock()` instead, the two stop
/// excluding each other. That is exactly how prism-cli broke after nine
/// previous fixes to this same shape. Aliasing makes both spellings one mutex.
#[cfg(test)]
pub(crate) use prism_runtime::offline::test_support::ENV_LOCK as TEST_ENV_LOCK;

#[cfg(test)]
pub(crate) fn test_env_lock() -> std::sync::MutexGuard<'static, ()> {
    TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

/// Restores `PRISM_OFFLINE` on drop, so a failed assertion cannot leave it set
/// for the rest of the test binary.
#[cfg(test)]
pub(crate) struct OfflineEnvGuard(Option<String>);

#[cfg(test)]
impl OfflineEnvGuard {
    pub(crate) fn capture() -> Self {
        Self(std::env::var(prism_runtime::offline::ENV).ok())
    }
}

#[cfg(test)]
impl Drop for OfflineEnvGuard {
    fn drop(&mut self) {
        unsafe {
            match self.0.take() {
                Some(v) => std::env::set_var(prism_runtime::offline::ENV, v),
                None => std::env::remove_var(prism_runtime::offline::ENV),
            }
        }
    }
}

/// Why the mesh will not start, if it will not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MeshRefusal {
    /// Hard offline. mDNS announce is link-local multicast — it never leaves
    /// the LAN, but it broadcasts this node's name, capabilities and
    /// auth-token hash to every device on it, and `discover` then pulls peers
    /// in. An operator who set PRISM_OFFLINE asked not to participate in a
    /// network, not "remote hosts only".
    Offline,
    /// The mesh is a trusted network: you cannot join without proving identity.
    /// The auth token carries org/project/roles, checked by every peer.
    NotAuthenticated,
}

/// The start decision, split out of `start_mesh` so it is testable without a
/// clock.
///
/// The first version of this test asserted the gate indirectly — "the spawned
/// task joined within 5 s, so it must have refused" — which measures the
/// runtime's ability to schedule, not the decision. An adversarial reviewer
/// reproduced that as a real failure at 5.03 s under a concurrent cargo build,
/// against a path that returns in under 20 ms. A 250x margin was not enough,
/// because wall-clock was the wrong observable.
///
/// Offline is checked FIRST: it is the operator's explicit instruction, and it
/// should not depend on whether they also happen to be logged in.
pub(crate) fn mesh_start_refusal(opts: &MeshStartOptions) -> Option<MeshRefusal> {
    if prism_runtime::offline::enabled() {
        return Some(MeshRefusal::Offline);
    }
    if opts.auth_token.is_none() {
        return Some(MeshRefusal::NotAuthenticated);
    }
    None
}

pub fn start_mesh(
    handle: MeshHandle,
    opts: MeshStartOptions,
    cancel: tokio_util::sync::CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Some(refusal) = mesh_start_refusal(&opts) {
            match refusal {
                MeshRefusal::Offline => tracing::info!("mesh disabled: offline mode"),
                MeshRefusal::NotAuthenticated => {
                    // States the fact, not a command to go and type. Every
                    // surface that renders this (app, TUI, CLI) must offer
                    // authentication in place; telling a human to quit and run
                    // something is the defect this product keeps being called
                    // out for.
                    eprintln!("\x1b[33m  ⚠ Mesh disabled: not authenticated.\x1b[0m");
                    eprintln!(
                        "\x1b[33m    The mesh requires platform authentication for RBAC enforcement.\x1b[0m"
                    );
                }
            }
            return;
        }

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
        if opts.broadcast
            && let Err(e) = mdns.announce(
                node_id,
                &opts.node_name,
                &opts.capabilities,
                opts.auth_token.as_deref(),
            )
        {
            tracing::warn!(error = %e, "mDNS announce failed (continuing without broadcast)");
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
                                // ── RBAC gate: reject unauthenticated peers ──
                                // Only authenticated peers (those with an
                                // auth hash in their mDNS TXT records) are
                                // added to the peer list. Unauthenticated
                                // peers are logged but not trusted.
                                if !peer.authenticated {
                                    tracing::warn!(
                                        peer_name = %peer.name,
                                        peer_id = %peer.node_id,
                                        "discovered peer is NOT authenticated — refusing to add to mesh. \
                                         The peer must authenticate before joining."
                                    );
                                    continue;
                                }

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
            kafka_brokers: None,
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
            authenticated: true,
            auth_hash: None,
        }
    }

    /// The gate is asserted DIRECTLY, not inferred from how fast a task joined.
    ///
    /// The previous version spawned `start_mesh` and asserted it finished
    /// within 5 s. That measures the runtime's ability to schedule: a reviewer
    /// reproduced a real failure at 5.03 s under a concurrent cargo build,
    /// against a path that returns in under 20 ms. Wall-clock was the wrong
    /// observable, and a 250x margin did not save it.
    #[test]
    fn offline_refuses_the_mesh_before_authentication_is_even_considered() {
        let _guard = test_env_lock();
        let _restore = OfflineEnvGuard::capture();
        unsafe { std::env::set_var(prism_runtime::offline::ENV, "1") };

        // Authenticated, so NotAuthenticated cannot be what refuses — otherwise
        // this would pass for the wrong reason.
        let opts = MeshStartOptions {
            auth_token: Some("test-token".into()),
            ..bare_opts()
        };
        assert_eq!(mesh_start_refusal(&opts), Some(MeshRefusal::Offline));

        // And offline outranks a missing token: an operator's explicit
        // instruction should not depend on whether they happen to be logged in.
        let anon = MeshStartOptions {
            auth_token: None,
            ..bare_opts()
        };
        assert_eq!(mesh_start_refusal(&anon), Some(MeshRefusal::Offline));
    }

    /// Online: the pre-existing RBAC gate still refuses an anonymous node, and
    /// an authenticated one is allowed to start. Without this the test above
    /// would pass even if the function refused unconditionally.
    #[test]
    fn online_keeps_the_rbac_gate_and_allows_an_authenticated_node() {
        let _guard = test_env_lock();
        let _restore = OfflineEnvGuard::capture();
        unsafe { std::env::remove_var(prism_runtime::offline::ENV) };

        let anon = MeshStartOptions {
            auth_token: None,
            ..bare_opts()
        };
        assert_eq!(
            mesh_start_refusal(&anon),
            Some(MeshRefusal::NotAuthenticated)
        );

        let ok = MeshStartOptions {
            auth_token: Some("test-token".into()),
            ..bare_opts()
        };
        assert_eq!(mesh_start_refusal(&ok), None, "nothing should refuse");
    }

    fn bare_opts() -> MeshStartOptions {
        MeshStartOptions {
            node_name: "offline-test".into(),
            publish_port: 9100,
            broadcast: true,
            capabilities: vec!["compute".into()],
            discovery_interval_secs: 3600,
            event_tx: None,
            auth_token: None,
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
            kafka_brokers: Some("localhost:9092".into()),
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
