#!/usr/bin/env python3
"""Process-level smoke for the existing PRISM coding-agent harness.

The primary mode talks to a real OpenAI-compatible provider, for example a
local llama.cpp `llama-server` serving Gemma 4 12B. The fake provider mode is
kept as a deterministic regression check for CI and approval prompting.

The smoke uses the existing stack:

1. `prism backend` JSON-RPC server.
2. Rust agent loop and OpenAI-compatible LLM client.
3. Real Python `app.tool_server`.
4. Real `file` tool execution.
5. Recursive model loop: model -> tool -> model -> final answer.
"""

from __future__ import annotations

import argparse
import json
import os
import queue
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any, Callable


FILE_TOOL = "file"
FILE_ARGS = {"action": "read", "path": "README.md"}
FAKE_FINAL_MARKER = "SMOKE_OK tool result returned to model"
LIVE_FINAL_MARKER = "GEMMA4_SMOKE_OK"
CONTEXT_MARKER = "CONTEXT_OK README.md"
APPROVAL_MARKER = "APPROVAL_DENIED_OK"


class FakeOpenAIProvider:
    def __init__(self, scenario: str) -> None:
        self.scenario = scenario
        self.requests: list[dict[str, Any]] = []
        provider = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, fmt: str, *args: Any) -> None:
                return

            def do_POST(self) -> None:
                length = int(self.headers.get("content-length", "0"))
                body = self.rfile.read(length)
                try:
                    payload = json.loads(body.decode("utf-8") or "{}")
                except json.JSONDecodeError:
                    payload = {}
                provider.requests.append(payload)

                messages = payload.get("messages", [])
                has_tool_result = any(
                    isinstance(message, dict) and message.get("role") == "tool"
                    for message in messages
                )

                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Cache-Control", "no-cache")
                self.send_header("Connection", "close")
                self.end_headers()

                if has_tool_result:
                    marker = (
                        APPROVAL_MARKER
                        if provider.scenario == "approval-deny"
                        else FAKE_FINAL_MARKER
                    )
                    chunks = self._final_answer_chunks(marker)
                elif provider.scenario == "approval-deny":
                    chunks = self._tool_call_chunks(
                        "execute_bash",
                        {"command": "printf APPROVAL_SHOULD_NOT_EXECUTE"},
                    )
                else:
                    chunks = self._tool_call_chunks(FILE_TOOL, FILE_ARGS)

                for chunk in chunks:
                    self.wfile.write(("data: " + json.dumps(chunk) + "\n\n").encode())
                    self.wfile.flush()
                self.wfile.write(b"data: [DONE]\n\n")
                self.wfile.flush()

            @staticmethod
            def _tool_call_chunks(tool_name: str, tool_args: dict[str, Any]) -> list[dict[str, Any]]:
                args = json.dumps(tool_args, separators=(",", ":"))
                split_at = max(1, len(args) // 2)
                return [
                    {
                        "choices": [
                            {
                                "delta": {
                                    "tool_calls": [
                                        {
                                            "index": 0,
                                            "id": f"call_{tool_name}",
                                            "type": "function",
                                            "function": {
                                                "name": tool_name,
                                                "arguments": "",
                                            },
                                        }
                                    ]
                                }
                            }
                        ]
                    },
                    {
                        "choices": [
                            {
                                "delta": {
                                    "tool_calls": [
                                        {
                                            "index": 0,
                                            "function": {"arguments": args[:split_at]},
                                        }
                                    ]
                                }
                            }
                        ]
                    },
                    {
                        "choices": [
                            {
                                "delta": {
                                    "tool_calls": [
                                        {
                                            "index": 0,
                                            "function": {"arguments": args[split_at:]},
                                        }
                                    ]
                                }
                            }
                        ],
                        "usage": {
                            "prompt_tokens": 10,
                            "completion_tokens": 5,
                            "total_tokens": 15,
                        },
                    },
                ]

            @staticmethod
            def _final_answer_chunks(marker: str) -> list[dict[str, Any]]:
                return [
                    {
                        "choices": [{"delta": {"content": marker}}],
                        "usage": {
                            "prompt_tokens": 12,
                            "completion_tokens": 7,
                            "total_tokens": 19,
                        },
                    }
                ]

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.port = self.server.server_address[1]
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)

    @property
    def base_url(self) -> str:
        return f"http://127.0.0.1:{self.port}"

    def start(self) -> None:
        self.thread.start()

    def stop(self) -> None:
        self.server.shutdown()
        self.server.server_close()


