//! Typed protocol contracts for the Rust PRISM backbone.
//!
//! The goal of this crate is to centralize the contracts that sit between:
//! - the Rust CLI/runtime and the Python worker
//! - the local `prism-node` runtime and the MARC27 platform
//!
//! The Python worker remains the active TAOR/tool runtime for now, but the
//! backbone should stop depending on untyped ad hoc JSON as migration continues.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const BACKEND_PROTOCOL_VERSION: u32 = 1;
pub const NODE_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JsonRpcEnvelope<T> {
    pub jsonrpc: String,
    #[serde(flatten)]
    pub payload: T,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum BackendPayload {
    Request(BackendRequest),
    Response(BackendResponse),
    Notification(BackendNotification),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BackendRequest {
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BackendResponse {
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<BackendError>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BackendNotification {
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackendError {
    pub code: i32,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeCapabilities {
    #[serde(default)]
    pub gpus: Vec<GpuInfo>,
    pub cpu_cores: u32,
    pub ram_gb: u64,
    pub disk_gb: u64,
    #[serde(default)]
    pub software: Vec<String>,
    pub container_runtime: Option<String>,
    #[serde(default)]
    pub docker: bool,
    pub scheduler: Option<String>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub storage_available_gb: u32,
    #[serde(default)]
    pub datasets: Vec<DatasetInfo>,
    #[serde(default)]
    pub models: Vec<ModelInfo>,
    #[serde(default)]
    pub services: Vec<NodeService>,
    #[serde(default = "default_visibility")]
    pub visibility: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price_per_hour_usd: Option<f64>,
    /// Base64-encoded X25519 public key for E2EE node-to-node communication.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_key: Option<String>,
}

fn default_visibility() -> String {
    "private".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GpuInfo {
    pub gpu_type: String,
    pub count: u32,
    pub vram_gb: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DatasetInfo {
    pub name: String,
    pub path: String,
    pub size_gb: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entries: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelInfo {
    pub name: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_gb: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeService {
    pub kind: String,
    pub name: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NodeMessage {
    Register {
        name: String,
        org_id: Option<Uuid>,
        capabilities: Box<NodeCapabilities>,
    },
    Heartbeat {
        cpu_load: f64,
        memory_usage: f64,
        gpus_free: u32,
        active_jobs: u32,
    },
    JobUpdate {
        job_id: Uuid,
        progress: f64,
        message: Option<String>,
    },
    JobComplete {
        job_id: Uuid,
        output: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        output_path: Option<String>,
        duration_secs: u64,
    },
    JobFailed {
        job_id: Uuid,
        error: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        output: Option<serde_json::Value>,
        duration_secs: u64,
    },
    JobLogs {
        job_id: Uuid,
        lines: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PlatformMessage {
    Registered {
        node_id: Uuid,
        heartbeat_interval_secs: u32,
    },
    SubmitJob {
        job_id: Uuid,
        image: String,
        inputs: serde_json::Value,
        #[serde(default)]
        env_vars: BTreeMap<String, String>,
        gpu_type: Option<String>,
        timeout_secs: u64,
    },
    CancelJob {
        job_id: Uuid,
    },
    Ping,
    Error {
        code: String,
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_notification_round_trip() {
        let notification = JsonRpcEnvelope {
            jsonrpc: "2.0".to_string(),
            payload: BackendPayload::Notification(BackendNotification {
                method: "ui.text.delta".to_string(),
                params: serde_json::json!({ "text": "hello" }),
            }),
        };

        let json = serde_json::to_string(&notification).unwrap();
        let parsed: JsonRpcEnvelope<BackendPayload> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, notification);
    }

    #[test]
    fn node_register_round_trip() {
        let message = NodeMessage::Register {
            name: "node-1".to_string(),
            org_id: None,
            capabilities: Box::new(NodeCapabilities {
                gpus: vec![],
                cpu_cores: 8,
                ram_gb: 32,
                disk_gb: 512,
                software: vec!["docker".to_string(), "pyiron".to_string()],
                container_runtime: Some("docker".to_string()),
                docker: true,
                scheduler: Some("slurm".to_string()),
                labels: BTreeMap::new(),
                storage_available_gb: 256,
                datasets: vec![],
                models: vec![],
                services: vec![],
                visibility: "private".to_string(),
                price_per_hour_usd: None,
                public_key: Some("dGVzdC1wdWJsaWMta2V5".to_string()),
            }),
        };

        let json = serde_json::to_string(&message).unwrap();
        let parsed: NodeMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, message);
    }
}
