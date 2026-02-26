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
from app.cli.tui.cards import detect_result_type
from app.cli.tui.spinner import TOOL_VERBS
from app.cli.tui.welcome import detect_capabilities, _detect_provider


def _verb_for_tool(tool_name: str) -> str:
    """Look up a human-readable verb for a tool, falling back to 'Thinking...'."""
    return TOOL_VERBS.get(tool_name, "Thinking\u2026")


def _skill_count() -> int:
    """Count available built-in skills, returning 0 on import failure."""
    try:
        from app.skills.registry import load_builtin_skills
        return len(load_builtin_skills().list_skills())
    except Exception:
        return 0


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
        """Emit a ui.welcome event with version, provider, and capabilities."""
        capabilities = detect_capabilities()
        provider = _detect_provider() or "unknown"
        tool_count = len(self.agent.tools.list_tools())
        skill_count = _skill_count()
        return make_event("ui.welcome", {
            "version": __version__,
            "provider": provider,
            "capabilities": capabilities,
            "tool_count": tool_count,
            "skill_count": skill_count,
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

        # Unknown command
        yield make_event("ui.card", {
            "card_type": "info", "tool_name": "", "elapsed_ms": 0,
            "content": f"Unknown: {base_cmd}  \u2014  /help for commands",
            "data": {},
        })
