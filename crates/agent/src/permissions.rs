//! Immutable tool permission context with 3-tier permission model.
//!
//! A frozen context that captures the permission state for a session.
//! Thread-safe, clonable, can be passed around without mutation concerns.
//!
//! ```rust
//! use prism_agent::permissions::{ToolPermissionContext, PermissionMode, get_tool_permission};
//!
//! let ctx = ToolPermissionContext::default();
//! assert!(!ctx.blocks("materials_search"));
//! assert!(ctx.auto_approves("materials_search"));
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

/// Every tool PRISM ships: the Python tool server's registry plus the Rust
/// command tools, captured on 2026-09-06. The test below fails if the permission map drifts
/// from this list in either direction — a tool with no class, or a class for a
/// tool that no longer exists.
pub const SHIPPED_TOOLS: &[&str] = &[
    "acquire_materials",
    "agent",
    "agent_capabilities",
    "analyze_phases",
    "bash_task",
    "billing",
    "billing_balance",
    "billing_history",
    "billing_prices",
    "billing_read",
    "billing_usage",
    "calphad",
    "calphad_compute",
    "cancel_background_research",
    "check_background_research",
    "check_hpc_queue",
    "cold_start_active_learning",
    "cold_start_calphad_augmentation",
    "cold_start_campaign_handoff",
    "cold_start_foundation_bootstrap",
    "compare_materials",
    "compute_cancel",
    "compute_descriptor",
    "compute_estimate",
    "compute_gpus",
    "compute_providers",
    "compute_read",
    "compute_status",
    "compute_submit",
    "dataset",
    "deploy",
    "deploy_create",
    "deploy_health",
    "deploy_list",
    "deploy_read",
    "deploy_status",
    "deploy_stop",
    "deploy_write",
    "describe_structure",
    "discourse",
    "discourse_create",
    "discourse_list",
    "discourse_read",
    "discourse_run",
    "discourse_show",
    "discourse_status",
    "discourse_turns",
    "discourse_write",
    "doctor",
    "doctor_fix",
    "evaluate_candidate",
    "evaluation_tier_status",
    "example",
    "execute_bash",
    "execute_python",
    "fetch_artifact",
    "file",
    "generate_report",
    "goal_list",
    "goal_resume",
    "goal_start",
    "goal_status",
    "hea_descriptors",
    "hea_phase_stability",
    "ingest",
    "ingest_and_wait",
    "ingest_file",
    "ingest_watch",
    "job_status_lookup",
    "knowledge_corpora",
    "knowledge_entity",
    "knowledge_ingest",
    "knowledge_paths",
    "knowledge_write",
    "labs",
    "list_artifacts",
    "list_background_research",
    "list_models",
    "list_potentials",
    "list_predictable_properties",
    "lookup_structure",
    "lpbf_kou_cracking_index",
    "lpbf_printability_map",
    "mace_cancel_job",
    "mace_compute_dilute_solute",
    "mace_compute_elastic",
    "mace_estimate_cost",
    "mace_get_cached_structure",
    "mace_get_job",
    "mace_list_jobs",
    "mace_md_equilibrate",
    "mace_phonon_harmonic",
    "mace_relax_structure",
    "marketplace",
    "marketplace_find",
    "marketplace_info",
    "marketplace_install",
    "marketplace_read",
    "marketplace_search",
    "marketplace_write",
    "materials_discovery",
    "materials_search",
    "mcp_services",
    "mcp_services_invoke",
    "mesh",
    "mesh_discover",
    "mesh_health",
    "mesh_peers",
    "mesh_publish",
    "mesh_read",
    "mesh_subscribe",
    "mesh_subscriptions",
    "mesh_sync",
    "mesh_unsubscribe",
    "mesh_write",
    "model_train",
    "models",
    "models_info",
    "models_list",
    "models_read",
    "models_search",
    "node",
    "node_logs",
    "node_probe",
    "node_read",
    "node_status",
    "notebook_exec",
    "notebook_reset",
    "notebook_status",
    "ontology_proposals",
    "ontology_proposals_accept",
    "ontology_proposals_reject",
    "ontology_proposals_show",
    "ontology_read",
    "ontology_write",
    "papers",
    "papers_fulltext",
    "papers_ingest",
    "pareto_screen",
    "phase_stability",
    "plan_simulations",
    "platform_jobs",
    "platform_jobs_submit",
    "platform_workflows",
    "platform_workflows_run",
    "plot",
    "plugins",
    "policy_evaluate",
    "predict",
    "predict_properties",
    "predict_property",
    "predict_synthesizability",
    "prior_art_search",
    "provision",
    "publish",
    "publish_artifact",
    "qe_parse_output",
    "qe_resolve_pseudopotentials",
    "qe_run",
    "qe_status",
    "qe_write_input",
    "query",
    "query_materials_project",
    "report_bug",
    "research",
    "research_query",
    "reverify_assertion",
    "reverify_candidates",
    "reverify_history",
    "run",
    "run_convergence_test",
    "run_model",
    "run_submit",
    "run_workflow",
    "schedule_cancel",
    "schedule_create",
    "schedule_list",
    "scheil_solidification",
    "screen_materials",
    "search_artifacts",
    "select_materials",
    "session_context",
    "show_scratchpad",
    "sim_job",
    "sim_run",
    "start_background_research",
    "status",
    "stop_bash_task",
    "structure",
    "structure_import",
    "structure_similarity",
    "suggest_next_experiments",
    "symbolic_check",
    "tool_reasoning",
    "tools",
    "usage_status",
    "web",
    "web_browse",
    "wf",
    "workflow",
    "workflow_list",
    "workflow_run",
    "workflow_show",
];

