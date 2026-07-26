// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! Typed protocol contracts for the PRISM backbone.
//!
//! Centralizes the wire types shared between:
//!
//! - The Rust CLI/runtime and the Python TAOR worker ([`BackendRequest`], [`BackendResponse`]).
//! - The `prism-node` daemon and the MARC27 platform ([`NodeMessage`], [`PlatformMessage`]).
//! - Node capability advertisement ([`NodeCapabilities`], [`GpuInfo`], [`NodeService`]).
//!
//! All types derive `Serialize`/`Deserialize` for JSON transport. The crate
//! carries no business logic; the only behaviour it owns is what the wire
//! contract itself defines and both sides must agree on — [`ResourceRequest::validate`],
//! [`NodeCapabilities::has_scheduler`], and the drift guard
//! ([`SUBMIT_JOB_FIELDS`] / [`unknown_submit_job_fields`]).
//!
//! # Two-repo protocol
//!
//! The hub half of the node protocol lives in a separate, private repository
//! (marc27-core `crates/protocol`, crate `marc27-protocol`) which this public
//! crate cannot depend on. The node types below are therefore a deliberate,
//! *guarded* mirror rather than a shared crate: see the "Wire-drift guard"
//! section near [`unknown_submit_job_fields`] for how a hub that runs ahead of
//! this node is made to fail loudly instead of silently.

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

impl NodeCapabilities {
    /// Does this node dispatch work through the `wanted` scheduler?
    ///
    /// THE canonical scheduler match — the hub's node selection and the node's
    /// own request validation must agree on what "slurm" means, so neither
    /// compares the strings itself. Case-insensitive because `scheduler` is
    /// populated from a probe of the local binaries (`sbatch` → "slurm") on one
    /// side and from a job's [`ResourceRequest::scheduler`] on the other.
    ///
    /// A node that advertises no scheduler matches nothing: "I run containers
    /// directly" is not a weaker form of "I have SLURM".
    #[must_use]
    pub fn has_scheduler(&self, wanted: &str) -> bool {
        self.scheduler
            .as_deref()
            .is_some_and(|s| s.eq_ignore_ascii_case(wanted))
    }
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

// ── Resource requests ───────────────────────────────────────────────

/// How much of what accelerator ONE unit of work needs.
///
/// A request for ZERO devices is a config error, not a CPU-only workload —
/// CPU-only is the enclosing `Option` being `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accelerator {
    /// Normalized accelerator class matched against a provider catalog or a
    /// scheduler's GRES name (e.g. "A100-80GB"); mirrors `SubmitJob::gpu_type`.
    pub class: String,
    /// How many of `class` one unit of work needs.
    pub count: u32,
}

/// What a facility scheduler (SLURM today) is asked to allocate for ONE job.
///
/// Every field is optional and means "the submitter did not say" when absent —
/// the runner then omits the corresponding directive and lets the site default
/// apply, rather than inventing a number. Zero is NEVER "unspecified": a
/// request for zero cores, zero memory, zero seconds, or zero accelerators is
/// malformed and [`Self::validate`] rejects it.
///
/// ## Compatibility contract
///
/// * **Old hub → this node.** The hub omits `resource_request` entirely; serde
///   fills `None` and the node reproduces its pre-existing behaviour exactly.
/// * **New hub → this node.** The ask arrives populated and the node either
///   honours it or REFUSES the job — it is never dropped. A directive this node
///   cannot render (a queue on a machine with no queues, more accelerators than
///   exist, a scheduler it does not dispatch through) is an error, because a
///   job that quietly runs with resources other than the ones asked for burns a
///   real allocation and returns a result nobody can reproduce.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceRequest {
    /// CPU cores for the job's single task (`--cpus-per-task`).
    #[serde(default)]
    pub cpus: Option<u32>,
    /// Memory for the whole job in GB (`--mem`).
    #[serde(default)]
    pub memory_gb: Option<u32>,
    /// Walltime to REQUEST FROM THE SCHEDULER, in seconds (`--time`).
    ///
    /// Deliberately distinct from `SubmitJob::timeout_secs`: the scheduler
    /// walltime drives queue priority and backfill (ask for 30 minutes, get
    /// scheduled sooner), while `timeout_secs` is how long the hub waits before
    /// giving up.
    #[serde(default)]
    pub time_limit_secs: Option<u64>,
    /// Partition / queue to submit to (`--partition`).
    #[serde(default)]
    pub partition: Option<String>,
    /// Scheduler this job REQUIRES (e.g. "slurm"), matched against
    /// [`NodeCapabilities::scheduler`] via [`NodeCapabilities::has_scheduler`].
    /// A runner that does not dispatch through this scheduler refuses the job
    /// instead of quietly running it some other way.
    #[serde(default)]
    pub scheduler: Option<String>,
    /// Accelerator request, or `None` for a CPU-ONLY job — the runner then
    /// emits no GPU directive at all, even when `SubmitJob::gpu_type` is set.
    #[serde(default)]
    pub accelerator: Option<Accelerator>,
}