@dataclass
class BackendRun:
    proc: subprocess.Popen[str]
    stdout: "queue.Queue[str]"
    stderr: "queue.Queue[str]"
    lines: list[str] = field(default_factory=list)
    err_lines: list[str] = field(default_factory=list)

    def send(self, obj: dict[str, Any]) -> None:
        assert self.proc.stdin is not None
        self.proc.stdin.write(json.dumps(obj) + "\n")
        self.proc.stdin.flush()

    def collect_until(
        self,
        predicate: Callable[[str, Any], bool],
        timeout_seconds: float,
    ) -> tuple[list[str], bool]:
        deadline = time.time() + timeout_seconds
        batch: list[str] = []
        while time.time() < deadline:
            self._drain_stderr()
            try:
                line = self.stdout.get(timeout=0.2)
            except queue.Empty:
                if self.proc.poll() is not None:
                    break
                continue
            self.lines.append(line)
            batch.append(line)
            try:
                parsed = json.loads(line)
            except json.JSONDecodeError:
                parsed = None
            if predicate(line, parsed):
                self._drain_stderr()
                return batch, True
        self._drain_stderr()
        return batch, False

    def collect_turn(self, timeout_seconds: float) -> list[str]:
        batch, ok = self.collect_until(
            lambda _line, parsed: method_is(parsed, "ui.turn.complete"),
            timeout_seconds,
        )
        if not ok:
            raise RuntimeError("backend did not emit ui.turn.complete")
        return batch

    def _drain_stderr(self) -> None:
        while True:
            try:
                line = self.stderr.get_nowait()
            except queue.Empty:
                return
            self.err_lines.append(line)

    def stop(self) -> None:
        try:
            if self.proc.stdin is not None:
                self.proc.stdin.close()
        except Exception:
            pass
        try:
            self.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait(timeout=5)
        self._drain_stderr()


def repo_root() -> Path:
    return Path(__file__).resolve().parents[1]


def default_python(repo: Path) -> Path:
    venv_python = repo / ".venv/bin/python"
    if venv_python.exists():
        return venv_python
    return Path(sys.executable)


def default_prism_bin(repo: Path) -> Path:
    exe = "prism.exe" if os.name == "nt" else "prism"
    return repo / "target" / "debug" / exe


def ensure_prism_bin(repo: Path, prism_bin: Path) -> None:
    if prism_bin.exists():
        return
    print(f"[smoke] {prism_bin} missing; building prism-cli", flush=True)
    subprocess.run(["cargo", "build", "-p", "prism-cli"], cwd=repo, check=True)


def redact_env(env: dict[str, str]) -> dict[str, str]:
    redacted = {}
    for key, value in env.items():
        if "KEY" in key or "TOKEN" in key or "SECRET" in key:
            redacted[key] = "<redacted>"
        else:
            redacted[key] = value
    return redacted


def read_stream(stream: Any, output: "queue.Queue[str]") -> None:
    for line in stream:
        output.put(line.rstrip("\n"))


def method_is(parsed: Any, method: str) -> bool:
    return isinstance(parsed, dict) and parsed.get("method") == method


def params_field(parsed: Any, field: str) -> Any:
    if not isinstance(parsed, dict):
        return None
    params = parsed.get("params")
    if not isinstance(params, dict):
        return None
    return params.get(field)


def emitted_text(lines: list[str]) -> str:
    chunks: list[str] = []
    for line in lines:
        try:
            parsed = json.loads(line)
        except json.JSONDecodeError:
            continue
        if method_is(parsed, "ui.text.delta"):
            text = params_field(parsed, "text")
            if isinstance(text, str):
                chunks.append(text)
    return "".join(chunks)


