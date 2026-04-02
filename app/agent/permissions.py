"""Immutable tool permission context — inspired by claw-code's pattern.

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
from typing import FrozenSet, Tuple


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
