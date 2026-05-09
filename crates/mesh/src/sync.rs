//! Mesh data synchronisation — processes incoming Kafka messages and
//! syncs published dataset updates to the local knowledge graph.
//!
//! This is the "data plane" of the mesh: when a remote node publishes a
//! dataset update, the consumer receives a `DataPublish` message. This
//! module reacts to those messages by fetching the actual graph data from
//! the publishing node's REST API and upserting it into the local Neo4j.

use std::sync::{Arc, RwLock};

use anyhow::Result;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::PeerNode;
use crate::protocol::MeshMessage;
use crate::subscription::SubscriptionManager;

/// Configuration for the sync handler.
#[derive(Debug, Clone)]
pub struct SyncConfig {
    /// Local Neo4j HTTP endpoint for writing synced data.
    pub neo4j_url: String,
    pub neo4j_user: String,
    pub neo4j_pass: String,
}

/// Processes incoming mesh messages and performs data synchronisation.
///
/// Spawn this as a background task alongside the Kafka consumer.
pub async fn run_sync_handler(
    mut rx: mpsc::Receiver<MeshMessage>,
    peers: Arc<RwLock<Vec<PeerNode>>>,
    subscriptions: Arc<RwLock<SubscriptionManager>>,
    our_node_id: Uuid,
    sync_config: Option<SyncConfig>,
) {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("failed to build HTTP client for sync");

    info!("mesh sync handler started");

    while let Some(msg) = rx.recv().await {
        match msg {
            // ── Peer discovery messages ──────────────────────────
            MeshMessage::Announce {
                node_id,
                name,
                address,
                port,
                capabilities,
            } => {
                if node_id == our_node_id {
                    continue; // ignore our own announcements
                }
                // Reject obviously-abusive announces. Without these caps a
                // peer with Kafka access could announce themselves with
                // a 1 MB name (or N fake node_ids) and grow the in-memory
                // peers list until OOM. See Bug #57.
                const MAX_NAME_LEN: usize = 256;
                const MAX_ADDRESS_LEN: usize = 256;
                const MAX_CAPABILITIES: usize = 64;
                const MAX_CAPABILITY_LEN: usize = 256;
                const MAX_PEERS: usize = 10_000;
                if name.len() > MAX_NAME_LEN
                    || address.len() > MAX_ADDRESS_LEN
                    || capabilities.len() > MAX_CAPABILITIES
                    || capabilities.iter().any(|c| c.len() > MAX_CAPABILITY_LEN)
                {
                    warn!(
                        %node_id,
                        name_len = name.len(),
                        address_len = address.len(),
                        cap_count = capabilities.len(),
                        "rejecting Announce: field length exceeds caps"
                    );
                    continue;
                }
                info!(node = %name, id = %node_id, "peer announced");
                let peer = PeerNode {
                    node_id,
                    name,
                    address,
                    port,
                    last_seen: chrono::Utc::now(),
                    capabilities,
                };
                let mut list = peers.write().unwrap_or_else(|e| e.into_inner());
                if list.iter().any(|p| p.node_id == node_id) {
                    // Already known — refresh `last_seen`. Without this an
                    // honest peer that drops Kafka briefly and rejoins
                    // would have stale `last_seen` until the next
                    // announce, while we'd miss the update.
                    if let Some(existing) = list.iter_mut().find(|p| p.node_id == node_id) {
                        existing.last_seen = peer.last_seen;
                    }
                } else if list.len() >= MAX_PEERS {
                    warn!(
                        peer_count = list.len(),
                        max = MAX_PEERS,
                        "rejecting Announce: peer list at capacity (Bug #57 DoS guard)"
                    );
                } else {
                    list.push(peer);
                }
            }

            MeshMessage::Goodbye { node_id } => {
                if node_id == our_node_id {
                    continue;
                }
                info!(%node_id, "peer departed");
                let mut list = peers.write().unwrap_or_else(|e| e.into_inner());
                list.retain(|p| p.node_id != node_id);
            }

            // ── Dataset publication ─────────────────────────────
            MeshMessage::DataPublish {
                node_id,
                dataset_name,
                schema_version,
                ..
            } => {
                if node_id == our_node_id {
                    continue;
                }

                // Check if we're subscribed to this dataset from this node
                let is_subscribed = {
                    let subs = subscriptions.read().unwrap_or_else(|e| e.into_inner());
                    subs.subscriptions()
                        .iter()
                        .any(|s| s.dataset_name == dataset_name && s.publisher_node == node_id)
                };

                if !is_subscribed {
                    debug!(
                        dataset = %dataset_name,
                        publisher = %node_id,
                        "ignoring DataPublish — not subscribed"
                    );
                    continue;
                }

                info!(
                    dataset = %dataset_name,
                    publisher = %node_id,
                    version = %schema_version,
                    "syncing subscribed dataset update"
                );

                // Find the peer's address
                let peer_addr = {
                    let list = peers.read().unwrap_or_else(|e| e.into_inner());
                    list.iter()
                        .find(|p| p.node_id == node_id)
                        .map(|p| format!("http://{}:{}", p.address, p.port))
                };

                if let Some(addr) = peer_addr {
                    if let Err(e) =
                        sync_dataset_from_peer(&client, &addr, &dataset_name, &sync_config).await
                    {
                        error!(
                            dataset = %dataset_name,
                            error = %e,
                            "failed to sync dataset from peer"
                        );
                    }
                } else {
                    warn!(
                        publisher = %node_id,
                        "subscribed but peer address unknown — cannot sync"
                    );
                }
            }

            // ── Subscription tracking ───────────────────────────
            MeshMessage::DataSubscribe {
                subscriber_id,
                dataset_name,
            } => {
                if subscriber_id == our_node_id {
                    continue;
                }
                debug!(
                    subscriber = %subscriber_id,
                    dataset = %dataset_name,
                    "remote node subscribed to our dataset"
                );
                // Track the subscriber in our published datasets — but cap
                // the list. Without this a peer could fan in N fake
                // subscriber_ids per dataset and grow the per-publication
                // Vec<Uuid> indefinitely. Same DoS class as Bug #57 — the
                // peer-list cap protects the receiving side, this protects
                // the publisher side.
                const MAX_SUBSCRIBERS_PER_DATASET: usize = 10_000;
                let mut subs = subscriptions.write().unwrap_or_else(|e| e.into_inner());
                for d in subs.published_mut() {
                    if d.name == dataset_name && !d.subscribers.contains(&subscriber_id) {
                        if d.subscribers.len() >= MAX_SUBSCRIBERS_PER_DATASET {
                            warn!(
                                dataset = %dataset_name,
                                count = d.subscribers.len(),
                                "rejecting DataSubscribe: subscriber list at cap"
                            );
                        } else {
                            d.subscribers.push(subscriber_id);
                        }
                    }
                }
            }

            MeshMessage::DataUnsubscribe {
                subscriber_id,
                dataset_name,
            } => {
                if subscriber_id == our_node_id {
                    continue;
                }
                debug!(
                    subscriber = %subscriber_id,
                    dataset = %dataset_name,
                    "remote node unsubscribed from our dataset"
                );
                let mut subs = subscriptions.write().unwrap_or_else(|e| e.into_inner());
                for d in subs.published_mut() {
                    if d.name == dataset_name {
                        d.subscribers.retain(|id| *id != subscriber_id);
                    }
                }
            }

            // ── Query forwarding (federated search) ──────────
            //
            // **Disabled**: this code path used to execute the peer's
            // raw Cypher string against the local Neo4j with no
            // verification, no allow-list, and no auth — any peer in
            // the Kafka mesh could send an arbitrary `MATCH/DELETE`
            // statement and our graph would obey. This was
            // remote-attacker-controlled, full graph-DB compromise of
            // the receiving node (see Bug #46).
            //
            // The Kafka federated-query path was already marked
            // "direct REST federation preferred" in the result-return
            // comment below, so wholesale dropping it is consistent
            // with the architectural direction. Direct REST
            // federation goes through the server's regular auth and
            // RBAC layers; this path bypassed both.
            //
            // If a future protocol revision wants to re-enable Kafka
            // query forwarding, the prerequisites are: (a) F1 chunk 4
            // verify_peer wiring (Bug #33), (b) an allow-list of
            // read-only Cypher patterns, (c) an explicit per-org
            // policy gate. Until all three exist, ignore the message.
            MeshMessage::QueryForward {
                query_id,
                origin_node,
                ..
            } => {
                if origin_node == our_node_id {
                    continue;
                }
                warn!(
                    query_id = %query_id,
                    origin = %origin_node,
                    "ignoring QueryForward — Kafka federated-query path disabled \
                     pending peer verification (Bug #46)"
                );
            }

            MeshMessage::QueryResult { query_id, results } => {
                debug!(
                    query_id = %query_id,
                    result_count = results.as_array().map(|a| a.len()).unwrap_or(0),
                    "received query result from peer"
                );
                // Direct REST federation handles this — Kafka path is for async results
            }

            _ => {
                debug!("unhandled mesh message type");
            }
        }
    }

    info!("mesh sync handler stopped");
}

