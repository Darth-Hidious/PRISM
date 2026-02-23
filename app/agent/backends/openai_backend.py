"""OpenAI-compatible backend (also supports OpenRouter via base_url)."""
import json
import os
from typing import Dict, List, Optional
from openai import OpenAI
from app.agent.backends.base import Backend
from app.agent.events import AgentResponse, ToolCallEvent


class OpenAIBackend(Backend):
    def __init__(self, model: str = None, api_key: str = None, base_url: str = None):
        kwargs = {}
        if api_key:
            kwargs["api_key"] = api_key
        if base_url:
            kwargs["base_url"] = base_url
        self.client = OpenAI(**kwargs)
        self.model = model or os.getenv("PRISM_MODEL", "gpt-4o")

    def complete(self, messages: List[Dict], tools: List[dict], system_prompt: Optional[str] = None) -> AgentResponse:
        formatted_messages = self._format_messages(messages, system_prompt)
        kwargs = {"model": self.model, "messages": formatted_messages}
        if tools:
            kwargs["tools"] = self._format_tools(tools)
        response = self.client.chat.completions.create(**kwargs)
        return self._parse_response(response)

    def _format_messages(self, messages: List[Dict], system_prompt: Optional[str] = None) -> List[Dict]:
        formatted = []
        if system_prompt:
            formatted.append({"role": "system", "content": system_prompt})
        for msg in messages:
            role = msg["role"]
            if role == "tool_calls":
                tool_calls = []
                for tc in msg["calls"]:
                    tool_calls.append({"id": tc["id"], "type": "function", "function": {"name": tc["name"], "arguments": json.dumps(tc["args"])}})
                formatted.append({"role": "assistant", "content": msg.get("text"), "tool_calls": tool_calls})
            elif role == "tool_result":
                formatted.append({"role": "tool", "tool_call_id": msg["tool_call_id"],
                    "content": json.dumps(msg["result"]) if isinstance(msg["result"], dict) else str(msg["result"])})
            else:
                formatted.append({"role": role, "content": msg["content"]})
        return formatted

    def _format_tools(self, tools: List[dict]) -> List[dict]:
        return [{"type": "function", "function": {"name": t["name"], "description": t["description"], "parameters": t.get("input_schema", {})}} for t in tools]

    def _parse_response(self, response) -> AgentResponse:
        msg = response.choices[0].message
        tool_calls = []
        if msg.tool_calls:
            for tc in msg.tool_calls:
                try:
                    tool_args = json.loads(tc.function.arguments)
                except (json.JSONDecodeError, TypeError):
                    tool_args = {}
                tool_calls.append(ToolCallEvent(tool_name=tc.function.name, tool_args=tool_args, call_id=tc.id))
        return AgentResponse(text=msg.content, tool_calls=tool_calls)
