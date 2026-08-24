//! Two nodes reach each other by KEY, with no address between them.
//!
//! This is the claim the mesh could not make before: `mdns` only finds peers
//! on one broadcast domain, and `federated_query` needs a routable
//! `http://{address}:{port}`. Neither describes a laptop in Poland talking to
//! a laptop in Germany. Here neither side is ever told the other's address —
//! only its Ed25519 public key, which is exactly what `PeerIdentity` already
//! carries as `node_pubkey_hex`.
//!
//! Marked `#[ignore]` because it uses real networking (n0's address-lookup
//! relay). It is a REAL connection on purpose: a mocked transport would prove
//! the mock works. Run it deliberately:
//!
//!   cargo test -p prism-mesh --test iroh_peer_roundtrip -- --ignored --nocapture

use prism_mesh::iroh_transport::{MeshEndpoint, PRISM_MESH_ALPN, endpoint_id_from_pubkey_hex};

#[tokio::test]
#[ignore = "uses real networking; run with --ignored"]
async fn two_nodes_talk_knowing_only_each_others_public_key() {
    let responder = MeshEndpoint::bind([11u8; 32])
        .await
        .expect("bind responder");
    let asker = MeshEndpoint::bind([22u8; 32]).await.expect("bind asker");

    // Binding is not the same as being reachable: the responder has a socket
    // but no relay yet, and its address is not published until it does.
    tokio::time::timeout(
        std::time::Duration::from_secs(30),
        responder.wait_until_reachable(),
    )
    .await
    .expect("responder must reach a relay");

    // The ONLY thing the asker is given: a hex Ed25519 key, the same shape a
    // `PeerIdentity` publishes. No IP, no port, no hostname.
    let responder_pubkey_hex = hex::encode(responder.endpoint_id().as_bytes());

    let responder_endpoint = responder.endpoint().clone();
    let serve = tokio::spawn(async move {
        let incoming = responder_endpoint.accept().await.expect("an inbound conn");
        let conn = incoming.await.expect("accepted");
        let (mut send, mut recv) = conn.accept_bi().await.expect("peer opened a stream");
        let question = recv.read_to_end(64 * 1024).await.expect("read question");
        assert_eq!(&question, b"which papers do you hold?");
        send.write_all(b"gludovatz-2016;gludovatz-2022")
            .await
            .expect("write answer");
        send.finish().expect("finish answer");
        conn.closed().await;
    });

    let peer = endpoint_id_from_pubkey_hex(&responder_pubkey_hex).expect("key parses");
    let reply = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        asker.request(peer, b"which papers do you hold?"),
    )
    .await
    .expect("connecting by key must not hang")
    .expect("request must succeed");

    assert_eq!(
        String::from_utf8_lossy(&reply),
        "gludovatz-2016;gludovatz-2022",
        "the answer must survive the hop"
    );
    serve.await.expect("responder task");
    asker.close().await;
}

/// The ALPN is versioned so a mismatched protocol fails to negotiate rather
/// than half-understanding a newer peer.
#[test]
fn the_wire_protocol_is_versioned() {
    assert_eq!(PRISM_MESH_ALPN, b"prism/mesh/1");
}
