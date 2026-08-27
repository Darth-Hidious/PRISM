//! Mesh data synchronisation — processes incoming Kafka messages and
//! syncs published dataset updates to the local knowledge graph.
//!
//! This is the "data plane" of the mesh: when a remote node publishes a
//! dataset update, the consumer receives a `DataPublish` message. This
//! module reacts to those messages by fetching the actual graph data from
//! the publishing node's REST API (authenticating via [`PeerSessions`])
//! and writing it as EMMO facts into the bundled Turso provenance store
//! under the publisher's own tenant ([`mesh_tenant`]), so peer-synced data
//! never blends with locally ingested data — or with another peer's.
//!
//! The same fetch is reachable without Kafka through
//! [`sync_dataset_from_peer`], which `prism mesh sync` calls directly.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use anyhow::Result;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::peer_session::{PeerAddress, PeerSessions};
use crate::protocol::MeshMessage;
use crate::subscription::SubscriptionManager;
use crate::{PeerNode, PeerTrust};

/// Tenant under which facts synced from one publishing peer land in the
/// bundled Turso store. Distinct from the local-ingest tenant ("local") so
/// synced data stays attributable, and PER PEER — under the old shared
/// `"mesh"` tenant two peers publishing a same-named dataset produced the
/// SAME assertion id and corroborated each other.
///
/// `prism-provenance` treats any `mesh:`-prefixed tenant as a relay
/// (`is_relay`), so peer-conveyed origins keep landing in the namespaced
/// `mesh:…` evidence keyspace.
#[must_use]
pub fn mesh_tenant(publisher: &Uuid) -> String {
    format!("mesh:{publisher}")
}

/// Configuration for the sync handler.
#[derive(Debug, Clone)]
pub struct SyncConfig {
    /// Path to the bundled Turso provenance store where peer-synced facts
    /// are written (tenant [`mesh_tenant`]).
    pub provenance_db: PathBuf,
}

