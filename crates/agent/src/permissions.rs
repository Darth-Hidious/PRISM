//! Immutable tool permission context with 3-tier permission model.
//!
//! A frozen context that captures the permission state for a session.
//! Thread-safe, clonable, can be passed around without mutation concerns.
//!
//! ```rust
//! use prism_agent::permissions::{ToolPermissionContext, PermissionMode, get_tool_permission};
//!
//! let ctx = ToolPermissionContext::default();
//! assert!(!ctx.blocks("search_materials"));
//! assert!(ctx.auto_approves("search_materials"));
//!
//! // Create a more restrictive context
//! let restricted = ctx.with_deny(
//!     &["execute_python".to_string()],
//!     &["compute_".to_string()],
//! );
//! assert!(restricted.blocks("execute_python"));
//! assert!(restricted.blocks("compute_submit"));
//! ```

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use tokio::sync::RwLock;

// ── PermissionMode ─────────────────────────────────────────────────

/// Three-tier permission model for tool access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PermissionMode {
    /// Search, read, query — no mutations.
    ReadOnly = 0,
    /// File edits, exports, code execution.
    WorkspaceWrite = 1,
    /// Everything including destructive ops.
    FullAccess = 2,
}

impl PermissionMode {
    /// Check if this mode allows a tool requiring the given level.
    #[must_use]
    pub fn allows(self, required: PermissionMode) -> bool {
        self >= required
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::FullAccess => "full-access",
        }
    }
}

impl std::fmt::Display for PermissionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ── TOOL_PERMISSIONS ───────────────────────────────────────────────

/// Global tool → minimum permission mapping for loaded Python tools.
fn tool_permissions() -> &'static HashMap<&'static str, PermissionMode> {
    static PERMISSIONS: OnceLock<HashMap<&str, PermissionMode>> = OnceLock::new();
    PERMISSIONS.get_or_init(|| {
        use PermissionMode::*;
        let mut m = HashMap::new();

        // Read-only tools (safe, no side effects)
        m.insert("search_materials", ReadOnly);
        m.insert("query_materials_project", ReadOnly);
        m.insert("literature_search", ReadOnly);
        m.insert("patent_search", ReadOnly);
        m.insert("web_search", ReadOnly);
        m.insert("web_read", ReadOnly);
        m.insert("read_file", ReadOnly);
        m.insert("show_scratchpad", ReadOnly);
        m.insert("list_models", ReadOnly);
        m.insert("list_predictable_properties", ReadOnly);
        m.insert("discover_capabilities", ReadOnly);
        m.insert("knowledge_search", ReadOnly);
        m.insert("knowledge_entity", ReadOnly);
        m.insert("knowledge_paths", ReadOnly);
        m.insert("knowledge_stats", ReadOnly);
        m.insert("semantic_search", ReadOnly);
        m.insert("list_corpora", ReadOnly);
        m.insert("list_lab_services", ReadOnly);
        m.insert("get_lab_service_info", ReadOnly);
        m.insert("check_lab_subscriptions", ReadOnly);
        m.insert("compute_gpus", ReadOnly);
        m.insert("compute_providers", ReadOnly);
        m.insert("compute_status", ReadOnly);
        m.insert("list_bash_tasks", ReadOnly);
        m.insert("read_bash_task", ReadOnly);
        m.insert("status", ReadOnly);
        m.insert("tools", ReadOnly);
        m.insert("query", ReadOnly);
        m.insert("query_local", ReadOnly);
        m.insert("query_platform", ReadOnly);
        m.insert("query_federated", ReadOnly);
        m.insert("job-status", ReadOnly);
        m.insert("job_status_lookup", ReadOnly);
        m.insert("workflow_list", ReadOnly);
        m.insert("workflow_show", ReadOnly);
        m.insert("marketplace_search", ReadOnly);
        m.insert("marketplace_info", ReadOnly);
        m.insert("node_probe", ReadOnly);
        m.insert("node_status", ReadOnly);
        m.insert("node_logs", ReadOnly);
        m.insert("mesh_discover", ReadOnly);
        m.insert("mesh_peers", ReadOnly);
        m.insert("mesh_subscriptions", ReadOnly);
        m.insert("models", ReadOnly);
        m.insert("models_list", ReadOnly);
        m.insert("models_search", ReadOnly);
        m.insert("models_info", ReadOnly);
        m.insert("deploy_list", ReadOnly);
        m.insert("deploy_status", ReadOnly);
        m.insert("deploy_health", ReadOnly);
        m.insert("discourse_list", ReadOnly);
        m.insert("discourse_show", ReadOnly);
        m.insert("discourse_status", ReadOnly);
        m.insert("discourse_turns", ReadOnly);

        // Workspace-write tools (create/modify files, run code)
        m.insert("write_file", WorkspaceWrite);
        m.insert("edit_file", WorkspaceWrite);
        m.insert("export_results_csv", WorkspaceWrite);
        m.insert("import_dataset", WorkspaceWrite);
        m.insert("execute_python", WorkspaceWrite);
        m.insert("predict_property", WorkspaceWrite);
        m.insert("predict_structure", WorkspaceWrite);
        m.insert("plot_materials_comparison", WorkspaceWrite);
        m.insert("plot_property_distribution", WorkspaceWrite);
        m.insert("plot_correlation_matrix", WorkspaceWrite);
        m.insert("knowledge_ingest", WorkspaceWrite);
        m.insert("compute_estimate", WorkspaceWrite);
        m.insert("workflow", WorkspaceWrite);
        m.insert("workflow_run", WorkspaceWrite);
        m.insert("marketplace", WorkspaceWrite);
        m.insert("marketplace_install", WorkspaceWrite);
        m.insert("ingest", WorkspaceWrite);
        m.insert("ingest_file", WorkspaceWrite);
        m.insert("ingest_watch", WorkspaceWrite);
        m.insert("discourse", WorkspaceWrite);
        m.insert("discourse_create", WorkspaceWrite);
        m.insert("discourse_run", WorkspaceWrite);

        // Full-access tools (destructive, costly, or system-level)
        m.insert("execute_bash", FullAccess);
        m.insert("stop_bash_task", FullAccess);
        m.insert("compute_submit", FullAccess);
        m.insert("compute_cancel", FullAccess);
        m.insert("submit_lab_job", FullAccess);
        m.insert("mesh", FullAccess);
        m.insert("mesh_publish", FullAccess);
        m.insert("mesh_subscribe", FullAccess);
        m.insert("mesh_unsubscribe", FullAccess);
        m.insert("node", FullAccess);
        m.insert("agent", FullAccess);
        m.insert("run", FullAccess);
        m.insert("research", FullAccess);
        m.insert("research_query", FullAccess);
        m.insert("deploy", FullAccess);
        m.insert("deploy_create", FullAccess);
        m.insert("deploy_stop", FullAccess);
        m.insert("publish", FullAccess);

        m
    })
}

