"""Abstract base class for agent backends."""
import time
from abc import ABC, abstractmethod
from typing import Dict, Generator, List, Optional
from app.agent.events import AgentResponse, TextDelta, TurnComplete


class Backend(ABC):
    """Provider-agnostic backend interface."""

    _last_stream_response: Optional[AgentResponse] = None
    _retryable_exceptions: tuple = ()
    _RETRYABLE_STATUS_CODES = (429, 500, 502, 503)

    def _retry_api_call(self, fn, max_retries: int = 3):
        """Call fn() with exponential backoff on transient API errors.

        Retries on exceptions in _retryable_exceptions whose status_code is
        in _RETRYABLE_STATUS_CODES. Respects Retry-After header when present.
        Non-retryable errors pass through immediately.
        """
        for attempt in range(max_retries + 1):
            try:
                return fn()
            except self._retryable_exceptions as exc:
                status = getattr(exc, "status_code", None)
                if status not in self._RETRYABLE_STATUS_CODES or attempt == max_retries:
                    raise
                headers = getattr(exc, "headers", {}) or {}
                retry_after = headers.get("Retry-After") or headers.get("retry-after")
                delay = min(2 ** attempt, 8)
                if retry_after is not None:
                    delay = max(delay, float(retry_after))
                time.sleep(delay)

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
