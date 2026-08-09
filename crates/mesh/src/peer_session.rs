//! Per-peer session minting for authenticated mesh pulls.
//!
//! Every data route on a PRISM node (`/api/query` included) sits behind the
//! server's `auth_stack`, so a peer fetch without a session token is a 401 —
//! which is exactly how the one pull path that existed could never work.
//! This module mints a session on the peer via its public
//! `POST /api/sessions`, presenting the owner's platform token so the peer
//! verifies the identity against the platform and mints the session for the
//! VERIFIED user (`crates/server/src/handlers/sessions.rs`). Sessions are
//! cached per peer and re-minted once when a request comes back 401.
//!
//! # Where the platform token travels — the trust gate
//!
//! Presenting the platform token to the peer is the DESIGNED remote-auth
//! flow: the peer cannot verify an identity it is never shown. But the
//! token is the owner's MARC27 credential, so WHO gets shown it is decided
//! by how the peer's address was learned ([`PeerTrust`]), carried on every
//! address as a type ([`PeerAddress`]) rather than a convention:
//!
//! - [`PeerTrust::OperatorNamed`] / [`PeerTrust::PlatformRegistry`] — the
//!   human typed it, or the authenticated platform registry vouched for
//!   it. The token may be presented.
//! - [`PeerTrust::Announced`] — the address arrived over mDNS or a Kafka
//!   `Announce`, channels any LAN/broker participant can forge. The token
//!   is NEVER attached; the mint is attempted tokenless (a loopback peer
//!   legitimately answers it), and a refusal carries the remedy.
//!
//! Redirects are refused outright on top: a 307/308 would replay the JSON
//! body, token included, to a host the offline gate never checked (same
//! reasoning as the CLI's dashboard session mint). Cryptographic peer
//! verification (`federation::verify_peer`) is the eventual fix, but it is
//! deliberately NOT wired here: nothing issues a platform-signed
//! `PeerIdentity` today and no node holds a `platform_pubkey.bin`, so that
//! path answers 503 — until the platform half exists, address provenance
//! is the trust decision.

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::{Context, Result};

use crate::{PeerNode, PeerTrust};

/// A peer base URL together with how it was learned — the input every
/// session mint and authenticated fetch keys on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerAddress {
    pub url: String,
    pub trust: PeerTrust,
}

impl PeerAddress {
    /// An address the operator typed (`prism mesh sync --peer <url>`).
    pub fn operator_named(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            trust: PeerTrust::OperatorNamed,
        }
    }

    /// The base URL of a discovered peer, carrying the trust of the
    /// channel that discovered it.
    #[must_use]
    pub fn of_peer(peer: &PeerNode) -> Self {
        Self {
            url: format!("http://{}:{}", peer.address, peer.port),
            trust: peer.trust,
        }
    }
}

/// Mints and caches one session token per peer.
#[derive(Debug)]
pub struct PeerSessions {
    /// Owner's platform credential (`cli-state.json` access token or a
    /// `m27_…` API key). `None` still works against a LOOPBACK peer, which
    /// mints an anonymous-local session; a REMOTE peer refuses tokenless
    /// mints by design.
    platform_token: Option<String>,
    client: reqwest::Client,
    cache: Mutex<HashMap<String, String>>,
}

#[derive(serde::Deserialize)]
struct SessionResponse {
    session_id: String,
}