impl ResourceRequest {
    /// Reject a request that cannot be rendered into a truthful scheduler or
    /// container directive, so the caller fails loud BEFORE anything runs.
    ///
    /// A zero quantity, an empty name, or a name carrying characters that are
    /// not legal in an `#SBATCH` value is an error here rather than something
    /// to silently clamp or strip.
    pub fn validate(&self) -> Result<(), String> {
        if self.cpus == Some(0) {
            return Err(
                "resource request asks for 0 CPU cores — omit `cpus` to accept the site \
                 default instead"
                    .into(),
            );
        }
        if self.memory_gb == Some(0) {
            return Err(
                "resource request asks for 0 GB of memory — omit `memory_gb` to accept the \
                 site default instead"
                    .into(),
            );
        }
        if self.time_limit_secs == Some(0) {
            return Err(
                "resource request asks for a 0s time limit — omit `time_limit_secs` to \
                 derive the walltime from the job timeout instead"
                    .into(),
            );
        }
        if let Some(p) = &self.partition {
            validate_scheduler_token(p, "partition")?;
        }
        if let Some(s) = &self.scheduler {
            validate_scheduler_token(s, "scheduler")?;
        }
        if let Some(a) = &self.accelerator {
            if a.count == 0 {
                return Err(
                    "resource request asks for 0 accelerators — omit `accelerator` entirely \
                     for a CPU-only job"
                        .into(),
                );
            }
            validate_scheduler_token(&a.class, "accelerator class")?;
        }
        Ok(())
    }
}

