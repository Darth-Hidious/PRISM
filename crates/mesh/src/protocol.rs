//! Inter-node message types for mesh communication.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Tagged message envelope for all inter-node communication.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum MeshMessage {
    /// A node announces its presence on the mesh.
    Announce {
        node_id: Uuid,
        name: String,
        address: String,
        port: u16,
        capabilities: Vec<String>,
    },

    /// A node is leaving the mesh gracefully.
    Goodbye { node_id: Uuid },

    /// A node publishes a dataset for subscription.
    DataPublish {
        node_id: Uuid,
        dataset_name: String,
        schema_version: String,
        update_frequency: String,
    },

    /// A node subscribes to a published dataset.
    DataSubscribe {
        subscriber_id: Uuid,
        dataset_name: String,
    },

    /// A node unsubscribes from a dataset.
    DataUnsubscribe {
        subscriber_id: Uuid,
        dataset_name: String,
    },

    /// Forward a query to peer nodes for federated execution.
    QueryForward {
        query_id: Uuid,
        query: String,
        origin_node: Uuid,
    },

    /// Results returned from a forwarded query.
    QueryResult {
        query_id: Uuid,
        results: serde_json::Value,
    },

    /// Heartbeat request.
    Ping { node_id: Uuid },

    /// Heartbeat response.
    Pong { node_id: Uuid },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt<T: Serialize + for<'de> Deserialize<'de> + std::fmt::Debug>(value: &T) -> T {
        let json = serde_json::to_string(value).expect("serialize");
        serde_json::from_str(&json).expect("deserialize")
    }

    #[test]
    fn announce_serde_roundtrip() {
        let msg = MeshMessage::Announce {
            node_id: Uuid::new_v4(),
            name: "test-node".into(),
            address: "192.168.1.10".into(),
            port: 9100,
            capabilities: vec!["compute".into(), "storage".into()],
        };
        let back = rt(&msg);
        // Compare via JSON since MeshMessage doesn't derive PartialEq
        assert_eq!(
            serde_json::to_value(&msg).unwrap(),
            serde_json::to_value(&back).unwrap()
        );
    }

    #[test]
    fn announce_json_has_type_tag() {
        let msg = MeshMessage::Announce {
            node_id: Uuid::new_v4(),
            name: "n".into(),
            address: "127.0.0.1".into(),
            port: 1,
            capabilities: vec![],
        };
        let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["type"], "Announce");
    }

    #[test]
    fn goodbye_serde_roundtrip() {
        let id = Uuid::new_v4();
        let msg = MeshMessage::Goodbye { node_id: id };
        let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["type"], "Goodbye");
        assert_eq!(v["node_id"], id.to_string());
        let back: MeshMessage = serde_json::from_value(v).unwrap();
        if let MeshMessage::Goodbye { node_id } = back {
            assert_eq!(node_id, id);
        } else {
            panic!("expected Goodbye");
        }
    }

    #[test]
    fn data_publish_serde_roundtrip() {
        let msg = MeshMessage::DataPublish {
            node_id: Uuid::new_v4(),
            dataset_name: "alloy-db".into(),
            schema_version: "2.0".into(),
            update_frequency: "hourly".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("DataPublish"));
        assert!(json.contains("alloy-db"));
        let back: MeshMessage = serde_json::from_str(&json).unwrap();
        if let MeshMessage::DataPublish { dataset_name, .. } = back {
            assert_eq!(dataset_name, "alloy-db");
        } else {
            panic!("expected DataPublish");
        }
    }

    #[test]
    fn data_subscribe_serde_roundtrip() {
        let id = Uuid::new_v4();
        let msg = MeshMessage::DataSubscribe {
            subscriber_id: id,
            dataset_name: "phase-diagrams".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("DataSubscribe"));
        let back: MeshMessage = serde_json::from_str(&json).unwrap();
        if let MeshMessage::DataSubscribe {
            subscriber_id,
            dataset_name,
        } = back
        {
            assert_eq!(subscriber_id, id);
            assert_eq!(dataset_name, "phase-diagrams");
        } else {
            panic!("expected DataSubscribe");
        }
    }

    #[test]
    fn query_forward_serde_roundtrip() {
        let query_id = Uuid::new_v4();
        let origin = Uuid::new_v4();
        let msg = MeshMessage::QueryForward {
            query_id,
            query: "SELECT * FROM alloys".into(),
            origin_node: origin,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("QueryForward"));
        let back: MeshMessage = serde_json::from_str(&json).unwrap();
        if let MeshMessage::QueryForward {
            query_id: qid,
            query,
            ..
        } = back
        {
            assert_eq!(qid, query_id);
            assert_eq!(query, "SELECT * FROM alloys");
        } else {
            panic!("expected QueryForward");
        }
    }

    #[test]
    fn query_result_complex_json_roundtrip() {
        let qid = Uuid::new_v4();
        let results = serde_json::json!({
            "rows": [
                {"element": "Fe", "weight_pct": 72.5},
                {"element": "Cr", "weight_pct": 18.0},
                {"element": "Ni", "weight_pct": 9.5},
            ],
            "count": 3,
            "truncated": false,
        });
        let msg = MeshMessage::QueryResult {
            query_id: qid,
            results: results.clone(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("QueryResult"));
        let back: MeshMessage = serde_json::from_str(&json).unwrap();
        if let MeshMessage::QueryResult {
            query_id,
            results: r,
        } = back
        {
            assert_eq!(query_id, qid);
            assert_eq!(r, results);
        } else {
            panic!("expected QueryResult");
        }
    }

    #[test]
    fn ping_serde_roundtrip() {
        let id = Uuid::new_v4();
        let msg = MeshMessage::Ping { node_id: id };
        let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["type"], "Ping");
        assert_eq!(v["node_id"], id.to_string());
        let back: MeshMessage = serde_json::from_value(v).unwrap();
        if let MeshMessage::Ping { node_id } = back {
            assert_eq!(node_id, id);
        } else {
            panic!("expected Ping");
        }
    }

    #[test]
    fn pong_serde_roundtrip() {
        let id = Uuid::new_v4();
        let msg = MeshMessage::Pong { node_id: id };
        let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["type"], "Pong");
        assert_eq!(v["node_id"], id.to_string());
        let back: MeshMessage = serde_json::from_value(v).unwrap();
        if let MeshMessage::Pong { node_id } = back {
            assert_eq!(node_id, id);
        } else {
            panic!("expected Pong");
        }
    }

    #[test]
    fn tag_discrimination_announce_vs_goodbye() {
        // Give both messages the same node_id; the "type" tag must route correctly.
        let id = Uuid::new_v4();
        let announce_json = serde_json::json!({
            "type": "Announce",
            "node_id": id,
            "name": "n",
            "address": "1.2.3.4",
            "port": 9000,
            "capabilities": [],
        });
        let goodbye_json = serde_json::json!({
            "type": "Goodbye",
            "node_id": id,
        });
        let ann: MeshMessage = serde_json::from_value(announce_json).unwrap();
        let bye: MeshMessage = serde_json::from_value(goodbye_json).unwrap();
        assert!(matches!(ann, MeshMessage::Announce { .. }));
        assert!(matches!(bye, MeshMessage::Goodbye { .. }));
    }
}
