"""UIEmitter — bridges AgentCore streaming events to UI protocol events.

This is the shared presentation logic layer. Both the Python Rich frontend
and the TypeScript Ink frontend consume the same ui.* event stream.
The emitter contains NO rendering code — only event translation.
"""
import time
from typing import Generator

from app import __version__
from app.agent.events import (
    TextDelta, ToolCallStart, ToolCallResult, TurnComplete,
    ToolApprovalRequest,
)
from app.backend.protocol import make_event
from app.backend.tool_meta import detect_result_type, TOOL_VERBS
from app.backend.status import build_status


def _verb_for_tool(tool_name: str) -> str:
    """Look up a human-readable verb for a tool, falling back to 'Thinking...'."""
    return TOOL_VERBS.get(tool_name, "Thinking\u2026")


class UIEmitter:
    """Translates AgentCore stream events into ui.* protocol events.

    Stateful per session: tracks session_cost and message_count.
    """

    def __init__(self, agent, auto_approve: bool = False):
        self.agent = agent
        self.auto_approve = auto_approve
        self.session_cost: float = 0.0
        self.message_count: int = 0

    def welcome(self) -> dict:
        """Emit a ui.welcome event with version and full status."""
        status = build_status(tool_registry=self.agent.tools)
        return make_event("ui.welcome", {
            "version": __version__,
            "status": status,
            "auto_approve": self.auto_approve,
        })

    def process(self, user_input: str) -> Generator[dict, None, None]:
        """Yield ui.* events by consuming agent.process_stream().

        Handles text accumulation, plan detection, tool start/result
        translation, approval prompts, and cost tracking.
        """
        self.message_count += 1

        accumulated_text = ""
        plan_buffer = ""
        in_plan = False
        tool_start_time = None

        for event in self.agent.process_stream(user_input):
            if isinstance(event, TextDelta):
                accumulated_text += event.text

                # Plan tag detection: opening tag
                if "<plan>" in accumulated_text and not in_plan:
                    in_plan = True
                    before, _, after = accumulated_text.partition("<plan>")
                    plan_buffer = after
                    if before.strip():
                        yield make_event("ui.text.flush", {"text": before.strip()})
                    accumulated_text = ""
                    continue

                # Plan tag detection: inside plan, accumulating
                if in_plan:
                    if "</plan>" in event.text:
                        # Closing tag found — extract plan content
                        plan_buffer += event.text.split("</plan>")[0]
                        in_plan = False
                        yield make_event("ui.card", {
                            "card_type": "plan",
                            "tool_name": "",
                            "elapsed_ms": 0.0,
                            "content": plan_buffer.strip(),
                            "data": {},
                        })
                        yield make_event("ui.prompt", {
                            "prompt_type": "plan_confirm",
                            "message": "Execute this plan?",
                            "choices": ["y", "n"],
                            "tool_name": "",
                            "tool_args": {},
                        })
                        # Capture any text after </plan>
                        remainder = event.text.split("</plan>", 1)[1] if "</plan>" in event.text else ""
                        accumulated_text = remainder
                    else:
                        plan_buffer += event.text
                    continue

                # Normal text streaming
                yield make_event("ui.text.delta", {"text": event.text})

            elif isinstance(event, ToolCallStart):
                # Flush any accumulated text before the tool starts
                if accumulated_text.strip():
                    yield make_event("ui.text.flush", {"text": accumulated_text})
                    accumulated_text = ""

                tool_start_time = time.monotonic()
                verb = _verb_for_tool(event.tool_name)
                yield make_event("ui.tool.start", {
                    "tool_name": event.tool_name,
                    "call_id": event.call_id,
                    "verb": verb,
                })

            elif isinstance(event, ToolApprovalRequest):
                yield make_event("ui.prompt", {
                    "prompt_type": "approval",
                    "message": f"Approve {event.tool_name}?",
                    "choices": ["y", "n", "a"],
                    "tool_name": event.tool_name,
                    "tool_args": event.tool_args,
                })

            elif isinstance(event, ToolCallResult):
                elapsed_ms = 0.0
                if tool_start_time is not None:
                    elapsed_ms = (time.monotonic() - tool_start_time) * 1000
                    tool_start_time = None

                result = event.result if isinstance(event.result, dict) else {}
                card_type = detect_result_type(result)
                yield make_event("ui.card", {
                    "card_type": card_type,
                    "tool_name": event.tool_name,
                    "elapsed_ms": elapsed_ms,
                    "content": event.summary,
                    "data": result,
                })

            elif isinstance(event, TurnComplete):
                # Flush remaining text
                if accumulated_text.strip():
                    yield make_event("ui.text.flush", {"text": accumulated_text})
                    accumulated_text = ""

                # Cost event
                if event.usage:
                    turn_cost = event.estimated_cost or 0.0
                    self.session_cost += turn_cost
                    yield make_event("ui.cost", {
                        "input_tokens": event.usage.input_tokens,
                        "output_tokens": event.usage.output_tokens,
                        "turn_cost": turn_cost,
                        "session_cost": self.session_cost,
                    })

                yield make_event("ui.turn.complete", {})

    def _emit_model_list(self) -> Generator[dict, None, None]:
        """Emit a ui.model.list event with MARC27 API models + local registry + Ollama."""
        import os
        import json as _json
        import urllib.request
        from app.agent.models import MODEL_REGISTRY

        current_model = getattr(self.agent.backend, "model", "unknown")
        models = []
        seen_ids = set()

        # 1. Fetch from MARC27 API (primary source — has all routed models + pricing)
        token = os.getenv("MARC27_API_KEY") or os.getenv("MARC27_TOKEN")
        platform_url = os.getenv("MARC27_PLATFORM_URL", "https://api.marc27.com")
        if token:
            for endpoint in ["/api/v1/llm/models", "/api/v1/models"]:
                try:
                    req = urllib.request.Request(
                        f"{platform_url}{endpoint}",
                        headers={"Authorization": f"Bearer {token}"},
                    )
                    resp = urllib.request.urlopen(req, timeout=5)
                    data = _json.loads(resp.read())
                    api_models = data if isinstance(data, list) else data.get("models", data.get("data", []))
                    for m in api_models:
                        mid = m.get("id") or m.get("model_id") or m.get("name", "")
                        if not mid or mid in seen_ids:
                            continue
                        seen_ids.add(mid)
                        models.append({
                            "id": mid,
                            "provider": m.get("provider", "marc27"),
                            "context_window": m.get("context_window", 0),
                            "input_price": m.get("input_price_per_mtok", m.get("input_price", 0.0)),
                            "output_price": m.get("output_price_per_mtok", m.get("output_price", 0.0)),
                            "supports_tools": m.get("supports_tools", True),
                            "supports_thinking": m.get("supports_thinking", False),
                            "local": False,
                        })
                    if models:
                        break  # Got models from API, don't try other endpoints
                except Exception:
                    continue

        # 2. Fall back to / supplement with local registry (for models not in API)
        for model_id, cfg in MODEL_REGISTRY.items():
            if model_id in seen_ids:
                continue
            seen_ids.add(model_id)
            models.append({
                "id": model_id,
                "provider": cfg.provider,
                "context_window": cfg.context_window,
                "input_price": cfg.input_price_per_mtok,
                "output_price": cfg.output_price_per_mtok,
                "supports_tools": cfg.supports_tools,
                "supports_thinking": cfg.supports_thinking,
                "local": False,
            })

        # 3. Local Ollama models
        try:
            req = urllib.request.Request("http://localhost:11434/api/tags", method="GET")
            resp = urllib.request.urlopen(req, timeout=2)
            data = _json.loads(resp.read())
            for m in data.get("models", []):
                name = m.get("name", "unknown")
                ollama_id = f"ollama/{name}"
                if ollama_id in seen_ids:
                    continue
                seen_ids.add(ollama_id)
                size_gb = m.get("size", 0) / (1024**3)
                models.append({
                    "id": ollama_id,
                    "provider": "ollama",
                    "context_window": 0,
                    "input_price": 0.0,
                    "output_price": 0.0,
                    "supports_tools": False,
                    "supports_thinking": False,
                    "local": True,
                    "size_gb": round(size_gb, 1),
                })
        except Exception:
            pass

        yield make_event("ui.model.list", {
            "current": current_model,
            "models": models,
        })

    def _switch_model(self, model_id: str) -> bool:
        """Switch the active model. Returns True on success."""
        from app.agent.models import MODEL_REGISTRY, get_model_config

        # Check if model exists in registry or is an Ollama model
        cfg = get_model_config(model_id)
        if cfg.provider == "unknown" and not model_id.startswith("ollama/"):
            return False

        # Update the backend's model
        if hasattr(self.agent.backend, "model"):
            self.agent.backend.model = model_id
        if hasattr(self.agent.backend, "upstream_model"):
            self.agent.backend.upstream_model = model_id.split("/", 1)[-1] if "/" in model_id else model_id
        if hasattr(self.agent.backend, "model_config"):
            self.agent.backend.model_config = cfg

        # Persist to settings
        try:
            from app.config.settings_schema import get_settings
            import json as _json
            import os
            settings_path = os.path.expanduser("~/.prism/settings.json")
            if os.path.exists(settings_path):
                with open(settings_path) as f:
                    data = _json.load(f)
                data.setdefault("agent", {})["model"] = model_id
                with open(settings_path, "w") as f:
                    _json.dump(data, f, indent=4)
                    f.write("\n")
        except Exception:
            pass

        return True

    def handle_command(self, command: str) -> Generator[dict, None, None]:
        """Handle slash commands. Yields ui.* events."""
        parts = command.strip().split(maxsplit=1)
        base_cmd = parts[0].lower()
        arg = parts[1].strip() if len(parts) > 1 else ""

        if base_cmd in ("/exit", "/quit"):
            yield make_event("ui.status", {
                "auto_approve": self.auto_approve,
                "message_count": self.message_count,
                "has_plan": False,
            })
            return

        if base_cmd == "/help":
            from app.cli.slash.registry import REPL_COMMANDS
            lines = []
            for name, desc in REPL_COMMANDS.items():
                if name == "/quit":
                    continue
                lines.append(f"  {name:<16} {desc}")
            yield make_event("ui.card", {
                "card_type": "info",
                "tool_name": "",
                "elapsed_ms": 0,
                "content": "\n".join(lines),
                "data": {},
            })
            return

        if base_cmd == "/tools":
            tools = self.agent.tools.list_tools()
            lines = []
            for tool in tools:
                flag = " \u2605" if tool.requires_approval else ""
                lines.append(f"  {tool.name:<28} {tool.description[:55]}{flag}")
            lines.append(f"\n  {len(tools)} tools  \u2605 = requires approval")
            yield make_event("ui.card", {
                "card_type": "info",
                "tool_name": "",
                "elapsed_ms": 0,
                "content": "\n".join(lines),
                "data": {},
            })
            return

        if base_cmd == "/approve-all":
            self.auto_approve = True
            self.agent.auto_approve = True
            yield make_event("ui.status", {
                "auto_approve": True,
                "message_count": self.message_count,
                "has_plan": False,
            })
            return

        if base_cmd == "/history":
            yield make_event("ui.card", {
                "card_type": "info", "tool_name": "", "elapsed_ms": 0,
                "content": f"{len(self.agent.history)} messages",
                "data": {},
            })
            return

        if base_cmd == "/sessions":
            from app.agent.memory import SessionMemory
            sessions = SessionMemory.list_sessions()
            yield make_event("ui.session.list", {
                "sessions": [
                    {"session_id": s["session_id"],
                     "timestamp": s.get("timestamp", ""),
                     "message_count": s.get("message_count", 0)}
                    for s in sessions[:20]
                ],
            })
            return

        if base_cmd == "/model":
            if arg:
                # Switch to specified model
                success = self._switch_model(arg)
                if success:
                    yield make_event("ui.card", {
                        "card_type": "info", "tool_name": "", "elapsed_ms": 0,
                        "content": f"Switched to **{arg}**",
                        "data": {},
                    })
                else:
                    yield make_event("ui.card", {
                        "card_type": "error", "tool_name": "", "elapsed_ms": 0,
                        "content": f"Unknown model: {arg}",
                        "data": {"error": f"Model {arg} not found in registry"},
                    })
            else:
                # List available models
                yield from self._emit_model_list()
            return

        # Unknown command
        yield make_event("ui.card", {
            "card_type": "info", "tool_name": "", "elapsed_ms": 0,
            "content": f"Unknown: {base_cmd}  \u2014  /help for commands",
            "data": {},
        })
