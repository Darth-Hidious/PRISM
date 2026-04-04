#![allow(clippy::await_holding_lock)]
//! Integration tests for mesh data flow.
//!
//! These tests simulate the message passing between nodes WITHOUT requiring
//! a real Kafka broker. Messages flow through tokio channels just like the
//! real sync handler receives them from the Kafka consumer.

use std::sync::{Arc, RwLock};

use chrono::Utc;
use tokio::sync::mpsc;
use uuid::Uuid;

use prism_mesh::protocol::MeshMessage;
use prism_mesh::subscription::{PublishedDataset, SubscriptionManager};
use prism_mesh::PeerNode;

/// Helper: create a PeerNode for testing.
fn make_peer(id: Uuid, name: &str, port: u16) -> PeerNode {
    PeerNode {
        node_id: id,
        name: name.into(),
        address: "127.0.0.1".into(),
        port,
        last_seen: Utc::now(),
        capabilities: vec!["compute".into()],
    }
}

// ��─ Test: Announce message adds peer to the list ──────────────────

#[tokio::test]
async fn sync_handler_announce_adds_peer() {
    let our_id = Uuid::new_v4();
    let remote_id = Uuid::new_v4();

    let peers: Arc<RwLock<Vec<PeerNode>>> = Arc::new(RwLock::new(Vec::new()));
    let subs = Arc::new(RwLock::new(SubscriptionManager::new()));

    let (tx, rx) = mpsc::channel(16);

    // Spawn sync handler
    let peers_clone = peers.clone();
    let handle = tokio::spawn(async move {
        prism_mesh::sync::run_sync_handler(rx, peers_clone, subs, our_id, None).await;
    });

    // Send announce from remote node
    tx.send(MeshMessage::Announce {
        node_id: remote_id,
        name: "remote-lab".into(),
        address: "192.168.1.50".into(),
        port: 9100,
        capabilities: vec!["gpu".into()],
    })
    .await
    .unwrap();

    // Give sync handler time to process
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Verify peer was added
    let peer_list = peers.read().unwrap();
    assert_eq!(peer_list.len(), 1);
    assert_eq!(peer_list[0].node_id, remote_id);
    assert_eq!(peer_list[0].name, "remote-lab");
    assert_eq!(peer_list[0].address, "192.168.1.50");

    // Cleanup
    drop(tx);
    handle.await.unwrap();
}

// ── Test: Our own announce is ignored ─────────────────────────────

#[tokio::test]
async fn sync_handler_ignores_own_announce() {
    let our_id = Uuid::new_v4();

    let peers: Arc<RwLock<Vec<PeerNode>>> = Arc::new(RwLock::new(Vec::new()));
    let subs = Arc::new(RwLock::new(SubscriptionManager::new()));

    let (tx, rx) = mpsc::channel(16);
    let peers_clone = peers.clone();
    let handle = tokio::spawn(async move {
        prism_mesh::sync::run_sync_handler(rx, peers_clone, subs, our_id, None).await;
    });

    // Send announce from ourselves — should be ignored
    tx.send(MeshMessage::Announce {
        node_id: our_id,
        name: "us".into(),
        address: "127.0.0.1".into(),
        port: 9100,
        capabilities: vec![],
    })
    .await
    .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    {
        let peer_list = peers.read().unwrap();
        assert!(peer_list.is_empty(), "should not add ourselves as a peer");
    }

    drop(tx);
    handle.await.unwrap();
}

// ── Test: Goodbye removes peer ────────────────────────────────────

#[tokio::test]
async fn sync_handler_goodbye_removes_peer() {
    let our_id = Uuid::new_v4();
    let remote_id = Uuid::new_v4();

    // Pre-populate peer list
    let peers: Arc<RwLock<Vec<PeerNode>>> =
        Arc::new(RwLock::new(vec![make_peer(remote_id, "remote-lab", 9100)]));
    let subs = Arc::new(RwLock::new(SubscriptionManager::new()));

    let (tx, rx) = mpsc::channel(16);
    let peers_clone = peers.clone();
    let handle = tokio::spawn(async move {
        prism_mesh::sync::run_sync_handler(rx, peers_clone, subs, our_id, None).await;
    });

    assert_eq!(peers.read().unwrap().len(), 1);

    tx.send(MeshMessage::Goodbye { node_id: remote_id })
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    assert!(peers.read().unwrap().is_empty(), "peer should be removed");

    drop(tx);
    handle.await.unwrap();
}

// ─�� Test: DataSubscribe updates subscriber list ───────────────────

