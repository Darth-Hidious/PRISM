# Copyright (c) 2025-2026 Mirdyne. Licensed under MIT License.
"""Science sidecar — a second venv for deps the main Python can't install.

The main PRISM venv rides the system Python (3.14 today); the scientific
stack (pyiron_atomistics, pycalphad) caps out at 3.12. Instead of asking
the user to juggle interpreters, PRISM provisions `~/.prism/venv-sci` on
Python 3.12 automatically and proxies the affected tools into a sidecar
process (`app.sidecar_server`, same JSON-line protocol as the main tool
server). One agent-visible catalog, two interpreters, zero user setup.

Contract: everything here returns {"error": ...} dicts instead of raising —
these paths run inside tool calls.

Every child here goes through `app.tools.spawn`, never bare subprocess: these
run in the tool-server process, which by then has almost certainly served a
materials search, and any fork() after that SIGSEGVs. See app/tools/spawn.py.
"""
from __future__ import annotations

import collections
import json
import os
import shutil
import subprocess
import sys
import threading
from pathlib import Path
from typing import Any, Optional

from app.tools import spawn

SIDECAR_VENV = Path.home() / ".prism" / "venv-sci"
# Newest first; 3.13 excluded on purpose — the point is escaping >=3.13 caps.
_PYTHON_CANDIDATES = ["python3.12", "python3.11"]

# Packages the sidecar needs, grouped by CAPABILITY.
#
# They used to be one list installed in one run, which meant the two
# capabilities shared a fate they do not share in reality: on macOS arm64,
# pyiron_atomistics 0.5.x pins mpi4py<=3.1.6, which ships no wheel for this
# platform and needs an MPI compiler to build. Bundled, that unsatisfiable
# requirement took CALPHAD down with it — and CALPHAD alone resolves to 32
# packages in 102ms. So a capability that CAN be installed now is, and one that
# cannot says exactly why without silencing the other.
SIDECAR_CAPABILITIES: "dict[str, list[str]]" = {
    "calphad": ["pycalphad"],
    "pyiron": ["pyiron_atomistics>=0.5,<0.6"],
}

# Flat view, kept for callers that only want to know what the sidecar is for.
SIDECAR_PACKAGES = [pkg for pkgs in SIDECAR_CAPABILITIES.values() for pkg in pkgs]

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


