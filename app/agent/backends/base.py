"""Abstract base class for agent backends."""
from abc import ABC, abstractmethod
from typing import Dict, Generator, List, Optional
from app.agent.events import AgentResponse, TextDelta, TurnComplete


class Backend(ABC):
    """Provider-agnostic backend interface."""

    _last_stream_response: Optional[AgentResponse] = None

    @abstractmethod
    def complete(self, messages: List[Dict], tools: List[dict], system_prompt: Optional[str] = None) -> AgentResponse:
        pass

    def complete_stream(self, messages: List[Dict], tools: List[dict], system_prompt: Optional[str] = None) -> Generator:
        """Stream events from the backend. Default: fallback to complete()."""
        response = self.complete(messages, tools, system_prompt)
        self._last_stream_response = response
        if response.text:
            yield TextDelta(text=response.text)
        yield TurnComplete(text=response.text, has_more=response.has_tool_calls)
