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
class UsageInfo:
    """Token usage from a single API call."""
    input_tokens: int = 0
    output_tokens: int = 0
    cache_creation_tokens: int = 0
    cache_read_tokens: int = 0

    @property
    def total_tokens(self) -> int:
        return self.input_tokens + self.output_tokens

    def __add__(self, other: "UsageInfo") -> "UsageInfo":
        return UsageInfo(
            input_tokens=self.input_tokens + other.input_tokens,
            output_tokens=self.output_tokens + other.output_tokens,
            cache_creation_tokens=self.cache_creation_tokens + other.cache_creation_tokens,
            cache_read_tokens=self.cache_read_tokens + other.cache_read_tokens,
        )


@dataclass
class AgentResponse:
    """Structured response from a backend LLM call."""
    text: Optional[str] = None
    tool_calls: List[ToolCallEvent] = field(default_factory=list)
    usage: Optional[UsageInfo] = None

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
class ToolApprovalRequest:
    """Requests user approval before executing an expensive tool."""
    tool_name: str
    tool_args: dict
    call_id: str


@dataclass
class ToolApprovalResponse:
    """User's response to a tool approval request."""
    call_id: str
    approved: bool


@dataclass
class TurnComplete:
    """Signals the end of a streaming turn."""
    text: Optional[str] = None
    has_more: bool = False
    usage: Optional[UsageInfo] = None
    total_usage: Optional[UsageInfo] = None
    estimated_cost: Optional[float] = None