// ── TOOL_PERMISSIONS ───────────────────────────────────────────────

/// Global tool → minimum permission mapping for loaded Python tools.
fn tool_permissions() -> &'static HashMap<&'static str, PermissionMode> {
    static PERMISSIONS: OnceLock<HashMap<&str, PermissionMode>> = OnceLock::new();
    PERMISSIONS.get_or_init(|| {
        use PermissionMode::*;
        let mut m = HashMap::new();

        // Read-only: no side effect outside the process.
        m.insert("agent_capabilities", ReadOnly);
        m.insert("analyze_phases", ReadOnly);
        m.insert("billing", ReadOnly);
        m.insert("billing_balance", ReadOnly);
        m.insert("billing_history", ReadOnly);
        m.insert("billing_prices", ReadOnly);
        m.insert("billing_read", ReadOnly);
        m.insert("billing_usage", ReadOnly);
        m.insert("calphad", ReadOnly);
        m.insert("check_background_research", ReadOnly);
        m.insert("check_hpc_queue", ReadOnly);
        m.insert("compare_materials", ReadOnly);
        m.insert("compute_descriptor", ReadOnly);
        m.insert("compute_estimate", ReadOnly);
        m.insert("compute_gpus", ReadOnly);
        m.insert("compute_providers", ReadOnly);
        m.insert("compute_read", ReadOnly);
        m.insert("compute_status", ReadOnly);
        m.insert("deploy_health", ReadOnly);
        m.insert("deploy_list", ReadOnly);
        m.insert("deploy_read", ReadOnly);
        m.insert("deploy_status", ReadOnly);
        m.insert("describe_structure", ReadOnly);
        m.insert("discourse", ReadOnly);
        m.insert("discourse_list", ReadOnly);
        m.insert("discourse_read", ReadOnly);
        m.insert("discourse_show", ReadOnly);
        m.insert("discourse_status", ReadOnly);
        m.insert("discourse_turns", ReadOnly);
        m.insert("doctor", ReadOnly);
        m.insert("evaluate_candidate", ReadOnly);
        m.insert("evaluation_tier_status", ReadOnly);
        m.insert("fetch_artifact", ReadOnly);
        m.insert("goal_list", ReadOnly);
        m.insert("goal_status", ReadOnly);
        m.insert("hea_descriptors", ReadOnly);
        m.insert("hea_phase_stability", ReadOnly);
        m.insert("job_status_lookup", ReadOnly);
        m.insert("knowledge_corpora", ReadOnly);
        m.insert("knowledge_entity", ReadOnly);
        m.insert("knowledge_paths", ReadOnly);
        m.insert("list_artifacts", ReadOnly);
        m.insert("list_background_research", ReadOnly);
        m.insert("list_models", ReadOnly);
        m.insert("list_potentials", ReadOnly);
        m.insert("list_predictable_properties", ReadOnly);
        m.insert("lookup_structure", ReadOnly);
        m.insert("lpbf_kou_cracking_index", ReadOnly);
        m.insert("lpbf_printability_map", ReadOnly);
        m.insert("mace_estimate_cost", ReadOnly);
        m.insert("mace_get_cached_structure", ReadOnly);
        m.insert("mace_get_job", ReadOnly);
        m.insert("mace_list_jobs", ReadOnly);
        m.insert("marketplace", ReadOnly);
        m.insert("marketplace_find", ReadOnly);
        m.insert("marketplace_info", ReadOnly);
        m.insert("marketplace_read", ReadOnly);
        m.insert("marketplace_search", ReadOnly);
        m.insert("materials_search", ReadOnly);
        m.insert("mcp_services", ReadOnly);
        m.insert("mesh", ReadOnly);
        m.insert("mesh_discover", ReadOnly);
        m.insert("mesh_health", ReadOnly);
        m.insert("mesh_peers", ReadOnly);
        m.insert("mesh_read", ReadOnly);
        m.insert("mesh_subscriptions", ReadOnly);
        m.insert("models", ReadOnly);
        m.insert("models_info", ReadOnly);
        m.insert("models_list", ReadOnly);
        m.insert("models_read", ReadOnly);
        m.insert("models_search", ReadOnly);
        m.insert("node", ReadOnly);
        m.insert("node_logs", ReadOnly);
        m.insert("node_probe", ReadOnly);
        m.insert("node_read", ReadOnly);
        m.insert("node_status", ReadOnly);
        m.insert("notebook_status", ReadOnly);
        m.insert("ontology_proposals", ReadOnly);
        m.insert("ontology_proposals_show", ReadOnly);
        m.insert("ontology_read", ReadOnly);
        m.insert("papers", ReadOnly);
        m.insert("papers_fulltext", ReadOnly);
        m.insert("pareto_screen", ReadOnly);
        m.insert("phase_stability", ReadOnly);
        m.insert("plan_simulations", ReadOnly);
        m.insert("platform_jobs", ReadOnly);
        m.insert("platform_workflows", ReadOnly);
        m.insert("policy_evaluate", ReadOnly);
        m.insert("predict", ReadOnly);
        m.insert("predict_properties", ReadOnly);
        m.insert("predict_property", ReadOnly);
        m.insert("predict_synthesizability", ReadOnly);
        m.insert("prior_art_search", ReadOnly);
        m.insert("qe_parse_output", ReadOnly);
        m.insert("qe_resolve_pseudopotentials", ReadOnly);
        m.insert("qe_status", ReadOnly);
        m.insert("query", ReadOnly);
        m.insert("query_materials_project", ReadOnly);
        m.insert("reverify_history", ReadOnly);
        m.insert("schedule_list", ReadOnly);
        m.insert("scheil_solidification", ReadOnly);
        m.insert("screen_materials", ReadOnly);
        m.insert("search_artifacts", ReadOnly);
        m.insert("select_materials", ReadOnly);
        m.insert("session_context", ReadOnly);
        m.insert("show_scratchpad", ReadOnly);
        m.insert("status", ReadOnly);
        m.insert("structure", ReadOnly);
        m.insert("structure_similarity", ReadOnly);
        m.insert("symbolic_check", ReadOnly);
        m.insert("tool_reasoning", ReadOnly);
        m.insert("tools", ReadOnly);
        m.insert("usage_status", ReadOnly);
        m.insert("web", ReadOnly);
        m.insert("web_browse", ReadOnly);
        m.insert("workflow_list", ReadOnly);
        m.insert("workflow_show", ReadOnly);

        // Workspace write: local files and local compute. Reversible, free.
        m.insert("calphad_compute", WorkspaceWrite);
        m.insert("cancel_background_research", WorkspaceWrite);
        m.insert("cold_start_active_learning", WorkspaceWrite);
        m.insert("cold_start_calphad_augmentation", WorkspaceWrite);
        m.insert("cold_start_campaign_handoff", WorkspaceWrite);
        m.insert("cold_start_foundation_bootstrap", WorkspaceWrite);
        m.insert("dataset", WorkspaceWrite);
        m.insert("example", WorkspaceWrite);
        m.insert("file", WorkspaceWrite);
        m.insert("generate_report", WorkspaceWrite);
        m.insert("mace_cancel_job", WorkspaceWrite);
        m.insert("mace_compute_dilute_solute", WorkspaceWrite);
        m.insert("mace_compute_elastic", WorkspaceWrite);
        m.insert("mace_md_equilibrate", WorkspaceWrite);
        m.insert("mace_phonon_harmonic", WorkspaceWrite);
        m.insert("mace_relax_structure", WorkspaceWrite);
        m.insert("materials_discovery", WorkspaceWrite);
        m.insert("notebook_reset", WorkspaceWrite);
        m.insert("plot", WorkspaceWrite);
        m.insert("qe_run", WorkspaceWrite);
        m.insert("qe_write_input", WorkspaceWrite);
        m.insert("report_bug", WorkspaceWrite);
        m.insert("research_query", WorkspaceWrite);
        m.insert("run_convergence_test", WorkspaceWrite);
        m.insert("run_workflow", WorkspaceWrite);
        m.insert("sim_job", WorkspaceWrite);
        m.insert("sim_run", WorkspaceWrite);
        m.insert("start_background_research", WorkspaceWrite);
        m.insert("structure_import", WorkspaceWrite);
        m.insert("suggest_next_experiments", WorkspaceWrite);

        // Full access: runs code, spends money or a paid quota, or mutates
        // state other people can see. Never auto-approved unattended.
        m.insert("acquire_materials", FullAccess);
        m.insert("agent", FullAccess);
        m.insert("bash_task", FullAccess);
        m.insert("compute_cancel", FullAccess);
        m.insert("compute_submit", FullAccess);
        m.insert("deploy", FullAccess);
        m.insert("deploy_create", FullAccess);
        m.insert("deploy_stop", FullAccess);
        m.insert("deploy_write", FullAccess);
        m.insert("discourse_create", FullAccess);
        m.insert("discourse_run", FullAccess);
        m.insert("discourse_write", FullAccess);
        m.insert("doctor_fix", FullAccess);
        m.insert("execute_bash", FullAccess);
        m.insert("execute_python", FullAccess);
        m.insert("goal_resume", FullAccess);
        m.insert("goal_start", FullAccess);
        m.insert("ingest", FullAccess);
        m.insert("ingest_and_wait", FullAccess);
        m.insert("ingest_file", FullAccess);
        m.insert("ingest_watch", FullAccess);
        m.insert("knowledge_ingest", FullAccess);
        m.insert("knowledge_write", FullAccess);
        m.insert("labs", FullAccess);
        m.insert("marketplace_install", FullAccess);
        m.insert("marketplace_write", FullAccess);
        m.insert("mcp_services_invoke", FullAccess);
        m.insert("mesh_publish", FullAccess);
        m.insert("mesh_subscribe", FullAccess);
        m.insert("mesh_sync", FullAccess);
        m.insert("mesh_unsubscribe", FullAccess);
        m.insert("mesh_write", FullAccess);
        m.insert("model_train", FullAccess);
        m.insert("notebook_exec", FullAccess);
        m.insert("ontology_proposals_accept", FullAccess);
        m.insert("ontology_proposals_reject", FullAccess);
        m.insert("ontology_write", FullAccess);
        m.insert("papers_ingest", FullAccess);
        m.insert("platform_jobs_submit", FullAccess);
        m.insert("platform_workflows_run", FullAccess);
        m.insert("plugins", FullAccess);
        m.insert("provision", FullAccess);
        m.insert("publish", FullAccess);
        m.insert("publish_artifact", FullAccess);
        m.insert("research", FullAccess);
        m.insert("reverify_assertion", FullAccess);
        m.insert("reverify_candidates", FullAccess);
        m.insert("run", FullAccess);
        m.insert("run_model", FullAccess);
        m.insert("run_submit", FullAccess);
        m.insert("schedule_cancel", FullAccess);
        m.insert("schedule_create", FullAccess);
        m.insert("stop_bash_task", FullAccess);
        m.insert("wf", FullAccess);
        m.insert("workflow", FullAccess);
        m.insert("workflow_run", FullAccess);

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

    /// Session-wide "approve everything from here on" — the user's `Allow All`
    /// / `allow-session` choice. Inserts the `"*"` wildcard, mirroring the same
    /// convention in `ToolPermissionContext::auto_approves`. Does NOT clear
    /// explicit denials: a blocked tool stays blocked.
    pub fn allow_all(&mut self) {
        self.allow_names.insert("*".to_string());
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
        self.allow_names.contains("*") || self.allow_names.contains(&tool_name.to_ascii_lowercase())
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
                "materials_search".to_string(),
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
                "list_lab_services".to_string(),
                "get_lab_service_info".to_string(),
                "check_lab_subscriptions".to_string(),
            ]),
        }
    }
}

