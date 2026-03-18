"""Native MARC27 backend via marc27-sdk PlatformClient."""
from __future__ import annotations

import json
import os
from typing import Any, Dict, Generator, List, Optional

from app.agent.backends.base import Backend
from app.agent.events import (
    AgentResponse,
    TextDelta,
    ToolCallEvent,
    ToolCallStart,
    TurnComplete,
    UsageInfo,
)
from app.agent.models import get_default_model, get_model_config

DEFAULT_MARC27_PLATFORM_URL = "https://api.marc27.com"


def _load_platform_client_cls():
    from marc27 import PlatformClient

    return PlatformClient


def _resolve_marc27_platform_url() -> str:
    return os.getenv("MARC27_PLATFORM_URL", DEFAULT_MARC27_PLATFORM_URL).rstrip("/")


def _load_marc27_project_id_from_credentials() -> Optional[str]:
    try:
        from marc27.credentials import CredentialsManager

        creds = CredentialsManager().load()
        if creds and creds.project_id:
            return str(creds.project_id)
    except Exception:
        pass
    return None


class Marc27Backend(Backend):
    """Backend that calls MARC27 platform's native LLM stream endpoint."""

    def __init__(
        self,
        model: str | None = None,
        api_key: str | None = None,
        project_id: str | None = None,
        upstream_provider: str | None = None,
        platform_url: str | None = None,
    ) -> None:
        self.model = model or os.getenv("PRISM_MODEL") or get_default_model("marc27")
        self.model_config = get_model_config(self.model)
        self._synthetic_tool_call_seq = 0
        self.platform_url = (platform_url or _resolve_marc27_platform_url()).rstrip("/")
        # Keep env in sync so SDK internals (and auth refresh) use the same host.
        os.environ.setdefault("MARC27_PLATFORM_URL", self.platform_url)

        key = api_key or os.getenv("MARC27_API_KEY") or os.getenv("MARC27_TOKEN")
        platform_client_cls = _load_platform_client_cls()
        self.client = platform_client_cls(api_key=key, platform_url=self.platform_url)

        self.project_id = (
            project_id
            or os.getenv("MARC27_PROJECT_ID")
            or _load_marc27_project_id_from_credentials()
        )
        if not self.project_id:
            raise ValueError(
                "MARC27 project id not found. Set MARC27_PROJECT_ID or login with "
                "marc27-sdk so ~/.prism/credentials.json has project_id."
            )

        self.upstream_provider = (
            upstream_provider
            or os.getenv("MARC27_LLM_PROVIDER")
            or self._infer_provider(self.model)
        )
        self.upstream_model = self.model.split("/", 1)[1] if "/" in self.model else self.model

    def _new_tool_call_id(self) -> str:
        self._synthetic_tool_call_seq += 1
        return f"prism_call_{self._synthetic_tool_call_seq}"

    @staticmethod
    def _infer_provider(model: str) -> str:
        if "/" in model:
            return model.split("/", 1)[0]
        cfg = get_model_config(model)
        return cfg.provider if cfg.provider != "unknown" else "anthropic"

    def complete(
        self, messages: List[Dict], tools: List[dict], system_prompt: Optional[str] = None
    ) -> AgentResponse:
        for _ in self.complete_stream(messages=messages, tools=tools, system_prompt=system_prompt):
            pass
        return self._last_stream_response or AgentResponse()

    def complete_stream(
        self, messages: List[Dict], tools: List[dict], system_prompt: Optional[str] = None
    ) -> Generator:
        formatted_messages = self._format_messages(messages, system_prompt)
        kwargs: Dict[str, Any] = {
            "project_id": self.project_id,
            "provider": self.upstream_provider,
            "model": self.upstream_model,
            "messages": formatted_messages,
        }
        if tools:
            kwargs["tools"] = self._format_tools(tools)

        stream = self.client.llm.stream_completion(**kwargs)

        text_parts: List[str] = []
        tool_calls_acc: Dict[int, Dict[str, Any]] = {}
        seen_tool_starts: set[int] = set()
        usage: Optional[UsageInfo] = None

        for chunk in stream:
            if not isinstance(chunk, dict):
                continue

            chunk_usage = self._parse_usage(chunk)
            if chunk_usage:
                usage = chunk_usage

            for text in self._extract_text_deltas(chunk):
                text_parts.append(text)
                yield TextDelta(text=text)

            for delta in self._extract_tool_call_deltas(chunk):
                idx = int(delta.get("index", 0))
                if idx not in tool_calls_acc:
                    tool_calls_acc[idx] = {"id": "", "name": "", "args_str": "", "args_obj": None}

                acc = tool_calls_acc[idx]
                if delta.get("id"):
                    acc["id"] = str(delta["id"])
                if delta.get("name"):
                    acc["name"] = str(delta["name"])
                if delta.get("args_obj") is not None:
                    acc["args_obj"] = delta["args_obj"]
                if delta.get("args_fragment"):
                    acc["args_str"] += str(delta["args_fragment"])
                if not acc["id"]:
                    acc["id"] = self._new_tool_call_id()

                if idx not in seen_tool_starts and acc["name"]:
                    seen_tool_starts.add(idx)
                    yield ToolCallStart(tool_name=acc["name"], call_id=acc["id"])

        tool_calls: List[ToolCallEvent] = []
        for idx in sorted(tool_calls_acc):
            acc = tool_calls_acc[idx]
            if not acc["name"]:
                continue
            args = {}
            if isinstance(acc.get("args_obj"), dict):
                args = acc["args_obj"]
            elif acc.get("args_str"):
                try:
                    args = json.loads(acc["args_str"])
                except (json.JSONDecodeError, TypeError):
                    args = {}
            tool_calls.append(
                ToolCallEvent(
                    tool_name=acc["name"],
                    tool_args=args,
                    call_id=acc["id"] or self._new_tool_call_id(),
                )
            )

        full_text = "".join(text_parts) if text_parts else None
        response = AgentResponse(text=full_text, tool_calls=tool_calls, usage=usage)
        self._last_stream_response = response
        yield TurnComplete(text=full_text, has_more=response.has_tool_calls)

    def _format_messages(self, messages: List[Dict], system_prompt: Optional[str] = None) -> List[Dict]:
        formatted: List[Dict] = []
        pending_tool_call_ids: List[str] = []
        if system_prompt:
            formatted.append({"role": "system", "content": system_prompt})
        for msg in messages:
            role = msg["role"]
            if role == "tool_calls":
                tool_calls = []
                pending_tool_call_ids = []
                for tc in msg["calls"]:
                    call_id = tc.get("id") or self._new_tool_call_id()
                    pending_tool_call_ids.append(call_id)
                    tool_calls.append(
                        {
                            "id": call_id,
                            "type": "function",
                            "function": {
                                "name": tc["name"],
                                "arguments": json.dumps(tc["args"]),
                            },
                        }
                    )
                formatted.append(
                    {"role": "assistant", "content": msg.get("text"), "tool_calls": tool_calls}
                )
            elif role == "tool_result":
                tool_call_id = msg.get("tool_call_id") or (
                    pending_tool_call_ids.pop(0) if pending_tool_call_ids else self._new_tool_call_id()
                )
                formatted.append(
                    {
                        "role": "tool",
                        "tool_call_id": tool_call_id,
                        "content": json.dumps(msg["result"])
                        if isinstance(msg["result"], dict)
                        else str(msg["result"]),
                    }
                )
            else:
                formatted.append({"role": role, "content": msg["content"]})
        return formatted

    def _format_tools(self, tools: List[dict]) -> List[dict]:
        formatted = []
        for t in tools:
            if t.get("type") == "function" and isinstance(t.get("function"), dict):
                formatted.append(t)
                continue
            formatted.append(
                {
                    "type": "function",
                    "function": {
                        "name": t["name"],
                        "description": t["description"],
                        "parameters": t.get("input_schema", {}),
                    },
                }
            )
        return formatted

    @staticmethod
    def _parse_usage(chunk: dict) -> Optional[UsageInfo]:
        usage = chunk.get("usage")
        if not isinstance(usage, dict):
            return None
        input_tokens = usage.get("input_tokens", usage.get("prompt_tokens", 0)) or 0
        output_tokens = usage.get("output_tokens", usage.get("completion_tokens", 0)) or 0
        if not input_tokens and not output_tokens:
            return None
        return UsageInfo(
            input_tokens=int(input_tokens),
            output_tokens=int(output_tokens),
        )

    @staticmethod
    def _extract_text_deltas(chunk: dict) -> List[str]:
        out: List[str] = []

        token = chunk.get("token")
        if isinstance(token, str) and token:
            out.append(token)

        if chunk.get("type") in {"text_delta", "response.output_text.delta"}:
            text = chunk.get("text") or chunk.get("delta")
            if isinstance(text, str) and text:
                out.append(text)

        choices = chunk.get("choices")
        if isinstance(choices, list):
            for choice in choices:
                if not isinstance(choice, dict):
                    continue
                delta = choice.get("delta")
                if not isinstance(delta, dict):
                    continue
                content = delta.get("content")
                if isinstance(content, str) and content:
                    out.append(content)

        if chunk.get("type") == "content_block_delta":
            delta = chunk.get("delta")
            if isinstance(delta, dict):
                text = delta.get("text")
                if isinstance(text, str) and text:
                    out.append(text)

        return out

    @staticmethod
    def _extract_tool_call_deltas(chunk: dict) -> List[dict]:
        deltas: List[dict] = []

        choices = chunk.get("choices")
        if isinstance(choices, list):
            for choice in choices:
                if not isinstance(choice, dict):
                    continue
                delta = choice.get("delta")
                if not isinstance(delta, dict):
                    continue
                tool_calls = delta.get("tool_calls")
                if not isinstance(tool_calls, list):
                    continue
                for i, tc in enumerate(tool_calls):
                    if not isinstance(tc, dict):
                        continue
                    fn = tc.get("function") if isinstance(tc.get("function"), dict) else {}
                    deltas.append(
                        {
                            "index": tc.get("index", i),
                            "id": tc.get("id"),
                            "name": fn.get("name"),
                            "args_fragment": fn.get("arguments"),
                        }
                    )

        if chunk.get("type") == "content_block_start":
            block = chunk.get("content_block")
            if isinstance(block, dict) and block.get("type") == "tool_use":
                deltas.append(
                    {
                        "index": chunk.get("index", 0),
                        "id": block.get("id"),
                        "name": block.get("name"),
                        "args_obj": block.get("input", {}),
                    }
                )

        if chunk.get("type") == "content_block_delta":
            delta = chunk.get("delta")
            if isinstance(delta, dict):
                fragment = delta.get("partial_json")
                if isinstance(fragment, str) and fragment:
                    deltas.append(
                        {
                            "index": chunk.get("index", 0),
                            "id": None,
                            "name": None,
                            "args_fragment": fragment,
                        }
                    )

        return deltas