def artifact_stats(db_path: Path) -> dict[str, Any]:
    if not db_path.exists():
        return {"exists": False, "count": 0, "tools": []}
    conn = sqlite3.connect(db_path)
    try:
        count = conn.execute("SELECT COUNT(*) FROM artifacts").fetchone()[0]
        tools = [
            row[0]
            for row in conn.execute(
                "SELECT DISTINCT tool_name FROM artifacts ORDER BY tool_name"
            )
        ]
        return {"exists": True, "count": int(count), "tools": tools}
    finally:
        conn.close()


def request_contains_tool_def(request: dict[str, Any], tool_name: str) -> bool:
    tools = request.get("tools")
    if not isinstance(tools, list):
        return False
    return any(
        isinstance(tool, dict)
        and tool.get("function", {}).get("name") == tool_name
        for tool in tools
    )


def request_contains_tool_result(request: dict[str, Any]) -> bool:
    messages = request.get("messages")
    if not isinstance(messages, list):
        return False
    return any(
        isinstance(message, dict) and message.get("role") == "tool"
        for message in messages
    )


def start_backend(
    *,
    repo: Path,
    prism_bin: Path,
    python_bin: Path,
    base_url: str,
    model: str,
    timeout: float,
    auto_approve: bool,
    enable_memory: bool,
    tmp: Path,
) -> BackendRun:
    home = tmp / "home"
    home.mkdir()
    env = os.environ.copy()
    smoke_env = {
        "HOME": str(home),
        # PRISM resolves its managed venv from HOME, but the smoke isolates HOME
        # to a tempdir — so point the resolver at the real venv explicitly.
        "PRISM_PYTHON": str(python_bin),
        "LLM_BASE_URL": base_url,
        "LLM_MODEL": model,
        "LLM_API_KEY": "redacted-smoke-key",
        "PRISM_ENABLE_MCP": "0",
        "PRISM_ENABLE_PLUGINS": "0",
        "XDG_CONFIG_HOME": str(tmp / "config"),
        "XDG_STATE_HOME": str(tmp / "state"),
        "XDG_CACHE_HOME": str(tmp / "cache"),
        "XDG_DATA_HOME": str(tmp / "data"),
    }
    if enable_memory:
        smoke_env["PRISM_ARTIFACT_DB"] = str(tmp / "artifacts.db")
    else:
        smoke_env["PRISM_DISABLE_MEMORY"] = "1"
    env.update(smoke_env)

    # PRISM resolves its managed venv (~/.prism/venv) automatically; there is
    # no --python flag. The smoke still seeds that venv via PRISM_PYTHON below.
    #
    # LLM routing goes via env only (LLM_BASE_URL / LLM_MODEL / LLM_API_KEY,
    # set in smoke_env above): `prism backend` dropped its --llm-url/--model/
    # --api-key flags, and passing them makes clap exit before ui.welcome.
    cmd = [
        str(prism_bin),
        "--project-root",
        str(repo),
        "backend",
        "--project-root",
        str(repo),
    ]

    print("[smoke] command:", " ".join(cmd))
    print("[smoke] env:", json.dumps(redact_env(smoke_env), sort_keys=True))

    proc = subprocess.Popen(
        cmd,
        cwd=repo,
        env=env,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
    )
    stdout: "queue.Queue[str]" = queue.Queue()
    stderr: "queue.Queue[str]" = queue.Queue()
    assert proc.stdout is not None
    assert proc.stderr is not None
    threading.Thread(target=read_stream, args=(proc.stdout, stdout), daemon=True).start()
    threading.Thread(target=read_stream, args=(proc.stderr, stderr), daemon=True).start()

    backend = BackendRun(proc=proc, stdout=stdout, stderr=stderr)
    backend.send(
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "init",
            "params": {"auto_approve": auto_approve},
        }
    )
    init_lines, ok = backend.collect_until(
        lambda _line, parsed: method_is(parsed, "ui.welcome"),
        timeout,
    )
    if not ok:
        raise RuntimeError("backend did not emit ui.welcome")

    welcome = next(
        json.loads(line)
        for line in init_lines
        if json.loads(line).get("method") == "ui.welcome"
    )
    params = welcome.get("params", {})
    tool_count = params.get("loaded_tool_count", params.get("tool_count", 0))
    if not isinstance(tool_count, int) or tool_count <= 0:
        raise RuntimeError(f"backend reported invalid loaded tool count={tool_count!r}")
    # model_tool_selection is an OPTIONAL welcome field: the backend does
    # not implement a per-request tool cap today (dynamic selection is
    # find_tools-driven). Only validate it when the backend reports it —
    # the old unconditional assert failed every run against a backend
    # that never emitted the field.
    selection = params.get("model_tool_selection")
    if isinstance(selection, dict) and "max_per_request" in selection:
        max_per_request = selection.get("max_per_request")
        if not isinstance(max_per_request, int) or max_per_request <= 0:
            raise RuntimeError(
                f"backend reported invalid model tool selection limit={max_per_request!r}"
            )

    return backend


