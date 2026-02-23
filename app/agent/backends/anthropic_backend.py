"""Anthropic backend using the Anthropic Python SDK."""
import json
import os
from typing import Dict, List, Optional
from anthropic import Anthropic
from app.agent.backends.base import Backend
from app.agent.events import AgentResponse, ToolCallEvent


class AnthropicBackend(Backend):
    """Backend that uses Anthropic's Messages API with tool use."""

    def __init__(self, model: str = None, api_key: str = None):
        self.client = Anthropic(api_key=api_key or os.getenv("ANTHROPIC_API_KEY"))
        self.model = model or os.getenv("PRISM_MODEL", "claude-sonnet-4-20250514")

    def complete(self, messages: List[Dict], tools: List[dict], system_prompt: Optional[str] = None) -> AgentResponse:
        kwargs = {"model": self.model, "max_tokens": 4096, "messages": self._format_messages(messages)}
        if system_prompt:
            kwargs["system"] = system_prompt
        if tools:
            kwargs["tools"] = tools
        response = self.client.messages.create(**kwargs)
        return self._parse_response(response)

    def _format_messages(self, messages: List[Dict]) -> List[Dict]:
        """Convert neutral message format to Anthropic format."""
        formatted = []
        for msg in messages:
            role = msg["role"]
            if role == "tool_calls":
                content = []
                if msg.get("text"):
                    content.append({"type": "text", "text": msg["text"]})
                for tc in msg["calls"]:
                    content.append({"type": "tool_use", "id": tc["id"], "name": tc["name"], "input": tc["args"]})
                formatted.append({"role": "assistant", "content": content})
            elif role == "tool_result":
                content = [{"type": "tool_result", "tool_use_id": msg["tool_call_id"],
                    "content": json.dumps(msg["result"]) if isinstance(msg["result"], dict) else str(msg["result"])}]
                formatted.append({"role": "user", "content": content})
            else:
                formatted.append({"role": role, "content": msg["content"]})
        return formatted

    def _parse_response(self, response) -> AgentResponse:
        text_parts = []
        tool_calls = []
        for block in response.content:
            if block.type == "text":
                text_parts.append(block.text)
            elif block.type == "tool_use":
                tool_calls.append(ToolCallEvent(tool_name=block.name, tool_args=block.input, call_id=block.id))
        return AgentResponse(text="\n".join(text_parts) if text_parts else None, tool_calls=tool_calls)