/// Processes incoming mesh messages and performs data synchronisation.
///
/// Spawn this as a background task alongside the Kafka consumer.
/// `sessions` authenticates every peer fetch — without it the peer's
/// `auth_stack` answers 401 and nothing ever arrives.
pub async fn run_sync_handler(
    mut rx: mpsc::Receiver<MeshMessage>,
    peers: Arc<RwLock<Vec<PeerNode>>>,
    subscriptions: Arc<RwLock<SubscriptionManager>>,
    our_node_id: Uuid,
    sync_config: Option<SyncConfig>,
    sessions: Arc<PeerSessions>,
) {
    let client = sync_http_client();

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
                    authenticated: true,
                    auth_hash: None,
                    // A Kafka Announce is writable by anyone with broker
                    // access: the owner's platform credential is never
                    // shown to an address learned this way.
                    trust: PeerTrust::Announced,
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

                // Find the peer's address, carrying the trust of the
                // channel that discovered it — a registry-vouched peer may
                // be shown the platform credential, an announced one never.
                let peer_addr = {
                    let list = peers.read().unwrap_or_else(|e| e.into_inner());
                    list.iter()
                        .find(|p| p.node_id == node_id)
                        .map(PeerAddress::of_peer)
                };

                if let Some(addr) = peer_addr {
                    if let Err(e) = sync_dataset_from_peer(
                        &client,
                        &addr,
                        &dataset_name,
                        node_id,
                        &sync_config,
                        &sessions,
                    )
                    .await
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

/// The HTTP client every sync fetch uses. Redirects are refused: the
/// request carries a session token in a header reqwest's cross-host
/// scrubbing does NOT strip (`X-Session-Token` is not `Authorization`), so
/// a hostile 307 could bounce it off-box.
#[must_use]
pub fn sync_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("failed to build HTTP client for sync")
}

/// Fetch graph data from a peer's query API and write it as EMMO facts
/// into the bundled Turso store under the publisher's tenant
/// ([`mesh_tenant`]). Returns how many entities were written.
///
/// Transport-agnostic: the Kafka sync handler calls this on `DataPublish`,
/// and `prism mesh sync <dataset> --peer <url>` calls it directly, so a
/// pull no longer requires a Kafka broker. The [`PeerAddress`] carries how
/// the peer's address was learned; [`PeerSessions`] refuses to show the
/// owner's platform credential to an announced address.
pub async fn sync_dataset_from_peer(
    client: &reqwest::Client,
    peer: &PeerAddress,
    dataset_name: &str,
    publisher: Uuid,
    sync_config: &Option<SyncConfig>,
    sessions: &PeerSessions,
) -> Result<usize> {
    // Reject malformed dataset names up front — defense-in-depth before
    // we hand the value to the parameterized Turso writer, which *should*
    // be safe but isn't worth trusting blindly. Marketplace and platform
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

    // Query the peer's graph for entities related to the dataset. The
    // "cypher" mode this used to send was removed with the Neo4j
    // retirement; "graph" mode is a name/substring lookup over the peer's
    // knowledge graph and returns the same {type, name, properties} rows.
    // The dataset name travels as plain JSON data, never spliced into a
    // query language string (Bug #45 stays fixed by construction).
    let query_url = format!("{}/api/query", peer.url);
    // Hard offline. The peer address may be one another node ANNOUNCED over
    // the mesh, so it is attacker-influenceable by any peer with Kafka
    // access — the destination is not ours to trust. `check_url` rather
    // than `enabled()`: a loopback peer is legitimate. (What the peer may
    // be SHOWN is a separate decision, keyed on `peer.trust` inside
    // `PeerSessions`.)
    prism_runtime::offline::check_url(&query_url).map_err(|r| anyhow::anyhow!(r))?;
    let body = serde_json::json!({
        "query": dataset_name,
        "mode": "graph",
        "limit": 1000,
    });

    // Authenticated fetch: mint (or reuse) a session on the peer, and on a
    // 401 — an expired cached session — re-mint exactly once and retry.
    let mut session = sessions.session_for(peer).await?;
    let mut resp = client
        .post(&query_url)
        .header("X-Session-Token", &session)
        .json(&body)
        .send()
        .await?;
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        debug!(peer = %peer.url, "peer query 401 — re-minting session and retrying once");
        sessions.invalidate(peer);
        session = sessions.session_for(peer).await?;
        resp = client
            .post(&query_url)
            .header("X-Session-Token", &session)
            .json(&body)
            .send()
            .await?;
    }

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
        return Ok(0);
    }

    // Write into the bundled Turso store if configured. Every value —
    // entity names, labels, the dataset name, properties — is bound
    // through parameterized SQL, never spliced into a statement string, so
    // a malicious peer cannot inject via the data it serves (the same
    // property the old parameterized Cypher write preserved; see Bug #45).
    let Some(cfg) = sync_config else {
        info!(
            dataset = %dataset_name,
            results = result_count,
            "dataset fetched from peer (no local provenance store configured — nothing written)"
        );
        return Ok(0);
    };
    let Some(results) = data["results"].as_array() else {
        return Ok(0);
    };

    let tenant = mesh_tenant(&publisher);
    let store = prism_provenance::ProvenanceStore::open(&cfg.provenance_db).await?;
    let now = chrono::Utc::now().to_rfc3339();
    let prov = prism_provenance::LocalProvenance {
        activity_id: Uuid::new_v4().to_string(),
        agent_id: "prism-mesh-sync".into(),
        agent_kind: "SoftwareAgent".into(),
        source_entity_id: format!("{}#{dataset_name}", peer.url),
        source_kind: "Dataset".into(),
        tenant: tenant.clone(),
        started_at: now.clone(),
        ended_at: now,
        locality: "mesh".into(),
        // Per-row: rows that name their origin get it set below.
        origin_source_id: None,
        // The agent tool call that launched this sync, when one did. A
        // sync run from cron or by hand has no launching action and stays
        // `None`; the per-row clone below inherits whichever it is.
        origin_action_id: prism_provenance::action_id_from_env(),
    };
    store.record_activity(&prov).await?;

    // Each peer row lands as a typed entity under the peer's own label
    // with the properties it served, linked to its source dataset — the
    // old write kept only `name`, so a peer's `Phase` arrived as a bare
    // mislabeled node.
    let mut synced = 0usize;
    for row in results {
        let Some(name) = row.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        // Carry the ORIGIN the peer conveyed for this row, so a fact
        // relayed by two peers from two genuinely different sources can
        // corroborate. A row without one stays `origin_source_id: None`
        // and collapses onto `mesh:unattributed` — never invented.
        let row_prov = prism_provenance::LocalProvenance {
            origin_source_id: peer_row_origin(row),
            ..prov.clone()
        };
        store
            .write_synced_entity_with_identity(
                name,
                peer_row_entity_type(row),
                peer_row_label(row),
                peer_row_class_iri(row),
                peer_row_props(row),
                dataset_name,
                &row_prov,
            )
            .await?;
        synced += 1;
    }

    info!(
        dataset = %dataset_name,
        synced,
        tenant = %tenant,
        "dataset synced to local Turso store"
    );

    Ok(synced)
}

