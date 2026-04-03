"""Tool execution server — stdin/stdout JSON-line protocol.

Run as: python3 -m app.tool_server

Reads one JSON object per line from stdin, writes one JSON object per line
to stdout.  Methods: list_tools, call_tool.
"""
import json
import sys

from app.plugins.bootstrap import build_full_registry


def _handle(registry, request: dict) -> dict:
    method = request.get("method")
    if method is None:
        return {"error": "missing 'method' field"}

    if method == "list_tools":
        return {
            "tools": [
                {
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.input_schema,
                    "requires_approval": t.requires_approval,
                }
                for t in registry.list_tools()
            ]
        }

    if method == "call_tool":
        name = request.get("tool", "")
        try:
            tool = registry.get(name)
        except KeyError:
            return {"error": f"unknown tool: {name}"}
        try:
            result = tool.execute(**(request.get("args") or {}))
            return {"result": result}
        except Exception as exc:
            return {"error": str(exc)}

    return {"error": f"unknown method: {method}"}


def main():
    tool_reg, _, _ = build_full_registry(enable_mcp=False, enable_plugins=False)

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            request = json.loads(line)
        except json.JSONDecodeError as exc:
            response = {"error": f"invalid JSON: {exc}"}
        else:
            response = _handle(tool_reg, request)

        sys.stdout.write(json.dumps(response) + "\n")
        sys.stdout.flush()


if __name__ == "__main__":
    main()