impl PeerSessions {
    pub fn new(platform_token: Option<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            // Never follow a redirect on a request whose BODY carries the
            // platform token: reqwest's cross-host scrubbing removes auth
            // HEADERS only, and a 307/308 re-sends the body verbatim.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("failed to build peer-session HTTP client");
        Self {
            platform_token: platform_token.filter(|t| !t.trim().is_empty()),
            client,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// The token this peer may be shown, per its address provenance.
    fn token_for(&self, peer: &PeerAddress) -> Option<&str> {
        peer.trust
            .may_carry_platform_token()
            .then_some(self.platform_token.as_deref())
            .flatten()
    }

    /// Cache key: a session minted WITH the platform credential must never
    /// be conflated with an anonymous one for the same URL, or the trust
    /// levels would launder through the cache.
    fn cache_key(&self, peer: &PeerAddress) -> String {
        let kind = if self.token_for(peer).is_some() {
            "platform"
        } else {
            "anon"
        };
        format!("{}|{kind}", peer.url)
    }

    /// The cached session for this peer, minting one if none is cached.
    pub async fn session_for(&self, peer: &PeerAddress) -> Result<String> {
        let key = self.cache_key(peer);
        if let Some(token) = self
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .cloned()
        {
            return Ok(token);
        }
        let token = self.mint(peer).await?;
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, token.clone());
        Ok(token)
    }

    /// Drop the cached session for this peer (call on a 401, then
    /// [`Self::session_for`] again to re-mint once).
    pub fn invalidate(&self, peer: &PeerAddress) {
        let key = self.cache_key(peer);
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&key);
    }

