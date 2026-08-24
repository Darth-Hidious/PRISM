// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Peer transport that dials an Ed25519 public key instead of an IP address.
//!
//! The mesh could only reach a peer two ways, and neither survives contact
//! with the real deployment this exists for — a colleague in Poland reading a
//! colleague's papers in Germany:
//!
//! * mDNS ([`crate::mdns`]) finds peers on the same broadcast domain. Two
//!   offices are not one broadcast domain.
//! * [`crate::federated_query`] builds `http://{address}:{port}`, which needs
//!   a routable address. Behind NAT, on a laptop, on hotel wifi, there is
//!   none — and the cross-org path that papered over this was brokered by
//!   `platform.marc27.com`, which has been retired.
//!
//! So this is not a faster carrier for something that worked. It is the
//! missing one.
//!
//! **Identity is unchanged.** [`crate::federation::PeerIdentity`] already
//! carries `node_pubkey_hex`: an Ed25519 public key, 32 bytes. An iroh
//! `EndpointId` *is* an Ed25519 public key, 32 bytes. The same bytes name the
//! same node, so a peer keeps the identity it already had and only the
//! carrier underneath changes. Nothing here mints, stores, or rotates a key.
//!
//! **What this does NOT give you.** Reachability is not authorisation.
//! `PeerIdentity` is signed by the platform (`platform_signature_hex`), and
//! that platform is gone, so the trust root needs re-establishing
//! independently of transport. Dialling a node proves you reached the holder
//! of that key; it says nothing about whether they may ask you anything.
//! [`crate::federation`] remains the gate and this must not become a way
//! around it.

use anyhow::{Context, Result};
use iroh::endpoint::presets::N0;
use iroh::{Endpoint, EndpointId, SecretKey};

/// The ALPN this mesh speaks. Version it: a peer running an older protocol
/// must fail to negotiate loudly rather than half-understand a newer one.
pub const PRISM_MESH_ALPN: &[u8] = b"prism/mesh/1";

/// Parse the hex Ed25519 key a `PeerIdentity` already carries into the
/// `EndpointId` iroh dials.
///
/// This is a re-reading of existing bytes, not a conversion between two
/// identity schemes — which is the whole reason iroh fits here rather than
/// forcing a migration.
pub fn endpoint_id_from_pubkey_hex(pubkey_hex: &str) -> Result<EndpointId> {
    let raw = hex::decode(pubkey_hex.trim())
        .with_context(|| format!("node public key is not hex: {pubkey_hex:?}"))?;
    let bytes: [u8; 32] = raw.as_slice().try_into().map_err(|_| {
        anyhow::anyhow!(
            "node public key must be 32 bytes for Ed25519, got {}",
            raw.len()
        )
    })?;
    EndpointId::from_bytes(&bytes).context("node public key is not a valid Ed25519 point")
}

/// A bound iroh endpoint for this node.
pub struct MeshEndpoint {
    endpoint: Endpoint,
}

impl MeshEndpoint {
    /// Bind an endpoint from THIS node's existing signing key.
    ///
    /// The caller passes the same 32-byte secret whose public half is already
    /// published as `node_pubkey_hex`, so the address other peers hold keeps
    /// working. Discovery is enabled so a node can be found by key alone; it
    /// still hole-punches to a direct connection whenever one is possible and
    /// only relays when it is not.
    pub async fn bind(secret_key_bytes: [u8; 32]) -> Result<Self> {
        let secret = SecretKey::from_bytes(&secret_key_bytes);
        // The N0 preset publishes and resolves addresses through n0's DNS
        // relay, which is what lets a peer be found by key alone from another
        // network. It is a default, not a dependency: `iroh-relay` is
        // self-hostable and swapping the preset is the seam for doing so.
        let endpoint = Endpoint::builder(N0)
            .secret_key(secret)
            .alpns(vec![PRISM_MESH_ALPN.to_vec()])
            .bind()
            .await
            .context("binding the iroh mesh endpoint")?;
        Ok(Self { endpoint })
    }

    /// Wait until this node is actually reachable by other peers.
    ///
    /// `bind` deliberately returns as soon as the local socket is up, which
    /// is BEFORE a relay has been chosen and this node's address published.
    /// A peer that dials the key in that window gets "all address lookup
    /// services failed" — observed on the first live run of
    /// `tests/iroh_peer_roundtrip.rs`, and it looks exactly like a broken
    /// peer rather than an impatient one.
    ///
    /// So any node that expects to be DIALLED must await this before saying
    /// it is available; a node that only dials out need not.
    pub async fn wait_until_reachable(&self) {
        self.endpoint.online().await;
    }

    /// This node's own dialable identity.
    #[must_use]
    pub fn endpoint_id(&self) -> EndpointId {
        self.endpoint.id()
    }

    /// The underlying endpoint, for accept loops and connection handling.
    #[must_use]
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Send one request to a peer named by key and read the reply.
    ///
    /// Deliberately request/response over a fresh bi-directional stream: it
    /// mirrors what `federated_query` already does over HTTP, so the calling
    /// code does not have to learn a streaming model to gain reachability.
    pub async fn request(&self, peer: EndpointId, payload: &[u8]) -> Result<Vec<u8>> {
        let connection = self
            .endpoint
            .connect(peer, PRISM_MESH_ALPN)
            .await
            .with_context(|| format!("dialling peer {peer}"))?;
        let (mut send, mut recv) = connection
            .open_bi()
            .await
            .context("opening a bi-directional stream to the peer")?;
        send.write_all(payload)
            .await
            .context("writing the request")?;
        // Half-close so the peer sees a complete request rather than waiting
        // for more of one.
        send.finish().context("finishing the request stream")?;
        let reply = recv
            .read_to_end(MAX_REPLY_BYTES)
            .await
            .context("reading the peer's reply")?;
        Ok(reply)
    }

    /// Close the endpoint and its connections.
    pub async fn close(self) {
        self.endpoint.close().await;
    }
}

/// Ceiling on a single peer reply. A peer is not trusted to bound its own
/// response, and an unbounded `read_to_end` against a hostile or broken node
/// is an out-of-memory abort in this process.
pub const MAX_REPLY_BYTES: usize = 64 * 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    /// The bytes a `PeerIdentity` already carries ARE an iroh NodeId. If this
    /// ever fails, the "keep your identity, change the carrier" claim is
    /// false and the design needs revisiting, not patching.
    #[test]
    fn a_peer_identity_public_key_is_an_iroh_endpoint_id() {
        let secret = SecretKey::from_bytes(&[7u8; 32]);
        let expected = secret.public();
        let pubkey_hex = hex::encode(expected.as_bytes());

        let parsed = endpoint_id_from_pubkey_hex(&pubkey_hex).expect("valid key parses");
        assert_eq!(parsed, expected);
    }

    #[test]
    fn a_malformed_key_is_refused_with_its_reason() {
        let err = endpoint_id_from_pubkey_hex("not-hex")
            .unwrap_err()
            .to_string();
        assert!(err.contains("not hex"), "{err}");

        let short = hex::encode([1u8; 16]);
        let err = endpoint_id_from_pubkey_hex(&short).unwrap_err().to_string();
        assert!(err.contains("32 bytes"), "{err}");
    }

    #[test]
    fn whitespace_around_a_key_does_not_change_the_node() {
        let secret = SecretKey::from_bytes(&[9u8; 32]);
        let hexed = hex::encode(secret.public().as_bytes());
        assert_eq!(
            endpoint_id_from_pubkey_hex(&format!("  {hexed}\n")).unwrap(),
            secret.public()
        );
    }
}