def assert_file_tool_loop(lines: list[str], *, final_marker: str | None) -> dict[str, bool]:
    text = emitted_text(lines)
    checks = {
        "tool_start_seen": any('"method":"ui.tool.start"' in line and FILE_TOOL in line for line in lines),
        "tool_result_card_seen": any('"method":"ui.card"' in line and FILE_TOOL in line and "README.md" in line for line in lines),
        "approval_prompt_absent_for_read": not any('"method":"ui.prompt"' in line and FILE_TOOL in line for line in lines),
        "artifact_id_seen": any("_artifact_id" in line for line in lines),
    }
    if final_marker is not None:
        checks["final_marker_seen"] = final_marker in text
    if not checks["tool_start_seen"]:
        raise RuntimeError("file tool did not start")
    if not checks["tool_result_card_seen"]:
        raise RuntimeError("file tool result card was not observed")
    if not checks["approval_prompt_absent_for_read"]:
        raise RuntimeError("read-only file tool still prompted for approval")
    if final_marker is not None and not checks.get("final_marker_seen", False):
        raise RuntimeError(f"final marker {final_marker!r} was not observed")
    return checks


def run_fake_smoke(args: argparse.Namespace, scenario: str) -> int:
    repo = repo_root()
    prism_bin = args.prism_bin or default_prism_bin(repo)
    python_bin = args.python or default_python(repo)
    ensure_prism_bin(repo, prism_bin)

    provider = FakeOpenAIProvider(scenario)
    provider.start()
    backend: BackendRun | None = None
    try:
        with tempfile.TemporaryDirectory(prefix="prism-agent-smoke-") as tmpdir:
            backend = start_backend(
                repo=repo,
                prism_bin=prism_bin,
                python_bin=python_bin,
                base_url=provider.base_url,
                model="fake-openai-smoke",
                timeout=args.timeout,
                auto_approve=False,
                enable_memory=False,
                tmp=Path(tmpdir),
            )

            if scenario == "approval-deny":
                backend.send(
                    {
                        "jsonrpc": "2.0",
                        "id": 2,
                        "method": "input.message",
                        "params": {"text": "Ask for a shell command approval."},
                    }
                )
                _prompt_lines, ok = backend.collect_until(
                    lambda _line, parsed: method_is(parsed, "ui.prompt")
                    and params_field(parsed, "tool_name") == "execute_bash",
                    args.timeout,
                )
                if not ok:
                    raise RuntimeError("approval prompt was not emitted")
                backend.send(
                    {
                        "jsonrpc": "2.0",
                        "id": 3,
                        "method": "input.prompt_response",
                        "params": {"response": "n", "tool_name": "execute_bash"},
                    }
                )
                backend.collect_turn(args.timeout)
                checks = {
                    "approval_prompt_seen": any('"method":"ui.prompt"' in line for line in backend.lines),
                    "denial_card_seen": any("denied by user" in line for line in backend.lines),
                    "final_marker_seen": APPROVAL_MARKER in emitted_text(backend.lines),
                    "tool_result_returned_to_provider": any(
                        request_contains_tool_result(request) for request in provider.requests
                    ),
                }
            else:
                backend.send(
                    {
                        "jsonrpc": "2.0",
                        "id": 2,
                        "method": "input.message",
                        "params": {
                            "text": "Use the file tool to read README.md, then report smoke status."
                        },
                    }
                )
                backend.collect_turn(args.timeout)
                checks = assert_file_tool_loop(backend.lines, final_marker=FAKE_FINAL_MARKER)
                checks.update(
                    {
                        "provider_request_count": len(provider.requests),
                        "tool_definition_sent_to_provider": any(
                            request_contains_tool_def(request, FILE_TOOL)
                            for request in provider.requests
                        ),
                        "tool_result_returned_to_provider": any(
                            request_contains_tool_result(request)
                            for request in provider.requests
                        ),
                    }
                )

            print("[smoke] checks:", json.dumps(checks, sort_keys=True))
            print("[smoke] transcript:")
            for line in backend.lines:
                print(line)

            if scenario == "approval-deny":
                required = [
                    checks["approval_prompt_seen"],
                    checks["denial_card_seen"],
                    checks["final_marker_seen"],
                    checks["tool_result_returned_to_provider"],
                ]
            else:
                required = [
                    checks["tool_start_seen"],
                    checks["tool_result_card_seen"],
                    checks["tool_result_returned_to_provider"],
                    checks["tool_definition_sent_to_provider"],
                    checks["approval_prompt_absent_for_read"],
                    checks["final_marker_seen"],
                ]
            if not all(required):
                raise RuntimeError("one or more fake smoke assertions failed")
            print(f"[smoke] PASS: fake provider scenario {scenario}")
            backend.stop()
            backend = None
            return 0
    except Exception as exc:
        print("[smoke] FAILED:", exc, file=sys.stderr)
        if backend is not None:
            dump_backend(backend)
        return 1
    finally:
        if backend is not None:
            backend.stop()
        provider.stop()


