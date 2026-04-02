"""Immutable tool permission context with 3-tier permission model.

A frozen dataclass that captures the permission state for a session.
Thread-safe, hashable, can be passed around without mutation concerns.

Usage:
    ctx = ToolPermissionContext.default()
    if ctx.blocks("execute_python"):
        ... # tool is blocked

    # Create a more restrictive context
    ctx = ctx.with_deny(names={"execute_python"}, prefixes=("compute_",))
"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import FrozenSet, Tuple


class PermissionMode(Enum):
    """Three-tier permission model for tool access."""
    READ_ONLY = "read-only"           # Search, read, query — no mutations
    WORKSPACE_WRITE = "workspace-write"  # File edits, exports, code execution
    FULL_ACCESS = "full-access"        # Everything including destructive ops

    def allows(self, required: 'PermissionMode') -> bool:
        """Check if this mode allows a tool requiring the given level."""
        order = {
            PermissionMode.READ_ONLY: 0,
            PermissionMode.WORKSPACE_WRITE: 1,
            PermissionMode.FULL_ACCESS: 2,
        }
        return order[self] >= order[required]


# Tool → minimum permission level required
TOOL_PERMISSIONS: dict[str, PermissionMode] = {
    # Read-only tools (safe, no side effects)
    "search_materials": PermissionMode.READ_ONLY,
    "query_materials_project": PermissionMode.READ_ONLY,
    "literature_search": PermissionMode.READ_ONLY,
    "patent_search": PermissionMode.READ_ONLY,
    "web_search": PermissionMode.READ_ONLY,
    "web_read": PermissionMode.READ_ONLY,
    "read_file": PermissionMode.READ_ONLY,
    "show_scratchpad": PermissionMode.READ_ONLY,
    "list_models": PermissionMode.READ_ONLY,
    "list_predictable_properties": PermissionMode.READ_ONLY,
    "discover_capabilities": PermissionMode.READ_ONLY,
    "knowledge_search": PermissionMode.READ_ONLY,
    "knowledge_entity": PermissionMode.READ_ONLY,
    "knowledge_paths": PermissionMode.READ_ONLY,
    "knowledge_stats": PermissionMode.READ_ONLY,
    "semantic_search": PermissionMode.READ_ONLY,
    "list_corpora": PermissionMode.READ_ONLY,
    "list_lab_services": PermissionMode.READ_ONLY,
    "get_lab_service_info": PermissionMode.READ_ONLY,
    "check_lab_subscriptions": PermissionMode.READ_ONLY,
    "compute_gpus": PermissionMode.READ_ONLY,
    "compute_providers": PermissionMode.READ_ONLY,
    "compute_status": PermissionMode.READ_ONLY,

    # Workspace-write tools (create/modify files, run code)
    "write_file": PermissionMode.WORKSPACE_WRITE,
    "export_results_csv": PermissionMode.WORKSPACE_WRITE,
    "import_dataset": PermissionMode.WORKSPACE_WRITE,
    "execute_python": PermissionMode.WORKSPACE_WRITE,
    "predict_property": PermissionMode.WORKSPACE_WRITE,
    "predict_structure": PermissionMode.WORKSPACE_WRITE,
    "plot_materials_comparison": PermissionMode.WORKSPACE_WRITE,
    "plot_property_distribution": PermissionMode.WORKSPACE_WRITE,
    "plot_correlation_matrix": PermissionMode.WORKSPACE_WRITE,
    "knowledge_ingest": PermissionMode.WORKSPACE_WRITE,
    "compute_estimate": PermissionMode.WORKSPACE_WRITE,

    # Full-access tools (destructive, costly, or system-level)
    "compute_submit": PermissionMode.FULL_ACCESS,
    "compute_cancel": PermissionMode.FULL_ACCESS,
    "submit_lab_job": PermissionMode.FULL_ACCESS,
}


def get_tool_permission(tool_name: str) -> PermissionMode:
    """Get the minimum permission level required for a tool.
    Unknown tools default to WORKSPACE_WRITE (safe middle ground).
    """
    return TOOL_PERMISSIONS.get(tool_name, PermissionMode.WORKSPACE_WRITE)


@dataclass(frozen=True)
class ToolPermissionContext:
    """Immutable permission context for tool execution.

    Uses a deny-list model: everything is allowed unless explicitly blocked.
    This is secure-by-default when combined with the requires_approval flag
    on individual tools.
    """

    deny_names: FrozenSet[str] = field(default_factory=frozenset)
    deny_prefixes: Tuple[str, ...] = ()
    auto_approve_names: FrozenSet[str] = field(default_factory=frozenset)

    def blocks(self, tool_name: str) -> bool:
        """Check if a tool is blocked by this context."""
        lowered = tool_name.lower()
        if lowered in self.deny_names:
            return True
        return any(lowered.startswith(p) for p in self.deny_prefixes)

    def auto_approves(self, tool_name: str) -> bool:
        """Check if a tool is auto-approved (skip user confirmation)."""
        return tool_name.lower() in self.auto_approve_names

    def with_deny(
        self,
        names: set[str] | None = None,
        prefixes: tuple[str, ...] | None = None,
    ) -> ToolPermissionContext:
        """Return a new context with additional denials."""
        new_names = self.deny_names | frozenset(n.lower() for n in (names or set()))
        new_prefixes = self.deny_prefixes + tuple(p.lower() for p in (prefixes or ()))
        return ToolPermissionContext(
            deny_names=new_names,
            deny_prefixes=new_prefixes,
            auto_approve_names=self.auto_approve_names,
        )

    def with_auto_approve(self, names: set[str]) -> ToolPermissionContext:
        """Return a new context with additional auto-approved tools."""
        new_approved = self.auto_approve_names | frozenset(n.lower() for n in names)
        return ToolPermissionContext(
            deny_names=self.deny_names,
            deny_prefixes=self.deny_prefixes,
            auto_approve_names=new_approved,
        )

    @staticmethod
    def default() -> ToolPermissionContext:
        """Default context — blocks known destructive tools."""
        return ToolPermissionContext(
            deny_names=frozenset(),
            deny_prefixes=(),
            auto_approve_names=frozenset({
                "search_materials",
                "literature_search",
                "web_search",
                "web_read",
                "knowledge_search",
                "knowledge_entity",
                "knowledge_stats",
                "knowledge_paths",
                "semantic_search",
                "list_corpora",
                "list_models",
                "list_predictable_properties",
                "discover_capabilities",
                "show_scratchpad",
                "read_file",
                "list_lab_services",
                "get_lab_service_info",
                "check_lab_subscriptions",
            }),
        )

    @staticmethod
    def accept_all() -> ToolPermissionContext:
        """Everything auto-approved, nothing blocked."""
        return ToolPermissionContext(
            deny_names=frozenset(),
            deny_prefixes=(),
            auto_approve_names=frozenset({"*"}),
        )
