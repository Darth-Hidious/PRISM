//! OPA/Rego policy engine for PRISM.
//!
//! Evaluates agent actions, workflow executions, and tool calls against
//! declarative Rego policies. Uses Microsoft's `regorus` crate — a pure-Rust
//! OPA interpreter with no external dependencies.
//!
//! # Architecture
//!
//! ```text
//! Agent/Workflow ──► PolicyEngine::evaluate() ──► Rego evaluation
//!                         │                            │
//!                    PolicyInput {                  PolicyDecision {
//!                      action, user,                 allowed, reason,
//!                      resource, context             obligations
//!                    }                              }
//! ```
//!
//! # Policy Discovery
//!
//! Rego files are loaded from (in order, later overrides earlier):
//! 1. Built-in defaults (embedded in binary)
//! 2. `~/.prism/policies/*.rego` (global user policies)
//! 3. `.prism/policies/*.rego` (project-level policies)

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Built-in default policy
// ---------------------------------------------------------------------------

const DEFAULT_POLICY: &str = include_str!("default.rego");

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// What the caller wants to do.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyInput {
    /// Action category: "workflow.execute", "tool.call", "agent.action", "data.query"
    pub action: String,
    /// Who is requesting (user ID, role, or "agent")
    pub principal: String,
    /// Principal's role: "admin", "operator", "viewer", "agent"
    pub role: String,
    /// Target resource name (workflow name, tool name, etc.)
    pub resource: String,
    /// Additional context (workflow step config, tool inputs, etc.)
    #[serde(default)]
    pub context: serde_json::Value,
}

/// What the policy engine decided.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyDecision {
    /// Whether the action is allowed.
    pub allowed: bool,
    /// Human-readable reason for the decision.
    pub reason: String,
    /// Optional obligations the caller must fulfill (e.g. "log_audit", "require_mfa").
    #[serde(default)]
    pub obligations: Vec<String>,
    /// Violations found (only populated when denied).
    #[serde(default)]
    pub violations: Vec<String>,
}

impl PolicyDecision {
    fn allow(reason: impl Into<String>) -> Self {
        Self {
            allowed: true,
            reason: reason.into(),
            obligations: Vec::new(),
            violations: Vec::new(),
        }
    }

    fn deny(reason: impl Into<String>, violations: Vec<String>) -> Self {
        Self {
            allowed: false,
            reason: reason.into(),
            obligations: Vec::new(),
            violations,
        }
    }
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// The PRISM policy engine. Wraps `regorus::Engine` with PRISM-specific
/// input/output types and policy discovery.
pub struct PolicyEngine {
    engine: regorus::Engine,
    policy_count: usize,
}

impl PolicyEngine {
    /// Create a new policy engine with the built-in default policy.
    pub fn new() -> Result<Self> {
        let mut engine = regorus::Engine::new();

        engine
            .add_policy("builtin/default.rego".into(), DEFAULT_POLICY.into())
            .context("failed to load built-in default policy")?;

        Ok(Self {
            engine,
            policy_count: 1,
        })
    }

    /// Create with default policy + discover user/project policies.
    pub fn with_discovery(project_root: Option<&Path>) -> Result<Self> {
        let mut pe = Self::new()?;

        for dir in policy_search_paths(project_root) {
            if dir.is_dir() {
                pe.load_directory(&dir)?;
            }
        }

        tracing::info!(policies = pe.policy_count, "policy engine initialized");
        Ok(pe)
    }

    /// Load all `.rego` files from a directory.
    pub fn load_directory(&mut self, dir: &Path) -> Result<usize> {
        let mut loaded = 0;
        let entries = fs::read_dir(dir)
            .with_context(|| format!("failed to read policy directory {}", dir.display()))?;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("rego") {
                continue;
            }
            let text = fs::read_to_string(&path)
                .with_context(|| format!("failed to read policy {}", path.display()))?;
            let id = path.display().to_string();
            self.engine
                .add_policy(id.clone(), text)
                .with_context(|| format!("failed to compile policy {id}"))?;
            self.policy_count += 1;
            loaded += 1;
            tracing::debug!(policy = %id, "loaded policy");
        }
        Ok(loaded)
    }

    /// Load reference data (e.g. approved workflow list, budget limits).
    pub fn add_data(&mut self, data: serde_json::Value) -> Result<()> {
        let regorus_val = regorus::Value::from_json_str(&data.to_string())
            .context("failed to convert data to regorus Value")?;
        self.engine
            .add_data(regorus_val)
            .context("failed to add data to policy engine")
    }