def run_live_smoke(args: argparse.Namespace) -> int:
    repo = repo_root()
    prism_bin = args.prism_bin or default_prism_bin(repo)
    python_bin = args.python or default_python(repo)
    ensure_prism_bin(repo, prism_bin)

    backend: BackendRun | None = None
    try:
        with tempfile.TemporaryDirectory(prefix="prism-agent-gemma4-") as tmpdir:
            tmp_path = Path(tmpdir)
            artifact_db = tmp_path / "artifacts.db"
            backend = start_backend(
                repo=repo,
                prism_bin=prism_bin,
                python_bin=python_bin,
                base_url=args.base_url,
                model=args.model,
                timeout=args.timeout,
                auto_approve=False,
                enable_memory=True,
                tmp=tmp_path,
            )
            backend.send(
                {
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "input.message",
                    "params": {
                        "text": (
                            "Use the available file tool exactly once to read README.md. "
                            "Do not answer from memory. After the tool result is returned, "
                            f"reply with exactly: {LIVE_FINAL_MARKER}"
                        )
                    },
                }
            )
            first_turn = backend.collect_turn(args.timeout)
            checks = assert_file_tool_loop(backend.lines, final_marker=LIVE_FINAL_MARKER)
            stats = artifact_stats(artifact_db)
            checks.update(
                {
                    "artifact_db_exists": bool(stats["exists"]),
                    "artifact_row_seen": int(stats["count"]) > 0,
                    "artifact_file_tool_seen": FILE_TOOL in stats["tools"],
                }
            )

            backend.send(
                {
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "input.command",
                    "params": {"command": "/memory"},
                }
            )
            memory_lines, ok = backend.collect_until(
                lambda _line, parsed: method_is(parsed, "ui.view")
                and params_field(parsed, "view_type") == "memory",
                args.timeout,
            )
            if not ok:
                raise RuntimeError("/memory did not return a memory view")
            memory_tail = backend.collect_turn(args.timeout)
            memory_lines.extend(memory_tail)

            backend.send(
                {
                    "jsonrpc": "2.0",
                    "id": 4,
                    "method": "input.command",
                    "params": {"command": "/context"},
                }
            )
            context_lines, ok = backend.collect_until(
                lambda _line, parsed: method_is(parsed, "ui.view")
                and params_field(parsed, "view_type") == "context",
                args.timeout,
            )
            if not ok:
                raise RuntimeError("/context did not return a context view")
            context_tail = backend.collect_turn(args.timeout)
            context_lines.extend(context_tail)

            backend.send(
                {
                    "jsonrpc": "2.0",
                    "id": 5,
                    "method": "input.message",
                    "params": {
                        "text": (
                            "Using only context from this chat, what file did you read "
                            f"in the previous turn? Reply exactly: {CONTEXT_MARKER}"
                        )
                    },
                }
            )
            second_turn = backend.collect_turn(args.timeout)
            checks.update(
                {
                    "memory_view_seen": any("Session memory" in line for line in memory_lines),
                    "memory_mentions_file_tool": any(FILE_TOOL in line for line in memory_lines),
                    "context_view_seen": any("Model-facing API view" in line for line in context_lines),
                    "context_compaction_none": any("none; full visible history is in play" in line for line in context_lines),
                    "context_marker_seen": CONTEXT_MARKER in emitted_text(second_turn),
                    "recursive_model_loop_seen": LIVE_FINAL_MARKER in emitted_text(first_turn),
                }
            )

            print("[smoke] checks:", json.dumps(checks, sort_keys=True))
            print("[smoke] transcript:")
            for line in backend.lines:
                print(line)

            required = [
                checks["tool_start_seen"],
                checks["tool_result_card_seen"],
                checks["approval_prompt_absent_for_read"],
                checks["final_marker_seen"],
                checks["artifact_db_exists"],
                checks["artifact_row_seen"],
                checks["artifact_file_tool_seen"],
                checks["memory_view_seen"],
                checks["memory_mentions_file_tool"],
                checks["context_view_seen"],
                checks["context_marker_seen"],
                checks["recursive_model_loop_seen"],
            ]
            if not all(required):
                raise RuntimeError("one or more live Gemma smoke assertions failed")
            print("[smoke] PASS: live OpenAI-compatible provider completed model/tool/context loop")
            backend.stop()
            backend = None
            return 0
    except Exception as exc:
        print("[smoke] FAILED:", exc, file=sys.stderr)
        if backend is not None:
            dump_backend(backend)
        return 1
    finally:
        if backend is not None:
            backend.stop()