#[tokio::test]
async fn sync_handler_data_subscribe_adds_subscriber() {
    let our_id = Uuid::new_v4();
    let subscriber_id = Uuid::new_v4();

    let peers: Arc<RwLock<Vec<PeerNode>>> = Arc::new(RwLock::new(Vec::new()));
    let mut mgr = SubscriptionManager::new();
    mgr.publish(PublishedDataset {
        name: "alloy-db".into(),
        schema_version: "1.0".into(),
        subscribers: vec![],
    });
    let subs = Arc::new(RwLock::new(mgr));

    let (tx, rx) = mpsc::channel(16);
    let subs_clone = subs.clone();
    let handle = tokio::spawn(async move {
        prism_mesh::sync::run_sync_handler(rx, peers, subs_clone, our_id, None).await;
    });

    tx.send(MeshMessage::DataSubscribe {
        subscriber_id,
        dataset_name: "alloy-db".into(),
    })
    .await
    .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    {
        let mgr = subs.read().unwrap();
        let published = mgr.published();
        assert_eq!(published.len(), 1);
        assert!(
            published[0].subscribers.contains(&subscriber_id),
            "subscriber should be tracked"
        );
    }

    drop(tx);
    handle.await.unwrap();
}

// ── Test: DataUnsubscribe removes subscriber ──────────────────────

#[tokio::test]
async fn sync_handler_data_unsubscribe_removes_subscriber() {
    let our_id = Uuid::new_v4();
    let subscriber_id = Uuid::new_v4();

    let peers: Arc<RwLock<Vec<PeerNode>>> = Arc::new(RwLock::new(Vec::new()));
    let mut mgr = SubscriptionManager::new();
    mgr.publish(PublishedDataset {
        name: "alloy-db".into(),
        schema_version: "1.0".into(),
        subscribers: vec![subscriber_id],
    });
    let subs = Arc::new(RwLock::new(mgr));

    let (tx, rx) = mpsc::channel(16);
    let subs_clone = subs.clone();
    let handle = tokio::spawn(async move {
        prism_mesh::sync::run_sync_handler(rx, peers, subs_clone, our_id, None).await;
    });

    tx.send(MeshMessage::DataUnsubscribe {
        subscriber_id,
        dataset_name: "alloy-db".into(),
    })
    .await
    .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    {
        let mgr = subs.read().unwrap();
        assert!(
            mgr.published()[0].subscribers.is_empty(),
            "subscriber should be removed"
        );
    }

    drop(tx);
    handle.await.unwrap();
}

// ── Test: DataPublish for unsubscribed dataset is ignored ─────────

#[tokio::test]
async fn sync_handler_ignores_unsubscribed_data_publish() {
    let our_id = Uuid::new_v4();
    let remote_id = Uuid::new_v4();

    let peers: Arc<RwLock<Vec<PeerNode>>> =
        Arc::new(RwLock::new(vec![make_peer(remote_id, "remote", 9100)]));
    // We have NO subscriptions — so DataPublish should be ignored
    let subs = Arc::new(RwLock::new(SubscriptionManager::new()));

    let (tx, rx) = mpsc::channel(16);
    let peers_clone = peers.clone();
    let handle = tokio::spawn(async move {
        prism_mesh::sync::run_sync_handler(rx, peers_clone, subs, our_id, None).await;
    });

    tx.send(MeshMessage::DataPublish {
        node_id: remote_id,
        dataset_name: "alloy-db".into(),
        schema_version: "1.0".into(),
        update_frequency: "hourly".into(),
    })
    .await
    .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Should not crash, should not attempt to sync (no Neo4j configured)
    // This test verifies the handler gracefully skips unsubscribed datasets

    drop(tx);
    handle.await.unwrap();
}

// ── Test: Full peer lifecycle (announce → subscribe → unsubscribe → goodbye) ──

#[tokio::test]
async fn sync_handler_full_lifecycle() {
    let our_id = Uuid::new_v4();
    let remote_id = Uuid::new_v4();

    let peers: Arc<RwLock<Vec<PeerNode>>> = Arc::new(RwLock::new(Vec::new()));
    let mut mgr = SubscriptionManager::new();
    mgr.publish(PublishedDataset {
        name: "phase-diagrams".into(),
        schema_version: "2.0".into(),
        subscribers: vec![],
    });
    let subs = Arc::new(RwLock::new(mgr));

    let (tx, rx) = mpsc::channel(32);
    let peers_clone = peers.clone();
    let subs_clone = subs.clone();
    let handle = tokio::spawn(async move {
        prism_mesh::sync::run_sync_handler(rx, peers_clone, subs_clone, our_id, None).await;
    });

    // Step 1: Remote node announces
    tx.send(MeshMessage::Announce {
        node_id: remote_id,
        name: "lab-b".into(),
        address: "10.0.0.5".into(),
        port: 9200,
        capabilities: vec!["storage".into()],
    })
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert_eq!(peers.read().unwrap().len(), 1);

    // Step 2: Remote node subscribes to our dataset
    tx.send(MeshMessage::DataSubscribe {
        subscriber_id: remote_id,
        dataset_name: "phase-diagrams".into(),
    })
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    {
        let mgr = subs.read().unwrap();
        assert!(mgr.published()[0].subscribers.contains(&remote_id));
    }

    // Step 3: Remote node unsubscribes
    tx.send(MeshMessage::DataUnsubscribe {
        subscriber_id: remote_id,
        dataset_name: "phase-diagrams".into(),
    })
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    {
        let mgr = subs.read().unwrap();
        assert!(mgr.published()[0].subscribers.is_empty());
    }

    // Step 4: Remote node says goodbye
    tx.send(MeshMessage::Goodbye { node_id: remote_id })
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert!(peers.read().unwrap().is_empty());

    drop(tx);
    handle.await.unwrap();
}

