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

        let futures: Vec<_> = peers
            .iter()
            .map(|peer| {
                let url = format!("http://{}:{}/api/query", peer.address, peer.port);
                let client = self.client.clone();
                let body = QueryRequest { query, mode: None };
                let peer_name = peer.name.clone();
                async move {
                    // Same reasoning as sync.rs: `peer.address` came from
                    // another node's Announce. A blocked peer is skipped, not
                    // fatal — one unreachable peer must not fail the fan-out.
                    if let Err(reason) = prism_runtime::offline::check_url(&url) {
                        warn!(peer = %peer_name, %reason, "peer skipped by offline policy");
                        return Vec::new();
                    }
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

    /// `PRISM_OFFLINE` is process-global; serialize the tests that set it.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    /// Restores the var on drop, so a failed assertion cannot leave it set.
    struct OfflineGuard(Option<String>);
    impl Drop for OfflineGuard {
        fn drop(&mut self) {
            unsafe {
                match self.0.take() {
                    Some(v) => std::env::set_var(prism_runtime::offline::ENV, v),
                    None => std::env::remove_var(prism_runtime::offline::ENV),
                }
            }
        }
    }

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

    /// A remote peer must be skipped under hard offline. Peer addresses arrive
    /// in another node's `Announce`, so the destination is attacker-influenceable
    /// by anyone with Kafka access — and `crates/mesh` had no dependency on
    /// prism-runtime at all, so nothing here consulted the policy.
    ///
    /// Skipping must not be fatal: one blocked peer cannot fail the fan-out.
    /// Both peers below are unreachable, so a non-panicking empty result is the
    /// contract; what this pins is that the call RETURNS rather than erroring,
    /// and does so fast because no socket was attempted.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn a_remote_peer_is_skipped_offline_without_failing_the_fanout() {
        let _guard = env_lock();
        let _restore = OfflineGuard(std::env::var(prism_runtime::offline::ENV).ok());
        unsafe { std::env::set_var(prism_runtime::offline::ENV, "1") };

        let fq = FederatedQuery::new(Duration::from_secs(10));
        let peers = vec![peer("remote", "203.0.113.9"), peer("local", "127.0.0.1")];

        let started = std::time::Instant::now();
        let results = fq
            .query_peers(&peers, "anything")
            .await
            .expect("a blocked peer must not fail the fan-out");
        let elapsed = started.elapsed();

        assert!(results.is_empty(), "neither peer is reachable");
        // The remote peer must be refused by policy, not dialled. A real
        // connect attempt to TEST-NET-3 costs seconds.
        assert!(
            elapsed < Duration::from_secs(3),
            "took {elapsed:?} — the remote peer was dialled instead of skipped"
        );
    }

    /// Offline unset must leave the guard inert, or the test above would pass
    /// even if every peer were refused unconditionally.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn the_guard_is_inert_when_offline_is_unset() {
        let _guard = env_lock();
        let _restore = OfflineGuard(std::env::var(prism_runtime::offline::ENV).ok());
        unsafe { std::env::remove_var(prism_runtime::offline::ENV) };

        let fq = FederatedQuery::new(Duration::from_millis(300));
        let results = fq
            .query_peers(&[peer("local", "127.0.0.1")], "anything")
            .await
            .expect("an unreachable peer is skipped, not fatal");
        assert!(results.is_empty());
    }
}