/// Peer-supplied entity labels are untrusted input. Anything that is not a
/// plain identifier collapses to the generic `Entity` — mislabeling costs
/// less than letting a peer mint arbitrary label strings into the store.
const MAX_LABEL_LEN: usize = 64;

fn peer_row_identifier<'a>(row: &'a serde_json::Value, field: &str) -> Option<&'a str> {
    let value = row.get(field)?.as_str()?.trim();
    (!value.is_empty()
        && value.len() <= MAX_LABEL_LEN
        && value.chars().all(|c| c.is_ascii_alphanumeric()))
    .then_some(value)
}

fn peer_row_label(row: &serde_json::Value) -> &str {
    peer_row_identifier(row, "type").unwrap_or("Entity")
}

/// New peers send the declared extraction type separately. An old peer has
/// only `type`, whose historical meaning is the storage label, so that label
/// is also the most honest declared-type fallback available.
fn peer_row_entity_type(row: &serde_json::Value) -> &str {
    peer_row_identifier(row, "entity_type").unwrap_or_else(|| peer_row_label(row))
}

/// Canonical IRIs are optional peer input. Keep only bounded absolute IRI
/// strings with a syntactically valid ASCII scheme and no whitespace or
/// control characters; absent or malformed input remains honestly unknown.
const MAX_CLASS_IRI_LEN: usize = 2_048;

fn peer_row_class_iri(row: &serde_json::Value) -> Option<&str> {
    let iri = row.get("class_iri")?.as_str()?.trim();
    if iri.is_empty()
        || iri.len() > MAX_CLASS_IRI_LEN
        || iri.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return None;
    }
    let (scheme, remainder) = iri.split_once(':')?;
    let mut chars = scheme.chars();
    let valid_scheme = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    (valid_scheme && !remainder.is_empty()).then_some(iri)
}

/// Peer-supplied properties, kept only when they are a non-empty JSON
/// object of sane size. 8 KiB per row is generous for graph-node metadata
/// and stops a hostile peer from parking megabytes in `props_json` (the
/// 32 MiB body cap bounds the fetch, not the per-row write).
const MAX_PROPS_BYTES: usize = 8 * 1024;

fn peer_row_props(row: &serde_json::Value) -> Option<String> {
    let props = row.get("properties")?.as_object()?;
    if props.is_empty() {
        return None;
    }
    let json = serde_json::to_string(props).ok()?;
    (json.len() <= MAX_PROPS_BYTES).then_some(json)
}

/// A peer-supplied origin locator may be junk of any size — it is
/// attacker-influenceable input. Anything overlong is treated as
/// unattributed rather than trusted; 512 bytes is generous for a DOI, URL,
/// or file path (the Announce caps upstream use 256 for names/addresses).
const MAX_ORIGIN_LEN: usize = 512;

