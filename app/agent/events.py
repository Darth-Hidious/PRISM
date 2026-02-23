"""Event types for the agent loop."""
from dataclasses import dataclass, field
from typing import List, Optional


@dataclass
class ToolCallEvent:
    """Represents a tool call requested by the LLM."""
    tool_name: str
    tool_args: dict
    call_id: str


@dataclass
class AgentResponse:
    """Structured response from a backend LLM call."""
    text: Optional[str] = None
    tool_calls: List[ToolCallEvent] = field(default_factory=list)

    @property
    def has_tool_calls(self) -> bool:
        return len(self.tool_calls) > 0
