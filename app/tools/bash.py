"""Bash execution tool with conservative guardrails for local repo work."""

from __future__ import annotations

import atexit
import os
import re
import shlex
import signal
import subprocess
import threading
import time
from pathlib import Path
from typing import Any, Iterable, Sequence
from uuid import uuid4

from app.tools.base import Tool, ToolRegistry


_ALLOWED_BASE = Path.cwd().resolve()
_MAX_TIMEOUT = 300
_TASK_TAIL_BYTES = 16_000
_TOKEN_PUNCTUATION = "|&;<>"
_SEGMENT_OPERATORS = {"&&", "||", ";", "|"}
_SPECIAL_DEVICE_PATHS = {"-", "/dev/null", "/dev/stdin", "/dev/stdout", "/dev/stderr"}
_DISALLOWED_COMMANDS = {
    "bash",
    "sh",
    "zsh",
    "fish",
    "source",
    ".",
    "eval",
    "exec",
    "sudo",
    "doas",
    "pkexec",
    "nohup",
    "screen",
    "tmux",
    "launchctl",
    "open",
    "osascript",
}
_NETWORK_COMMANDS = {
    "curl",
    "wget",
    "aria2c",
    "ssh",
    "scp",
    "sftp",
    "rsync",
    "nc",
    "ncat",
    "socat",
    "telnet",
    "ftp",
}
_READ_ONLY_GIT_SUBCOMMANDS = {
    "",
    "status",
    "diff",
    "log",
    "show",
    "rev-parse",
    "grep",
    "ls-files",
    "blame",
    "cat-file",
    "describe",
    "merge-base",
    "symbolic-ref",
}
_BLOCKED_PATTERNS: list[tuple[re.Pattern[str], str]] = [
    (re.compile(r"`"), "Backticks are not supported in execute_bash."),
    (re.compile(r"\$\("), "Command substitution is not supported in execute_bash."),
    (re.compile(r"<\("), "Process substitution is not supported in execute_bash."),
    (re.compile(r">\("), "Process substitution is not supported in execute_bash."),
    (re.compile(r"<<<?"), "Heredocs and herestrings are not supported in execute_bash."),
]
_PATH_COMMANDS = {
    "awk",
    "cat",
    "cd",
    "cp",
    "diff",
    "file",
    "find",
    "git",
    "grep",
    "head",
    "jq",
    "ln",
    "ls",
    "mkdir",
    "mv",
    "nl",
    "od",
    "readlink",
    "rg",
    "rm",
    "rmdir",
    "sed",
    "sort",
    "stat",
    "tail",
    "tee",
    "test",
    "touch",
    "uniq",
    "wc",
    "[",
}
_BASH_TASKS: dict[str, dict[str, Any]] = {}
_BASH_TASKS_LOCK = threading.Lock()


def _blocked_result(message: str) -> dict:
    return {
        "success": False,
        "exit_code": 126,
        "stdout": "",
        "stderr": message,
        "error": message,
    }


def _bash_tasks_dir() -> Path:
    path = _ALLOWED_BASE / ".prism" / "bash_tasks"
    path.mkdir(parents=True, exist_ok=True)
    return path


def _bash_task_paths(task_id: str) -> tuple[Path, Path]:
    base = _bash_tasks_dir()
    return base / f"{task_id}.stdout.log", base / f"{task_id}.stderr.log"


def _read_tail(path: Path, max_bytes: int = _TASK_TAIL_BYTES) -> str:
    if not path.exists():
        return ""

    with path.open("rb") as handle:
        handle.seek(0, os.SEEK_END)
        size = handle.tell()
        handle.seek(max(size - max_bytes, 0), os.SEEK_SET)
        data = handle.read()

    return data.decode("utf-8", errors="replace")


def _terminate_process(process: subprocess.Popen) -> None:
    if process.poll() is not None:
        return

    if os.name != "nt":
        os.killpg(process.pid, signal.SIGTERM)
    else:
        process.kill()