/// The origin locator one peer row conveys, if any: `properties.origin_source`
/// as a non-empty string within [`MAX_ORIGIN_LEN`].
///
/// This is the ONLY thing the peer payload can say about origin today —
/// `{type, name, properties}` rows historically shipped `properties: {}`,
/// which cannot express one, so rows from older peers (or entities whose
/// origin the peer does not know) return `None` and stay conservatively
/// `mesh:unattributed`. The string is a locator (e.g. `doi:10.x/y`, a URL),
/// NOT a pre-normalized key: `origin_source_key` normalization is not
/// idempotent, so the receiver must be the one to derive the key.
///
/// The value is peer-chosen and untrusted. Constraining it further here
/// would not add safety: `prism-provenance` namespaces every relayed origin
/// key under `mesh:…`, so no string a peer picks can collide with a
/// locally-derived source key or with `mesh:unattributed`.
fn peer_row_origin(row: &serde_json::Value) -> Option<String> {
    let origin = row
        .get("properties")?
        .get("origin_source")?
        .as_str()?
        .trim();
    (!origin.is_empty() && origin.len() <= MAX_ORIGIN_LEN).then(|| origin.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OfflineEnvGuard, test_env_lock};
    use std::io::{Read, Write};

    /// Minimal loopback peer: answers `POST /api/sessions` with a session
    /// and `POST /api/query` with the given full HTTP response. Serves
    /// until the test binary exits.
    fn spawn_mock_peer(query_response: String) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let base = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                // Read until the full headers+body arrived (tiny requests;
                // stop when the body length matches Content-Length).
                while let Ok(n) = stream.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf);
                    if let Some(header_end) = text.find("\r\n\r\n") {
                        let content_length = text
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(str::trim)
                                    .map(String::from)
                            })
                            .and_then(|v| v.parse::<usize>().ok())
                            .unwrap_or(0);
                        if buf.len() >= header_end + 4 + content_length {
                            break;
                        }
                    }
                }
                let request = String::from_utf8_lossy(&buf);
                let response = if request.starts_with("POST /api/sessions") {
                    let body = r#"{"session_id":"sess-mock"}"#;
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    )
                } else {
                    query_response.clone()
                };
                let _ = stream.write_all(response.as_bytes());
            }
        });
        base
    }

    fn http_response(status: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    /// Tempfile-backed Turso DB, removed (with SQLite journal sidecars) on drop.
    struct TempDb {
        path: PathBuf,
    }

    impl TempDb {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("prism_mesh_sync_test_{}.db", Uuid::new_v4()));
            Self { path }
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let mut p = self.path.clone().into_os_string();
                p.push(suffix);
                let _ = std::fs::remove_file(p);
            }
        }
    }

    /// The guard `9926eac0` described as the more consequential of the two had
    /// NO test at all — a reviewer's point that stood: best-covered path was
    /// not highest-risk path. `peer_url` is built from an address another node
    /// ANNOUNCED, so this refuses a destination we do not control.
    ///
    /// `sync` propagates rather than skipping (unlike the fan-out): an explicit
    /// single-dataset sync should say why it refused.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn a_remote_peer_sync_is_refused_offline() {
        let _guard = test_env_lock();
        let _restore = OfflineEnvGuard::capture();
        unsafe { std::env::set_var(prism_runtime::offline::ENV, "1") };

        let client = reqwest::Client::new();
        let err = sync_dataset_from_peer(
            &client,
            &PeerAddress::operator_named("http://203.0.113.9:9100"),
            "ds",
            Uuid::new_v4(),
            &None,
            &PeerSessions::new(None),
        )
        .await
        .expect_err("offline must refuse a remote peer");
        let msg = err.to_string();
        assert!(msg.contains("offline mode"), "{msg}");
        assert!(
            msg.contains("203.0.113.9"),
            "must name what it blocked: {msg}"
        );
    }

    /// A loopback peer is not refused by policy — it fails on connect instead.
    /// Without this, the test above would pass even if the guard refused every
    /// peer unconditionally.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn a_loopback_peer_sync_is_not_refused_by_policy() {
        let _guard = test_env_lock();
        let _restore = OfflineEnvGuard::capture();
        unsafe { std::env::set_var(prism_runtime::offline::ENV, "1") };

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(400))
            .build()
            .expect("client");
        let err = sync_dataset_from_peer(
            &client,
            &PeerAddress::operator_named("http://127.0.0.1:1"),
            "ds",
            Uuid::new_v4(),
            &None,
            &PeerSessions::new(None),
        )
        .await
        .expect_err("nothing is listening on port 1");
        assert!(
            !err.to_string().contains("offline mode"),
            "loopback must not be refused by policy: {err}"
        );
    }

    /// Two peers relaying the SAME dataset name land under their own
    /// tenants (`mesh:{publisher}`) — under the old shared `"mesh"` tenant
    /// they produced one assertion id and corroborated each other. Each
    /// write keeps the peer's label and its conveyed origin.
    #[tokio::test]
    async fn two_publishers_land_under_their_own_tenants() {
        let peer = spawn_mock_peer(http_response(
            "200 OK",
            r#"{"results":[{"type":"Matter","entity_type":"Phase","class_iri":"https://w3id.org/emmo#EMMO_example_phase","name":"alpha phase","properties":{"origin_source":"doi:10.1234/abc"}}],"count":1,"mode":"graph"}"#,
        ));
        let db = TempDb::new();
        let cfg = Some(SyncConfig {
            provenance_db: db.path.clone(),
        });
        let client = sync_http_client();
        let sessions = PeerSessions::new(None);
        let publisher_a = Uuid::new_v4();
        let publisher_b = Uuid::new_v4();

        for publisher in [publisher_a, publisher_b] {
            let synced = sync_dataset_from_peer(
                &client,
                &PeerAddress::operator_named(&peer),
                "alpha",
                publisher,
                &cfg,
                &sessions,
            )
            .await
            .expect("sync must succeed against the mock peer");
            assert_eq!(synced, 1);
        }

        // The expected tenant is the publisher-qualified STRING, written
        // out — deriving it from `mesh_tenant()` alone would let a
        // regression to the shared "mesh" tenant pass by matching its own
        // output (a mutation proved exactly that).
        let store = prism_provenance::ProvenanceStore::open(&db.path)
            .await
            .unwrap();
        for publisher in [publisher_a, publisher_b] {
            let tenant = format!("mesh:{publisher}");
            assert_eq!(mesh_tenant(&publisher), tenant);
            let nodes = store
                .graph_search("alpha phase", &tenant, 10)
                .await
                .unwrap();
            let node = nodes
                .iter()
                .find(|node| node.name == "alpha phase")
                .expect("peer entity must be present");
            assert_eq!(node.label, "Matter", "storage identity must survive sync");
            assert_eq!(node.entity_type, "Phase", "declared type must survive sync");
            assert_eq!(
                node.class_iri.as_deref(),
                Some("https://w3id.org/emmo#EMMO_example_phase"),
                "canonical class identity must survive sync"
            );
            // Each tenant's assertion carries exactly ONE evidence row with
            // the mesh-namespaced origin — the peers never corroborated
            // each other.
            let evidence = store
                .assertion_evidence(&tenant, "alpha phase", "SYNCED_FROM", "alpha")
                .await
                .unwrap();
            assert_eq!(evidence.len(), 1, "no cross-peer corroboration");
            assert_eq!(evidence[0].source_key, "mesh:doi:10.1234/abc");
        }
        // And nothing may land under the legacy shared tenant.
        let stray = store.graph_search("alpha phase", "mesh", 10).await.unwrap();
        assert!(
            stray.is_empty(),
            "peer data landed under the shared legacy tenant: {stray:?}"
        );
    }

    /// An authentication failure must be an error naming the status — the
    /// old pull deserialised the 401 body into an empty result set and
    /// reported a successful sync of nothing.
    #[tokio::test]
    async fn a_peer_401_surfaces_as_an_error_not_an_empty_sync() {
        let peer = spawn_mock_peer(http_response(
            "401 Unauthorized",
            r#"{"error":"unauthorized"}"#,
        ));
        let client = sync_http_client();
        let err = sync_dataset_from_peer(
            &client,
            &PeerAddress::operator_named(&peer),
            "ds",
            Uuid::new_v4(),
            &None,
            &PeerSessions::new(None),
        )
        .await
        .expect_err("a 401 peer must fail the sync");
        assert!(
            err.to_string().contains("401"),
            "the error must carry the status: {err}"
        );
    }

    /// Peer-supplied labels are untrusted: only plain identifiers survive,
    /// everything else lands as the generic `Entity`.
    #[test]
    fn peer_row_label_accepts_identifiers_and_rejects_junk() {
        let row = |t: serde_json::Value| serde_json::json!({ "type": t, "name": "x" });
        assert_eq!(peer_row_label(&row("Phase".into())), "Phase");
        assert_eq!(peer_row_label(&row("  Matter ".into())), "Matter");
        assert_eq!(peer_row_label(&row("".into())), "Entity");
        assert_eq!(peer_row_label(&row("has spaces".into())), "Entity");
        assert_eq!(peer_row_label(&row("a'; DROP--".into())), "Entity");
        assert_eq!(peer_row_label(&row("x".repeat(65).into())), "Entity");
        assert_eq!(peer_row_label(&row(serde_json::json!(7))), "Entity");
        assert_eq!(peer_row_label(&serde_json::json!({"name": "x"})), "Entity");
    }

    #[test]
    fn peer_classification_fields_are_additive_with_old_peer_fallbacks() {
        let classified = serde_json::json!({
            "type": "Matter",
            "entity_type": "Alloy",
            "class_iri": "https://w3id.org/emmo#EMMO_example_alloy",
        });
        assert_eq!(peer_row_label(&classified), "Matter");
        assert_eq!(peer_row_entity_type(&classified), "Alloy");
        assert_eq!(
            peer_row_class_iri(&classified),
            Some("https://w3id.org/emmo#EMMO_example_alloy")
        );

        let old_peer = serde_json::json!({ "type": "Phase" });
        assert_eq!(peer_row_entity_type(&old_peer), "Phase");
        assert_eq!(peer_row_class_iri(&old_peer), None);

        for malformed in [
            serde_json::json!({ "class_iri": "relative/path" }),
            serde_json::json!({ "class_iri": "1http://invalid-scheme" }),
            serde_json::json!({ "class_iri": "https://bad iri" }),
            serde_json::json!({ "class_iri": "x".repeat(MAX_CLASS_IRI_LEN + 1) }),
        ] {
            assert_eq!(peer_row_class_iri(&malformed), None);
        }
    }

    /// Properties survive only as a bounded JSON object; junk and oversize
    /// payloads are dropped rather than stored.
    #[test]
    fn peer_row_props_keeps_bounded_objects_only() {
        let with = serde_json::json!({ "properties": { "origin_source": "doi:10.1/x", "n": 1 } });
        let kept = peer_row_props(&with).expect("object props must be kept");
        assert!(kept.contains("doi:10.1/x"));

        assert_eq!(
            peer_row_props(&serde_json::json!({ "properties": {} })),
            None
        );
        assert_eq!(
            peer_row_props(&serde_json::json!({ "properties": [1] })),
            None
        );
        assert_eq!(peer_row_props(&serde_json::json!({ "name": "x" })), None);
        let oversize = serde_json::json!({ "properties": { "blob": "y".repeat(MAX_PROPS_BYTES) } });
        assert_eq!(
            peer_row_props(&oversize),
            None,
            "oversize props must be dropped"
        );
    }

    /// The origin travels in `properties.origin_source`; everything the
    /// wire cannot honestly attribute — absent, empty, non-string, overlong
    /// — must come back `None` so the write stays `mesh:unattributed`.
    #[test]
    fn peer_row_origin_reads_only_a_sane_origin_string() {
        let with = serde_json::json!({
            "type": "Matter", "name": "Ti-6Al-4V",
            "properties": { "origin_source": "  doi:10.1234/abc  " },
        });
        assert_eq!(peer_row_origin(&with).as_deref(), Some("doi:10.1234/abc"));

        for (label, row) in [
            (
                "legacy empty properties",
                serde_json::json!({ "type": "Matter", "name": "x", "properties": {} }),
            ),
            ("no properties at all", serde_json::json!({ "name": "x" })),
            (
                "empty string",
                serde_json::json!({ "properties": { "origin_source": "   " } }),
            ),
            (
                "non-string",
                serde_json::json!({ "properties": { "origin_source": 7 } }),
            ),
            (
                "overlong",
                serde_json::json!({ "properties": { "origin_source": "x".repeat(513) } }),
            ),
        ] {
            assert_eq!(peer_row_origin(&row), None, "{label} must be unattributed");
        }
    }
}
