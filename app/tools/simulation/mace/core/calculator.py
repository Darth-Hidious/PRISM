"""MACE-MH-1 calculator factory.

Loads the foundation MLIP from the Hugging Face Hub and returns an ASE-style
calculator. Supports all heads shipped with mace-foundations/mace-mh-1.

Heads (as of MACE-MH-1 v1):
  - omat_pbe       (default)   — bulk PBE
  - matpes_r2scan              — r²SCAN
  - oc20_usemppbe              — Open Catalyst 2020 (PBE+U)
  - omol                       — organic molecules
  - spice_wB97M                — small mols, hybrid DFT
  - rgd1_b3lyp                 — radicals, B3LYP

This module imports ``mace`` lazily so unit tests can run without mace-torch
installed (the FakeBackend never touches it).
"""

from __future__ import annotations

from typing import Literal

Head = Literal[
    "omat_pbe",
    "matpes_r2scan",
    "oc20_usemppbe",
    "omol",
    "spice_wB97M",
    "rgd1_b3lyp",
]

HEADS: tuple[Head, ...] = (
    "omat_pbe",
    "matpes_r2scan",
    "oc20_usemppbe",
    "omol",
    "spice_wB97M",
    "rgd1_b3lyp",
)

DEFAULT_HEAD: Head = "omat_pbe"
DEFAULT_DTYPE = "float64"  # CUDA supports float64; MPS does not.
MODEL_REPO_ID = "mace-foundations/mace-mh-1"
MODEL_FILENAME = "mace-mh-1.model"


def make_calc(
    head: Head = DEFAULT_HEAD,
    device: str | None = None,
    dtype: str = DEFAULT_DTYPE,
):
    """Build the mace-mh-1 calculator.

    Imports of mace-torch / torch are lazy so this module can be parsed
    (and the rest of mace_core can be imported) without those dependencies
    actually being installed at import time. The dependencies are only
    needed if you actually call this function.

    Parameters
    ----------
    head : Head
        Foundation-MLIP head selector. Changes both physics and chemistry
        domain. Default ``omat_pbe`` is the bulk-PBE head.
    device : str | None
        ``"cuda"``, ``"cpu"``, ``"mps"``. Auto-detected if None: cuda > cpu.
        MPS is never auto-selected because float64 is unsupported on it.
    dtype : str
        ``"float32"`` or ``"float64"``. float64 only on cuda or cpu.
    """
    from huggingface_hub import hf_hub_download
    from mace.calculators import mace_mp

    import torch

    if device is None:
        device = "cuda" if torch.cuda.is_available() else "cpu"

    if device == "mps" and dtype == "float64":
        raise ValueError("MPS does not support float64; use float32 or cuda/cpu.")

    path = hf_hub_download(repo_id=MODEL_REPO_ID, filename=MODEL_FILENAME)
    return mace_mp(model=path, default_dtype=dtype, device=device, head=head)


def calc_signature(head: Head, device: str, dtype: str) -> dict[str, str]:
    """Compact dict describing a calculator config — embedded in provenance."""
    return {
        "repo_id": MODEL_REPO_ID,
        "filename": MODEL_FILENAME,
        "head": head,
        "device": device,
        "dtype": dtype,
    }
