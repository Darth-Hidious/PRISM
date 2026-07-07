# PRISM Default Policy — OPA/Rego
#
# Governs what agents, users, and workflows can do on a PRISM node.
# Override by placing .rego files in ~/.prism/policies/ or .prism/policies/
#
# Input schema:
#   input.action    - "workflow.execute", "tool.call", "agent.action", "data.query"
#   input.principal - user ID or "agent"
#   input.role      - "admin", "operator", "viewer", "agent"
#   input.resource  - target name (workflow, tool, etc.)
#   input.context   - additional JSON context

package prism.policy

import rego.v1

# =========================================================================
# Role hierarchy
# =========================================================================

role_level := {"admin": 100, "operator": 50, "agent": 30, "viewer": 10}

principal_level := role_level[input.role]

# =========================================================================
# ALLOW rules — any matching rule grants access
# =========================================================================

# Admins can do anything
allow if {
    input.role == "admin"
}

# Operators can execute workflows and call tools
allow if {
    input.role == "operator"
    input.action in {"workflow.execute", "tool.call", "data.query"}
}

# Agents can call read-only tools
allow if {
    input.role == "agent"
    input.action == "tool.call"
    not tool_is_destructive
}

# Research ingestion — owner decision 2026-07-07 ("should be allowed"): the
# research agent may ingest into the knowledge graph without admin approval.
# Scoped to knowledge_ingest only; every other destructive tool stays gated.
allow if {
    input.role in {"agent", "operator"}
    input.action == "tool.call"
    research_ingest_exempt
}

research_ingest_exempt if {
    input.resource == "knowledge_ingest"
    input.role in {"agent", "operator"}
    # An ingest call is a write by nature; a delete-mode call is NOT research
    # ingestion and stays admin-gated.
    not delete_mode
}

delete_mode if {
    input.context.mode == "delete"
}

# Agents can execute approved workflows
allow if {
    input.role == "agent"
    input.action == "workflow.execute"
    input.resource in agent_approved_workflows
}

# Agents can query data
allow if {
    input.role == "agent"
    input.action == "data.query"
}

# Anyone can do read-only queries
allow if {
    input.action == "data.query"
    not query_is_write
}

# =========================================================================
# DENY rules — any matching rule blocks access (collected as violations)
# =========================================================================

deny contains msg if {
    input.role == "viewer"
    input.action in {"workflow.execute", "tool.call"}
    msg := "viewers cannot execute workflows or call tools"
}

deny contains msg if {
    input.action == "tool.call"
    tool_is_destructive
    input.role != "admin"
    not research_ingest_exempt
    msg := sprintf("destructive tool '%s' requires admin role", [input.resource])
}

deny contains msg if {
    input.action == "data.query"
    query_is_write
    input.role != "admin"
    msg := "write queries require admin role"
}

# =========================================================================
# OBLIGATIONS — things the caller must do if allowed
# =========================================================================

obligations contains "audit_log" if {
    input.action == "workflow.execute"
}

obligations contains "audit_log" if {
    input.action == "tool.call"
    input.role == "agent"
}

obligations contains "notify_admin" if {
    input.action == "workflow.execute"
    input.role == "agent"
}

# =========================================================================
# REASON — human-readable explanation
# =========================================================================

reason := sprintf("%s allowed: %s has %s role", [input.action, input.principal, input.role]) if {
    allow
}

reason := sprintf("%s denied: %s", [input.action, concat("; ", deny)]) if {
    not allow
}

# =========================================================================
# Helper rules
# =========================================================================

# Tools that modify state, spend money, or change deployed systems — agent
# needs explicit approval (admin role). Mirrors the FullAccess / ingest-write
# classification in the agent's tool permission map (crates/agent permissions).
destructive_tools := {
    "knowledge_ingest",
    "data_delete",
    "node_restart",
    "config_update",
    "user_manage",
    # Compute jobs — submit/cancel real (billable) compute work.
    "compute_submit",
    "compute_cancel",
    "submit_lab_job",
    # Deployments — create/stop hosted model deployments.
    "deploy",
    "deploy_create",
    "deploy_stop",
    # Ingest-class writes — siblings of knowledge_ingest.
    "ingest",
    "ingest_file",
    "ingest_watch",
}

tool_is_destructive if {
    input.resource in destructive_tools
}

tool_is_destructive if {
    input.context.mode == "delete"
}

tool_is_destructive if {
    input.context.mode == "write"
}

# Workflows the agent can run without human approval
agent_approved_workflows := {
    "train-indexer",
    "forge",
    "search",
    "predict",
    "data-export",
}

# Detect write operations in queries
query_is_write if {
    contains(upper(input.resource), "DELETE")
}

query_is_write if {
    contains(upper(input.resource), "CREATE")
}

query_is_write if {
    contains(upper(input.resource), "MERGE")
}

query_is_write if {
    contains(upper(input.resource), "DROP")
}
