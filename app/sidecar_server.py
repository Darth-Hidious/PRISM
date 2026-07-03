# Copyright (c) 2025-2026 MARC27. Licensed under MIT License.
"""Science-sidecar tool server — stdin/stdout JSON-line protocol.

Same wire protocol as `app.tool_server`, but runs inside the SIDECAR venv
(`~/.prism/venv-sci`, Python 3.12) and registers ONLY the tools whose
dependencies do not install on the main venv's Python (pyiron, pycalphad).
The main tool server proxies matching tool calls here transparently — the
agent never knows two interpreters are involved.

Run as: ~/.prism/venv-sci/bin/python3 -m app.sidecar_server
(cwd must be the PRISM repo root so `app` is importable).
"""
import json
import sys

# Same banner guard as tool_server: keep stdout pure JSON.
_PROTOCOL_OUT = sys.stdout
sys.stdout = sys.stderr

from app.tools.base import ToolRegistry  # noqa: E402
from app.tool_server import _handle  # noqa: E402


def build_sidecar_registry() -> ToolRegistry:
    registry = ToolRegistry()
    # Each block independent: one missing dep must not take down the rest.
    try:
        from app.tools.sim_tools import create_simulation_tools

        create_simulation_tools(registry)
    except Exception as exc:  # pragma: no cover - env-dependent
        print(f"sidecar: sim tools unavailable: {exc}", file=sys.stderr)
    try:
        from app.tools.calphad import create_calphad_tools

        create_calphad_tools(registry)
    except Exception as exc:  # pragma: no cover - env-dependent
        print(f"sidecar: calphad tools unavailable: {exc}", file=sys.stderr)
    return registry


def main() -> None:
    registry = build_sidecar_registry()
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            request = json.loads(line)
        except json.JSONDecodeError as exc:
            response = {"error": f"invalid JSON: {exc}"}
        else:
            response = _handle(registry, request)
        _PROTOCOL_OUT.write(json.dumps(response) + "\n")
        _PROTOCOL_OUT.flush()


if __name__ == "__main__":
    main()