/// Get the minimum permission level required for a tool.
/// Unknown tools default to `WorkspaceWrite` (safe middle ground).
#[must_use]
pub fn get_tool_permission(tool_name: &str) -> PermissionMode {
    tool_permissions()
        .get(tool_name)
        .copied()
        .unwrap_or(PermissionMode::WorkspaceWrite)
}

/// Session-scoped allow/deny edits layered over the baseline permission
/// context. These are persisted by the protocol layer and may also be updated
/// live while a turn is running.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PermissionOverrides {
    allow_names: BTreeSet<String>,
    deny_names: BTreeSet<String>,
}

pub type SharedPermissionOverrides = Arc<RwLock<PermissionOverrides>>;

impl PermissionOverrides {
    pub fn allow(&mut self, tool_name: &str) {
        let lowered = tool_name.to_ascii_lowercase();
        self.deny_names.remove(&lowered);
        self.allow_names.insert(lowered);
    }

    pub fn deny(&mut self, tool_name: &str) {
        let lowered = tool_name.to_ascii_lowercase();
        self.allow_names.remove(&lowered);
        self.deny_names.insert(lowered);
    }

    pub fn clear(&mut self, tool_name: &str) {
        let lowered = tool_name.to_ascii_lowercase();
        self.allow_names.remove(&lowered);
        self.deny_names.remove(&lowered);
    }

    pub fn reset(&mut self) {
        self.allow_names.clear();
        self.deny_names.clear();
    }

    pub fn allow_names(&self) -> impl Iterator<Item = &String> {
        self.allow_names.iter()
    }

    pub fn deny_names(&self) -> impl Iterator<Item = &String> {
        self.deny_names.iter()
    }

