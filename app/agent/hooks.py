"""Pre/post tool execution hooks — extensibility without modifying the core loop.

- Hooks fire before and after every tool call
- Can block execution (pre-hook returns abort=True)
- Cost hooks fire observationally (never block)
- Hooks are registered per-session, immutable after build
"""

from __future__ import annotations

import logging
import time
from dataclasses import dataclass, field
from typing import Any, Callable, Optional

logger = logging.getLogger(__name__)


@dataclass(frozen=True)
class HookResult:
    """Result of a pre-hook execution."""
    abort: bool = False
    reason: str = ""
    modified_inputs: Optional[dict] = None  # can modify tool inputs


@dataclass(frozen=True)
class PostHookResult:
    """Result of a post-hook execution."""
    modified_result: Optional[dict] = None  # can modify tool output
    log_message: str = ""


@dataclass
class Hook:
    """A named hook with optional pre/post callbacks."""
    name: str
    # Pre-tool: receives (tool_name, inputs) → HookResult
    before: Optional[Callable[[str, dict], HookResult]] = None
    # Post-tool: receives (tool_name, inputs, result, elapsed_ms) → PostHookResult
    after: Optional[Callable[[str, dict, dict, float], PostHookResult]] = None
    # Tool name filter — if set, hook only fires for matching tools
    tool_filter: Optional[set[str]] = None

    def matches(self, tool_name: str) -> bool:
        if self.tool_filter is None:
            return True
        return tool_name in self.tool_filter


class HookRegistry:
    """Registry of pre/post tool hooks.

    Hooks fire in registration order. Pre-hooks can abort execution.
    Post-hooks are observational (fire-and-forget).
    """

    def __init__(self):
        self._hooks: list[Hook] = []

    def register(self, hook: Hook) -> None:
        self._hooks.append(hook)
        logger.debug("Registered hook: %s", hook.name)

    def fire_before(self, tool_name: str, inputs: dict) -> HookResult:
        """Fire all matching pre-hooks. Returns abort if any hook aborts."""
        for hook in self._hooks:
            if hook.before and hook.matches(tool_name):
                try:
                    result = hook.before(tool_name, inputs)
                    if result.abort:
                        logger.info("Hook '%s' aborted %s: %s", hook.name, tool_name, result.reason)
                        return result
                    if result.modified_inputs is not None:
                        inputs.update(result.modified_inputs)
                except Exception as e:
                    logger.warning("Pre-hook '%s' failed: %s", hook.name, e)
        return HookResult()

    def fire_after(
        self, tool_name: str, inputs: dict, result: dict, elapsed_ms: float
    ) -> dict:
        """Fire all matching post-hooks. Returns potentially modified result."""
        for hook in self._hooks:
            if hook.after and hook.matches(tool_name):
                try:
                    post_result = hook.after(tool_name, inputs, result, elapsed_ms)
                    if post_result.log_message:
                        logger.info("Post-hook '%s': %s", hook.name, post_result.log_message)
                    if post_result.modified_result is not None:
                        result = post_result.modified_result
                except Exception as e:
                    logger.warning("Post-hook '%s' failed: %s", hook.name, e)
        return result


# ── Built-in hooks ──────────────────────────────────────────────────

def cost_hook() -> Hook:
    """Track tool execution cost and timing."""
    def _after(tool_name: str, inputs: dict, result: dict, elapsed_ms: float) -> PostHookResult:
        return PostHookResult(
            log_message=f"{tool_name}: {elapsed_ms:.0f}ms"
        )
    return Hook(name="cost_tracker", after=_after)


def safety_hook() -> Hook:
    """Block destructive operations without explicit approval."""
    DESTRUCTIVE_PATTERNS = {"delete", "drop", "remove", "destroy", "truncate", "reset"}

    def _before(tool_name: str, inputs: dict) -> HookResult:
        # Check if any input contains destructive keywords
        for key, val in inputs.items():
            if isinstance(val, str):
                lowered = val.lower()
                for pattern in DESTRUCTIVE_PATTERNS:
                    if pattern in lowered:
                        return HookResult(
                            abort=True,
                            reason=f"Blocked: '{pattern}' detected in {tool_name}.{key}. "
                                   "Use --dangerously-accept-all to override."
                        )
        return HookResult()

    return Hook(name="safety_guard", before=_before)


def audit_hook(log_fn: Callable[[str], None] | None = None) -> Hook:
    """Log all tool executions for audit trail."""
    def _after(tool_name: str, inputs: dict, result: dict, elapsed_ms: float) -> PostHookResult:
        has_error = "error" in result
        status = "ERROR" if has_error else "OK"
        msg = f"[AUDIT] {tool_name} {status} ({elapsed_ms:.0f}ms)"
        if log_fn:
            log_fn(msg)
        return PostHookResult(log_message=msg)

    return Hook(name="audit_log", after=_after)


def build_default_hooks(audit_log_fn: Callable[[str], None] | None = None) -> HookRegistry:
    """Build the default hook registry with safety + cost + audit hooks."""
    registry = HookRegistry()
    registry.register(safety_hook())
    registry.register(cost_hook())
    registry.register(audit_hook(audit_log_fn))
    return registry
