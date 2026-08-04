"""Agent-triggered provisioning for optional science dependencies."""
from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path
from typing import Any

from app.tools import spawn

SCIENCE_EXTRAS = frozenset(
    {"qe", "calphad", "mace", "precipitation", "lpbf", "simulation", "ml"}
)


def provision_extra(extra: str) -> dict[str, Any]:
    """Install one supported extra through the Rust CLI harness.

    The Rust path owns validation, wheelhouse selection, and the offline
    ``--no-index`` policy. This helper keeps Python tool failures actionable:
    it returns structured output instead of asking the model to tell a human
    to copy a pip command into another shell.
    """
    extra = extra.strip().lower()
    if extra not in SCIENCE_EXTRAS:
        return {
            "error": f"unsupported PRISM extra: {extra}",
            "supported_extras": sorted(SCIENCE_EXTRAS),
        }

    binary = os.environ.get("PRISM_BINARY")
    project_root = os.environ.get("PRISM_PROJECT_ROOT") or str(
        Path(__file__).resolve().parents[2]
    )
    wheelhouse = os.environ.get("PRISM_WHEELHOUSE")
    if not binary:
        return {
            "error": "PRISM provisioning harness is unavailable in this worker",
            "provision_command": f"prism provision extra {extra}",
        }

    command = [binary, "--python", sys.executable, "--project-root", project_root]
    command.extend(["provision", "extra", extra])
    if wheelhouse:
        command.extend(["--wheelhouse", wheelhouse])
    try:
        result = spawn.run(
            command,
            cwd=project_root,
            capture_output=True,
            text=True,
            timeout=900,
            check=False,
        )
    except subprocess.TimeoutExpired:
        return {
            "error": f"provisioning extra [{extra}] timed out after 900 seconds",
            "provision_command": " ".join(command),
        }
    except OSError as exc:
        return {
            "error": f"could not start PRISM provisioning harness: {exc}",
            "provision_command": " ".join(command),
        }

    if result.returncode != 0:
        return {
            "error": f"provisioning extra [{extra}] failed with exit code {result.returncode}",
            "provision_command": " ".join(command),
            "stderr": result.stderr[-2000:],
        }
    return {
        "provisioned": True,
        "extra": extra,
        "stdout": result.stdout[-2000:],
        "provision_command": " ".join(command),
    }