def dump_backend(backend: BackendRun) -> None:
    print("[smoke] stdout:")
    for line in backend.lines:
        print(line)
    if backend.err_lines:
        print("[smoke] stderr:", file=sys.stderr)
        for line in backend.err_lines:
            print(line, file=sys.stderr)


def main() -> int:
    parser = argparse.ArgumentParser(description="Smoke the real PRISM coding-agent harness.")
    parser.add_argument("--provider", choices=["fake", "existing"], default="fake")
    parser.add_argument("--scenario", choices=["normal", "approval-deny"], default="normal")
    parser.add_argument("--base-url", default=os.environ.get("LLM_BASE_URL", "http://127.0.0.1:18081/v1"))
    parser.add_argument("--model", default=os.environ.get("LLM_MODEL", "gemma-4-12b"))
    parser.add_argument("--prism-bin", type=Path, default=None)
    parser.add_argument("--python", type=Path, default=None)
    parser.add_argument("--timeout", type=float, default=240.0)
    args = parser.parse_args()

    repo = repo_root()
    python_bin = args.python or default_python(repo)
    if not python_bin.exists():
        print(f"[smoke] Python worker does not exist: {python_bin}", file=sys.stderr)
        return 2
    if not (repo / "README.md").exists():
        print("[smoke] README.md missing; smoke tool target is absent", file=sys.stderr)
        return 2

    if args.provider == "fake":
        return run_fake_smoke(args, args.scenario)
    if args.scenario != "normal":
        print("[smoke] --scenario approval-deny is only supported with --provider fake", file=sys.stderr)
        return 2
    return run_live_smoke(args)


if __name__ == "__main__":
    raise SystemExit(main())
