"""OpenAI-compatible backend (also supports OpenRouter via base_url)."""
import json
import os
from typing import Dict, Generator, List, Optional
from openai import OpenAI
from app.agent.backends.base import Backend
from app.agent.events import AgentResponse, ToolCallEvent, TextDelta, ToolCallStart, TurnComplete


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

    def complete_stream(self, messages: List[Dict], tools: List[dict], system_prompt: Optional[str] = None) -> Generator:
        formatted_messages = self._format_messages(messages, system_prompt)
        kwargs = {"model": self.model, "messages": formatted_messages, "stream": True}
        if tools:
            kwargs["tools"] = self._format_tools(tools)
        stream = self.client.chat.completions.create(**kwargs)

        text_parts = []
        tool_calls_acc = {}  # index -> {id, name, args_str}
        seen_tool_starts = set()

        for chunk in stream:
            delta = chunk.choices[0].delta if chunk.choices else None
            if delta is None:
                continue
            if delta.content:
                text_parts.append(delta.content)
                yield TextDelta(text=delta.content)
            if delta.tool_calls:
                for tc_delta in delta.tool_calls:
                    idx = tc_delta.index
                    if idx not in tool_calls_acc:
                        tool_calls_acc[idx] = {"id": "", "name": "", "args_str": ""}
                    if tc_delta.id:
                        tool_calls_acc[idx]["id"] = tc_delta.id
                    if tc_delta.function and tc_delta.function.name:
                        tool_calls_acc[idx]["name"] = tc_delta.function.name
                    if tc_delta.function and tc_delta.function.arguments:
                        tool_calls_acc[idx]["args_str"] += tc_delta.function.arguments
                    if idx not in seen_tool_starts and tool_calls_acc[idx]["name"]:
                        seen_tool_starts.add(idx)
                        yield ToolCallStart(tool_name=tool_calls_acc[idx]["name"], call_id=tool_calls_acc[idx]["id"])

        # Build final AgentResponse
        tool_calls = []
        for idx in sorted(tool_calls_acc):
            acc = tool_calls_acc[idx]
            try:
                tool_args = json.loads(acc["args_str"]) if acc["args_str"] else {}
            except (json.JSONDecodeError, TypeError):
                tool_args = {}
            tool_calls.append(ToolCallEvent(tool_name=acc["name"], tool_args=tool_args, call_id=acc["id"]))

        full_text = "".join(text_parts) if text_parts else None
        response = AgentResponse(text=full_text, tool_calls=tool_calls)
        self._last_stream_response = response
        yield TurnComplete(text=full_text, has_more=response.has_tool_calls)

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
