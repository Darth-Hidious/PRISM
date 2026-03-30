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

        let results: Vec<Vec<serde_json::Value>> =
            futures_util::future::join_all(futures).await;
        Ok(results.into_iter().flatten().collect())
    }
}