/// Whether `v` is safe to place verbatim in an `#SBATCH` directive value.
///
/// The directive lines are line-oriented `#`-comments, so whitespace or a
/// newline in a value would break out of the directive and become an
/// executable script line. Restricting to the character set SLURM partition,
/// QOS and GRES names actually use makes that impossible without having to
/// quote (quoting is not portable inside `#SBATCH`).
fn validate_scheduler_token(v: &str, field: &str) -> Result<(), String> {
    if v.is_empty() {
        return Err(format!(
            "resource request {field} is empty — omit the field instead"
        ));
    }
    if !v
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(format!(
            "resource request {field} '{v}' contains characters that are not valid in an \
             #SBATCH directive (allowed: letters, digits, '.', '_', '-')"
        ));
    }
    Ok(())
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
    DeploymentReady {
        deployment_id: Uuid,
        endpoint_url: String,
    },
    DeploymentHealthUpdate {
        deployment_id: Uuid,
        healthy: bool,
        message: Option<String>,
    },
    DeploymentStopped {
        deployment_id: Uuid,
        reason: String,
    },
    /// Result of a relayed tool invocation ([`PlatformMessage::InvokeTool`]).
    ToolInvokeResult {
        invocation_id: Uuid,
        /// False when the tool errored, RBAC denied the caller, or the node
        /// could not run it — `result` then carries the honest error.
        ok: bool,
        result: serde_json::Value,
    },
    /// Result of a relayed deployment inference request
    /// ([`PlatformMessage::InvokeDeployment`]) — the tool-relay pattern applied
    /// to HTTP, with an HTTP-faithful payload. Mirrors the platform's
    /// `NodeMessage::DeploymentInvokeResult` (marc27-core
    /// `crates/protocol/src/messages.rs`) byte-for-byte: `body`/`error` carry no
    /// `skip_serializing_if`, so a `None` serializes as explicit `null` exactly
    /// as the platform emits and expects.
    DeploymentInvokeResult {
        invocation_id: Uuid,
        /// HTTP status the local endpoint answered with.
        status: u16,
        /// Response headers, forwarded verbatim.
        headers: BTreeMap<String, String>,
        /// Response body (bytes-as-JSON for now; streaming is a follow-up).
        body: Option<serde_json::Value>,
        /// Set when the node could not reach the local endpoint at all —
        /// `status`/`body` are then meaningless and must not be trusted.
        error: Option<String>,
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
        /// Max duration in seconds the PLATFORM will wait before it gives up on
        /// this job and cancels it. This is the hub's patience, NOT the walltime
        /// the facility scheduler is asked for — see
        /// [`ResourceRequest::time_limit_secs`].
        timeout_secs: u64,
        /// What the facility scheduler (or container runtime) should allocate.
        ///
        /// `None` means "the submitter said nothing", and the node MUST then
        /// behave exactly as it did before this field existed. `Some` is an
        /// instruction the node either honours or refuses — see
        /// [`ResourceRequest`].
        #[serde(default)]
        resource_request: Option<ResourceRequest>,
    },
    CancelJob {
        job_id: Uuid,
    },
    DeployModel {
        deployment_id: Uuid,
        image: String,
        #[serde(default)]
        env_vars: BTreeMap<String, String>,
        gpu_type: Option<String>,
        deploy_config: serde_json::Value,
    },
    StopDeployment {
        deployment_id: Uuid,
    },
    /// Invoke a tool registered on this node (the tool-call relay). The node
    /// runs it via its LOCAL tool runner AS `caller_user_id` (node-side RBAC
    /// and audit see the real principal) and answers with
    /// [`NodeMessage::ToolInvokeResult`].
    InvokeTool {
        invocation_id: Uuid,
        tool: String,
        #[serde(default)]
        args: serde_json::Value,
        caller_user_id: Uuid,
        timeout_secs: u64,
    },
    /// Relay an HTTP inference request to a deployment served on this node (the
    /// deployment inference relay — [`InvokeTool`]'s pattern applied to HTTP).
    /// The node resolves `deployment_id` to the deployment's LOCAL endpoint
    /// (LAN/127.0.0.1 — reachable FROM the node, not the platform), performs
    /// `{method} {endpoint}{path}` and answers with
    /// [`NodeMessage::DeploymentInvokeResult`] carrying the same
    /// `invocation_id`. One buffered response; SSE/streaming is a follow-up.
    /// Mirrors the platform's `PlatformMessage::InvokeDeployment` (marc27-core
    /// `crates/protocol/src/messages.rs`).
    ///
    /// [`InvokeTool`]: PlatformMessage::InvokeTool
    InvokeDeployment {
        invocation_id: Uuid,
        deployment_id: Uuid,
        /// HTTP method (GET | POST | …).
        method: String,
        /// Path appended to the local endpoint (e.g. "/v1/chat/completions").
        path: String,
        /// Request headers, forwarded verbatim.
        #[serde(default)]
        headers: BTreeMap<String, String>,
        /// Request body (bytes-as-JSON for now).
        body: Option<serde_json::Value>,
    },
    Ping,
    Error {
        code: String,
        message: String,
    },
}

// ── Wire-drift guard ────────────────────────────────────────────────
//
// `prism-proto` is the NODE half of a two-repo wire protocol whose HUB half
// lives in a separate, private repository (marc27-core `crates/protocol`). The
// node can only ever *receive* [`PlatformMessage`], so every drift risk points
// one way: the hub gains a field, this node's `#[serde(tag = "type")]`
// deserializer ignores the unknown key, and the job runs with the instruction
// silently discarded. That is the worst failure shape — no exception, no log,
// no signal, a real allocation burned on the wrong resources.
//
// These two items close it for `submit_job`, the one message where a dropped
// key changes what actually runs. The node compares the raw frame against the
// keys this build understands and REFUSES the job when they do not match,
// naming the fields, over `JobFailed` — a channel the hub already surfaces. So
// the hub finds out, instead of believing the ask was honoured.

/// Every key a `submit_job` frame may legally carry, including the `type` tag.
///
/// Not documentation: [`unknown_submit_job_fields`] enforces it at runtime, and
/// the `submit_job_field_list_matches_the_type` test enforces that this list
/// still describes [`PlatformMessage::SubmitJob`] exactly — add a field to the
/// variant without adding it here and the build fails.
pub const SUBMIT_JOB_FIELDS: &[&str] = &[
    "type",
    "job_id",
    "image",
    "inputs",
    "env_vars",
    "gpu_type",
    "timeout_secs",
    "resource_request",
];

