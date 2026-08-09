//! Platform-mediated peer discovery ([`crate::DiscoveryMethod::Platform`]).
//!
//! Both machines of one owner register with the MARC27 node registry at
//! `node up`, so the org-scoped node list is an authenticated peer
//! directory — unlike mDNS, where "authenticated" is the mere PRESENCE of
//! a djb2 TXT hash whose own comment says it is not a security mechanism.
//!
//! The registry's own record carries no reachable address; the node's
//! registration CAPABILITIES payload is what can. `node up` now includes
//! `mesh_node_id` (the persisted mesh identity) and `mesh_advertise_url`
//! (`http://{lan ip}:{dashboard port}`) in that payload, and this module
//! reads them back out of each `NodeSummary.profile`. A node whose profile
//! does not carry both is skipped — an address is never invented.
//!
//! Whether the platform echoes the registration capabilities back through
//! `profile` is the platform's behaviour, not this repo's; when it does
//! not, discovery honestly yields nothing and logs why.

use anyhow::Result;
use chrono::Utc;
use uuid::Uuid;

use crate::PeerNode;

/// Capability keys `node up` registers and this module reads back.
pub const CAP_MESH_NODE_ID: &str = "mesh_node_id";
pub const CAP_MESH_ADVERTISE_URL: &str = "mesh_advertise_url";

/// Fetch the org's registered nodes and map the reachable ones to peers.
/// `our_mesh_id` filters this node's own registration out.
pub async fn discover_platform_peers(
    platform: &prism_client::PlatformClient,
    org_id: Option<&str>,
    our_mesh_id: Uuid,
) -> Result<Vec<PeerNode>> {
    let registry = prism_client::node_registry::NodeRegistryClient::new(platform);
    let nodes = registry.list_nodes(org_id).await?;
    let mut peers = Vec::new();
    let mut skipped = 0usize;
    for node in &nodes {
        match peer_from_profile(node.name.clone(), node.profile.as_ref()) {
            Some(peer) if peer.node_id != our_mesh_id => peers.push(peer),
            Some(_) => {} // our own registration
            None => skipped += 1,
        }
    }
    if skipped > 0 {
        tracing::debug!(
            skipped,
            "platform nodes without a mesh identity + advertised URL in their \
             profile were skipped (older registration, or the platform does \
             not echo capabilities into `profile`)"
        );
    }
    Ok(peers)
}

/// Map one registry profile to a peer, requiring BOTH the mesh identity and
/// the advertised URL. The capabilities may sit at the profile top level or
/// under a `capabilities` key, depending on how the platform stores them —
/// both are accepted, nothing is guessed.
fn peer_from_profile(name: String, profile: Option<&serde_json::Value>) -> Option<PeerNode> {
    let profile = profile?;
    let lookup = |key: &str| {
        profile
            .get(key)
            .or_else(|| profile.get("capabilities").and_then(|c| c.get(key)))
            .and_then(|v| v.as_str())
    };
    let node_id = Uuid::parse_str(lookup(CAP_MESH_NODE_ID)?.trim()).ok()?;
    let (address, port) = split_http_url(lookup(CAP_MESH_ADVERTISE_URL)?)?;
    Some(PeerNode {
        node_id,
        name,
        address,
        port,
        last_seen: Utc::now(),
        capabilities: vec!["platform".into()],
        // Org-scoped registry membership is a real authentication signal —
        // the platform verified the credential that registered this node.
        authenticated: true,
        auth_hash: None,
        trust: crate::PeerTrust::PlatformRegistry,
    })
}

/// `http://host:port` → (host, port). Anything else — https (the node API
/// is plain HTTP today), a path, a missing port — is rejected rather than
/// guessed at.
fn split_http_url(url: &str) -> Option<(String, u16)> {
    let rest = url.trim().strip_prefix("http://")?;
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let (host, port) = rest.rsplit_once(':')?;
    if host.is_empty() || host.contains('/') {
        return None;
    }
    Some((host.to_string(), port.parse().ok()?))
}

/// The URL a peer should advertise for this node, if the LAN address is
/// knowable. The UDP-connect trick never sends a packet — it only asks the
/// OS which local interface would route to a public address.
#[must_use]
pub fn advertise_url(dashboard_port: u16) -> Option<String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    let ip = socket.local_addr().ok()?.ip();
    if ip.is_unspecified() || ip.is_loopback() {
        return None;
    }
    Some(format!("http://{ip}:{dashboard_port}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_profile_with_both_capabilities_becomes_a_peer() {
        let id = Uuid::new_v4();
        for profile in [
            // Capabilities echoed at the profile top level…
            serde_json::json!({
                "mesh_node_id": id.to_string(),
                "mesh_advertise_url": "http://192.168.1.20:7327",
            }),
            // …or nested under a `capabilities` key.
            serde_json::json!({
                "capabilities": {
                    "mesh_node_id": id.to_string(),
                    "mesh_advertise_url": "http://192.168.1.20:7327",
                }
            }),
        ] {
            let peer = peer_from_profile("machine-b".into(), Some(&profile))
                .expect("a complete profile must map to a peer");
            assert_eq!(peer.node_id, id);
            assert_eq!(peer.address, "192.168.1.20");
            assert_eq!(peer.port, 7327);
            assert!(peer.authenticated);
            // The registry vouched for this address, so it may be shown
            // the platform credential — unlike an announced peer.
            assert_eq!(peer.trust, crate::PeerTrust::PlatformRegistry);
        }
    }

    /// No address is ever invented: missing id, missing URL, or an
    /// unparsable URL all mean "not reachable through the registry".
    #[test]
    fn incomplete_profiles_are_skipped_not_guessed() {
        let id = Uuid::new_v4().to_string();
        for profile in [
            serde_json::json!(null),
            serde_json::json!({}),
            serde_json::json!({ "mesh_node_id": id }),
            serde_json::json!({ "mesh_advertise_url": "http://10.0.0.4:7327" }),
            serde_json::json!({ "mesh_node_id": "junk", "mesh_advertise_url": "http://10.0.0.4:7327" }),
            serde_json::json!({ "mesh_node_id": id, "mesh_advertise_url": "https://10.0.0.4:7327" }),
            serde_json::json!({ "mesh_node_id": id, "mesh_advertise_url": "http://10.0.0.4" }),
            serde_json::json!({ "mesh_node_id": id, "mesh_advertise_url": "http://10.0.0.4:7327/api" }),
        ] {
            assert!(
                peer_from_profile("x".into(), Some(&profile)).is_none(),
                "profile must be skipped: {profile}"
            );
        }
        assert!(peer_from_profile("x".into(), None).is_none());
    }

    #[test]
    fn split_http_url_parses_exactly_the_shape_node_up_advertises() {
        assert_eq!(
            split_http_url("http://192.168.1.7:7327"),
            Some(("192.168.1.7".into(), 7327))
        );
        assert_eq!(
            split_http_url("http://192.168.1.7:7327/"),
            Some(("192.168.1.7".into(), 7327))
        );
        assert_eq!(split_http_url("192.168.1.7:7327"), None);
        assert_eq!(split_http_url("http://:7327"), None);
    }
}
