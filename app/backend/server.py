"""JSON-RPC 2.0 server over stdio -- thin wrapper around UIEmitter.

Usage: python3 -m app.backend
The Ink frontend spawns this as a child process and communicates
via stdin (JSON lines) / stdout (JSON lines).
"""
import json
import sys
from typing import TextIO

from app.backend.protocol import parse_input


class StdioServer:
    """JSON-RPC server on stdin/stdout."""

    def __init__(self):
        self.emitter = None

    def handle_message(self, raw: str, output: TextIO):
        """Process a single JSON-RPC message and write responses to output."""
        try:
            msg = parse_input(raw)
        except ValueError as e:
            self._send_error(output, None, -32700, str(e))
            return

        method = msg["method"]
        params = msg.get("params", {})
        msg_id = msg.get("id")

        if method == "init":
            self._handle_init(params, msg_id, output)
        elif method == "input.message":
            self._handle_input(params, output)
        elif method == "input.command":
            self._handle_command(params, output)
        elif method == "input.prompt_response":
            self._handle_prompt_response(params, output)
        elif method == "input.load_session":
            self._handle_load_session(params, msg_id, output)
        else:
            self._send_error(output, msg_id, -32601, f"Unknown method: {method}")

    def _handle_init(self, params: dict, msg_id, output: TextIO):
        from app.agent.factory import create_backend
        from app.agent.core import AgentCore
        from app.plugins.bootstrap import build_full_registry

        provider = params.get("provider") or None
        auto_approve = params.get("auto_approve", False)

        backend = create_backend(provider=provider)
        tools, _, _ = build_full_registry(enable_mcp=True)
        agent = AgentCore(backend=backend, tools=tools, auto_approve=auto_approve)

        from app.backend.ui_emitter import UIEmitter
        self.emitter = UIEmitter(agent, auto_approve=auto_approve)

        self._send_result(output, msg_id, {"ok": True})
        self._emit(output, self.emitter.welcome())

    def _handle_input(self, params: dict, output: TextIO):
        if not self.emitter:
            return
        for event in self.emitter.process(params.get("text", "")):
            self._emit(output, event)

    def _handle_command(self, params: dict, output: TextIO):
        if not self.emitter:
            return
        for event in self.emitter.handle_command(params.get("command", "")):
            self._emit(output, event)

    def _handle_prompt_response(self, params: dict, output: TextIO):
        # Store response for UIEmitter to consume
        pass

    def _handle_load_session(self, params: dict, msg_id, output: TextIO):
        if not self.emitter:
            self._send_error(output, msg_id, -32000, "Not initialized")
            return
        try:
            from app.agent.memory import SessionMemory
            memory = SessionMemory()
            memory.load(params["session_id"])
            self.emitter.agent.history = list(memory.get_history())
            self._send_result(output, msg_id, {
                "ok": True,
                "messages": len(self.emitter.agent.history),
            })
        except Exception as e:
            self._send_error(output, msg_id, -32000, str(e))

    def _emit(self, output: TextIO, event: dict):
        output.write(json.dumps(event) + "\n")
        output.flush()

    def _send_result(self, output: TextIO, msg_id, result):
        output.write(json.dumps({
            "jsonrpc": "2.0", "id": msg_id, "result": result,
        }) + "\n")
        output.flush()

    def _send_error(self, output: TextIO, msg_id, code: int, message: str):
        output.write(json.dumps({
            "jsonrpc": "2.0", "id": msg_id,
            "error": {"code": code, "message": message},
        }) + "\n")
        output.flush()

    def run(self):
        """Main loop: read stdin line by line, dispatch, write stdout."""
        for line in sys.stdin:
            line = line.strip()
            if not line:
                continue
            self.handle_message(line, sys.stdout)