// ── Test: Multiple peers coexist correctly ────────────────────────

#[tokio::test]
async fn sync_handler_multiple_peers() {
    let our_id = Uuid::new_v4();
    let node_a = Uuid::new_v4();
    let node_b = Uuid::new_v4();
    let node_c = Uuid::new_v4();

    let peers: Arc<RwLock<Vec<PeerNode>>> = Arc::new(RwLock::new(Vec::new()));
    let subs = Arc::new(RwLock::new(SubscriptionManager::new()));

    let (tx, rx) = mpsc::channel(32);
    let peers_clone = peers.clone();
    let handle = tokio::spawn(async move {
        prism_mesh::sync::run_sync_handler(rx, peers_clone, subs, our_id, None).await;
    });

    // Three nodes announce
    for (id, name) in [(node_a, "alpha"), (node_b, "beta"), (node_c, "gamma")] {
        tx.send(MeshMessage::Announce {
            node_id: id,
            name: name.into(),
            address: "10.0.0.1".into(),
            port: 9100,
            capabilities: vec![],
        })
        .await
        .unwrap();
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(peers.read().unwrap().len(), 3);

    // Node B departs
    tx.send(MeshMessage::Goodbye { node_id: node_b })
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    {
        let list = peers.read().unwrap();
        assert_eq!(list.len(), 2);
        assert!(!list.iter().any(|p| p.node_id == node_b));
        assert!(list.iter().any(|p| p.node_id == node_a));
        assert!(list.iter().any(|p| p.node_id == node_c));
    }

    drop(tx);
    handle.await.unwrap();
}

// ── Test: Duplicate announce is idempotent ────────────────────────

#[tokio::test]
async fn sync_handler_duplicate_announce_ignored() {
    let our_id = Uuid::new_v4();
    let remote_id = Uuid::new_v4();

    let peers: Arc<RwLock<Vec<PeerNode>>> = Arc::new(RwLock::new(Vec::new()));
    let subs = Arc::new(RwLock::new(SubscriptionManager::new()));

    let (tx, rx) = mpsc::channel(16);
    let peers_clone = peers.clone();
    let handle = tokio::spawn(async move {
        prism_mesh::sync::run_sync_handler(rx, peers_clone, subs, our_id, None).await;
    });

    // Send same announce twice
    for _ in 0..2 {
        tx.send(MeshMessage::Announce {
            node_id: remote_id,
            name: "lab-x".into(),
            address: "10.0.0.1".into(),
            port: 9100,
            capabilities: vec![],
        })
        .await
        .unwrap();
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Should only have 1 peer, not 2
    assert_eq!(peers.read().unwrap().len(), 1);

    drop(tx);
    handle.await.unwrap();
}

// ── Test: MeshMessage serde roundtrip through channel ─────────────

#[tokio::test]
async fn message_roundtrip_through_channel() {
    let (tx, mut rx) = mpsc::channel::<MeshMessage>(8);

    let original_id = Uuid::new_v4();
    let msg = MeshMessage::DataPublish {
        node_id: original_id,
        dataset_name: "titanium-alloys".into(),
        schema_version: "3.1".into(),
        update_frequency: "daily".into(),
    };

    // Serialize → send as JSON (simulating Kafka)
    let json = serde_json::to_string(&msg).unwrap();
    let deserialized: MeshMessage = serde_json::from_str(&json).unwrap();
    tx.send(deserialized).await.unwrap();

    let received = rx.recv().await.unwrap();
    if let MeshMessage::DataPublish {
        node_id,
        dataset_name,
        schema_version,
        update_frequency,
    } = received
    {
        assert_eq!(node_id, original_id);
        assert_eq!(dataset_name, "titanium-alloys");
        assert_eq!(schema_version, "3.1");
        assert_eq!(update_frequency, "daily");
    } else {
        panic!("expected DataPublish");
    }
}
