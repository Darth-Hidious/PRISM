"""Tool base class and registry for provider-agnostic tool definitions.

Tool.execute is the single integration point for cross-cutting concerns
(currently: artifact recording for the stateful memory subsystem). Every
caller — `app/tool_server.py`, `app/mcp_server.py`, future callers — flows
through this method, so attaching behavior here is the only place that
catches every path. Earlier designs that monkey-patched at bootstrap broke
the MCP path because mcp_server captures `tool.execute` as a bound method
at registration time; bound methods cache `(func, instance)` and don't
see later class-level patches.
"""

import logging
from dataclasses import dataclass, field
from typing import Any, Callable, Dict, List, Optional

logger = logging.getLogger(__name__)


@dataclass
class Tool:
    """A single tool that can be called by the agent."""

    name: str
    description: str
    input_schema: dict
    func: Callable
    requires_approval: bool = False
    source: str = "builtin"
    source_detail: Optional[str] = None
    # Memory subsystem opt-out. Tools that ARE the memory subsystem
    # (recall, fetch_artifact, list_artifacts, show_scratchpad) MUST set
    # this False to avoid pointless self-indexing and infinite recursion.
    record_artifacts: bool = True

    def execute(self, **kwargs) -> dict:
        """Execute the tool with given arguments.

        After the underlying function returns, hand the result to the
        memory recorder. If memory is disabled or the tool opts out, the
        result passes through unchanged. Recording failures are
        logged-and-swallowed inside the recorder — they never break tool
        execution.
        """
        result = self.func(**kwargs)
        if not self.record_artifacts:
            return result
        # Lazy import to avoid a circular dependency at module load. The
        # recorder is purely additive — if memory hasn't been configured,
        # it returns the original result unchanged.
        try:
            from app.tools.memory.recorder import record_if_enabled
        except ImportError:
            return result
        return record_if_enabled(tool_name=self.name, args=kwargs, result=result)


class ToolRegistry:
    """Registry of available tools, with format conversion for each backend."""

    def __init__(self):
        self._tools: Dict[str, Tool] = {}

    def register(self, tool: Tool) -> None:
        """Register a tool. Logs a warning if the name is already taken."""
        if tool.name in self._tools:
            logger.warning(
                "tool '%s' re-registered (overwrites %s from %s)",
                tool.name,
                self._tools[tool.name].source,
                self._tools[tool.name].source_detail or "unknown",
            )
        self._tools[tool.name] = tool

    def get(self, name: str) -> Tool:
        """Get a tool by name. Raises KeyError if not found."""
        return self._tools[name]

    def list_tools(self) -> List[Tool]:
        """Return all registered tools."""
        return list(self._tools.values())

    def to_anthropic_format(self) -> List[dict]:
        """Convert tools to Anthropic API format."""
        return [
            {
                "name": t.name,
                "description": t.description,
                "input_schema": t.input_schema,
            }
            for t in self._tools.values()
        ]

    def to_openai_format(self) -> List[dict]:
        """Convert tools to OpenAI API format."""
        return [
            {
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.input_schema,
                },
            }
            for t in self._tools.values()
        ]