def _refresh_bash_task(task: dict[str, Any]) -> None:
    process = task.get("process")
    if process is None:
        return

    exit_code = process.poll()
    if exit_code is None:
        return

    task["process"] = None
    task["exit_code"] = exit_code
    if task.get("status") == "running":
        task["status"] = "completed" if exit_code == 0 else "failed"
    task["ended_at"] = task.get("ended_at") or time.time()


def _serialize_bash_task(task: dict[str, Any], include_output: bool = False) -> dict:
    _refresh_bash_task(task)
    stdout_path = Path(task["stdout_path"])
    stderr_path = Path(task["stderr_path"])
    data = {
        "task_id": task["task_id"],
        "status": task["status"],
        "command": task["command"],
        "description": task["description"],
        "cwd": task["cwd"],
        "created_at": task["created_at"],
        "started_at": task["started_at"],
        "ended_at": task.get("ended_at"),
        "exit_code": task.get("exit_code"),
        "timed_out": bool(task.get("timed_out")),
        "stdout_path": str(stdout_path),
        "stderr_path": str(stderr_path),
        "stdout_size": stdout_path.stat().st_size if stdout_path.exists() else 0,
        "stderr_size": stderr_path.stat().st_size if stderr_path.exists() else 0,
    }
    if task.get("process") is not None:
        data["pid"] = task["process"].pid

    if include_output:
        data["stdout_tail"] = _read_tail(stdout_path)
        data["stderr_tail"] = _read_tail(stderr_path)

    return data


def _watch_bash_task_timeout(task_id: str, timeout: int) -> None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        with _BASH_TASKS_LOCK:
            task = _BASH_TASKS.get(task_id)
            if task is None:
                return
            _refresh_bash_task(task)
            process = task.get("process")
            if process is None:
                return
        time.sleep(0.25)

    with _BASH_TASKS_LOCK:
        task = _BASH_TASKS.get(task_id)
        if task is None:
            return
        _refresh_bash_task(task)
        process = task.get("process")
        if process is None:
            return

    try:
        _terminate_process(process)
        process.wait(timeout=5)
    except Exception:
        pass

    with _BASH_TASKS_LOCK:
        task = _BASH_TASKS.get(task_id)
        if task is None:
            return
        task["process"] = None
        task["status"] = "timed_out"
        task["timed_out"] = True
        task["exit_code"] = 124
        task["ended_at"] = time.time()


def _spawn_background_bash(command: str, description: str = "", timeout: int | None = None) -> dict:
    shell = _resolve_shell()
    env = {**os.environ}
    task_id = uuid4().hex[:12]
    stdout_path, stderr_path = _bash_task_paths(task_id)

    stdout_handle = stdout_path.open("w", encoding="utf-8")
    stderr_handle = stderr_path.open("w", encoding="utf-8")
    try:
        process = subprocess.Popen(
            [shell, "-lc", command],
            stdout=stdout_handle,
            stderr=stderr_handle,
            text=True,
            encoding="utf-8",
            errors="replace",
            cwd=str(_ALLOWED_BASE),
            env=env,
            preexec_fn=os.setsid if os.name != "nt" else None,
        )
    finally:
        stdout_handle.close()
        stderr_handle.close()

    task = {
        "task_id": task_id,
        "command": command,
        "description": description,
        "cwd": str(_ALLOWED_BASE),
        "created_at": time.time(),
        "started_at": time.time(),
        "ended_at": None,
        "exit_code": None,
        "timed_out": False,
        "status": "running",
        "stdout_path": str(stdout_path),
        "stderr_path": str(stderr_path),
        "process": process,
    }
    with _BASH_TASKS_LOCK:
        _BASH_TASKS[task_id] = task

    # Background commands are intentionally long-lived. A timeout only applies
    # when the caller opts in explicitly, so the default background flow keeps
    # running until completion or an explicit stop request.
    if timeout is not None and timeout > 0:
        watcher = threading.Thread(
            target=_watch_bash_task_timeout,
            args=(task_id, timeout),
            daemon=True,
        )
        watcher.start()

    return {
        "success": True,
        "backgrounded": True,
        "task": _serialize_bash_task(task, include_output=False),
    }


def _list_bash_tasks() -> dict:
    with _BASH_TASKS_LOCK:
        tasks = sorted(
            (_serialize_bash_task(task) for task in _BASH_TASKS.values()),
            key=lambda item: item["created_at"],
            reverse=True,
        )
    return {"tasks": tasks, "count": len(tasks)}


