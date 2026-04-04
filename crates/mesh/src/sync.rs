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

use crate::protocol::MeshMessage;
use crate::subscription::SubscriptionManager;
use crate::PeerNode;

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
                if !list.iter().any(|p| p.node_id == node_id) {
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
                // Track the subscriber in our published datasets
                let mut subs = subscriptions.write().unwrap_or_else(|e| e.into_inner());
                for d in subs.published_mut() {
                    if d.name == dataset_name && !d.subscribers.contains(&subscriber_id) {
                        d.subscribers.push(subscriber_id);
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
            MeshMessage::QueryForward {
                query_id,
                query,
                origin_node,
            } => {
                if origin_node == our_node_id {
                    continue;
                }
                debug!(
                    query_id = %query_id,
                    origin = %origin_node,
                    "received forwarded query from peer"
                );
                // Execute locally and respond — requires Neo4j
                if let Some(ref cfg) = sync_config {
                    let neo4j_url = format!("{}/db/neo4j/tx/commit", cfg.neo4j_url);
                    let neo4j_body = serde_json::json!({
                        "statements": [{
                            "statement": query,
                        }]
                    });
                    match client
                        .post(&neo4j_url)
                        .basic_auth(&cfg.neo4j_user, Some(&cfg.neo4j_pass))
                        .json(&neo4j_body)
                        .send()
                        .await
                    {
                        Ok(resp) => {
                            let results = resp.json::<serde_json::Value>().await.unwrap_or_default();
                            debug!(
                                query_id = %query_id,
                                "forwarded query executed, results ready"
                            );
                            // Note: QueryResult would be published back via Kafka
                            // if we had access to the producer here. For now, log it.
                            info!(
                                query_id = %query_id,
                                "query result available (direct REST federation preferred)"
                            );
                            let _ = results; // consumed by direct REST federation
                        }
                        Err(e) => {
                            warn!(
                                query_id = %query_id,
                                error = %e,
                                "failed to execute forwarded query"
                            );
                        }
                    }
                }
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
    // Query the peer's graph for all entities in the dataset
    let query_url = format!("{peer_url}/api/query");
    let body = serde_json::json!({
        "query": format!("MATCH (n) WHERE n.dataset = '{dataset_name}' RETURN n LIMIT 1000"),
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

    let data: serde_json::Value = resp.json().await?;
    let result_count = data["results"].as_array().map(|a| a.len()).unwrap_or(0);

    if result_count == 0 {
        debug!(dataset = %dataset_name, "no data returned from peer");
        return Ok(());
    }

    // Write to local Neo4j if configured
    if let Some(ref cfg) = sync_config {
        let neo4j_url = format!("{}/db/neo4j/tx/commit", cfg.neo4j_url);
        let results = data["results"].as_array().unwrap();

        // Batch-create nodes from peer data using UNWIND
        let cypher = format!(
            "UNWIND $rows AS row \
             MERGE (n:SyncedEntity {{name: row.name, source_dataset: '{dataset_name}'}}) \
             SET n += row.properties"
        );

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
                "parameters": { "rows": rows },
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
