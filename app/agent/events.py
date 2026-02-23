"""Event types for the agent loop."""
from dataclasses import dataclass, field
from typing import Any, List, Optional


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


# --- Streaming events ---

@dataclass
class TextDelta:
    """A chunk of streamed text from the LLM."""
    text: str


@dataclass
class ToolCallStart:
    """Signals the start of a tool call during streaming."""
    tool_name: str
    call_id: str


@dataclass
class ToolCallResult:
    """Result of a tool execution during streaming."""
    call_id: str
    tool_name: str
    result: Any
    summary: str


@dataclass
class TurnComplete:
    """Signals the end of a streaming turn."""
    text: Optional[str] = None
    has_more: bool = False
