"""provenance.json construction + (optional) HF Dataset push.

Every successful job emits one of these. PRISM reads them to verify that
quoted numbers can be reproduced months later from cache_key alone.

The push to HF Dataset is best-effort: failures are logged but never block
the tool result from being returned to the LLM.
"""

from __future__ import annotations

from ..core.calculator import calc_signature as _calc_signature

import json
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from app.tools import _provenance as prov_common

from .. import __version__ as MACE_MCP_VERSION
from ..auth import get_hf_token, get_results_repo, scrub_token
from ..ids import git_dirty, git_sha
from ..logging_cfg import get_logger

log = get_logger("mace_mcp.provenance")

#: MACE result fields carry their unit in the field NAME (schemas.py:
#: ``energy_per_atom_eV``, ``fmax_final_eV_per_A``, ``C_GPa``, ``mean_T_K``,
#: ``phonon_dos_omega_THz``, ``rdf_r_A``). This table decodes those suffixes
#: so `units` describes the keys a result actually has. A static map of
#: invented key names would have looked authoritative while saying nothing
#: about any number present.
_UNIT_SUFFIXES: tuple[tuple[str, str], ...] = (
    ("_eV_per_atom", "eV/atom"),
    ("_eV_per_A", "eV/Angstrom"),
    ("_per_atom_eV", "eV/atom"),
    ("_GPa", "GPa"),
    ("_THz", "THz"),
    ("_A3", "Angstrom^3"),
    ("_eV", "eV"),
    ("_K", "K"),
    ("_A", "Angstrom"),
    ("_s", "s"),
)


def units_for(result_summary: dict[str, Any]) -> dict[str, str]:
    """Unit per key of an actual MACE result, decoded from its field name."""
    out: dict[str, str] = {}
    for key in result_summary:
        for suffix, unit in _UNIT_SUFFIXES:
            if key.endswith(suffix):
                out[key] = unit
                break
        else:
            # Dimensionless (rdf_g, pugh_G_over_B, nu_Poisson) or genuinely
            # not encoded — say which rather than assert a unit.
            out[key] = "dimensionless or not encoded in the field name"
    return out


def collect_versions() -> dict[str, str]:
    """Detect installed versions of the physics stack. Best-effort."""
    out = prov_common.versions_of("numpy", "ase", "torch", "mace", "phonopy", "scipy")
    out["mace_mcp"] = MACE_MCP_VERSION
    return out


def collect_host() -> dict[str, str]:
    info = prov_common.collect_host()
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
    # Resolved, never hardcoded: provenance must name the weights the run
    # actually used. The default moved away from the ASL-licensed MH-1 weights.
    #
    # The `fake` backend loads NO weights: it returns deterministic stub
    # values from a lookup table (backends/fake.py). Signing those numbers
    # with a repo_id / filename / licence attributed the stub to real
    # MIT-licensed MACE weights — the exact failure calc_signature's own
    # docstring forbids ("provenance that names a model the run did not use
    # is worse than none"). Say plainly that nothing was loaded instead.
    if backend == "fake":
        mace_model = {
            "weights": None,
            "weights_absent_reason": (
                "the 'fake' backend loaded no interatomic potential — these "
                "numbers are deterministic stub values from a lookup table, "
                "not a MACE calculation"
            ),
            "head": head,
            "dtype": dtype,
        }
    else:
        mace_model = {
            **{
                key: value
                for key, value in _calc_signature(head, "", dtype).items()
                if key in ("repo_id", "filename", "license")
            },
            "head": head,
            "dtype": dtype,
        }
    versions = collect_versions()
    summary = _sanitise(result_summary)
    return {
        "schema_version": prov_common.PROVENANCE_SCHEMA_VERSION,
        "tool_name": tool_name,
        "tool_version": tool_version,
        "job_id": job_id,
        "cache_key": cache_key,
        "input": _sanitise(input_payload),
        "result_summary": summary,
        "mace_model": mace_model,
        "units": units_for(summary),
        "units_policy": prov_common.UNITS_POLICY,
        "versions": versions,
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
        # PROV-O relations — same three keys every PRISM tool emits
        # (app/tools/_provenance.py), so one reader handles every bundle.
        "wasGeneratedBy": {
            "activity": f"mace.{tool_name}",
            "engine": "mace-torch",
            "engine_version": versions.get("mace", "absent"),
            "backend": backend,
        },
        "wasDerivedFrom": [
            {"role": "interatomic_potential", **mace_model},
            {"role": "input_structure", "cache_key": cache_key},
        ],
        "wasAttributedTo": {
            "agent": "PRISM",
            "agent_type": "SoftwareAgent",
            "prism_version": MACE_MCP_VERSION,
            "host": collect_host().get("hostname", "unknown"),
        },
        "reproduce": (
            f"cache_key={cache_key} "
            f"(mace_get_cached_structure / rerun {tool_name})"
        ),
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
