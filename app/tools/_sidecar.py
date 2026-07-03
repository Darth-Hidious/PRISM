# Copyright (c) 2025-2026 MARC27. Licensed under MIT License.
"""Science sidecar — a second venv for deps the main Python can't install.

The main PRISM venv rides the system Python (3.14 today); the scientific
stack (pyiron_atomistics, pycalphad) caps out at 3.12. Instead of asking
the user to juggle interpreters, PRISM provisions `~/.prism/venv-sci` on
Python 3.12 automatically and proxies the affected tools into a sidecar
process (`app.sidecar_server`, same JSON-line protocol as the main tool
server). One agent-visible catalog, two interpreters, zero user setup.

Contract: everything here returns {"error": ...} dicts instead of raising —
these paths run inside tool calls.
"""
from __future__ import annotations

import json
import shutil
import subprocess
import sys
import threading
from pathlib import Path
from typing import Any, Optional

SIDECAR_VENV = Path.home() / ".prism" / "venv-sci"
# Newest first; 3.13 excluded on purpose — the point is escaping >=3.13 caps.
_PYTHON_CANDIDATES = ["python3.12", "python3.11"]

# Packages the sidecar needs. Kept here (not per-tool) so provisioning is
# one pip run; both are small enough to install together.
SIDECAR_PACKAGES = ["pyiron_atomistics>=0.5,<0.6", "pycalphad"]

_PROVISION_TIMEOUT_SECS = 900
_CALL_TIMEOUT_SECS = 600


def _sidecar_python() -> Path:
    return SIDECAR_VENV / "bin" / "python3"


def find_base_python() -> Optional[str]:
    for cand in _PYTHON_CANDIDATES:
        path = shutil.which(cand)
        if path:
            return path
    return None


def ensure_sidecar(install: bool = True) -> Optional[str]:
    """Make sure the sidecar venv exists with its packages.

    Returns None when ready, else a human-readable error string.
    Idempotent; safe to call on every proxied tool call (fast path is one
    marker-file stat).
    """
    marker = SIDECAR_VENV / ".provisioned"
    if marker.exists():
        return None
    if not install:
        return "science sidecar venv not provisioned — run `prism pyiron install`"

    base = find_base_python()
    if base is None:
        return (
            "no Python 3.12/3.11 found for the science sidecar — "
            "install one (e.g. `brew install python@3.12`) and retry"
        )
    try:
        if not _sidecar_python().exists():
            subprocess.run(
                [base, "-m", "venv", str(SIDECAR_VENV)],
                check=True,
                capture_output=True,
                timeout=120,
            )
        result = subprocess.run(
            [str(_sidecar_python()), "-m", "pip", "install", *SIDECAR_PACKAGES],
            capture_output=True,
            timeout=_PROVISION_TIMEOUT_SECS,
        )
        if result.returncode != 0:
            tail = result.stderr.decode(errors="replace")[-400:]
            return f"sidecar pip install failed: {tail}"
        marker.write_text("\n".join(SIDECAR_PACKAGES) + "\n")
        return None
    except subprocess.TimeoutExpired:
        return "sidecar provisioning timed out — retry, or install manually"
    except Exception as exc:
        return f"sidecar provisioning failed: {exc}"


class _SidecarProcess:
    """Lazy, lock-guarded handle on the sidecar server process."""

    def __init__(self) -> None:
        self._proc: Optional[subprocess.Popen] = None
        self._lock = threading.Lock()

    def _spawn(self) -> Optional[str]:
        repo_root = Path(__file__).resolve().parents[2]
        try:
            self._proc = subprocess.Popen(
                [str(_sidecar_python()), "-m", "app.sidecar_server"],
                cwd=str(repo_root),
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                text=True,
            )
            return None
        except Exception as exc:
            self._proc = None
            return f"failed to start science sidecar: {exc}"

    def call(self, tool: str, args: dict) -> dict[str, Any]:
        with self._lock:
            err = ensure_sidecar()
            if err:
                return {"error": err}
            if self._proc is None or self._proc.poll() is not None:
                err = self._spawn()
                if err:
                    return {"error": err}
            assert self._proc and self._proc.stdin and self._proc.stdout
            try:
                request = {"method": "call_tool", "tool": tool, "args": args}
                self._proc.stdin.write(json.dumps(request) + "\n")
                self._proc.stdin.flush()
                # One request in flight at a time (lock held) — the reply is
                # the next line. Reader thread with timeout guards a hang.
                line: list[str] = []

                def _read() -> None:
                    line.append(self._proc.stdout.readline())  # type: ignore[union-attr]

                reader = threading.Thread(target=_read, daemon=True)
                reader.start()
                reader.join(timeout=_CALL_TIMEOUT_SECS)
                if reader.is_alive() or not line or not line[0]:
                    self._proc.kill()
                    self._proc = None
                    return {"error": f"science sidecar timed out on {tool}"}
                response = json.loads(line[0])
            except Exception as exc:
                self._proc = None
                return {"error": f"science sidecar call failed: {exc}"}
        if "result" in response:
            return response["result"]
        return response  # already an {"error": ...} shape


_PROCESS = _SidecarProcess()


def call_tool(tool: str, args: dict) -> dict[str, Any]:
    """Run a tool inside the science sidecar. Never raises."""
    result = _PROCESS.call(tool, args)
    if isinstance(result, dict):
        return result
    return {"result": result}


def print_status() -> dict[str, Any]:
    """Status summary for CLI / diagnostics."""
    ready = (SIDECAR_VENV / ".provisioned").exists()
    return {
        "venv": str(SIDECAR_VENV),
        "provisioned": ready,
        "base_python": find_base_python(),
        "packages": SIDECAR_PACKAGES,
    }