    /// Evaluate a policy decision for the given input.
    pub fn evaluate(&mut self, input: &PolicyInput) -> Result<PolicyDecision> {
        let input_json = serde_json::to_string(input)?;
        let regorus_input = regorus::Value::from_json_str(&input_json)
            .context("failed to serialize policy input")?;
        self.engine.set_input(regorus_input);

        // Evaluate the main allow rule
        let allowed = match self.engine.eval_rule("data.prism.policy.allow".into()) {
            Ok(val) => val_to_bool(&val),
            Err(_) => {
                // If no allow rule matches, default deny
                false
            }
        };

        // Collect denial reasons
        let violations = match self.engine.eval_rule("data.prism.policy.deny".into()) {
            Ok(val) => val_to_string_set(&val),
            Err(_) => Vec::new(),
        };

        // Collect obligations
        let obligations =
            match self.engine.eval_rule("data.prism.policy.obligations".into()) {
                Ok(val) => val_to_string_set(&val),
                Err(_) => Vec::new(),
            };

        // Collect reason
        let reason = match self.engine.eval_rule("data.prism.policy.reason".into()) {
            Ok(val) => val_to_string(&val).unwrap_or_default(),
            Err(_) => String::new(),
        };

        if allowed && violations.is_empty() {
            let mut decision = PolicyDecision::allow(if reason.is_empty() {
                "policy allows action".to_string()
            } else {
                reason
            });
            decision.obligations = obligations;
            Ok(decision)
        } else {
            let reason = if reason.is_empty() {
                if violations.is_empty() {
                    "no policy rule allows this action".to_string()
                } else {
                    violations.join("; ")
                }
            } else {
                reason
            };
            Ok(PolicyDecision::deny(reason, violations))
        }
    }

    /// Convenience: evaluate and return an error if denied.
    pub fn require(&mut self, input: &PolicyInput) -> Result<PolicyDecision> {
        let decision = self.evaluate(input)?;
        if !decision.allowed {
            anyhow::bail!(
                "policy denied {}.{}: {}",
                input.action,
                input.resource,
                decision.reason
            );
        }
        Ok(decision)
    }

    /// How many policies are loaded.
    pub fn policy_count(&self) -> usize {
        self.policy_count
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Directories to search for `.rego` policy files, in load order.
pub fn policy_search_paths(project_root: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // Global user policies
    if let Some(home) = env::var_os("HOME") {
        paths.push(PathBuf::from(home).join(".prism").join("policies"));
    }

    // Project-level policies
    let root = project_root
        .map(Path::to_path_buf)
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    paths.push(root.join(".prism").join("policies"));

    paths
}

fn val_to_bool(val: &regorus::Value) -> bool {
    match val {
        regorus::Value::Bool(b) => *b,
        _ => false,
    }
}

fn val_to_string(val: &regorus::Value) -> Option<String> {
    match val {
        regorus::Value::String(s) => Some(s.to_string()),
        _ => None,
    }
}

fn val_to_string_set(val: &regorus::Value) -> Vec<String> {
    match val {
        regorus::Value::Set(items) => items
            .iter()
            .filter_map(|v| match v {
                regorus::Value::String(s) => Some(s.to_string()),
                _ => None,
            })
            .collect(),
        regorus::Value::Array(items) => items
            .iter()
            .filter_map(|v| match v {
                regorus::Value::String(s) => Some(s.to_string()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_loads() {
        let engine = PolicyEngine::new().unwrap();
        assert_eq!(engine.policy_count(), 1);
    }

    #[test]
    fn admin_allowed_everything() {
        let mut engine = PolicyEngine::new().unwrap();
        let input = PolicyInput {
            action: "workflow.execute".into(),
            principal: "alice".into(),
            role: "admin".into(),
            resource: "train-indexer".into(),
            context: serde_json::json!({}),
        };
        let decision = engine.evaluate(&input).unwrap();
        assert!(decision.allowed, "admin should be allowed: {:?}", decision);
    }

    #[test]
    fn viewer_denied_workflow_execute() {
        let mut engine = PolicyEngine::new().unwrap();
        let input = PolicyInput {
            action: "workflow.execute".into(),
            principal: "bob".into(),
            role: "viewer".into(),
            resource: "train-indexer".into(),
            context: serde_json::json!({}),
        };
        let decision = engine.evaluate(&input).unwrap();
        assert!(!decision.allowed, "viewer should be denied: {:?}", decision);
    }

    #[test]
    fn agent_allowed_safe_tool() {
        let mut engine = PolicyEngine::new().unwrap();
        let input = PolicyInput {
            action: "tool.call".into(),
            principal: "agent".into(),
            role: "agent".into(),
            resource: "knowledge_search".into(),
            context: serde_json::json!({}),
        };
        let decision = engine.evaluate(&input).unwrap();
        assert!(decision.allowed, "agent should call safe tools: {:?}", decision);
    }

    #[test]
    fn agent_denied_destructive_tool() {
        let mut engine = PolicyEngine::new().unwrap();
        let input = PolicyInput {
            action: "tool.call".into(),
            principal: "agent".into(),
            role: "agent".into(),
            resource: "knowledge_ingest".into(),
            context: serde_json::json!({"mode": "delete"}),
        };
        let decision = engine.evaluate(&input).unwrap();
        assert!(!decision.allowed, "agent should be denied destructive tool: {:?}", decision);
    }

    #[test]
    fn require_returns_error_on_deny() {
        let mut engine = PolicyEngine::new().unwrap();
        let input = PolicyInput {
            action: "workflow.execute".into(),
            principal: "bob".into(),
            role: "viewer".into(),
            resource: "anything".into(),
            context: serde_json::json!({}),
        };
        assert!(engine.require(&input).is_err());
    }

    #[test]
    fn operator_allowed_workflow_with_audit_obligation() {
        let mut engine = PolicyEngine::new().unwrap();
        let input = PolicyInput {
            action: "workflow.execute".into(),
            principal: "charlie".into(),
            role: "operator".into(),
            resource: "train-indexer".into(),
            context: serde_json::json!({}),
        };
        let decision = engine.evaluate(&input).unwrap();
        assert!(decision.allowed);
        assert!(decision.obligations.contains(&"audit_log".to_string()));
    }
}
