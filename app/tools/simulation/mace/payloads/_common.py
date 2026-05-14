"""Shared helpers for HF Job payload scripts.

Imported by every ``relax.py`` / ``elastic.py`` / etc. payload. Provides:

  - ``read_spec()``  — parse the JSON arg file into a dict
  - ``write_result()`` — write result.json + structure.cif + traj.json
  - ``push_to_dataset()`` — upload artifacts to MACE_MCP_RESULTS_REPO under
                              ``<cache_key>/``
  - ``ensure_mace_core()`` — install mace-mcp (which ships mace_core) into
                              the container's environment if not present.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path
from typing import Any


def read_spec(arg_path: str) -> dict[str, Any]:
    return json.loads(Path(arg_path).read_text())


def out_dir(cache_key: str) -> Path:
    base = Path(os.environ.get("OUT_DIR", f"/tmp/mace-mcp-out/{cache_key}"))
    base.mkdir(parents=True, exist_ok=True)
    return base


def write_result(cache_key: str, result: dict[str, Any]) -> Path:
    d = out_dir(cache_key)
    (d / "result.json").write_text(json.dumps(result, indent=2, default=str))
    return d


def write_cif(cache_key: str, cif_text: str) -> None:
    (out_dir(cache_key) / "structure.cif").write_text(cif_text)


def write_traj(cache_key: str, traj: dict[str, Any]) -> None:
    (out_dir(cache_key) / "traj.json").write_text(json.dumps(traj, default=str))


def push_to_dataset(cache_key: str, repo_id: str) -> str | None:
    token = os.environ.get("HF_TOKEN")
    if not token:
        return None
    from huggingface_hub import HfApi, create_repo

    api = HfApi(token=token)
    create_repo(repo_id, repo_type="dataset", exist_ok=True, token=token)
    d = out_dir(cache_key)
    for fn in d.iterdir():
        if not fn.is_file():
            continue
        api.upload_file(
            path_or_fileobj=str(fn),
            path_in_repo=f"{cache_key}/{fn.name}",
            repo_id=repo_id,
            repo_type="dataset",
            token=token,
        )
    return f"https://huggingface.co/datasets/{repo_id}/tree/main/{cache_key}"


def ensure_mace_core() -> None:
    """Make ``mace_core`` importable inside the container.

    Strategy: try ``import app.tools.simulation.mace.core as mace_core``; on failure, ``pip install`` either
    a dev wheel (URL in ``MACE_MCP_DEV_INSTALL_URL``) or the public PyPI
    release of ``mace-mcp``.
    """
    try:
        import app.tools.simulation.mace.core as mace_core  # noqa: F401
        return
    except ImportError:
        pass
    url = os.environ.get("MACE_MCP_DEV_INSTALL_URL") or "mace-mcp"
    subprocess.check_call([sys.executable, "-m", "pip", "install", "--quiet", url])


def cif_text(atoms) -> str:
    import io
    from ase.io import write as ase_write

    buf = io.StringIO()
    a = atoms.copy()
    a.calc = None
    ase_write(buf, a, format="cif")
    return buf.getvalue()


def build_atoms(input_payload: dict[str, Any], seed: int):
    import numpy as np

    from app.tools.simulation.mace.core.builders import build_c14_laves, build_supercell

    ip = input_payload
    if "composition" in ip:
        comp = ip["composition"]["atoms"]
        phase = ip.get("phase", "bcc")
    elif "matrix_composition" in ip:
        comp = ip["matrix_composition"]["atoms"]
        phase = ip.get("matrix_phase", "bcc")
    elif "structure" in ip:
        s = ip["structure"]
        comp = s["composition"]["atoms"]
        phase = s.get("phase", "bcc")
    else:
        raise ValueError("input has no composition / matrix_composition / structure")

    if phase == "c14_laves":
        sorted_els = sorted(comp.items(), key=lambda kv: -kv[1])
        small = sorted_els[0][0]
        big = sorted_els[1][0] if len(sorted_els) > 1 else "Nb"
        return build_c14_laves(big_atom=big, small_atom=small), comp, phase
    return build_supercell(comp, phase, rng=np.random.default_rng(seed)), comp, phase


def make_calc_for(input_payload: dict[str, Any]):
    from app.tools.simulation.mace.core.calculator import make_calc

    opts = input_payload.get("options", {})
    head = opts.get("head", "omat_pbe")
    dtype = opts.get("dtype", "float64")
    return make_calc(head=head, dtype=dtype)