def _read_bash_task(task_id: str) -> dict:
    with _BASH_TASKS_LOCK:
        task = _BASH_TASKS.get(task_id)
        if task is None:
            return {"success": False, "error": f"Unknown bash task: {task_id}"}
        return {"success": True, "task": _serialize_bash_task(task, include_output=True)}


def _stop_bash_task(task_id: str) -> dict:
    with _BASH_TASKS_LOCK:
        task = _BASH_TASKS.get(task_id)
        if task is None:
            return {"success": False, "error": f"Unknown bash task: {task_id}"}
        _refresh_bash_task(task)
        process = task.get("process")
        if process is None:
            return {"success": True, "task": _serialize_bash_task(task, include_output=True)}

    try:
        _terminate_process(process)
        process.wait(timeout=5)
    except Exception as exc:
        return {"success": False, "error": str(exc)}

    with _BASH_TASKS_LOCK:
        task = _BASH_TASKS.get(task_id)
        if task is None:
            return {"success": False, "error": f"Unknown bash task: {task_id}"}
        task["process"] = None
        task["status"] = "stopped"
        task["exit_code"] = process.returncode
        task["ended_at"] = time.time()
        return {"success": True, "task": _serialize_bash_task(task, include_output=True)}


def _cleanup_bash_tasks() -> None:
    with _BASH_TASKS_LOCK:
        running = [task for task in _BASH_TASKS.values() if task.get("process") is not None]

    for task in running:
        process = task.get("process")
        if process is None:
            continue
        try:
            _terminate_process(process)
            process.wait(timeout=1)
        except Exception:
            pass


atexit.register(_cleanup_bash_tasks)


def _tokenize(command: str) -> list[str]:
    lexer = shlex.shlex(command, posix=True, punctuation_chars=_TOKEN_PUNCTUATION)
    lexer.whitespace_split = True
    lexer.commenters = ""
    return list(lexer)


def _split_segments(tokens: Sequence[str]) -> list[list[str]]:
    segments: list[list[str]] = []
    current: list[str] = []
    for token in tokens:
        if token in _SEGMENT_OPERATORS:
            if current:
                segments.append(current)
                current = []
            continue
        current.append(token)
    if current:
        segments.append(current)
    return segments


def _strip_env_assignments(tokens: Sequence[str]) -> list[str]:
    idx = 0
    while idx < len(tokens) and re.match(r"^[A-Za-z_][A-Za-z0-9_]*=.*$", tokens[idx]):
        idx += 1
    return list(tokens[idx:])


def _primary_command(command: str) -> str:
    try:
        segments = _split_segments(_tokenize(command))
    except ValueError:
        return ""
    for segment in reversed(segments):
        stripped = _strip_env_assignments(segment)
        if stripped:
            return stripped[0]
    return ""


def _is_safe_path(path_str: str) -> bool:
    if path_str in _SPECIAL_DEVICE_PATHS:
        return True
    if "://" in path_str:
        return False
    if re.match(r"^[^/\s]+@[^/\s]+:.+$", path_str):
        return False
    try:
        candidate = Path(path_str).expanduser()
        resolved = candidate.resolve() if candidate.is_absolute() else (_ALLOWED_BASE / candidate).resolve()
        return resolved == _ALLOWED_BASE or _ALLOWED_BASE in resolved.parents
    except (OSError, RuntimeError, ValueError):
        return False


def _ensure_safe_path(path_str: str, context: str) -> str | None:
    if not _is_safe_path(path_str):
        return f"{context} path must stay within {_ALLOWED_BASE}: {path_str}"
    return None


def _collect_positionals(args: Sequence[str], flags_with_values: Iterable[str] = ()) -> list[str]:
    needs_value = set(flags_with_values)
    positionals: list[str] = []
    after_double_dash = False
    skip_next = False
    for token in args:
        if skip_next:
            skip_next = False
            continue
        if after_double_dash:
            positionals.append(token)
            continue
        if token == "--":
            after_double_dash = True
            continue
        if token.startswith("-") and token != "-":
            flag = token.split("=", 1)[0]
            if flag in needs_value and "=" not in token:
                skip_next = True
            continue
        positionals.append(token)
    return positionals


