"""provenance.json construction + (optional) HF Dataset push.

Every successful job emits one of these. PRISM reads them to verify that
quoted numbers can be reproduced months later from cache_key alone.

The push to HF Dataset is best-effort: failures are logged but never block
the tool result from being returned to the LLM.
"""

from __future__ import annotations

import json
import platform
import socket
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from .. import __version__ as MACE_MCP_VERSION
from ..auth import get_hf_token, get_results_repo, scrub_token
from ..ids import git_dirty, git_sha
from ..logging_cfg import get_logger

log = get_logger("mace_mcp.provenance")


def collect_versions() -> dict[str, str]:
    """Detect installed versions of the physics stack. Best-effort."""
    out: dict[str, str] = {
        "mace_mcp": MACE_MCP_VERSION,
        "python": platform.python_version(),
    }
    for mod in ("numpy", "ase", "torch", "mace", "phonopy", "scipy"):
        try:
            m = __import__(mod)
            ver = getattr(m, "__version__", "unknown")
            out[mod] = ver
        except Exception:
            out[mod] = "absent"
    return out


def collect_host() -> dict[str, str]:
    info: dict[str, str] = {
        "platform": sys.platform,
        "hostname": socket.gethostname(),
        "python_impl": platform.python_implementation(),
    }
    try:
        import torch  # type: ignore

        info["torch_cuda_available"] = str(torch.cuda.is_available())
        if torch.cuda.is_available():
            info["gpu"] = torch.cuda.get_device_name(0)
    except Exception:
        info["torch_cuda_available"] = "false"
    return info


def build(
    *,
    tool_name: str,
    tool_version: str = MACE_MCP_VERSION,
    job_id: str,
    cache_key: str,
    input_payload: dict[str, Any],
    result_summary: dict[str, Any],
    head: str,
    dtype: str,
    backend: str,
    backend_details: dict[str, Any],
    wall_time_s: float,
    quality_flags: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Build the provenance dict (not yet written to disk)."""
    return {
        "tool_name": tool_name,
        "tool_version": tool_version,
        "job_id": job_id,
        "cache_key": cache_key,
        "input": _sanitise(input_payload),
        "result_summary": _sanitise(result_summary),
        "mace_model": {
            "repo_id": "mace-foundations/mace-mh-1",
            "filename": "mace-mh-1.model",
            "head": head,
            "dtype": dtype,
        },
        "versions": collect_versions(),
        "host": collect_host(),
        "git": {
            "mace_mcp_sha": git_sha(),
            "dirty": git_dirty(),
        },
        "backend": backend,
        "backend_details": _sanitise(backend_details),
        "wall_time_s": float(wall_time_s),
        "quality_flags": quality_flags or {},
        "results_dataset": get_results_repo(),
        "created_at_iso8601": datetime.now(timezone.utc).isoformat(),
    }


def _sanitise(o: Any) -> Any:
    """Scrub HF_TOKEN substrings (defense in depth) from every string in a
    nested structure before it ever hits disk."""
    if isinstance(o, str):
        return scrub_token(o)
    if isinstance(o, dict):
        return {k: _sanitise(v) for k, v in o.items()}
    if isinstance(o, (list, tuple)):
        return [_sanitise(v) for v in o]
    return o


def push_to_dataset(
    cache_key: str,
    files: dict[str, Path],
    *,
    repo_id: str | None = None,
) -> str | None:
    """Push provenance + result files to the HF Dataset, keyed by cache_key.

    Returns the dataset URL on success; None if HF_TOKEN or repo is unset.
    """
    repo_id = repo_id or get_results_repo()
    if not repo_id:
        log.info("dataset_push_skipped", reason="no MACE_MCP_RESULTS_REPO")
        return None
    try:
        token = get_hf_token()
    except Exception as ex:
        log.warning("dataset_push_skipped", reason=str(ex))
        return None

    try:
        from huggingface_hub import HfApi, create_repo

        api = HfApi(token=token)
        create_repo(repo_id, repo_type="dataset", exist_ok=True, token=token)
        for kind, local_path in files.items():
            if local_path is None or not Path(local_path).exists():
                continue
            api.upload_file(
                path_or_fileobj=str(local_path),
                path_in_repo=f"{cache_key}/{kind}",
                repo_id=repo_id,
                repo_type="dataset",
                token=token,
            )
        return f"https://huggingface.co/datasets/{repo_id}/tree/main/{cache_key}"
    except Exception as ex:
        log.warning("dataset_push_failed", error=scrub_token(str(ex)))
        return None
