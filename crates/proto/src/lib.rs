// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! Typed protocol contracts for the PRISM backbone.
//!
//! Centralizes the wire types shared between:
//!
//! - The Rust CLI/runtime and the Python TAOR worker ([`BackendRequest`], [`BackendResponse`]).
//! - The `prism-node` daemon and the MARC27 platform ([`NodeMessage`], [`PlatformMessage`]).
//! - Node capability advertisement ([`NodeCapabilities`], [`GpuInfo`], [`NodeService`]).
//!
//! All types derive `Serialize`/`Deserialize` for JSON transport. This crate has
//! zero business logic — it is purely a type definition boundary.

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

    // ── New edge-case tests ─────────────────────────────────────────

    #[test]
    fn backend_request_serde_roundtrip() {
        let req = BackendRequest {
            id: 99,
            method: "agent.run".into(),
            params: serde_json::json!({"prompt": "hello"}),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: BackendRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn backend_response_ok_roundtrip() {
        let resp = BackendResponse {
            id: 1,
            result: Some(serde_json::json!({"done": true})),
            error: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: BackendResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back, resp);
        // error must be absent (skip_serializing_if None)
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v.get("error").is_none());
    }

    #[test]
    fn backend_response_error_roundtrip() {
        let resp = BackendResponse {
            id: 2,
            result: None,
            error: Some(BackendError {
                code: -1,
                message: "something went wrong".into(),
            }),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: BackendResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back, resp);
        // result must be absent (skip_serializing_if None)
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v.get("result").is_none());
    }

    #[test]
    fn backend_notification_roundtrip() {
        let notif = BackendNotification {
            method: "ui.plan.update".into(),
            params: serde_json::json!({"step": 3}),
        };
        let json = serde_json::to_string(&notif).unwrap();
        let back: BackendNotification = serde_json::from_str(&json).unwrap();
        assert_eq!(back, notif);
    }

    fn full_capabilities() -> NodeCapabilities {
        let mut labels = BTreeMap::new();
        labels.insert("region".into(), "eu-west".into());
        labels.insert("tier".into(), "premium".into());
        NodeCapabilities {
            gpus: vec![GpuInfo {
                gpu_type: "A100".into(),
                count: 4,
                vram_gb: 80,
            }],
            cpu_cores: 64,
            ram_gb: 256,
            disk_gb: 4096,
            software: vec!["docker".into(), "lammps".into()],
            container_runtime: Some("docker".into()),
            docker: true,
            scheduler: Some("slurm".into()),
            labels,
            storage_available_gb: 2048,
            datasets: vec![DatasetInfo {
                name: "alloy-db".into(),
                path: "/data/alloy-db".into(),
                size_gb: 12.5,
                entries: Some(1_000_000),
                format: Some("parquet".into()),
            }],
            models: vec![ModelInfo {
                name: "llama-3".into(),
                path: "/models/llama-3".into(),
                format: Some("gguf".into()),
                size_gb: Some(7.0),
            }],
            services: vec![NodeService {
                kind: "llm".into(),
                name: "llama-3-service".into(),
                status: "running".into(),
                endpoint: Some("http://localhost:8080".into()),
                model: Some("llama-3".into()),
            }],
            visibility: "public".into(),
            price_per_hour_usd: Some(2.50),
            public_key: Some("dGVzdC1rZXk=".into()),
        }
    }

    #[test]
    fn node_capabilities_full_serde_roundtrip() {
        let caps = full_capabilities();
        let json = serde_json::to_string(&caps).unwrap();
        let back: NodeCapabilities = serde_json::from_str(&json).unwrap();
        assert_eq!(back, caps);
    }

    #[test]
    fn node_capabilities_minimal_defaults() {
        // Only required (non-default) fields; everything with #[serde(default)] omitted
        let json = r#"{
            "cpu_cores": 2,
            "ram_gb": 4,
            "disk_gb": 50
        }"#;
        let caps: NodeCapabilities = serde_json::from_str(json).unwrap();
        assert!(caps.gpus.is_empty());
        assert!(caps.software.is_empty());
        assert!(caps.labels.is_empty());
        assert!(caps.datasets.is_empty());
        assert!(caps.models.is_empty());
        assert!(caps.services.is_empty());
        assert!(!caps.docker);
        assert_eq!(caps.storage_available_gb, 0);
        assert_eq!(caps.visibility, "private");
        assert!(caps.price_per_hour_usd.is_none());
        assert!(caps.public_key.is_none());
        assert!(caps.container_runtime.is_none());
        assert!(caps.scheduler.is_none());
    }

    #[test]
    fn node_message_heartbeat_roundtrip() {
        let msg = NodeMessage::Heartbeat {
            cpu_load: 0.75,
            memory_usage: 0.60,
            gpus_free: 2,
            active_jobs: 3,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: NodeMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "heartbeat");
    }

    #[test]
    fn node_message_job_update_roundtrip() {
        let job_id = Uuid::new_v4();
        let msg = NodeMessage::JobUpdate {
            job_id,
            progress: 0.42,
            message: Some("Running step 2".into()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: NodeMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn node_message_job_complete_roundtrip() {
        let job_id = Uuid::new_v4();
        let msg = NodeMessage::JobComplete {
            job_id,
            output: serde_json::json!({"energy": -3.1}),
            output_path: Some("/results/job.hdf5".into()),
            duration_secs: 120,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: NodeMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn node_message_job_failed_roundtrip() {
        let job_id = Uuid::new_v4();
        let msg = NodeMessage::JobFailed {
            job_id,
            error: "OOM killed".into(),
            output: None,
            duration_secs: 45,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: NodeMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
        // output_path is None, must not appear in JSON
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v.get("output").is_none());
    }

    #[test]
    fn node_message_job_logs_roundtrip() {
        let job_id = Uuid::new_v4();
        let msg = NodeMessage::JobLogs {
            job_id,
            lines: vec!["Step 1 done".into(), "Step 2 done".into()],
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: NodeMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn platform_message_registered_roundtrip() {
        let node_id = Uuid::new_v4();
        let msg = PlatformMessage::Registered {
            node_id,
            heartbeat_interval_secs: 30,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: PlatformMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "registered");
    }

    #[test]
    fn platform_message_submit_job_with_env_vars_roundtrip() {
        let mut env_vars = BTreeMap::new();
        env_vars.insert("LAMMPS_OMP_NUM_THREADS".into(), "4".into());
        env_vars.insert("MY_SECRET".into(), "hunter2".into());
        let msg = PlatformMessage::SubmitJob {
            job_id: Uuid::new_v4(),
            image: "marc27/lammps:latest".into(),
            inputs: serde_json::json!({"structure": "FCC-Fe.cif"}),
            env_vars,
            gpu_type: Some("A100".into()),
            timeout_secs: 3600,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: PlatformMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
        if let PlatformMessage::SubmitJob { env_vars, .. } = &back {
            assert_eq!(env_vars["LAMMPS_OMP_NUM_THREADS"], "4");
        } else {
            panic!("expected SubmitJob");
        }
    }

    #[test]
    fn platform_message_cancel_job_roundtrip() {
        let job_id = Uuid::new_v4();
        let msg = PlatformMessage::CancelJob { job_id };
        let json = serde_json::to_string(&msg).unwrap();
        let back: PlatformMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "cancel_job");
    }

    #[test]
    fn platform_message_ping_roundtrip() {
        let msg = PlatformMessage::Ping;
        let json = serde_json::to_string(&msg).unwrap();
        let back: PlatformMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "ping");
    }

    #[test]
    fn platform_message_error_roundtrip() {
        let msg = PlatformMessage::Error {
            code: "NODE_BANNED".into(),
            message: "Your node has been suspended.".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: PlatformMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn gpu_info_serde_roundtrip() {
        let gpu = GpuInfo {
            gpu_type: "H100".into(),
            count: 8,
            vram_gb: 80,
        };
        let json = serde_json::to_string(&gpu).unwrap();
        let back: GpuInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back, gpu);
    }

    #[test]
    fn dataset_info_with_optional_fields() {
        let full = DatasetInfo {
            name: "phase-db".into(),
            path: "/data/phase-db".into(),
            size_gb: 5.5,
            entries: Some(500_000),
            format: Some("hdf5".into()),
        };
        let json = serde_json::to_string(&full).unwrap();
        let back: DatasetInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back, full);
    }

    #[test]
    fn dataset_info_without_optional_fields() {
        let minimal = DatasetInfo {
            name: "tiny-db".into(),
            path: "/data/tiny".into(),
            size_gb: 0.1,
            entries: None,
            format: None,
        };
        let json = serde_json::to_string(&minimal).unwrap();
        // Optional fields should be absent in JSON
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v.get("entries").is_none());
        assert!(v.get("format").is_none());
        let back: DatasetInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back, minimal);
    }

    #[test]
    fn node_service_with_optional_fields() {
        let svc = NodeService {
            kind: "llm".into(),
            name: "mistral-7b".into(),
            status: "running".into(),
            endpoint: Some("http://localhost:11434".into()),
            model: Some("mistral:7b".into()),
        };
        let json = serde_json::to_string(&svc).unwrap();
        let back: NodeService = serde_json::from_str(&json).unwrap();
        assert_eq!(back, svc);
    }

    #[test]
    fn node_service_without_optional_fields() {
        let svc = NodeService {
            kind: "storage".into(),
            name: "minio".into(),
            status: "starting".into(),
            endpoint: None,
            model: None,
        };
        let json = serde_json::to_string(&svc).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v.get("endpoint").is_none());
        assert!(v.get("model").is_none());
        let back: NodeService = serde_json::from_str(&json).unwrap();
        assert_eq!(back, svc);
    }

    #[test]
    fn backend_protocol_version_is_1() {
        assert_eq!(BACKEND_PROTOCOL_VERSION, 1);
    }

    #[test]
    fn node_protocol_version_is_1() {
        assert_eq!(NODE_PROTOCOL_VERSION, 1);
    }

    #[test]
    fn node_capabilities_default_visibility_is_private() {
        let json = r#"{"cpu_cores":1,"ram_gb":1,"disk_gb":1}"#;
        let caps: NodeCapabilities = serde_json::from_str(json).unwrap();
        assert_eq!(caps.visibility, "private");
    }

    #[test]
    fn node_capabilities_public_key_none_not_in_json() {
        let caps = NodeCapabilities {
            gpus: vec![],
            cpu_cores: 1,
            ram_gb: 1,
            disk_gb: 1,
            software: vec![],
            container_runtime: None,
            docker: false,
            scheduler: None,
            labels: BTreeMap::new(),
            storage_available_gb: 0,
            datasets: vec![],
            models: vec![],
            services: vec![],
            visibility: "private".into(),
            price_per_hour_usd: None,
            public_key: None,
        };
        let json = serde_json::to_string(&caps).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(
            v.get("public_key").is_none(),
            "public_key=None must be omitted via skip_serializing_if"
        );
    }
}
