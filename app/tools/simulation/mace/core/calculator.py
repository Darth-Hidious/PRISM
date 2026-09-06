"""MACE foundation-MLIP calculator factory.

Loads the foundation MLIP from the Hugging Face Hub and returns an ASE-style
calculator.

LICENSING — why the default is MACE-MP-0 and not MACE-MH-1
----------------------------------------------------------
MACE *code* is MIT, but the *weights* are not uniformly licensed. Per upstream
(ACEsuit/mace), only ``MACE-MP-0a`` and ``MACE-MP-0b3`` are MIT. ``MACE-MH-0``,
``MACE-MH-1``, ``MACE-MPA-0``, ``OMAT-0``, ``MATPES-*``, ``OMOL-0`` and
``OFF23`` are released under the ASL, which states its grant is "for academic
non-commercial use only" and excludes work intended to lead to "the enhancement
of a product or service in or proposed for commerce". It is also reciprocal.

This platform bills for predictions, so MACE-MH-1 as the *default* put ASL
weights behind a commercial endpoint. The default is therefore the MIT
``mace-mp-0b3-medium``. MH-1 remains available to anyone who has actually
obtained a commercial licence from the authors — assert it explicitly with
``MACE_ACCEPT_ASL_LICENSE=1`` — rather than being deleted, because the
multi-head coverage is genuinely better and a licensee is entitled to it.

Both the repo and the filename are overridable (``MACE_MODEL_REPO`` /
``MACE_MODEL_FILE``) so a deployment can point at its own weights without a
code change.

HEADS
-----
Multi-head selection is an MH-1 feature. MACE-MP-0 is single-head, so asking
for a non-default head while on MP-0 is an error, not a silent fallback — a
head that is quietly ignored returns confident numbers from the wrong level of
theory, which is worse than no answer.

MH-1 heads:
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

import contextlib
import os
import threading
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

# MIT weights — safe to serve commercially.
MIT_REPO_ID = "mace-foundations/mace-mp-0"
MIT_FILENAME = "mace-mp-0b3-medium.model"

# ASL weights — academic non-commercial only unless you hold a commercial
# licence from the authors. Reachable, never default.
ASL_REPO_ID = "mace-foundations/mace-mh-1"
ASL_FILENAME = "mace-mh-1.model"

MODEL_REPO_ID = MIT_REPO_ID
MODEL_FILENAME = MIT_FILENAME


def _asl_accepted() -> bool:
    """Whether the operator asserts a commercial licence for the ASL weights.

    Deliberately an explicit opt-in rather than a "use MH-1 if available"
    heuristic: the question is not whether the file downloads, it is whether
    this deployment is permitted to sell results computed from it.
    """
    return os.environ.get("MACE_ACCEPT_ASL_LICENSE", "").strip().lower() in {
        "1",
        "true",
        "yes",
    }


def resolve_model() -> tuple[str, str, str]:
    """Return ``(repo_id, filename, licence)`` for this deployment.

    Explicit ``MACE_MODEL_REPO``/``MACE_MODEL_FILE`` win, so an operator with
    their own weights is never forced through this decision at all.
    """
    repo = os.environ.get("MACE_MODEL_REPO", "").strip()
    filename = os.environ.get("MACE_MODEL_FILE", "").strip()
    if repo and filename:
        return repo, filename, "operator-supplied"
    if _asl_accepted():
        return ASL_REPO_ID, ASL_FILENAME, "ASL"
    return MIT_REPO_ID, MIT_FILENAME, "MIT"


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
    # Validate BEFORE the heavy lazy imports. A licensing/config error should
    # not require torch to be installed to surface, and putting it first is
    # what makes it testable without the ML stack.
    repo_id, filename, licence = resolve_model()

    # Refuse rather than silently ignore. `mace_mp` accepts a `head` argument
    # against a single-head model and gives you the only head it has — so a
    # caller asking for r²SCAN would receive PBE numbers labelled r²SCAN.
    if licence != "ASL" and head != DEFAULT_HEAD:
        raise ValueError(
            f"head={head!r} is a MACE-MH-1 feature, and this deployment is "
            f"running {repo_id}/{filename} ({licence}), which is single-head. "
            "Ignoring the head would return numbers from the wrong level of "
            "theory. Either use the default head, or set "
            "MACE_ACCEPT_ASL_LICENSE=1 if you hold a commercial licence for "
            "the ASL weights (they are academic non-commercial by default)."
        )

    from huggingface_hub import hf_hub_download

    import torch

    if device is None:
        device = "cuda" if torch.cuda.is_available() else "cpu"

    if device == "mps" and dtype == "float64":
        raise ValueError("MPS does not support float64; use float32 or cuda/cpu.")

    path = hf_hub_download(repo_id=repo_id, filename=filename)
    return guard_forward(serialized_load(_construct, path, dtype, device, head))


# torch.fx patches `torch.nn.Module.__call__` **globally** while it traces, and
# loading a MACE model deserialises a traced GraphModule. So a load in one
# thread does not merely race other loads — it corrupts any forward pass
# running in another thread, which surfaces far from the cause as
# "CURRENT_PATCHER is None in finally block" (2026-09-05) or
# "NameError: module is not installed as a submodule" raised inside
# torch.fx's module_call_wrapper during an ordinary MD force evaluation
# (2026-09-06, four jobs, three hours after loads alone were serialised).
#
# The gate below is therefore a readers-writer lock: a load is an exclusive
# writer, every forward pass is a shared reader. Inference stays parallel,
# which matters because MD is thousands of forward passes; only the rare load
# stops the world.
class ModelGate:
    """Excludes forward passes while a model is being loaded."""

    def __init__(self) -> None:
        self._cv = threading.Condition()
        self._readers = 0
        self._loading = False

    def readers(self) -> int:
        with self._cv:
            return self._readers

    def is_loading(self) -> bool:
        with self._cv:
            return self._loading

    @contextlib.contextmanager
    def loading(self):
        """Exclusive: no other load and no forward pass may run."""
        with self._cv:
            while self._loading or self._readers:
                self._cv.wait()
            self._loading = True
        try:
            yield
        finally:
            with self._cv:
                self._loading = False
                self._cv.notify_all()

    @contextlib.contextmanager
    def running(self):
        """Shared: many forward passes may run, but never during a load."""
        with self._cv:
            while self._loading:
                self._cv.wait()
            self._readers += 1
        try:
            yield
        finally:
            with self._cv:
                self._readers -= 1
                self._cv.notify_all()


MODEL_GATE = ModelGate()
# Kept as the load side of the gate under its old name.
LOAD_LOCK = MODEL_GATE


def serialized_load(construct, *args):
    """Run `construct(*args)` with no other load and no forward pass in flight."""
    with MODEL_GATE.loading():
        return construct(*args)


def guard_forward(calculator):
    """Wrap a calculator so every evaluation takes the shared side of the gate.
    Without this the gate protects nothing: the corruption happens in the
    forward pass, not in the load."""
    inner = calculator.calculate

    def calculate(*args, **kwargs):
        with MODEL_GATE.running():
            return inner(*args, **kwargs)

    calculator.calculate = calculate
    return calculator


def _construct(path: str, dtype: str, device: str, head: str):
    from mace.calculators import mace_mp

    return mace_mp(model=path, default_dtype=dtype, device=device, head=head)


def calc_signature(head: Head, device: str, dtype: str) -> dict[str, str]:
    """Compact dict describing a calculator config — embedded in provenance.

    Reports the RESOLVED weights, not the module defaults: provenance that
    names a model the run did not use is worse than none, and which weights
    produced a number is now also a licensing fact.
    """
    repo_id, filename, licence = resolve_model()
    return {
        "repo_id": repo_id,
        "filename": filename,
        "license": licence,
        "head": head,
        "device": device,
        "dtype": dtype,
    }
