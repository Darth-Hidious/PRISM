"""Supercell builders for BCC / FCC / HCP random-substitution alloys
and the C14 Laves prototype.

All builders return an ``ase.Atoms`` object with PBC and chemical symbols set
by a deterministic shuffle of the supplied RNG.

The canonical BCC / FCC / HCP builder uses 100-atom cells (5×5×2, 5×5×1,
5×5×2 respectively) which is the size used throughout the WAMS paper's
phase-competition study.

ASE is imported lazily so this module can be inspected statically without
the dependency present.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

import numpy as np

from .lattices import A_BCC, A_FCC, A_HCP, COA_IDEAL, avg_a

if TYPE_CHECKING:
    from ase import Atoms


# Atoms per conventional / primitive cell, used to compute supercell repeats
# so total atom count matches N_ATOMS exactly.
_PHASE_REPEAT: dict[str, tuple[int, int, int]] = {
    "bcc": (5, 5, 2),  # cubic conventional BCC has 2 atoms/cell  -> 50 cells = 100
    "fcc": (5, 5, 1),  # cubic conventional FCC has 4 atoms/cell  -> 25 cells = 100
    "hcp": (5, 5, 2),  # primitive HCP has 2 atoms/cell           -> 50 cells = 100
}


def build_supercell(
    composition: dict[str, int],
    phase: str,
    rng: np.random.Generator | None = None,
) -> "Atoms":
    """Build a 100-atom supercell of ``phase`` filled by random substitution.

    Parameters
    ----------
    composition : dict[str, int]
        Element symbol -> integer atom count. Must sum to 100.
    phase : str
        ``"bcc"`` | ``"fcc"`` | ``"hcp"``.
    rng : numpy.random.Generator | None
        Seeded RNG for the symbol shuffle. If ``None``, a fresh ``default_rng()``
        is used (non-reproducible — pass a seeded one for reproducibility).
    """
    from ase.build import bulk

    n = sum(composition.values())
    if n != 100:
        raise ValueError(f"composition must sum to 100, got {n}")
    if phase not in _PHASE_REPEAT:
        raise ValueError(f"unsupported phase {phase!r}; valid: bcc/fcc/hcp")

    if rng is None:
        rng = np.random.default_rng()

    symbols = np.array(
        [el for el, c in composition.items() for _ in range(c)], dtype=object
    )
    rng.shuffle(symbols)

    a = avg_a(composition, phase)
    if phase == "bcc":
        proto = bulk("X", "bcc", a=a, cubic=True)
    elif phase == "fcc":
        proto = bulk("X", "fcc", a=a, cubic=True)
    else:  # hcp
        c = a * COA_IDEAL
        proto = bulk("X", "hcp", a=a, c=c)

    sc = proto * _PHASE_REPEAT[phase]
    if len(sc) != 100:
        raise AssertionError(f"{phase} supercell built with {len(sc)} atoms, expected 100")
    sc.set_chemical_symbols(list(symbols))
    return sc


def build_c14_laves(big_atom: str, small_atom: str = "Fe") -> "Atoms":
    """C14 (MgZn₂ prototype) Laves phase, 96-atom 2×2×2 supercell.

    Wyckoff sites (P6_3/mmc):
      - 4f (1/3, 2/3, z),  z ≈ 0.0625    -> ``big_atom`` (e.g. Nb / Ta)
      - 2a (0, 0, 0)                      -> ``small_atom`` (Fe)
      - 6h (x, 2x, 1/4),   x ≈ 0.833      -> ``small_atom`` (Fe)

    Starting lattice (Fe2Nb): a = 4.842 Å, c = 7.872 Å. Relaxation will
    adjust for the actual species pair.
    """
    from ase import Atoms
    from ase.cell import Cell

    a, c = 4.842, 7.872
    cell = Cell.fromcellpar([a, a, c, 90, 90, 120])

    z = 0.0625
    x = 5.0 / 6.0

    positions_frac = np.array(
        [
            [1 / 3, 2 / 3, z],
            [2 / 3, 1 / 3, z + 0.5],
            [2 / 3, 1 / 3, -z],
            [1 / 3, 2 / 3, -z + 0.5],
            # 2a
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 0.5],
            # 6h orbit
            [x, 2 * x, 0.25],
            [-2 * x, -x, 0.25],
            [x, -x, 0.25],
            [-x, -2 * x, 0.75],
            [2 * x, x, 0.75],
            [-x, x, 0.75],
        ]
    )
    positions_frac = positions_frac % 1.0
    symbols = [big_atom] * 4 + [small_atom] * 8

    primitive = Atoms(
        symbols=symbols,
        scaled_positions=positions_frac,
        cell=cell,
        pbc=True,
    )
    return primitive * (2, 2, 2)