    async fn mint(&self, peer: &PeerAddress) -> Result<String> {
        let url = format!("{}/api/sessions", peer.url);
        // The peer address may be another node's claim — hard offline applies.
        prism_runtime::offline::check_url(&url).map_err(|r| anyhow::anyhow!(r))?;
        // THE trust gate: an announced address never sees the credential.
        let platform_token = self.token_for(peer);
        let resp = self
            .client
            .post(&url)
            .json(&serde_json::json!({ "platform_token": platform_token }))
            .send()
            .await
            .with_context(|| format!("failed to reach {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            // Degrade honestly: when the credential exists but was WITHHELD
            // because of the channel, say so and name the deliberate path —
            // never silently do nothing, never silently send the token.
            let withheld = platform_token.is_none() && self.platform_token.is_some();
            if withheld {
                anyhow::bail!(
                    "peer session mint at {url} returned HTTP {status}: {body}\n\
                     peer {} was discovered over an unauthenticated channel \
                     (mDNS/Kafka announce), so your platform credential was \
                     withheld from it. To pull from this peer deliberately, run \
                     `prism mesh sync <dataset> --peer {}` — or register both \
                     nodes with the platform so discovery can vouch for it.",
                    peer.url,
                    peer.url
                );
            }
            anyhow::bail!("peer session mint at {url} returned HTTP {status}: {body}");
        }
        let session: SessionResponse = resp
            .json()
            .await
            .with_context(|| format!("peer session response from {url} was not JSON"))?;
        Ok(session.session_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// One-shot HTTP responder on loopback; forwards each raw request it
    /// served. Raw std TCP so the test needs no server framework.
    fn serve_responses(responses: Vec<String>) -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for response in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let _ = tx.send(String::from_utf8_lossy(&buf[..n]).to_string());
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (base, rx)
    }

    fn http(status: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    /// A refusal from the peer must surface as an error carrying the status
    /// — the exact opposite of the silent empty result the old pull decayed
    /// into.
    #[tokio::test]
    async fn a_401_mint_is_an_error_naming_the_status() {
        let (base, _rx) = serve_responses(vec![http(
            "401 Unauthorized",
            r#"{"error":"platform_token verification failed"}"#,
        )]);
        let sessions = PeerSessions::new(Some("bad-token".into()));
        let err = sessions
            .session_for(&PeerAddress::operator_named(&base))
            .await
            .expect_err("a 401 must be an error");
        let msg = err.to_string();
        assert!(msg.contains("401"), "must carry the status: {msg}");
    }

    /// A mint is cached: two `session_for` calls, one HTTP request.
    /// `invalidate` forces the re-mint the 401-retry path depends on.
    #[tokio::test]
    async fn sessions_are_cached_per_peer_and_invalidatable() {
        let ok = http("200 OK", r#"{"session_id":"sess-1"}"#);
        let ok2 = http("200 OK", r#"{"session_id":"sess-2"}"#);
        let (base, rx) = serve_responses(vec![ok, ok2]);
        let peer = PeerAddress::operator_named(&base);

        let sessions = PeerSessions::new(Some("tok".into()));
        assert_eq!(sessions.session_for(&peer).await.unwrap(), "sess-1");
        assert_eq!(
            sessions.session_for(&peer).await.unwrap(),
            "sess-1",
            "second call must come from the cache"
        );
        let first_request = rx.recv().expect("one request reached the peer");
        assert!(
            first_request.contains("\"platform_token\":\"tok\""),
            "the mint must present the platform token: {first_request}"
        );
        assert!(rx.try_recv().is_err(), "a cached session must not re-mint");

        sessions.invalidate(&peer);
        assert_eq!(sessions.session_for(&peer).await.unwrap(), "sess-2");
    }

    /// Trusted channels DO carry the credential: the operator typed the
    /// address, or the authenticated platform registry vouched for it.
    /// Asserted on the request the peer actually received.
    #[tokio::test]
    async fn the_token_is_minted_for_operator_named_and_registry_peers() {
        for trust in [PeerTrust::OperatorNamed, PeerTrust::PlatformRegistry] {
            let (base, rx) = serve_responses(vec![http("200 OK", r#"{"session_id":"sess-t"}"#)]);
            let sessions = PeerSessions::new(Some("owner-secret".into()));
            let peer = PeerAddress { url: base, trust };
            sessions
                .session_for(&peer)
                .await
                .expect("trusted mint succeeds");
            let request = rx.recv().expect("request reached the peer");
            assert!(
                request.contains(r#""platform_token":"owner-secret""#),
                "{trust:?} must present the platform token: {request}"
            );
        }
    }

    /// THE gate: an address learned over an unauthenticated channel never
    /// sees the credential — asserted on the wire bytes the peer received,
    /// not on any flag, so removing the gate fails this test.
    #[tokio::test]
    async fn the_token_is_never_sent_to_an_announced_peer() {
        let (base, rx) = serve_responses(vec![http("200 OK", r#"{"session_id":"sess-a"}"#)]);
        let sessions = PeerSessions::new(Some("owner-secret".into()));
        let peer = PeerAddress {
            url: base,
            trust: PeerTrust::Announced,
        };
        sessions
            .session_for(&peer)
            .await
            .expect("a tokenless loopback mint is legitimate");
        let request = rx.recv().expect("request reached the peer");
        assert!(
            !request.contains("owner-secret"),
            "the platform credential reached an announced peer: {request}"
        );
        assert!(
            request.contains(r#""platform_token":null"#),
            "the announced mint must be explicitly tokenless: {request}"
        );
    }

    /// An announced peer that refuses the tokenless mint degrades with the
    /// remedy — the deliberate `mesh sync --peer` path or platform
    /// registration — never silence, never the token.
    #[tokio::test]
    async fn an_announced_refusal_names_the_remedy() {
        let (base, _rx) = serve_responses(vec![http(
            "401 Unauthorized",
            r#"{"error":"Remote session creation requires a platform_token"}"#,
        )]);
        let sessions = PeerSessions::new(Some("owner-secret".into()));
        let peer = PeerAddress {
            url: base.clone(),
            trust: PeerTrust::Announced,
        };
        let err = sessions
            .session_for(&peer)
            .await
            .expect_err("the refused mint must surface");
        let msg = err.to_string();
        assert!(
            msg.contains("unauthenticated channel"),
            "the refusal must explain WHY the credential was withheld: {msg}"
        );
        assert!(
            msg.contains(&format!("--peer {base}")),
            "the refusal must name the deliberate path: {msg}"
        );
        assert!(
            !msg.contains("owner-secret"),
            "the error text must not leak the credential: {msg}"
        );
    }
}
