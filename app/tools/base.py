"""Tool base class and registry for provider-agnostic tool definitions."""
from dataclasses import dataclass, field
from typing import Any, Callable, Dict, List, Optional


@dataclass
class Tool:
    """A single tool that can be called by the agent."""

    name: str
    description: str
    input_schema: dict
    func: Callable

    def execute(self, **kwargs) -> dict:
        """Execute the tool with given arguments."""
        return self.func(**kwargs)


class ToolRegistry:
    """Registry of available tools, with format conversion for each backend."""

    def __init__(self):
        self._tools: Dict[str, Tool] = {}

    def register(self, tool: Tool) -> None:
        """Register a tool."""
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
