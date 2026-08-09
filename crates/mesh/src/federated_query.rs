//! Federated query execution across mesh nodes.

use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::PeerNode;

/// Coordinates federated query dispatch across peer nodes.
#[derive(Debug, Clone)]
pub struct FederatedQuery {
    client: reqwest::Client,
}

#[derive(Serialize)]
struct QueryRequest<'a> {
    query: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<&'a str>,
}

#[derive(Deserialize)]
struct QueryResponse {
    #[serde(default)]
    results: Vec<serde_json::Value>,
}

impl Default for FederatedQuery {
    fn default() -> Self {
        Self::new(Duration::from_secs(10))
    }
}

/// The peers a query may actually be sent to, paired with their URL.
///
/// Split out of `query_peers` so the POLICY is testable without a network or a
/// clock. The first version asserted the decision indirectly — "the fan-out
/// finished in under 3 s, so the remote peer must have been skipped" — which
/// measures the runtime's ability to schedule, not the guard. An adversarial
/// reviewer reproduced that as a real failure at 5.03 s under a concurrent
/// cargo build, against a path that returns in under 20 ms.
///
/// `peer.address` arrives in another node's `Announce`, so a blocked peer is
/// skipped rather than fatal: one refused peer must not fail a query across
/// all of them.
fn allowed_peers(peers: &[PeerNode]) -> Vec<(&PeerNode, String)> {
    peers
        .iter()
        .filter_map(|peer| {
            let url = format!("http://{}:{}/api/query", peer.address, peer.port);
            match prism_runtime::offline::check_url(&url) {
                Ok(()) => Some((peer, url)),
                Err(reason) => {
                    warn!(peer = %peer.name, %reason, "peer skipped by offline policy");
                    None
                }
            }
        })
        .collect()
}

impl FederatedQuery {
    pub fn new(timeout: Duration) -> Self {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .expect("failed to build reqwest client");
        Self { client }
    }

    /// Execute a query across the given peer nodes and collect merged results.
    ///
    /// Each peer is queried in parallel. Peers that fail or time out are skipped.
    pub async fn query_peers(
        &self,
        peers: &[PeerNode],
        query: &str,
    ) -> Result<Vec<serde_json::Value>> {
        if peers.is_empty() {
            return Ok(Vec::new());
        }

        debug!(peer_count = peers.len(), %query, "dispatching federated query");

        let futures: Vec<_> = allowed_peers(peers)
            .into_iter()
            .map(|(peer, url)| {
                let client = self.client.clone();
                let body = QueryRequest { query, mode: None };
                let peer_name = peer.name.clone();
                async move {
                    match client.post(&url).json(&body).send().await {
                        Ok(resp) => match resp.json::<QueryResponse>().await {
                            Ok(qr) => {
                                debug!(peer = %peer_name, results = qr.results.len(), "peer responded");
                                qr.results
                            }
                            Err(e) => {
                                warn!(peer = %peer_name, error = %e, "failed to parse peer response");
                                Vec::new()
                            }
                        },
                        Err(e) => {
                            warn!(peer = %peer_name, error = %e, "peer query failed");
                            Vec::new()
                        }
                    }
                }
            })
            .collect();

        let results: Vec<Vec<serde_json::Value>> = futures_util::future::join_all(futures).await;
        Ok(results.into_iter().flatten().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    use crate::{OfflineEnvGuard, test_env_lock};

    fn peer(name: &str, address: &str) -> PeerNode {
        PeerNode {
            node_id: Uuid::new_v4(),
            name: name.into(),
            address: address.into(),
            port: 9100,
            last_seen: Utc::now(),
            capabilities: vec!["compute".into()],
            authenticated: true,
            auth_hash: None,
        }
    }

    /// The policy is asserted DIRECTLY on `allowed_peers`, not inferred from
    /// how long a fan-out took. The previous version asserted `elapsed < 3s`,
    /// which measures scheduling rather than the decision — the same pattern a
    /// reviewer reproduced as a real 5.03 s failure in lib.rs.
    #[test]
    fn offline_drops_remote_peers_and_keeps_loopback() {
        let _guard = test_env_lock();
        let _restore = OfflineEnvGuard::capture();
        unsafe { std::env::set_var(prism_runtime::offline::ENV, "1") };

        let peers = vec![
            peer("remote", "203.0.113.9"),
            peer("local", "127.0.0.1"),
            peer("lan", "10.0.0.4"),
            peer("lookalike", "127.evil.example"),
        ];
        let allowed: Vec<&str> = allowed_peers(&peers)
            .into_iter()
            .map(|(p, _)| p.name.as_str())
            .collect();

        // Loopback survives — that is the carve-out, and it is now pinned
        // rather than inferred. A private LAN address is NOT loopback, and a
        // domain that merely starts "127." is attacker-controlled.
        assert_eq!(allowed, vec!["local"], "got {allowed:?}");
    }

    /// Offline unset must leave every peer allowed, or the test above would
    /// pass even if `allowed_peers` dropped everything unconditionally.
    #[test]
    fn every_peer_is_allowed_when_offline_is_unset() {
        let _guard = test_env_lock();
        let _restore = OfflineEnvGuard::capture();
        unsafe { std::env::remove_var(prism_runtime::offline::ENV) };

        let peers = vec![peer("remote", "203.0.113.9"), peer("local", "127.0.0.1")];
        assert_eq!(allowed_peers(&peers).len(), 2);
    }

    /// The URL the peer is actually queried on, pinned so a change to the
    /// format cannot silently bypass the check that reads it.
    #[test]
    fn the_checked_url_is_the_url_that_gets_queried() {
        let _guard = test_env_lock();
        let _restore = OfflineEnvGuard::capture();
        unsafe { std::env::remove_var(prism_runtime::offline::ENV) };

        let peers = vec![peer("local", "127.0.0.1")];
        let (_, url) = allowed_peers(&peers).remove(0);
        assert_eq!(url, "http://127.0.0.1:9100/api/query");
    }
}