def _grep_like_paths(command: str, args: Sequence[str]) -> list[str]:
    flag_values = {
        "-A",
        "-B",
        "-C",
        "-e",
        "-f",
        "-g",
        "-m",
        "--after-context",
        "--before-context",
        "--context",
        "--regexp",
        "--file",
        "--glob",
        "--max-count",
        "--max-depth",
        "--type",
        "-t",
    }
    paths: list[str] = []
    after_double_dash = False
    skip_next = False
    pattern_seen = False
    for token in args:
        if skip_next:
            skip_next = False
            continue
        if token == "--" and not after_double_dash:
            after_double_dash = True
            continue
        if not after_double_dash and token.startswith("-") and token != "-":
            flag = token.split("=", 1)[0]
            if flag in {"-e", "--regexp", "-f", "--file"}:
                pattern_seen = True
            if flag in flag_values and "=" not in token:
                skip_next = True
            continue
        if not pattern_seen:
            pattern_seen = True
            continue
        paths.append(token)
    if not paths and command == "find":
        return ["."]
    return paths


def _find_paths(args: Sequence[str]) -> list[str]:
    paths: list[str] = []
    after_double_dash = False
    for token in args:
        if token == "--" and not after_double_dash:
            after_double_dash = True
            continue
        if not after_double_dash and (
            token.startswith("-")
            or token in {"!", "(", ")"}
        ):
            break
        paths.append(token)
    return paths or ["."]


def _sed_paths(args: Sequence[str]) -> list[str] | str:
    paths: list[str] = []
    skip_next = False
    expression_seen = False
    for token in args:
        if skip_next:
            skip_next = False
            continue
        if token in {"-i", "--in-place"} or token.startswith("-i"):
            return "In-place sed edits are not supported in execute_bash yet."
        if token in {"-e", "--expression", "-f", "--file"}:
            skip_next = True
            expression_seen = True
            continue
        if token.startswith("-") and token != "-":
            continue
        if not expression_seen:
            expression_seen = True
            continue
        paths.append(token)
    return paths


def _awk_paths(args: Sequence[str]) -> list[str]:
    paths: list[str] = []
    skip_next = False
    program_seen = False
    for token in args:
        if skip_next:
            skip_next = False
            continue
        if token in {"-F", "-v"}:
            skip_next = True
            continue
        if token in {"-f"}:
            skip_next = True
            continue
        if token.startswith("-") and token != "-":
            continue
        if not program_seen:
            program_seen = True
            continue
        paths.append(token)
    return paths


def _git_error(args: Sequence[str]) -> str | None:
    if not args:
        return None
    for flag in args:
        if flag in {"-C", "--git-dir", "--work-tree", "-c", "--config-env"}:
            return f"git flag {flag} is not supported in execute_bash."
        if not flag.startswith("-"):
            break
    subcommand = next((token for token in args if not token.startswith("-")), "")
    if subcommand not in _READ_ONLY_GIT_SUBCOMMANDS:
        return f"git subcommand '{subcommand}' is not supported in execute_bash yet."
    if "--" in args:
        dd_index = args.index("--")
        for path_str in args[dd_index + 1:]:
            error = _ensure_safe_path(path_str, "Git path")
            if error:
                return error
    return None


def _validate_redirections(tokens: Sequence[str]) -> str | None:
    idx = 0
    while idx < len(tokens):
        token = tokens[idx]
        if token in {"<", ">", ">>"}:
            if idx + 1 >= len(tokens):
                return "Redirection is missing a target path."
            direction = "Input" if token == "<" else "Output"
            error = _ensure_safe_path(tokens[idx + 1], direction)
            if error:
                return error
            idx += 2
            continue
        if token == ">&":
            if idx + 1 >= len(tokens):
                return "Redirection is missing a target."
            if not tokens[idx + 1].isdigit():
                error = _ensure_safe_path(tokens[idx + 1], "Output")
                if error:
                    return error
            idx += 2
            continue
        idx += 1
    return None