    #[must_use]
    pub fn is_allowed(&self, tool_name: &str) -> bool {
        self.allow_names.contains(&tool_name.to_ascii_lowercase())
    }

    #[must_use]
    pub fn is_denied(&self, tool_name: &str) -> bool {
        self.deny_names.contains(&tool_name.to_ascii_lowercase())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolPermissionDecision {
    pub blocked: bool,
    pub auto_approved: bool,
}

// ── ToolPermissionContext ──────────────────────────────────────────

/// Immutable permission context for tool execution.
///
/// Uses a deny-list model: everything is allowed unless explicitly blocked.
/// This is secure-by-default when combined with the `requires_approval` flag
/// on individual tools.
#[derive(Debug, Clone)]
pub struct ToolPermissionContext {
    deny_names: HashSet<String>,
    deny_prefixes: Vec<String>,
    auto_approve_names: HashSet<String>,
}

impl ToolPermissionContext {
    /// Check if a tool is blocked by this context.
    #[must_use]
    pub fn blocks(&self, tool_name: &str) -> bool {
        let lowered = tool_name.to_ascii_lowercase();
        if self.deny_names.contains(&lowered) {
            return true;
        }
        self.deny_prefixes.iter().any(|p| lowered.starts_with(p))
    }

    /// Check if a tool is auto-approved (skip user confirmation).
    #[must_use]
    pub fn auto_approves(&self, tool_name: &str) -> bool {
        if self.auto_approve_names.contains("*") {
            return true;
        }
        self.auto_approve_names
            .contains(&tool_name.to_ascii_lowercase())
    }

    /// Resolve the effective permission decision after layering any live
    /// session overrides on top of the frozen baseline context.
    #[must_use]
    pub fn decision_for(
        &self,
        tool_name: &str,
        overrides: Option<&PermissionOverrides>,
    ) -> ToolPermissionDecision {
        let override_denied = overrides
            .map(|value| value.is_denied(tool_name))
            .unwrap_or(false);
        let override_allowed = overrides
            .map(|value| value.is_allowed(tool_name))
            .unwrap_or(false);

        ToolPermissionDecision {
            blocked: self.blocks(tool_name) || override_denied,
            auto_approved: self.auto_approves(tool_name) || override_allowed,
        }
    }

    /// Return a new context with additional denials.
    #[must_use]
    pub fn with_deny(&self, names: &[String], prefixes: &[String]) -> Self {
        let mut new_names = self.deny_names.clone();
        for n in names {
            new_names.insert(n.to_ascii_lowercase());
        }
        let mut new_prefixes = self.deny_prefixes.clone();
        for p in prefixes {
            new_prefixes.push(p.to_ascii_lowercase());
        }
        Self {
            deny_names: new_names,
            deny_prefixes: new_prefixes,
            auto_approve_names: self.auto_approve_names.clone(),
        }
    }

    /// Return a new context with additional auto-approved tools.
    #[must_use]
    pub fn with_auto_approve(&self, names: &[String]) -> Self {
        let mut new_approved = self.auto_approve_names.clone();
        for n in names {
            new_approved.insert(n.to_ascii_lowercase());
        }
        Self {
            deny_names: self.deny_names.clone(),
            deny_prefixes: self.deny_prefixes.clone(),
            auto_approve_names: new_approved,
        }
    }

    /// Everything auto-approved, nothing blocked.
    #[must_use]
    pub fn accept_all() -> Self {
        Self {
            deny_names: HashSet::new(),
            deny_prefixes: Vec::new(),
            auto_approve_names: HashSet::from(["*".to_string()]),
        }
    }
}

impl Default for ToolPermissionContext {
    /// Default context — auto-approves safe read-only tools.
    fn default() -> Self {
        Self {
            deny_names: HashSet::new(),
            deny_prefixes: Vec::new(),
            auto_approve_names: HashSet::from([
                "search_materials".to_string(),
                "literature_search".to_string(),
                "web_search".to_string(),
                "web_read".to_string(),
                "knowledge_search".to_string(),
                "knowledge_entity".to_string(),
                "knowledge_stats".to_string(),
                "knowledge_paths".to_string(),
                "semantic_search".to_string(),
                "list_corpora".to_string(),
                "list_models".to_string(),
                "list_predictable_properties".to_string(),
                "discover_capabilities".to_string(),
                "show_scratchpad".to_string(),
                "read_file".to_string(),
                "list_lab_services".to_string(),
                "get_lab_service_info".to_string(),
                "check_lab_subscriptions".to_string(),
            ]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_mode_ordering() {
        assert!(PermissionMode::FullAccess > PermissionMode::WorkspaceWrite);
        assert!(PermissionMode::WorkspaceWrite > PermissionMode::ReadOnly);
        assert!(PermissionMode::FullAccess.allows(PermissionMode::ReadOnly));
        assert!(PermissionMode::WorkspaceWrite.allows(PermissionMode::ReadOnly));
        assert!(!PermissionMode::ReadOnly.allows(PermissionMode::WorkspaceWrite));
    }

    #[test]
    fn tool_permission_lookup() {
        assert_eq!(
            get_tool_permission("search_materials"),
            PermissionMode::ReadOnly
        );
        assert_eq!(
            get_tool_permission("write_file"),
            PermissionMode::WorkspaceWrite
        );
        assert_eq!(
            get_tool_permission("compute_submit"),
            PermissionMode::FullAccess
        );
        // Unknown tool defaults to WorkspaceWrite
        assert_eq!(
            get_tool_permission("unknown_tool"),
            PermissionMode::WorkspaceWrite
        );
    }

    #[test]
    fn all_known_tools_mapped() {
        // 54 read-only + 22 workspace-write + 18 full-access = 94
        let perms = tool_permissions();
        assert_eq!(perms.len(), 94);
    }

    #[test]
    fn context_blocks_by_name() {
        let ctx = ToolPermissionContext::default().with_deny(&["execute_python".to_string()], &[]);
        assert!(ctx.blocks("execute_python"));
        assert!(ctx.blocks("Execute_Python")); // case-insensitive
        assert!(!ctx.blocks("read_file"));
    }

    #[test]
    fn context_blocks_by_prefix() {
        let ctx = ToolPermissionContext::default().with_deny(&[], &["compute_".to_string()]);
        assert!(ctx.blocks("compute_submit"));
        assert!(ctx.blocks("compute_cancel"));
        assert!(!ctx.blocks("search_materials"));
    }

    #[test]
    fn context_auto_approve_default() {
        let ctx = ToolPermissionContext::default();
        assert!(ctx.auto_approves("search_materials"));
        assert!(ctx.auto_approves("read_file"));
        assert!(!ctx.auto_approves("execute_python"));
    }

    #[test]
    fn context_accept_all() {
        let ctx = ToolPermissionContext::accept_all();
        assert!(ctx.auto_approves("anything"));
        assert!(ctx.auto_approves("execute_python"));
        assert!(!ctx.blocks("anything"));
    }

    #[test]
    fn context_with_auto_approve() {
        let ctx =
            ToolPermissionContext::default().with_auto_approve(&["execute_python".to_string()]);
        assert!(ctx.auto_approves("execute_python"));
        assert!(ctx.auto_approves("search_materials")); // still approved
    }

    #[test]
    fn context_deny_overrides_auto_approve() {
        let ctx =
            ToolPermissionContext::default().with_deny(&["search_materials".to_string()], &[]);
        // blocked even though it's in auto_approve
        assert!(ctx.blocks("search_materials"));
    }

    #[test]
    fn permission_mode_display() {
        assert_eq!(PermissionMode::ReadOnly.as_str(), "read-only");
        assert_eq!(PermissionMode::WorkspaceWrite.as_str(), "workspace-write");
        assert_eq!(PermissionMode::FullAccess.as_str(), "full-access");
    }

    #[test]
    fn live_allow_override_auto_approves_tool() {
        let ctx = ToolPermissionContext::default();
        let mut overrides = PermissionOverrides::default();
        overrides.allow("execute_bash");

        let decision = ctx.decision_for("execute_bash", Some(&overrides));
        assert!(decision.auto_approved);
        assert!(!decision.blocked);
    }

    #[test]
    fn live_deny_override_blocks_tool_even_if_auto_approved() {
        let ctx = ToolPermissionContext::default();
        let mut overrides = PermissionOverrides::default();
        overrides.deny("read_file");

        let decision = ctx.decision_for("read_file", Some(&overrides));
        assert!(decision.blocked);
    }
}