/// Keys in a raw `submit_job` frame that this build of the protocol does not
/// understand — i.e. instructions that deserialization is about to throw away.
///
/// A non-empty result means the hub is speaking a newer protocol than this
/// node. The caller must refuse the job and report these names rather than run
/// it: an instruction the node cannot even see is one it certainly cannot
/// honour.
#[must_use]
pub fn unknown_submit_job_fields(frame: &serde_json::Value) -> Vec<String> {
    let Some(obj) = frame.as_object() else {
        return Vec::new();
    };
    obj.keys()
        .filter(|k| !SUBMIT_JOB_FIELDS.contains(&k.as_str()))
        .cloned()
        .collect()
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

    #[test]
    fn deployment_messages_round_trip() {
        let ready = NodeMessage::DeploymentReady {
            deployment_id: Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap(),
            endpoint_url: "http://192.168.1.50:9001".to_string(),
        };
        let ready_json = serde_json::to_string(&ready).unwrap();
        let ready_back: NodeMessage = serde_json::from_str(&ready_json).unwrap();
        assert_eq!(ready_back, ready);

        let deploy = PlatformMessage::DeployModel {
            deployment_id: Uuid::parse_str("00000000-0000-4000-8000-000000000002").unwrap(),
            image: "hf://sentence-transformers/paraphrase-MiniLM-L3-v2".to_string(),
            env_vars: BTreeMap::from([("MODEL_NAME".to_string(), "mini".to_string())]),
            gpu_type: Some("A100-80GB".to_string()),
            deploy_config: serde_json::json!({
                "port": 9001,
                "health_path": "/health",
            }),
        };
        let deploy_json = serde_json::to_string(&deploy).unwrap();
        let deploy_back: PlatformMessage = serde_json::from_str(&deploy_json).unwrap();
        assert_eq!(deploy_back, deploy);
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

    /// The smallest legal capability profile — a machine that advertises
    /// nothing beyond having a CPU. Deliberately not a `Default` derive:
    /// `visibility` defaults to `"private"` through serde, and a derived
    /// `Default` would silently disagree with the wire by producing `""`.
    fn minimal_capabilities() -> NodeCapabilities {
        NodeCapabilities {
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
            visibility: default_visibility(),
            price_per_hour_usd: None,
            public_key: None,
        }
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
            resource_request: None,
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

    // ── Resource requests: cross-repo wire parity + drift guard ─────────
    //
    // The hub (marc27-core `crates/protocol`) is the source of truth for
    // `submit_job`. These vectors are its own test payloads, verbatim, so a
    // rename or reshape on either side shows up here as a failure rather than
    // as a job that quietly runs on the wrong hardware.

    /// THE REGRESSION TEST for the silent-drop defect. The hub asks for 64
    /// cores, 128 GB, a 4-hour scheduler walltime, a named partition and 4
    /// A100s; before `resource_request` existed on this side, every one of
    /// those vanished during deserialization without an error.
    #[test]
    fn submit_job_carries_the_hub_resource_request_intact() {
        let hub_wire = r#"{
            "type": "submit_job",
            "job_id": "00000000-0000-0000-0000-000000000000",
            "image": "marc27/vasp:latest",
            "inputs": {},
            "env_vars": {},
            "gpu_type": null,
            "timeout_secs": 7200,
            "resource_request": {
                "cpus": 64,
                "memory_gb": 128,
                "time_limit_secs": 14400,
                "partition": "gpu",
                "scheduler": "slurm",
                "accelerator": {"class": "A100-80GB", "count": 4}
            }
        }"#;

        let parsed: PlatformMessage = serde_json::from_str(hub_wire).unwrap();
        let PlatformMessage::SubmitJob {
            resource_request: Some(rr),
            timeout_secs,
            ..
        } = &parsed
        else {
            panic!("expected a submit_job carrying a resource request, got {parsed:?}");
        };
        assert_eq!(rr.cpus, Some(64));
        assert_eq!(rr.memory_gb, Some(128));
        // The scheduler walltime is the job's OWN 4 hours, not the hub's
        // 2-hour patience — the whole point of the split.
        assert_eq!(rr.time_limit_secs, Some(14400));
        assert_eq!(*timeout_secs, 7200);
        assert_eq!(rr.partition.as_deref(), Some("gpu"));
        assert_eq!(rr.scheduler.as_deref(), Some("slurm"));
        assert_eq!(
            rr.accelerator,
            Some(Accelerator {
                class: "A100-80GB".into(),
                count: 4
            })
        );

        // And it survives back onto the wire unchanged.
        let back: PlatformMessage =
            serde_json::from_str(&serde_json::to_string(&parsed).unwrap()).unwrap();
        assert_eq!(back, parsed);
    }

    /// BACKWARD COMPATIBILITY, old hub → this node: a `submit_job` produced
    /// before `resource_request` existed still parses and lands as `None` —
    /// the value that means "behave exactly as before".
    #[test]
    fn submit_job_without_resource_request_parses_as_none() {
        let legacy = r#"{
            "type": "submit_job",
            "job_id": "00000000-0000-0000-0000-000000000000",
            "image": "marc27/lammps:latest",
            "inputs": {},
            "gpu_type": "A100-80GB",
            "timeout_secs": 3600
        }"#;

        let parsed: PlatformMessage = serde_json::from_str(legacy).unwrap();
        let PlatformMessage::SubmitJob {
            resource_request,
            gpu_type,
            timeout_secs,
            ..
        } = parsed
        else {
            panic!("wrong variant");
        };
        assert!(resource_request.is_none());
        assert_eq!(gpu_type.as_deref(), Some("A100-80GB"));
        assert_eq!(timeout_secs, 3600);
    }

    /// CPU-only is `accelerator: None`, and it must survive the wire as such —
    /// if it round-tripped into "unspecified" the node would fall back to the
    /// legacy `gpu_type`-implies-a-GPU rule and allocate hardware nobody asked
    /// for.
    #[test]
    fn cpu_only_resource_request_round_trips() {
        let rr = ResourceRequest {
            cpus: Some(8),
            memory_gb: Some(16),
            accelerator: None,
            ..Default::default()
        };
        let parsed: ResourceRequest =
            serde_json::from_str(&serde_json::to_string(&rr).unwrap()).unwrap();
        assert!(parsed.accelerator.is_none());
        assert_eq!(parsed.cpus, Some(8));
        assert!(parsed.validate().is_ok());
    }

    #[test]
    fn validate_rejects_zero_quantities() {
        let zero_gpu = ResourceRequest {
            accelerator: Some(Accelerator {
                class: "A100-80GB".into(),
                count: 0,
            }),
            ..Default::default()
        };
        assert!(zero_gpu.validate().unwrap_err().contains("0 accelerators"));

        for (rr, needle) in [
            (
                ResourceRequest {
                    cpus: Some(0),
                    ..Default::default()
                },
                "0 CPU cores",
            ),
            (
                ResourceRequest {
                    memory_gb: Some(0),
                    ..Default::default()
                },
                "0 GB",
            ),
            (
                ResourceRequest {
                    time_limit_secs: Some(0),
                    ..Default::default()
                },
                "0s time limit",
            ),
        ] {
            let err = rr.validate().unwrap_err();
            assert!(err.contains(needle), "expected {needle:?} in {err:?}");
        }
    }

    /// A partition name carrying a newline would break out of an `#SBATCH`
    /// comment line into an executable script line. It is REFUSED, not silently
    /// stripped: a job that lands on a different queue than the one requested
    /// is a wrong answer, not a recovered one.
    #[test]
    fn validate_rejects_unsafe_directive_values() {
        let injected = ResourceRequest {
            partition: Some("gpu\n#SBATCH --account=victim".into()),
            ..Default::default()
        };
        assert!(injected.validate().unwrap_err().contains("not valid in an"));

        let empty = ResourceRequest {
            partition: Some(String::new()),
            ..Default::default()
        };
        assert!(empty.validate().unwrap_err().contains("is empty"));

        let ok = ResourceRequest {
            partition: Some("gpu-a100.2".into()),
            scheduler: Some("slurm".into()),
            ..Default::default()
        };
        assert!(ok.validate().is_ok());
    }

    #[test]
    fn has_scheduler_matches_case_insensitively_and_never_guesses() {
        let slurm = NodeCapabilities {
            scheduler: Some("slurm".into()),
            ..minimal_capabilities()
        };
        assert!(slurm.has_scheduler("slurm"));
        assert!(slurm.has_scheduler("SLURM"));
        assert!(!slurm.has_scheduler("pbs"));

        // No advertised scheduler matches nothing at all.
        let plain = minimal_capabilities();
        assert!(!plain.has_scheduler("slurm"));
        assert!(!plain.has_scheduler(""));
    }

    /// THE DRIFT GUARD's own guard. `SUBMIT_JOB_FIELDS` is what the node checks
    /// incoming frames against, so it must describe the variant *exactly*. Add
    /// a field to `PlatformMessage::SubmitJob` and forget this list and the
    /// build stops here — instead of the node rejecting its own hub's frames at
    /// runtime.
    #[test]
    fn submit_job_field_list_matches_the_type() {
        let full = PlatformMessage::SubmitJob {
            job_id: Uuid::nil(),
            image: "img".into(),
            inputs: serde_json::json!({}),
            env_vars: BTreeMap::new(),
            gpu_type: Some("A100".into()),
            timeout_secs: 1,
            resource_request: Some(ResourceRequest::default()),
        };
        let serialized = serde_json::to_value(&full).unwrap();
        let mut actual: Vec<&str> = serialized
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        actual.sort_unstable();
        let mut expected = SUBMIT_JOB_FIELDS.to_vec();
        expected.sort_unstable();
        assert_eq!(
            actual, expected,
            "SUBMIT_JOB_FIELDS no longer describes PlatformMessage::SubmitJob"
        );

        // A frame this build fully understands has nothing unknown in it.
        assert!(unknown_submit_job_fields(&serialized).is_empty());
    }

    /// The next divergence, caught. A hub running ahead of this node adds a
    /// field; deserialization would discard it without a murmur, so the raw
    /// frame is checked instead and the extra keys are named.
    #[test]
    fn unknown_submit_job_fields_names_a_future_hub_field() {
        let future_hub_wire = serde_json::json!({
            "type": "submit_job",
            "job_id": Uuid::nil(),
            "image": "marc27/vasp:latest",
            "inputs": {},
            "env_vars": {},
            "gpu_type": null,
            "timeout_secs": 60,
            "resource_request": null,
            // Not in this build's protocol — exactly the shape `resource_request`
            // itself had on the day it appeared.
            "node_placement": {"rack": "b12"},
            "budget_ceiling_eur": 40
        });

        // It still deserializes without error — that is the defect this guard
        // exists to catch.
        let parsed: PlatformMessage =
            serde_json::from_value(future_hub_wire.clone()).expect("silently parses");
        assert!(matches!(parsed, PlatformMessage::SubmitJob { .. }));

        let mut unknown = unknown_submit_job_fields(&future_hub_wire);
        unknown.sort();
        assert_eq!(unknown, vec!["budget_ceiling_eur", "node_placement"]);
    }

    #[test]
    fn platform_message_invoke_tool_roundtrip() {
        let msg = PlatformMessage::InvokeTool {
            invocation_id: Uuid::new_v4(),
            tool: "evaluate_material".into(),
            args: serde_json::json!({"formula": "Fe2O3"}),
            caller_user_id: Uuid::new_v4(),
            timeout_secs: 60,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: PlatformMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "invoke_tool");
        // args defaults to null when omitted on the wire.
        let no_args = r#"{"type":"invoke_tool","invocation_id":"00000000-0000-0000-0000-000000000000","tool":"status","caller_user_id":"00000000-0000-0000-0000-000000000000","timeout_secs":30}"#;
        let parsed: PlatformMessage = serde_json::from_str(no_args).unwrap();
        if let PlatformMessage::InvokeTool { args, .. } = parsed {
            assert!(args.is_null());
        } else {
            panic!("expected InvokeTool");
        }
    }

    // ── Deployment inference relay: cross-compat with the platform ──────
    //
    // The platform (marc27-core `crates/protocol/src/messages.rs`) is the
    // source of truth for these two frames. These tests pin the PRISM frames
    // to the platform's exact wire shape (snake_case tag, field names, field
    // order, null-for-None on body/error) and prove PRISM parses the literal
    // JSON the platform emits — a mismatched frame would make a deployed model
    // uninvokable.

    #[test]
    fn invoke_deployment_matches_platform_wire() {
        // Same values as marc27-core's `serialize_invoke_deployment` vector.
        let msg = PlatformMessage::InvokeDeployment {
            invocation_id: Uuid::nil(),
            deployment_id: Uuid::nil(),
            method: "POST".into(),
            path: "/v1/chat/completions".into(),
            headers: BTreeMap::from([("content-type".to_string(), "application/json".to_string())]),
            body: Some(serde_json::json!({"model": "llama", "messages": []})),
        };

        // Shape parity: tag + every field name/value the platform emits.
        let value = serde_json::to_value(&msg).unwrap();
        assert_eq!(value["type"], "invoke_deployment");
        assert_eq!(value["invocation_id"], Uuid::nil().to_string());
        assert_eq!(value["deployment_id"], Uuid::nil().to_string());
        assert_eq!(value["method"], "POST");
        assert_eq!(value["path"], "/v1/chat/completions");
        assert_eq!(value["headers"]["content-type"], "application/json");
        assert_eq!(value["body"]["model"], "llama");

        // The exact JSON the platform emits must parse back into the PRISM
        // frame (proves snake_case tag + field names are byte-compatible).
        let platform_wire = r#"{"type":"invoke_deployment","invocation_id":"00000000-0000-0000-0000-000000000000","deployment_id":"00000000-0000-0000-0000-000000000000","method":"POST","path":"/v1/chat/completions","headers":{"content-type":"application/json"},"body":{"model":"llama","messages":[]}}"#;
        let parsed: PlatformMessage = serde_json::from_str(platform_wire).unwrap();
        assert_eq!(parsed, msg);
    }

    #[test]
    fn invoke_deployment_tolerates_omitted_headers_and_body() {
        // The platform's health probe sends no body; `headers` is `#[serde(
        // default)]` and `body` is `Option`, so both may be absent on the wire.
        let wire = r#"{"type":"invoke_deployment","invocation_id":"00000000-0000-0000-0000-000000000000","deployment_id":"00000000-0000-0000-0000-000000000000","method":"GET","path":"/health"}"#;
        let parsed: PlatformMessage = serde_json::from_str(wire).unwrap();
        match parsed {
            PlatformMessage::InvokeDeployment {
                method,
                path,
                headers,
                body,
                ..
            } => {
                assert_eq!(method, "GET");
                assert_eq!(path, "/health");
                assert!(headers.is_empty());
                assert!(body.is_none());
            }
            other => panic!("expected InvokeDeployment, got {other:?}"),
        }
    }

    #[test]
    fn deployment_invoke_result_matches_platform_wire() {
        // Same values as marc27-core's `serialize_deployment_invoke_result`.
        let msg = NodeMessage::DeploymentInvokeResult {
            invocation_id: Uuid::nil(),
            status: 200,
            headers: BTreeMap::from([("content-type".to_string(), "application/json".to_string())]),
            body: Some(serde_json::json!({"choices": []})),
            error: None,
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"deployment_invoke_result\""));
        assert!(json.contains("\"status\":200"));

        // `error: None` MUST serialize as explicit `null` (no
        // skip_serializing_if) — this is how the platform emits it and how its
        // `NodeMessage::DeploymentInvokeResult` reads it back.
        let value = serde_json::to_value(&msg).unwrap();
        assert!(value.get("error").is_some());
        assert!(value["error"].is_null());
        assert!(value.get("body").is_some());

        let back: NodeMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn deployment_invoke_result_error_matches_platform_wire() {
        // The honest-failure shape (node could not reach the local endpoint) —
        // marc27-core's `serialize_deployment_invoke_result_error` vector.
        let msg = NodeMessage::DeploymentInvokeResult {
            invocation_id: Uuid::nil(),
            status: 0,
            headers: BTreeMap::new(),
            body: None,
            error: Some("connection refused (is the container running?)".into()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: NodeMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);

        // The platform reads exactly this wire shape off the node socket.
        let platform_wire = r#"{"type":"deployment_invoke_result","invocation_id":"00000000-0000-0000-0000-000000000000","status":0,"headers":{},"body":null,"error":"connection refused (is the container running?)"}"#;
        let parsed: NodeMessage = serde_json::from_str(platform_wire).unwrap();
        assert_eq!(parsed, msg);
    }

    #[test]
    fn node_message_tool_invoke_result_roundtrip() {
        let ok = NodeMessage::ToolInvokeResult {
            invocation_id: Uuid::new_v4(),
            ok: true,
            result: serde_json::json!({"energy": -5.3411}),
        };
        let json = serde_json::to_string(&ok).unwrap();
        let back: NodeMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ok);

        let err = NodeMessage::ToolInvokeResult {
            invocation_id: Uuid::new_v4(),
            ok: false,
            result: serde_json::json!({"error": "unknown tool 'foo'"}),
        };
        let json = serde_json::to_string(&err).unwrap();
        let back: NodeMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, err);
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
