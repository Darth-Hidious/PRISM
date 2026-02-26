"""UI Protocol — single source of truth for all event types.

Both the Python Rich frontend and TypeScript Ink frontend
consume these definitions. TypeScript types are generated via:
    python3 -m app.backend.protocol --emit-ts
"""
import json
from typing import Any


# Backend -> Frontend events (JSON-RPC notifications)
UI_EVENTS: dict[str, dict[str, type]] = {
    "ui.text.delta":    {"text": str},
    "ui.text.flush":    {"text": str},
    "ui.tool.start":    {"tool_name": str, "call_id": str, "verb": str},
    "ui.card":          {"card_type": str, "tool_name": str, "elapsed_ms": float,
                         "content": str, "data": dict},
    "ui.cost":          {"input_tokens": int, "output_tokens": int,
                         "turn_cost": float, "session_cost": float},
    "ui.prompt":        {"prompt_type": str, "message": str, "choices": list,
                         "tool_name": str, "tool_args": dict},
    "ui.welcome":       {"version": str, "provider": str, "capabilities": dict,
                         "tool_count": int, "skill_count": int, "auto_approve": bool},
    "ui.status":        {"auto_approve": bool, "message_count": int, "has_plan": bool},
    "ui.turn.complete": {},
    "ui.session.list":  {"sessions": list},
}

# Frontend -> Backend events (JSON-RPC requests/notifications)
INPUT_EVENTS: dict[str, dict[str, type]] = {
    "init":                   {"provider": str, "auto_approve": bool, "resume": str},
    "input.message":          {"text": str},
    "input.command":          {"command": str},
    "input.prompt_response":  {"prompt_type": str, "response": str},
    "input.load_session":     {"session_id": str},
}


def make_event(method: str, params: dict[str, Any] | None = None) -> dict:
    """Create a JSON-RPC 2.0 notification (backend -> frontend).

    Raises ValueError for unknown event methods.
    """
    if method not in UI_EVENTS:
        raise ValueError(f"Unknown event method: {method}")
    return {
        "jsonrpc": "2.0",
        "method": method,
        "params": params or {},
    }


def parse_input(raw: str) -> dict:
    """Parse a JSON-RPC 2.0 message from the frontend.

    Raises ValueError on malformed JSON or missing 'method' field.
    """
    try:
        msg = json.loads(raw)
    except json.JSONDecodeError as e:
        raise ValueError(f"Invalid JSON: {e}") from e
    if "method" not in msg:
        raise ValueError("Missing 'method' field")
    return msg


def _method_to_interface_name(method: str) -> str:
    """Convert 'ui.text.delta' -> 'UiTextDelta'."""
    return "".join(part.capitalize() for part in method.replace(".", "_").split("_"))


def _python_type_to_ts(t: type) -> str:
    """Map Python type hints to TypeScript types."""
    mapping = {
        str: "string",
        int: "number",
        float: "number",
        bool: "boolean",
        list: "any[]",
        dict: "Record<string, any>",
    }
    return mapping.get(t, "any")


def emit_typescript() -> str:
    """Generate TypeScript type declarations from event definitions."""
    lines = [
        "// Auto-generated from app/backend/protocol.py — DO NOT EDIT",
        "// Regenerate: python3 -m app.backend.protocol --emit-ts",
        "",
    ]

    # Backend -> Frontend interfaces
    for method, fields in UI_EVENTS.items():
        name = _method_to_interface_name(method)
        lines.append(f"export interface {name} {{")
        for field, ftype in fields.items():
            ts_type = _python_type_to_ts(ftype)
            lines.append(f"  {field}: {ts_type};")
        lines.append("}")
        lines.append("")

    # Frontend -> Backend interfaces (all fields optional)
    for method, fields in INPUT_EVENTS.items():
        name = _method_to_interface_name(method)
        lines.append(f"export interface {name} {{")
        for field, ftype in fields.items():
            ts_type = _python_type_to_ts(ftype)
            lines.append(f"  {field}?: {ts_type};")
        lines.append("}")
        lines.append("")

    # Union types
    ui_names = [_method_to_interface_name(m) for m in UI_EVENTS]
    input_names = [_method_to_interface_name(m) for m in INPUT_EVENTS]
    lines.append(f"export type UIEvent = {' | '.join(ui_names)};")
    lines.append(f"export type InputEvent = {' | '.join(input_names)};")
    lines.append("")

    # Method -> Interface lookup
    lines.append("export const UI_EVENT_MAP: Record<string, string> = {")
    for method in UI_EVENTS:
        name = _method_to_interface_name(method)
        lines.append(f'  "{method}": "{name}",')
    lines.append("};")
    lines.append("")

    return "\n".join(lines)


if __name__ == "__main__":
    import sys
    if "--emit-ts" in sys.argv:
        print(emit_typescript())
    else:
        print("Usage: python3 -m app.backend.protocol --emit-ts")