def _validate_paths(command: str, args: Sequence[str]) -> str | None:
    if command == "cd":
        if not args:
            return "cd without an explicit project path is not supported."
        return _ensure_safe_path(args[0], "cd")
    if command in {"grep", "rg"}:
        candidates = _grep_like_paths(command, args)
    elif command == "find":
        candidates = _find_paths(args)
    elif command == "sed":
        candidates = _sed_paths(args)
        if isinstance(candidates, str):
            return candidates
    elif command == "awk":
        candidates = _awk_paths(args)
    elif command == "git":
        return _git_error(args)
    elif command in {"head", "tail"}:
        candidates = _collect_positionals(args, {"-n", "-c", "--lines", "--bytes"})
    elif command == "tee":
        candidates = _collect_positionals(args, {"-a"})
    elif command in {"mkdir", "touch", "rm", "rmdir", "cp", "mv", "ln", "test", "["}:
        candidates = _collect_positionals(args)
    else:
        candidates = _collect_positionals(args)

    for path_str in candidates:
        error = _ensure_safe_path(path_str, f"{command} argument")
        if error:
            return error
    return None


def _validate_command(command: str) -> str | None:
    stripped = command.strip()
    if not stripped:
        return "Command must not be empty."
    for pattern, message in _BLOCKED_PATTERNS:
        if pattern.search(stripped):
            return message
    try:
        tokens = _tokenize(stripped)
    except ValueError as exc:
        return f"Unable to parse shell command: {exc}"
    if any(token == "&" for token in tokens):
        return "Background commands are not supported in execute_bash yet."
    for segment in _split_segments(tokens):
        command_tokens = _strip_env_assignments(segment)
        if not command_tokens:
            continue
        base_command = command_tokens[0]
        if base_command in _DISALLOWED_COMMANDS:
            return f"Command '{base_command}' is not supported in execute_bash."
        if base_command in _NETWORK_COMMANDS:
            return f"Network command '{base_command}' is not supported in execute_bash."
        if base_command in {"python", "python3"} and "-c" in command_tokens[1:]:
            return "Inline Python execution belongs in execute_python, not execute_bash."
        if base_command == "node" and any(flag in command_tokens[1:] for flag in {"-e", "--eval"}):
            return "Inline Node.js evaluation is not supported in execute_bash."
        error = _validate_redirections(command_tokens)
        if error:
            return error
        if base_command in _PATH_COMMANDS:
            error = _validate_paths(base_command, command_tokens[1:])
            if error:
                return error
    return None


def _interpret_exit_code(command: str, exit_code: int) -> tuple[bool, str | None]:
    base_command = _primary_command(command)
    if base_command in {"grep", "rg"}:
        return exit_code < 2, "No matches found" if exit_code == 1 else None
    if base_command == "find":
        return exit_code < 2, "Some directories were inaccessible" if exit_code == 1 else None
    if base_command == "diff":
        return exit_code < 2, "Files differ" if exit_code == 1 else None
    if base_command in {"test", "["}:
        return exit_code < 2, "Condition is false" if exit_code == 1 else None
    return exit_code == 0, None


def _resolve_shell() -> str:
    bash = Path("/bin/bash")
    if bash.exists():
        return str(bash)
    return os.environ.get("SHELL", "/bin/sh")


def _execute_bash(
    command: str,
    timeout: int | None = None,
    description: str = "",
    run_in_background: bool = False,
) -> dict:
    validation_error = _validate_command(command)
    if validation_error:
        return _blocked_result(validation_error)

    if timeout is None:
        timeout = 60
    timeout = max(1, min(timeout, _MAX_TIMEOUT))
    if run_in_background:
        # Keep the existing 60s default for foreground calls, but let
        # background tasks run until completion unless the caller explicitly
        # requests a runtime cap.
        background_timeout = None if timeout == 60 else timeout
        return _spawn_background_bash(
            command,
            description=description,
            timeout=background_timeout,
        )

    shell = _resolve_shell()
    env = {**os.environ}

    try:
        process = subprocess.Popen(
            [shell, "-lc", command],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            errors="replace",
            cwd=str(_ALLOWED_BASE),
            env=env,
            preexec_fn=os.setsid if os.name != "nt" else None,
        )
        try:
            stdout, stderr = process.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            if os.name != "nt":
                os.killpg(process.pid, signal.SIGTERM)
            else:
                process.kill()
            stdout, stderr = process.communicate()
            return {
                "success": False,
                "exit_code": 124,
                "stdout": stdout,
                "stderr": stderr,
                "error": f"Timed out after {timeout}s",
                "timed_out": True,
            }
    except Exception as exc:
        return {"success": False, "error": str(exc)}

    success, interpretation = _interpret_exit_code(command, process.returncode)
    return {
        "success": success,
        "exit_code": process.returncode,
        "stdout": stdout,
        "stderr": stderr,
        "return_code_interpretation": interpretation,
        "description": description,
        "cwd": str(_ALLOWED_BASE),
    }