#[cfg(test)]
mod tests {

    /// 2026-09-06: the map held 91 entries, 80 of which named tools that no
    /// longer exist, while `acquire_materials` (orders material — spends
    /// money) and `platform_jobs_submit` (submits a paid job) were unmapped
    /// and fell through to the WorkspaceWrite default. A default is the wrong
    /// answer for a tool nobody classified: the map must name every tool the
    /// product actually ships, and money or shared state must never be a
    /// silent default.
    #[test]
    fn every_shipped_tool_is_classified_and_none_are_stale() {
        let map = tool_permissions();
        let shipped: Vec<&str> = SHIPPED_TOOLS.to_vec();

        let unmapped: Vec<&&str> = shipped.iter().filter(|t| !map.contains_key(**t)).collect();
        assert!(
            unmapped.is_empty(),
            "{} shipped tools have no permission class: {unmapped:?}",
            unmapped.len()
        );

        let stale: Vec<&&str> = map.keys().filter(|k| !shipped.contains(k)).collect();
        assert!(
            stale.is_empty(),
            "{} entries name tools that do not exist: {stale:?}",
            stale.len()
        );
    }

    /// The tools that spend money or mutate state other people can see must
    /// require the highest class, so no unattended mode can run them quietly.
    #[test]
    fn spending_and_shared_state_are_never_below_full_access() {
        let map = tool_permissions();
        for tool in [
            "acquire_materials",
            "platform_jobs_submit",
            "platform_workflows_run",
            "knowledge_write",
            "mcp_services_invoke",
            "execute_bash",
            "execute_python",
            "compute_submit",
            "deploy_create",
            "publish",
            "marketplace_install",
            "schedule_create",
            "notebook_exec",
        ] {
            assert_eq!(
                map.get(tool).copied(),
                Some(PermissionMode::FullAccess),
                "{tool} spends money, runs code or mutates shared state — it must be FullAccess"
            );
        }
    }
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
            get_tool_permission("materials_search"),
            PermissionMode::ReadOnly
        );
        assert_eq!(get_tool_permission("file"), PermissionMode::WorkspaceWrite);
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
        // Was a literal 92 — a count that had already drifted from the truth
        // (91 entries, 80 of them naming tools that no longer existed). Tied
        // to the shipped list instead, so it cannot go stale silently.
        let perms = tool_permissions();
        assert_eq!(
            perms.len(),
            SHIPPED_TOOLS.len(),
            "one class per shipped tool, no more and no fewer"
        );
    }

    #[test]
    fn allow_all_auto_approves_every_tool() {
        // "Allow All" / allow-session: after allow_all(), decision_for() must
        // report auto_approved for an arbitrary tool the user never named —
        // this is what makes the AllowAll approval actually stick instead of
        // silently degrading to "Allow Once".
        let ctx = ToolPermissionContext::default();
        let mut overrides = PermissionOverrides::default();
        assert!(
            !ctx.decision_for("execute_bash", Some(&overrides))
                .auto_approved
        );
        overrides.allow_all();
        assert!(overrides.is_allowed("execute_bash"));
        assert!(overrides.is_allowed("some_tool_never_seen"));
        assert!(
            ctx.decision_for("execute_bash", Some(&overrides))
                .auto_approved
        );
    }

    #[test]
    fn allow_all_does_not_unblock_denied_tools() {
        // Auto-approve must never override an explicit block: allow_all() means
        // "stop prompting me", not "run things I forbade".
        let ctx = ToolPermissionContext::default().with_deny(&["execute_python".to_string()], &[]);
        let mut overrides = PermissionOverrides::default();
        overrides.allow_all();
        let decision = ctx.decision_for("execute_python", Some(&overrides));
        assert!(decision.blocked);
    }

    #[test]
    fn context_blocks_by_name() {
        let ctx = ToolPermissionContext::default().with_deny(&["execute_python".to_string()], &[]);
        assert!(ctx.blocks("execute_python"));
        assert!(ctx.blocks("Execute_Python")); // case-insensitive
        assert!(!ctx.blocks("file"));
    }

    #[test]
    fn context_blocks_by_prefix() {
        let ctx = ToolPermissionContext::default().with_deny(&[], &["compute_".to_string()]);
        assert!(ctx.blocks("compute_submit"));
        assert!(ctx.blocks("compute_cancel"));
        assert!(!ctx.blocks("materials_search"));
    }

    #[test]
    fn context_auto_approve_default() {
        let ctx = ToolPermissionContext::default();
        assert!(ctx.auto_approves("materials_search"));
        assert!(!ctx.auto_approves("execute_python"));
        // `read_file` used to sit in this list and was dropped, not renamed:
        // the tool that replaced it also WRITES and EDITS, so auto-approving
        // it by that name would have handed the model silent overwrite.
        assert!(!ctx.auto_approves("file"));
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
        assert!(ctx.auto_approves("materials_search")); // still approved
    }

    #[test]
    fn context_deny_overrides_auto_approve() {
        let ctx =
            ToolPermissionContext::default().with_deny(&["materials_search".to_string()], &[]);
        // blocked even though it's in auto_approve
        assert!(ctx.blocks("materials_search"));
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
        overrides.deny("materials_search");

        let decision = ctx.decision_for("materials_search", Some(&overrides));
        assert!(decision.blocked);
    }
}
