"""Cell + atomic-position relaxation via FrechetCellFilter + LBFGS."""

from __future__ import annotations

import time
from dataclasses import dataclass
from typing import TYPE_CHECKING, Callable

if TYPE_CHECKING:
    from ase import Atoms


@dataclass
class RelaxResult:
    energy_per_atom_eV: float
    volume_per_atom_A3: float
    lattice_a_eff_A: float
    n_steps: int
    fmax_final_eV_per_A: float
    wall_time_s: float


def relax(
    atoms: "Atoms",
    calc,
    fmax: float = 0.05,
    steps: int = 200,
    progress: Callable[[int, int, float], None] | None = None,
) -> RelaxResult:
    """Relax ``atoms`` in-place.

    Parameters
    ----------
    atoms : ase.Atoms
        The supercell. Will be modified in place. Calculator is set to ``calc``.
    calc : ASE-style calculator
        e.g. the one returned by :func:`mace_core.calculator.make_calc`.
    fmax : float
        Force convergence tolerance in eV/Å. Default 0.05.
    steps : int
        Maximum LBFGS steps. Default 200.
    progress : callable | None
        Optional callback ``progress(step, total_max, fmax_current)`` invoked
        after each LBFGS step. Used by the MCP runner to push
        ``notifications/progress`` messages.

    Returns
    -------
    RelaxResult
    """
    from ase.filters import FrechetCellFilter
    from ase.optimize import LBFGS

    import numpy as np

    atoms.calc = calc
    flt = FrechetCellFilter(atoms)
    opt = LBFGS(flt, logfile=None)

    if progress is not None:
        # Attach a step observer that snapshots the current max-force.
        def _observer():
            forces = atoms.get_forces()
            fmax_now = float(np.sqrt((forces ** 2).sum(axis=1).max()))
            progress(opt.get_number_of_steps(), steps, fmax_now)

        opt.attach(_observer, interval=1)

    t0 = time.time()
    opt.run(fmax=fmax, steps=steps)
    wall = time.time() - t0

    forces = atoms.get_forces()
    fmax_final = float((forces ** 2).sum(axis=1).max() ** 0.5)
    e_per_atom = float(atoms.get_potential_energy() / len(atoms))
    v_per_atom = float(atoms.get_volume() / len(atoms))

    # Effective lattice parameter via cube-root of conventional cell volume.
    # For BCC conventional (2 atoms/cell): a_eff = (2 V/atom)^(1/3).
    # For FCC conventional (4 atoms/cell): a_eff = (4 V/atom)^(1/3).
    # We can't know the original phase here, so we return the cubic-equivalent
    # of a 2-atom cell as a coarse summary; callers with phase knowledge
    # should recompute. This matches generate_rhea_mace.py's heuristic.
    a_eff = (2.0 * v_per_atom) ** (1.0 / 3.0)

    return RelaxResult(
        energy_per_atom_eV=e_per_atom,
        volume_per_atom_A3=v_per_atom,
        lattice_a_eff_A=a_eff,
        n_steps=int(opt.get_number_of_steps()),
        fmax_final_eV_per_A=fmax_final,
        wall_time_s=wall,
    )
