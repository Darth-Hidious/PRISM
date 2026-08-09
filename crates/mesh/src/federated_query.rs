//! Federated query execution across mesh nodes.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::PeerNode;
use crate::peer_session::{PeerAddress, PeerSessions};

/// Coordinates federated query dispatch across peer nodes.
#[derive(Debug, Clone)]
pub struct FederatedQuery {
    client: reqwest::Client,
    /// Authenticates each peer request — `/api/query` sits behind the
    /// peer's `auth_stack`, so an unauthenticated fan-out is N 401s.
    sessions: Arc<PeerSessions>,
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

/// The peers a query may actually be sent to, paired with their BASE URL.
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
            let base = format!("http://{}:{}", peer.address, peer.port);
            match prism_runtime::offline::check_url(&format!("{base}/api/query")) {
                Ok(()) => Some((peer, base)),
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
        Self::with_platform_token(timeout, None)
    }

    /// A federation client that authenticates to peers by minting sessions
    /// with the owner's platform token (see [`PeerSessions`]). Without a
    /// token only loopback peers — which mint anonymous-local sessions —
    /// can answer.
    pub fn with_platform_token(timeout: Duration, platform_token: Option<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            // The session token travels in a header reqwest's cross-host
            // redirect scrubbing does not strip; refuse redirects outright.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("failed to build reqwest client");
        Self {
            client,
            sessions: Arc::new(PeerSessions::new(platform_token)),
        }
    }

    /// Execute a query across the given peer nodes and collect merged results.
    ///
    /// Each peer is queried in parallel, authenticated with a per-peer
    /// session (re-minted once on a 401). A peer that fails — including any
    /// non-2xx status — is an ERROR for that peer, never an empty answer:
    /// the old path deserialised a 401 body into `results: []` and reported
    /// a successful empty fan-out. Partial failure is tolerated (the query
    /// still merges what answered, and the failures are logged); when EVERY
    /// dispatched peer fails, the whole query fails, naming each peer.
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
            .map(|(peer, base)| {
                let peer_name = peer.name.clone();
                // Carry HOW the address was learned: PeerSessions withholds
                // the platform credential from announced peers.
                let address = PeerAddress {
                    url: base,
                    trust: peer.trust,
                };
                async move {
                    let outcome = self.query_one_peer(&address, query).await;
                    (peer_name, outcome)
                }
            })
            .collect();

        let outcomes = futures_util::future::join_all(futures).await;
        if outcomes.is_empty() {
            return Ok(Vec::new()); // every peer was skipped by policy
        }
        let dispatched = outcomes.len();

        let mut merged = Vec::new();
        let mut failures = Vec::new();
        for (peer_name, outcome) in outcomes {
            match outcome {
                Ok(results) => {
                    debug!(peer = %peer_name, results = results.len(), "peer responded");
                    merged.extend(results);
                }
                Err(e) => {
                    warn!(peer = %peer_name, error = %e, "peer query failed");
                    failures.push(format!("{peer_name}: {e}"));
                }
            }
        }
        if !failures.is_empty() && failures.len() == dispatched {
            anyhow::bail!("every federated peer failed — {}", failures.join("; "));
        }
        Ok(merged)
    }

    /// One authenticated peer query. A 401 invalidates the cached session
    /// and retries exactly once with a fresh mint.
    async fn query_one_peer(
        &self,
        peer: &PeerAddress,
        query: &str,
    ) -> Result<Vec<serde_json::Value>> {
        let url = format!("{}/api/query", peer.url);
        let body = QueryRequest { query, mode: None };

        let mut session = self.sessions.session_for(peer).await?;
        let mut resp = self
            .client
            .post(&url)
            .header("X-Session-Token", &session)
            .json(&body)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            self.sessions.invalidate(peer);
            session = self.sessions.session_for(peer).await?;
            resp = self
                .client
                .post(&url)
                .header("X-Session-Token", &session)
                .json(&body)
                .send()
                .await?;
        }
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("peer returned HTTP {status}");
        }
        Ok(resp.json::<QueryResponse>().await?.results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    use crate::{OfflineEnvGuard, PeerTrust, test_env_lock};

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
            trust: PeerTrust::Announced,
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

    /// The base URL the peer is actually queried on, pinned so a change to
    /// the format cannot silently bypass the check that reads it
    /// (`query_one_peer` appends `/api/query` to exactly this base).
    #[test]
    fn the_checked_url_is_the_url_that_gets_queried() {
        let _guard = test_env_lock();
        let _restore = OfflineEnvGuard::capture();
        unsafe { std::env::remove_var(prism_runtime::offline::ENV) };

        let peers = vec![peer("local", "127.0.0.1")];
        let (_, base) = allowed_peers(&peers).remove(0);
        assert_eq!(base, "http://127.0.0.1:9100");
    }

    /// A peer answering 401 must surface as an ERROR, never as a successful
    /// empty fan-out — the old path deserialised the 401 body into
    /// `results: []` via `#[serde(default)]` and reported zero results.
    /// With one peer, "all peers failed" is the whole query failing.
    #[tokio::test]
    async fn a_401_peer_is_an_error_not_an_empty_result() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            // Session mint succeeds; every query answers 401 — including
            // the once-retried one after the re-mint.
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);
                let (status, body) = if request.starts_with("POST /api/sessions") {
                    ("200 OK", r#"{"session_id":"sess-mock"}"#)
                } else {
                    ("401 Unauthorized", r#"{"error":"unauthorized"}"#)
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });

        let mut unauthorized = peer("denied", "127.0.0.1");
        unauthorized.port = addr.port();
        let federation = FederatedQuery::new(Duration::from_secs(5));
        let err = federation
            .query_peers(&[unauthorized], "titanium")
            .await
            .expect_err("a 401 from the only peer must fail the query");
        let msg = err.to_string();
        assert!(
            msg.contains("401"),
            "the error must carry the status: {msg}"
        );
        assert!(
            msg.contains("denied"),
            "the error must name the peer: {msg}"
        );
    }
}
