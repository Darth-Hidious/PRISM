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

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

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

/// Global tool → minimum permission mapping. All 47 tools from the Python source.
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

        // Workspace-write tools (create/modify files, run code)
        m.insert("write_file", WorkspaceWrite);
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

        // Full-access tools (destructive, costly, or system-level)
        m.insert("compute_submit", FullAccess);
        m.insert("compute_cancel", FullAccess);
        m.insert("submit_lab_job", FullAccess);

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
        assert_eq!(get_tool_permission("search_materials"), PermissionMode::ReadOnly);
        assert_eq!(get_tool_permission("write_file"), PermissionMode::WorkspaceWrite);
        assert_eq!(get_tool_permission("compute_submit"), PermissionMode::FullAccess);
        // Unknown tool defaults to WorkspaceWrite
        assert_eq!(get_tool_permission("unknown_tool"), PermissionMode::WorkspaceWrite);
    }

    #[test]
    fn all_37_tools_mapped() {
        // 23 read-only + 11 workspace-write + 3 full-access = 37
        let perms = tool_permissions();
        assert_eq!(perms.len(), 37);
    }

    #[test]
    fn context_blocks_by_name() {
        let ctx = ToolPermissionContext::default()
            .with_deny(&["execute_python".to_string()], &[]);
        assert!(ctx.blocks("execute_python"));
        assert!(ctx.blocks("Execute_Python")); // case-insensitive
        assert!(!ctx.blocks("read_file"));
    }

    #[test]
    fn context_blocks_by_prefix() {
        let ctx = ToolPermissionContext::default()
            .with_deny(&[], &["compute_".to_string()]);
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
        let ctx = ToolPermissionContext::default()
            .with_auto_approve(&["execute_python".to_string()]);
        assert!(ctx.auto_approves("execute_python"));
        assert!(ctx.auto_approves("search_materials")); // still approved
    }

    #[test]
    fn context_deny_overrides_auto_approve() {
        let ctx = ToolPermissionContext::default()
            .with_deny(&["search_materials".to_string()], &[]);
        // blocked even though it's in auto_approve
        assert!(ctx.blocks("search_materials"));
    }

    #[test]
    fn permission_mode_display() {
        assert_eq!(PermissionMode::ReadOnly.as_str(), "read-only");
        assert_eq!(PermissionMode::WorkspaceWrite.as_str(), "workspace-write");
        assert_eq!(PermissionMode::FullAccess.as_str(), "full-access");
    }
}
