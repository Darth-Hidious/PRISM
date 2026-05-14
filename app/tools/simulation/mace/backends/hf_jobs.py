"""HfJobsBackend — invoke ``hf jobs`` CLI to run MACE on managed GPUs.

Wraps each primitive in a PEP 723 inline-dep script under
``mace_mcp.payloads.*``. The script reads its JSON input from a temp file
(passed via env var), runs the physics from ``mace_core``, writes its
result + CIF + provenance under ``$OUT_DIR``, then uploads to the
``MACE_MCP_RESULTS_REPO`` HF Dataset.

This backend:
  - Never echoes ``HF_TOKEN`` into logs (uses ``--secrets HF_TOKEN`` so the
    HF CLI injects it into the runtime container).
  - Polls ``hf jobs status`` every 5 s until the job terminates.
  - Pulls the artifact directory back from the dataset on success.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Any

from ..auth import get_hf_token, get_results_repo, scrub_token
from ..logging_cfg import get_logger
from .base import Backend, BackendJob, ProgressCb

log = get_logger("mace_mcp.hf_jobs")

# Map tool_name -> module path of the payload script that will run on HF Jobs.
PAYLOAD_MODULES = {
    "relax_structure": "mace_mcp.payloads.relax",
    "compute_elastic": "mace_mcp.payloads.elastic",
    "compute_dilute_solute": "mace_mcp.payloads.dilute",
    "md_equilibrate": "mace_mcp.payloads.md",
    "phonon_harmonic": "mace_mcp.payloads.phonon",
}

# Wall-time estimates per tool (seconds), used to set --timeout on the
# HF Jobs CLI. The caller passes seed + N_atoms in the spec; we widen the
# estimate to give the queue some breathing room.
DEFAULT_TIMEOUTS_S = {
    "relax_structure": 1800,        # 30 min
    "compute_elastic": 3600,        # 1 h
    "compute_dilute_solute": 2700,  # 45 min
    "md_equilibrate": 2700,         # 45 min
    "phonon_harmonic": 5400,        # 90 min
}


class HfJobsBackend(Backend):
    name = "hf_jobs"

    def __init__(self, flavor: str = "l4x1", poll_interval_s: float = 5.0) -> None:
        self.flavor = flavor
        self.poll_interval_s = poll_interval_s
        # Maps job.cache_key -> hf_job_id so we can cancel
        self._active: dict[str, str] = {}

    # ------------------------------------------------------------------
    def cancel(self, job_id: str) -> None:
        # job_id is the cache_key (since we register active jobs under it)
        hf_id = self._active.get(job_id)
        if not hf_id:
            return
        try:
            subprocess.run(
                ["hf", "jobs", "cancel", hf_id],
                check=False,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=30,
            )
        except Exception:
            pass
        finally:
            self._active.pop(job_id, None)

    # ------------------------------------------------------------------
    def execute(self, job: BackendJob, progress: ProgressCb | None = None) -> dict[str, Any]:
        if job.tool_name not in PAYLOAD_MODULES:
            raise ValueError(f"HfJobsBackend does not implement {job.tool_name!r}")
        token = get_hf_token()  # ensures token exists; never logged

        # Workspace for inputs / outputs.
        with tempfile.TemporaryDirectory(prefix="mace-mcp-hf-") as tmp:
            tmpd = Path(tmp)
            (tmpd / "input.json").write_text(
                json.dumps(
                    {
                        "tool_name": job.tool_name,
                        "input_payload": job.input_payload,
                        "cache_key": job.cache_key,
                        "seed": job.seed,
                        "results_repo": get_results_repo(),
                    },
                    indent=2,
                )
            )

            payload_path = self._materialise_payload(job.tool_name, tmpd)
            timeout_s = int(min(job.timeout_seconds or 0, DEFAULT_TIMEOUTS_S.get(job.tool_name, 3600)))
            if timeout_s <= 0:
                timeout_s = DEFAULT_TIMEOUTS_S[job.tool_name]

            # Launch
            cmd = [
                "hf",
                "jobs",
                "uv",
                "run",
                "--flavor",
                self.flavor,
                "--timeout",
                f"{timeout_s}s",
                "--secrets",
                "HF_TOKEN",
                "--detach",
                str(payload_path),
                str((tmpd / "input.json").resolve()),
            ]
            log.info("hf_jobs_launch", tool=job.tool_name, flavor=self.flavor, timeout_s=timeout_s)
            try:
                out = subprocess.run(
                    cmd, capture_output=True, text=True, check=True, timeout=60
                )
            except subprocess.CalledProcessError as ex:
                raise RuntimeError(
                    f"hf jobs launch failed: {scrub_token(ex.stderr or ex.stdout or '')}"
                ) from None

            hf_job_id = _parse_job_id(out.stdout or "")
            if not hf_job_id:
                raise RuntimeError(f"could not parse hf job id from: {out.stdout!r}")
            self._active[job.cache_key] = hf_job_id

            if progress is not None:
                progress(1.0, f"hf job {hf_job_id} submitted", 0, 100)

            # Poll
            try:
                status = self._poll(hf_job_id, job, progress)
            finally:
                self._active.pop(job.cache_key, None)

            if status != "completed":
                logs = _fetch_logs(hf_job_id)
                raise RuntimeError(
                    f"hf job {hf_job_id} ended with status {status}; "
                    f"tail: {scrub_token(logs[-2000:] if logs else '')}"
                )

            # Pull artifacts
            results_repo = get_results_repo()
            if not results_repo:
                raise RuntimeError("MACE_MCP_RESULTS_REPO not set; cannot fetch artifacts")
            artifacts = _pull_artifacts(results_repo, job.cache_key, token)

            result = json.loads((artifacts / "result.json").read_text())
            result.setdefault("backend_details", {})
            result["backend_details"].update(
                {
                    "backend": "hf_jobs",
                    "hf_job_id": hf_job_id,
                    "hf_job_url": f"https://huggingface.co/jobs/{hf_job_id}",
                    "flavor": self.flavor,
                }
            )
            if (artifacts / "structure.cif").exists():
                result["cif_text"] = (artifacts / "structure.cif").read_text()
            if (artifacts / "traj.json").exists():
                result["traj_json"] = json.loads((artifacts / "traj.json").read_text())
            return result

    # ------------------------------------------------------------------
    def _materialise_payload(self, tool: str, tmpd: Path) -> Path:
        """Copy the payload script for ``tool`` into ``tmpd`` so the HF CLI
        can find it as a plain file (PEP 723 inline-deps headers live in it).
        """
        from importlib.resources import files

        mod = PAYLOAD_MODULES[tool]
        src = files(mod.rsplit(".", 1)[0]).joinpath(mod.rsplit(".", 1)[1] + ".py")
        dest = tmpd / f"{tool}.py"
        dest.write_text(src.read_text())
        return dest

    # ------------------------------------------------------------------
    def _poll(self, hf_job_id: str, job: BackendJob, progress: ProgressCb | None) -> str:
        last_status = ""
        steps_seen = 0
        deadline = time.time() + DEFAULT_TIMEOUTS_S.get(job.tool_name, 3600) + 300
        while time.time() < deadline:
            try:
                r = subprocess.run(
                    ["hf", "jobs", "status", hf_job_id],
                    capture_output=True,
                    text=True,
                    check=False,
                    timeout=30,
                )
                status = _parse_status(r.stdout or "")
            except Exception:
                status = "unknown"
            if status != last_status:
                last_status = status
                if progress is not None:
                    progress(
                        _status_to_pct(status),
                        f"hf job {hf_job_id} {status}",
                        steps_seen,
                        100,
                    )
            if status in {"completed", "failed", "cancelled", "error"}:
                return "completed" if status == "completed" else status
            time.sleep(self.poll_interval_s)
        return "timeout"


def _parse_job_id(stdout: str) -> str:
    """Extract a job id from ``hf jobs run --detach`` output.

    The CLI prints a URL like ``https://huggingface.co/jobs/<id>`` on the
    last non-empty line. Be defensive about minor format changes.
    """
    for ln in reversed([l.strip() for l in stdout.splitlines() if l.strip()]):
        if "jobs/" in ln:
            return ln.split("jobs/")[-1].split()[0].rstrip("/")
        if ln.startswith("job ") and len(ln.split()) >= 2:
            return ln.split()[1]
    return ""


def _parse_status(stdout: str) -> str:
    """Best-effort parse of ``hf jobs status`` output.

    The output is small; we look for one of the known status words.
    """
    s = stdout.lower()
    for word in ("running", "completed", "failed", "cancelled", "error", "queued", "pending"):
        if word in s:
            return word
    return "unknown"


def _status_to_pct(status: str) -> float:
    return {"queued": 2.0, "pending": 2.0, "running": 50.0, "completed": 99.0}.get(status, 5.0)


def _fetch_logs(hf_job_id: str) -> str:
    try:
        r = subprocess.run(
            ["hf", "jobs", "logs", hf_job_id, "--tail", "200"],
            capture_output=True,
            text=True,
            check=False,
            timeout=30,
        )
        return r.stdout or ""
    except Exception:
        return ""


def _pull_artifacts(repo_id: str, cache_key: str, token: str) -> Path:
    """Download all files under ``<cache_key>/`` from the HF Dataset."""
    from huggingface_hub import HfApi, hf_hub_download

    api = HfApi(token=token)
    tmpd = Path(tempfile.mkdtemp(prefix="mace-mcp-art-"))
    files = [
        f for f in api.list_repo_files(repo_id, repo_type="dataset", token=token)
        if f.startswith(f"{cache_key}/")
    ]
    if not files:
        raise RuntimeError(f"no artifacts found under {cache_key}/ in {repo_id}")
    for fn in files:
        local = hf_hub_download(
            repo_id=repo_id,
            filename=fn,
            repo_type="dataset",
            token=token,
            local_dir=str(tmpd),
        )
        # local is the full local path; ensure dir structure
        _ = local
    # Files now sit under tmpd/<cache_key>/...
    return tmpd / cache_key


def cleanup_artifacts(p: Path) -> None:
    """Best-effort cleanup of a pulled-artifact directory. Public for tests."""
    try:
        shutil.rmtree(p, ignore_errors=True)
    except Exception:
        pass
