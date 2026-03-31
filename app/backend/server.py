"""JSON-RPC 2.0 server over stdio -- thin wrapper around UIEmitter.

Usage: python3 -m app.backend
The Ink frontend spawns this as a child process and communicates
via stdin (JSON lines) / stdout (JSON lines).

Approvals: When the agent needs tool approval, the generator thread blocks
on _approval_queue. The main loop reads stdin for input.prompt_response
messages and unblocks the generator by putting the response on the queue.
"""
import json
import queue
import select
import sys
import threading
from typing import TextIO

from app.backend.protocol import parse_input


class StdioServer:
    """JSON-RPC server on stdin/stdout with mid-stream approval support."""

    def __init__(self):
        self.emitter = None
        self._approval_queue: queue.Queue = queue.Queue()
        self._event_queue: queue.Queue = queue.Queue()

    def _approval_callback(self, tool_name: str, tool_args: dict) -> bool:
        """Called from the generator thread. Blocks until frontend responds."""
        return self._approval_queue.get()

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
        elif method == "input.model_select":
            self._handle_model_select(params, output)
        else:
            self._send_error(output, msg_id, -32601, f"Unknown method: {method}")

    def _handle_init(self, params: dict, msg_id, output: TextIO):
        import time
        import logging

        logger = logging.getLogger(__name__)
        t0 = time.monotonic()

        try:
            from app.agent.factory import create_backend
            from app.agent.core import AgentCore
            from app.plugins.bootstrap import build_full_registry

            provider = params.get("provider") or None
            auto_approve = params.get("auto_approve", False)

            t1 = time.monotonic()
            backend = create_backend(provider=provider)
            logger.debug("create_backend: %.1fms", (time.monotonic() - t1) * 1000)

            t1 = time.monotonic()
            tools, _, _ = build_full_registry(enable_mcp=True)
            logger.debug("build_full_registry: %.1fms (%d tools)", (time.monotonic() - t1) * 1000, len(tools.list_tools()))

            agent = AgentCore(
                backend=backend, tools=tools,
                auto_approve=auto_approve,
                approval_callback=self._approval_callback,
            )

            from app.backend.ui_emitter import UIEmitter
            self.emitter = UIEmitter(agent, auto_approve=auto_approve)

            logger.info("Init complete: %.1fms", (time.monotonic() - t0) * 1000)
            self._send_result(output, msg_id, {"ok": True})
            self._emit(output, self.emitter.welcome())

        except ValueError as exc:
            logger.error("Init config error: %s", exc)
            self._send_error(output, msg_id, -32000, f"Configuration error: {exc}")
        except ImportError as exc:
            logger.error("Init import error: %s", exc)
            self._send_error(output, msg_id, -32000, f"Missing dependency: {exc}")
        except Exception as exc:
            logger.exception("Init failed")
            self._send_error(output, msg_id, -32000, f"Initialization failed: {exc}")

    def _handle_input(self, params: dict, output: TextIO):
        """Run the UIEmitter generator in a thread, drain events while checking stdin."""
        if not self.emitter:
            return

        # Clear queues from any previous run
        while not self._event_queue.empty():
            try:
                self._event_queue.get_nowait()
            except queue.Empty:
                break
        while not self._approval_queue.empty():
            try:
                self._approval_queue.get_nowait()
            except queue.Empty:
                break

        text = params.get("text", "")

        def run_generator():
            try:
                for event in self.emitter.process(text):
                    self._event_queue.put(event)
            except Exception:
                pass
            self._event_queue.put(None)  # done sentinel

        thread = threading.Thread(target=run_generator, daemon=True)
        thread.start()

        self._drain_events_with_stdin(output)

    def _drain_events_with_stdin(self, output: TextIO):
        """Drain event queue while also reading stdin for mid-stream messages.

        This allows the frontend to send approval responses while the
        generator is blocked waiting for one.
        """
        while True:
            # Drain all available events
            try:
                while True:
                    event = self._event_queue.get_nowait()
                    if event is None:
                        return
                    self._emit(output, event)
            except queue.Empty:
                pass

            # Check stdin for approval responses (10ms timeout)
            try:
                readable, _, _ = select.select([sys.stdin], [], [], 0.01)
            except (ValueError, OSError):
                # stdin closed
                return

            if readable:
                line = sys.stdin.readline()
                if not line:
                    return  # EOF
                line = line.strip()
                if not line:
                    continue
                try:
                    msg = json.loads(line)
                    if msg.get("method") == "input.prompt_response":
                        self._handle_prompt_response(msg.get("params", {}), output)
                except (json.JSONDecodeError, KeyError):
                    pass

    def _handle_command(self, params: dict, output: TextIO):
        if not self.emitter:
            return
        for event in self.emitter.handle_command(params.get("command", "")):
            self._emit(output, event)

    def _handle_prompt_response(self, params: dict, output: TextIO):
        """Unblock the approval callback with the frontend's response."""
        response = params.get("response", "n")
        approved = response in ("y", "yes", "a")
        if response == "a" and self.emitter:
            self.emitter.auto_approve = True
            self.emitter.agent.auto_approve = True
        self._approval_queue.put(approved)

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

    def _handle_model_select(self, params: dict, output: TextIO):
        if not self.emitter:
            return
        model_id = params.get("model_id", "")
        if model_id:
            success = self.emitter._switch_model(model_id)
            if success:
                self._emit(output, {
                    "jsonrpc": "2.0",
                    "method": "ui.card",
                    "params": {
                        "card_type": "info", "tool_name": "", "elapsed_ms": 0,
                        "content": f"Switched to **{model_id}**",
                        "data": {},
                    },
                })

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
