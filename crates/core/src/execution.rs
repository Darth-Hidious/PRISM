//! Harness execution envelope and trace events.
//!
//! This is not the future Mission IR. It is the minimal control-plane object
//! that lets existing agent, API, workflow, MCP, and tool-server execution
//! paths state who is executing, under which policy mode, with which tool
//! bounds, and how the run can be reconstructed from trace events.

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const EXECUTION_ENVELOPE_SCHEMA_VERSION: &str = "prism.execution_envelope.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionRuntime {
    Workflow,
    Agent,
    Api,
    Mcp,
    ToolServer,
    Legacy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PolicyMode {
    Required,
    Optional,
    DisabledForDryRun,
    LegacyUnchecked,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionBudget {
    pub max_steps: u32,
    pub max_tool_calls: u32,
    pub max_wall_time_ms: u64,
}

impl ExecutionBudget {
    #[must_use]
    pub fn conservative_default() -> Self {
        Self {
            max_steps: 100,
            max_tool_calls: 50,
            max_wall_time_ms: 300_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionEnvelope {
    pub schema_version: String,
    pub mission_id: String,
    pub trace_id: String,
    pub runtime: ExecutionRuntime,
    pub entrypoint: String,
    pub policy_mode: PolicyMode,
    pub allowed_tools: Vec<String>,
    pub forbidden_tools: Vec<String>,
    pub budget: ExecutionBudget,
    pub provenance_required: bool,
    pub created_at: DateTime<Utc>,
}

impl ExecutionEnvelope {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        schema_version: impl Into<String>,
        mission_id: impl Into<String>,
        trace_id: impl Into<String>,
        runtime: ExecutionRuntime,
        entrypoint: impl Into<String>,
        policy_mode: PolicyMode,
        allowed_tools: Vec<String>,
        forbidden_tools: Vec<String>,
        budget: ExecutionBudget,
        provenance_required: bool,
    ) -> Result<Self> {
        let envelope = Self {
            schema_version: schema_version.into(),
            mission_id: mission_id.into(),
            trace_id: trace_id.into(),
            runtime,
            entrypoint: entrypoint.into(),
            policy_mode,
            allowed_tools,
            forbidden_tools,
            budget,
            provenance_required,
            created_at: Utc::now(),
        };
        envelope.validate()?;
        Ok(envelope)
    }

    /// Explicit compatibility context for legacy paths that have not yet been
    /// fully moved behind a policy-required harness. This is deliberately
    /// named and traceable; callers should not use it for new hosted execution.
    #[must_use]
    pub fn legacy_unchecked(runtime: ExecutionRuntime, entrypoint: impl Into<String>) -> Self {
        Self {
            schema_version: EXECUTION_ENVELOPE_SCHEMA_VERSION.to_string(),
            mission_id: format!("legacy-{}", Uuid::new_v4()),
            trace_id: Uuid::new_v4().to_string(),
            runtime,
            entrypoint: entrypoint.into(),
            policy_mode: PolicyMode::LegacyUnchecked,
            allowed_tools: vec!["*".to_string()],
            forbidden_tools: Vec::new(),
            budget: ExecutionBudget::conservative_default(),
            provenance_required: false,
            created_at: Utc::now(),
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != EXECUTION_ENVELOPE_SCHEMA_VERSION {
            bail!(
                "unsupported execution envelope schema_version '{}'",
                self.schema_version
            );
        }
        if self.mission_id.trim().is_empty() {
            bail!("execution envelope mission_id is required");
        }
        if self.trace_id.trim().is_empty() {
            bail!("execution envelope trace_id is required");
        }
        if self.entrypoint.trim().is_empty() {
            bail!("execution envelope entrypoint is required");
        }
        if self.budget.max_steps == 0
            || self.budget.max_tool_calls == 0
            || self.budget.max_wall_time_ms == 0
        {
            bail!("execution envelope budget values must be greater than zero");
        }
        if self.policy_mode != PolicyMode::LegacyUnchecked && self.allowed_tools.is_empty() {
            bail!("execution envelope allowed_tools must be explicit for non-legacy execution");
        }
        Ok(())
    }

    #[must_use]
    pub fn is_legacy_unchecked(&self) -> bool {
        self.policy_mode == PolicyMode::LegacyUnchecked
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HarnessEventType {
    ModelRequest,
    ModelResponse,
    ParsedAction,
    ValidationFailure,
    ValidationSuccess,
    PermissionDecision,
    PolicyDecision,
    ToolStart,
    ToolResult,
    WorkflowStepStart,
    WorkflowStepResult,
    FinalResult,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HarnessTraceEvent {
    pub trace_id: String,
    pub mission_id: String,
    pub parent_event_id: Option<String>,
    pub event_id: String,
    pub timestamp: DateTime<Utc>,
    pub entrypoint: String,
    pub runtime: ExecutionRuntime,
    pub step_index: Option<u32>,
    pub event_type: HarnessEventType,
    pub model_name: Option<String>,
    pub raw_model_output_digest: Option<String>,
    pub parsed_action: Option<Value>,
    pub validation_status: Option<String>,
    pub validation_error: Option<String>,
    pub permission_decision: Option<String>,
    pub policy_decision: Option<String>,
    pub tool_name: Option<String>,
    pub tool_args_digest: Option<String>,
    pub tool_result_digest: Option<String>,
    pub status: String,
    pub error: Option<String>,
    pub schema_version: String,
}

impl HarnessTraceEvent {
    #[must_use]
    pub fn new(envelope: &ExecutionEnvelope, event_type: HarnessEventType, status: &str) -> Self {
        Self {
            trace_id: envelope.trace_id.clone(),
            mission_id: envelope.mission_id.clone(),
            parent_event_id: None,
            event_id: Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            entrypoint: envelope.entrypoint.clone(),
            runtime: envelope.runtime.clone(),
            step_index: None,
            event_type,
            model_name: None,
            raw_model_output_digest: None,
            parsed_action: None,
            validation_status: None,
            validation_error: None,
            permission_decision: None,
            policy_decision: None,
            tool_name: None,
            tool_args_digest: None,
            tool_result_digest: None,
            status: status.to_string(),
            error: None,
            schema_version: EXECUTION_ENVELOPE_SCHEMA_VERSION.to_string(),
        }
    }

    #[must_use]
    pub fn with_parent(mut self, parent_event_id: Option<String>) -> Self {
        self.parent_event_id = parent_event_id;
        self
    }
}

#[derive(Debug, Clone)]
pub struct HarnessTrace {
    envelope: ExecutionEnvelope,
    events: Vec<HarnessTraceEvent>,
    last_event_id: Option<String>,
}

impl HarnessTrace {
    #[must_use]
    pub fn new(envelope: ExecutionEnvelope) -> Self {
        Self {
            envelope,
            events: Vec::new(),
            last_event_id: None,
        }
    }

    pub fn record(&mut self, event_type: HarnessEventType, status: &str) -> &HarnessTraceEvent {
        let event = HarnessTraceEvent::new(&self.envelope, event_type, status)
            .with_parent(self.last_event_id.clone());
        self.last_event_id = Some(event.event_id.clone());
        self.events.push(event);
        self.events.last().expect("event was just pushed")
    }

    pub fn push(&mut self, mut event: HarnessTraceEvent) -> &HarnessTraceEvent {
        event.parent_event_id = self.last_event_id.clone();
        self.last_event_id = Some(event.event_id.clone());
        self.events.push(event);
        self.events.last().expect("event was just pushed")
    }

    #[must_use]
    pub fn envelope(&self) -> &ExecutionEnvelope {
        &self.envelope
    }

    #[must_use]
    pub fn events(&self) -> &[HarnessTraceEvent] {
        &self.events
    }
}

#[must_use]
pub fn redacted_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let redacted = map
                .iter()
                .map(|(key, value)| {
                    let lowered = key.to_ascii_lowercase();
                    if lowered.contains("token")
                        || lowered.contains("secret")
                        || lowered.contains("password")
                        || lowered.contains("api_key")
                        || lowered.contains("authorization")
                    {
                        (key.clone(), Value::String("[REDACTED]".to_string()))
                    } else {
                        (key.clone(), redacted_value(value))
                    }
                })
                .collect();
            Value::Object(redacted)
        }
        Value::Array(items) => Value::Array(items.iter().map(redacted_value).collect()),
        Value::String(s) if s.len() > 4096 => Value::String(format!(
            "[{} chars sha256:{}]",
            s.len(),
            digest_bytes(s.as_bytes())
        )),
        _ => value.clone(),
    }
}

#[must_use]
pub fn value_digest(value: &Value) -> String {
    let redacted = redacted_value(value);
    let bytes = serde_json::to_vec(&redacted).unwrap_or_else(|_| b"<unserializable>".to_vec());
    digest_bytes(&bytes)
}

#[must_use]
pub fn digest_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("sha256:{}", hex::encode(digest))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_envelope() -> ExecutionEnvelope {
        ExecutionEnvelope::new(
            EXECUTION_ENVELOPE_SCHEMA_VERSION,
            "mission-1",
            "trace-1",
            ExecutionRuntime::Workflow,
            "test",
            PolicyMode::Required,
            vec!["file".to_string()],
            vec!["execute_bash".to_string()],
            ExecutionBudget::conservative_default(),
            true,
        )
        .unwrap()
    }

    #[test]
    fn valid_context_parses_and_has_trace_id() {
        let envelope = valid_envelope();
        assert_eq!(envelope.trace_id, "trace-1");
        assert!(!envelope.created_at.to_rfc3339().is_empty());
    }

    #[test]
    fn missing_version_is_rejected_by_deserializer() {
        let json = serde_json::json!({
            "mission_id": "mission-1",
            "trace_id": "trace-1",
            "runtime": "workflow",
            "entrypoint": "test",
            "policy_mode": "required",
            "allowed_tools": ["file"],
            "forbidden_tools": [],
            "budget": {"max_steps": 1, "max_tool_calls": 1, "max_wall_time_ms": 1000},
            "provenance_required": true,
            "created_at": Utc::now()
        });
        assert!(serde_json::from_value::<ExecutionEnvelope>(json).is_err());
    }

    #[test]
    fn unknown_version_is_rejected() {
        let mut envelope = valid_envelope();
        envelope.schema_version = "prism.execution_envelope.v99".to_string();
        assert!(envelope.validate().is_err());
    }

    #[test]
    fn missing_sensitive_fields_are_rejected_by_deserializer() {
        let json = serde_json::json!({
            "schema_version": EXECUTION_ENVELOPE_SCHEMA_VERSION,
            "mission_id": "mission-1",
            "trace_id": "trace-1",
            "runtime": "workflow",
            "entrypoint": "test",
            "created_at": Utc::now()
        });
        assert!(serde_json::from_value::<ExecutionEnvelope>(json).is_err());
    }

    #[test]
    fn explicit_legacy_context_is_marked_legacy() {
        let envelope = ExecutionEnvelope::legacy_unchecked(ExecutionRuntime::Legacy, "legacy-test");
        assert!(envelope.is_legacy_unchecked());
        assert_eq!(envelope.allowed_tools, vec!["*"]);
        assert!(!envelope.trace_id.is_empty());
    }

    #[test]
    fn trace_chain_uses_one_trace_id() {
        let envelope = valid_envelope();
        let trace_id = envelope.trace_id.clone();
        let mut trace = HarnessTrace::new(envelope);
        trace.record(HarnessEventType::ModelRequest, "started");
        trace.record(HarnessEventType::ModelResponse, "ok");
        assert_eq!(trace.events().len(), 2);
        assert!(trace.events()[1].parent_event_id.is_some());
        assert!(trace.events().iter().all(|e| e.trace_id == trace_id));
    }

    #[test]
    fn digest_redacts_secret_like_fields() {
        let value = serde_json::json!({"token": "abc", "nested": {"api_key": "def"}});
        let redacted = redacted_value(&value);
        assert_eq!(redacted["token"], "[REDACTED]");
        assert_eq!(redacted["nested"]["api_key"], "[REDACTED]");
        assert!(value_digest(&value).starts_with("sha256:"));
    }
}