/// Fetch graph data from a peer's query API and write to local Neo4j.
async fn sync_dataset_from_peer(
    client: &reqwest::Client,
    peer_url: &str,
    dataset_name: &str,
    sync_config: &Option<SyncConfig>,
) -> Result<()> {
    // Reject malformed dataset names up front — defense-in-depth before
    // we hand the value to a Cypher parameter binder that *should* be
    // safe but isn't worth trusting blindly. Marketplace and platform
    // dataset names are simple identifiers in practice; if a peer
    // publishes something exotic, refuse to sync rather than send it
    // through the query pipeline.
    if !dataset_name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        || dataset_name.is_empty()
        || dataset_name.len() > 128
    {
        anyhow::bail!(
            "refusing to sync dataset with invalid name (must be ASCII alphanumeric + _-., \
             non-empty, ≤128 chars): {dataset_name:?}"
        );
    }

    // Query the peer's graph for all entities in the dataset.
    // Use a parameterized Cypher query — earlier code interpolated
    // `dataset_name` straight into the query string, which let a
    // malicious peer (or a peer with a buggy publish call) inject
    // Cypher via the dataset name they publish. See Bug #45.
    let query_url = format!("{peer_url}/api/query");
    let body = serde_json::json!({
        "query": "MATCH (n) WHERE n.dataset = $dataset RETURN n LIMIT 1000",
        "params": { "dataset": dataset_name },
        "mode": "cypher",
    });

    let resp = client.post(&query_url).json(&body).send().await?;

    if !resp.status().is_success() {
        anyhow::bail!(
            "peer query returned HTTP {}: {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        );
    }

    // Body size cap — a malicious or runaway peer could otherwise
    // return gigabytes and OOM the local node. The peer query has
    // a `LIMIT 1000` clause, so a well-behaved peer stays well under
    // any reasonable cap. 32 MiB is far more headroom than the
    // limit needs but stops the obvious DoS vector. See Bug #52.
    const MAX_PEER_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
    if let Some(len) = resp.content_length()
        && len as usize > MAX_PEER_RESPONSE_BYTES
    {
        anyhow::bail!(
            "peer response too large ({} bytes; max {})",
            len,
            MAX_PEER_RESPONSE_BYTES
        );
    }
    let body_bytes = resp.bytes().await?;
    if body_bytes.len() > MAX_PEER_RESPONSE_BYTES {
        anyhow::bail!(
            "peer response too large ({} bytes; max {})",
            body_bytes.len(),
            MAX_PEER_RESPONSE_BYTES
        );
    }
    let data: serde_json::Value = serde_json::from_slice(&body_bytes)?;
    let result_count = data["results"].as_array().map(|a| a.len()).unwrap_or(0);

    if result_count == 0 {
        debug!(dataset = %dataset_name, "no data returned from peer");
        return Ok(());
    }

    // Write to local Neo4j if configured
    if let Some(cfg) = sync_config {
        let neo4j_url = format!("{}/db/neo4j/tx/commit", cfg.neo4j_url);
        let Some(results) = data["results"].as_array() else {
            return Ok(());
        };

        // Batch-create nodes from peer data using UNWIND.
        //
        // Both `$rows` AND `$source` are passed as parameters — the
        // earlier version interpolated `dataset_name` into the
        // statement string, which would have let a malicious peer
        // execute arbitrary Cypher against our LOCAL Neo4j (e.g. a
        // dataset named `x' DETACH DELETE n RETURN '` would wipe the
        // local graph). See Bug #45.
        let cypher = "UNWIND $rows AS row \
             MERGE (n:SyncedEntity {name: row.name, source_dataset: $source}) \
             SET n += row.properties";

        let rows: Vec<serde_json::Value> = results
            .iter()
            .filter_map(|r| {
                Some(serde_json::json!({
                    "name": r.get("name")?.as_str()?,
                    "properties": r,
                }))
            })
            .collect();

        let neo4j_body = serde_json::json!({
            "statements": [{
                "statement": cypher,
                "parameters": { "rows": rows, "source": dataset_name },
            }]
        });

        let neo4j_resp = client
            .post(&neo4j_url)
            .basic_auth(&cfg.neo4j_user, Some(&cfg.neo4j_pass))
            .json(&neo4j_body)
            .send()
            .await?;

        if neo4j_resp.status().is_success() {
            info!(
                dataset = %dataset_name,
                synced = rows.len(),
                "dataset synced to local Neo4j"
            );
        } else {
            warn!(
                dataset = %dataset_name,
                status = %neo4j_resp.status(),
                "Neo4j sync write returned error"
            );
        }
    } else {
        info!(
            dataset = %dataset_name,
            results = result_count,
            "dataset fetched from peer (no local Neo4j configured)"
        );
    }

    Ok(())
}