def create_bash_tools(registry: ToolRegistry) -> None:
    """Register local bash execution tools."""
    # These descriptions are intentionally procedural. Models do not infer the
    # bash task lifecycle on their own, so the schema needs to spell out when
    # to launch in the background and which follow-up tools to call next.
    registry.register(Tool(
        name="execute_bash",
        description=(
            "Execute a local bash command inside the current PRISM project. "
            "Best for repository inspection, search, git read-only commands, "
            "build/test commands, and other local CLI workflows. "
            "Set run_in_background=true for long-running commands that should keep "
            "running after this tool returns. When a background launch succeeds, "
            "PRISM returns a task_id. Use list_bash_tasks if you need to "
            "rediscover task IDs, read_bash_task to inspect progress and tail "
            "stdout/stderr, and stop_bash_task to cancel a running task. "
            "Commands that require privilege escalation, networking, shell "
            "nesting, or paths outside the project are blocked."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": (
                        "Bash command to execute in the project directory. "
                        "Prefer a single explicit command rather than shell wrappers."
                    ),
                },
                "timeout": {
                    "type": "integer",
                    "description": (
                        f"Timeout in seconds (foreground default 60, max {_MAX_TIMEOUT}). "
                        "For background tasks, omit this to let the command run until completion. "
                        "Set this only when the task should be force-stopped after a known limit."
                    ),
                },
                "description": {
                    "type": "string",
                    "description": (
                        "Short explanation of what the command does. "
                        "Use this when the command itself is terse or non-obvious."
                    ),
                },
                "run_in_background": {
                    "type": "boolean",
                    "description": (
                        "Run the command as a session-local background task. "
                        "When true, PRISM returns a task_id and the command keeps "
                        "running until completion or stop_bash_task. After launch, "
                        "use read_bash_task to tail logs or list_bash_tasks if you "
                        "need to recover the task_id later."
                    ),
                },
            },
            "required": ["command"],
        },
        func=_execute_bash,
        requires_approval=True,
    ))
    registry.register(Tool(
        name="list_bash_tasks",
        description=(
            "List session-local background bash tasks created by execute_bash "
            "with run_in_background=true. Use this when you need to rediscover "
            "task IDs before calling read_bash_task or stop_bash_task."
        ),
        input_schema={
            "type": "object",
            "properties": {},
        },
        func=_list_bash_tasks,
    ))
    registry.register(Tool(
        name="read_bash_task",
        description=(
            "Read the latest stdout and stderr from a session-local background "
            "bash task. Call this repeatedly to tail progress until the task "
            "status becomes completed, failed, timed_out, or stopped."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": (
                        "Background bash task ID returned by execute_bash or "
                        "rediscovered via list_bash_tasks."
                    ),
                },
            },
            "required": ["task_id"],
        },
        func=_read_bash_task,
    ))
    registry.register(Tool(
        name="stop_bash_task",
        description=(
            "Stop a session-local background bash task started by execute_bash. "
            "Call read_bash_task afterwards if you need the final stdout, stderr, "
            "status, or exit_code."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": (
                        "Background bash task ID returned by execute_bash or "
                        "rediscovered via list_bash_tasks."
                    ),
                },
            },
            "required": ["task_id"],
        },
        func=_stop_bash_task,
        requires_approval=True,
    ))