def ensure_sidecar(
    install: bool = True, capability: Optional[str] = None
) -> Optional[str]:
    """Make sure the sidecar venv exists with its packages.

    Returns None when ready, else a human-readable error string.
    Idempotent; safe to call on every proxied tool call (fast path is one
    marker-file stat).
    """
    marker = SIDECAR_VENV / ".provisioned"
    if marker.exists():
        if capability is None:
            return None
        # The marker lists what actually installed, so a partially provisioned
        # venv must not answer "ready" for a capability it does not have. That
        # is precisely how a half-installed sidecar passes for a whole one and
        # the caller discovers the truth as an ImportError deep inside a tool.
        have = {line.strip() for line in marker.read_text().splitlines() if line.strip()}
        wanted = SIDECAR_CAPABILITIES.get(capability, [])
        if all(pkg in have for pkg in wanted):
            return None
        return (
            f"science sidecar has no {capability} support: "
            f"{', '.join(wanted) or capability} did not install. "
            "Run `prism pyiron install` to retry, and see the recorded reason."
        )
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
            spawn.run(
                [base, "-m", "venv", str(SIDECAR_VENV)],
                check=True,
                capture_output=True,
                timeout=120,
            )
        # Resolve with uv when it is on PATH, else pip.
        #
        # Not a preference: pip CANNOT install this set. pyiron_atomistics and
        # pycalphad together defeat its resolver outright —
        # "resolution-too-deep: Dependency resolution exceeded maximum depth" —
        # and pip's own hint (add lower bounds) does not help; measured, it
        # still fails with `pycalphad>=0.10`. uv resolves the same two
        # requirements to 146 packages in 25ms. So on a machine with only pip
        # the science sidecar has simply never been installable, which is why
        # `structure` reported a sidecar failure on an ordinary run.
        uv = shutil.which("uv")
        if uv:
            pip_command = [
                uv,
                "pip",
                "install",
                "--python",
                str(_sidecar_python()),
            ]
        else:
            pip_command = [str(_sidecar_python()), "-m", "pip", "install"]
        wheelhouse = os.environ.get(
            "PRISM_WHEELHOUSE", str(Path.home() / ".prism" / "wheelhouse")
        )
        if os.environ.get("PRISM_OFFLINE") == "1":
            if not Path(wheelhouse).is_dir():
                return (
                    "offline mode: science sidecar is not provisioned and no "
                    f"wheelhouse exists at {wheelhouse}; pre-stage it with "
                    "`prism provision wheels` on a connected machine"
                )
            pip_command.extend(["--no-index", "--find-links", wheelhouse])
        resolver = "uv" if uv else "pip"
        installed: "list[str]" = []
        failures: "list[str]" = []
        for capability, packages in SIDECAR_CAPABILITIES.items():
            result = spawn.run(
                pip_command + packages,
                capture_output=True,
                timeout=_PROVISION_TIMEOUT_SECS,
            )
            if result.returncode == 0:
                installed.extend(packages)
                continue
            tail = result.stderr.decode(errors="replace")[-300:]
            failures.append(f"{capability}: {tail}")

        # Record what is ACTUALLY present, not what was asked for. A marker
        # listing packages that failed to install is how a half-provisioned
        # venv passes for a complete one.
        marker.write_text("\n".join(installed) + "\n")

        if not failures:
            return None
        if installed:
            # Partial success is not failure: the capabilities that installed
            # are usable now, and saying so is the difference between "CALPHAD
            # works, pyiron does not" and "the sidecar is broken".
            return (
                f"sidecar partially provisioned ({resolver}): "
                + f"{len(installed)} package(s) installed; "
                + "; ".join(failures)
            )
        hint = ""
        if not uv:
            hint = (
                " — pip cannot resolve this dependency set at all; install "
                "uv (https://docs.astral.sh/uv/) and retry"
            )
        return f"sidecar install failed ({resolver}){hint}: " + "; ".join(failures)
    except subprocess.TimeoutExpired:
        return "sidecar provisioning timed out — retry, or install manually"
    except Exception as exc:
        return f"sidecar provisioning failed: {exc}"


class _SidecarProcess:
    """Lazy, lock-guarded handle on the sidecar server process."""

    def __init__(self) -> None:
        self._proc: Optional[subprocess.Popen] = None
        self._lock = threading.Lock()
        # Last lines the sidecar wrote to stderr. A sidecar that crashes on
        # its first request used to be reported as "timed out" because its
        # stderr went to DEVNULL; the traceback is the diagnosis.
        self._stderr_tail: collections.deque[str] = collections.deque(maxlen=40)

    def _spawn(self) -> Optional[str]:
        repo_root = Path(__file__).resolve().parents[2]
        try:
            self._proc = spawn.popen(
                [str(_sidecar_python()), "-m", "app.sidecar_server"],
                cwd=str(repo_root),
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            self._stderr_tail.clear()
            threading.Thread(
                target=self._drain_stderr, args=(self._proc,), daemon=True
            ).start()
            return None
        except Exception as exc:
            self._proc = None
            return f"failed to start science sidecar: {exc}"

    def _drain_stderr(self, proc: subprocess.Popen) -> None:
        assert proc.stderr
        for line in proc.stderr:
            self._stderr_tail.append(line.rstrip("\n"))

    def _failure(self, tool: str) -> str:
        """What actually happened: an exit with its code and stderr, or a hang."""
        assert self._proc
        code = self._proc.poll()
        tail = "\n".join(self._stderr_tail).strip()
        if code is not None:
            head = f"science sidecar exited (code {code}) during {tool}"
        else:
            head = f"science sidecar timed out on {tool}"
        return f"{head}: {tail}" if tail else head

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
                    # EOF on stdout means the process is gone (or going): give
                    # it a moment to finish dying so the exit code is known.
                    if not reader.is_alive():
                        try:
                            self._proc.wait(timeout=1)
                        except subprocess.TimeoutExpired:
                            pass
                    message = self._failure(tool)
                    self._proc.kill()
                    self._proc = None
                    return {"error": message}
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
