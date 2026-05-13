"""Composition manipulation helpers and pure-element reference energies."""

from __future__ import annotations

from typing import TYPE_CHECKING

import numpy as np

from .lattices import A_BCC, A_FCC, A_HCP, COA_IDEAL, GROUND_STATE_PHASE

if TYPE_CHECKING:
    pass


def normalize_composition(comp: dict[str, float], n_atoms: int) -> dict[str, int]:
    """Snap a fractional composition to an integer count summing to n_atoms.

    Rounds each entry, then absorbs the residual into the most-fractional
    element so the sum equals ``n_atoms`` exactly. Drops zero-count entries.
    """
    total = sum(comp.values())
    if total <= 0:
        raise ValueError("composition values must sum to a positive number")
    scaled = {el: c * n_atoms / total for el, c in comp.items()}
    rounded = {el: int(round(v)) for el, v in scaled.items()}
    diff = n_atoms - sum(rounded.values())
    if diff != 0:
        # Adjust the element with the largest fractional component (positive
        # diff means we under-allocated; flip sign of residual otherwise).
        residuals = {el: scaled[el] - rounded[el] for el in scaled}
        if diff > 0:
            target = max(residuals, key=residuals.get)
        else:
            target = min(residuals, key=residuals.get)
        rounded[target] += diff
    return {el: v for el, v in rounded.items() if v > 0}


def composition_at_x(
    x_isru: float,
    n_atoms: int = 100,
    master_alloy: tuple[str, ...] = ("Mo", "Nb", "Ta", "Ti", "V"),
    isru_pair: tuple[str, str] = ("Fe", "Ti"),
    isru_ratio: tuple[float, float] = (0.5, 0.5),
) -> dict[str, int]:
    """Atomic counts for a dilution between an equimolar master alloy and an
    ISRU two-component end-point.

    Default: equimolar MoNbTaTiV master → Fe-50Ti ISRU. Matches the WAMS
    paper's Figure 12 trajectory.
    """
    rhea_frac = 1.0 - x_isru
    n_master_each = rhea_frac * n_atoms / len(master_alloy)
    raw: dict[str, float] = {el: n_master_each for el in master_alloy}
    el_a, el_b = isru_pair
    f_a, f_b = isru_ratio
    raw[el_a] = raw.get(el_a, 0.0) + x_isru * n_atoms * f_a
    raw[el_b] = raw.get(el_b, 0.0) + x_isru * n_atoms * f_b
    return normalize_composition(raw, n_atoms)


def pure_element_energy(symbol: str, calc, fmax: float = 0.02, steps: int = 80) -> float:
    """Energy per atom of a pure element in its conventional ground state.

    Used as a per-element chemical-potential reference μ for formation
    enthalpies ΔH_f. Imports of ASE/MACE happen lazily.
    """
    from ase.build import bulk
    from ase.filters import FrechetCellFilter
    from ase.optimize import LBFGS

    gs = GROUND_STATE_PHASE[symbol]
    if gs == "bcc":
        atoms = bulk(symbol, "bcc", a=A_BCC[symbol], cubic=True) * (3, 3, 3)
    elif gs == "fcc":
        atoms = bulk(symbol, "fcc", a=A_FCC[symbol], cubic=True) * (3, 3, 3)
    else:  # hcp
        a = A_HCP[symbol]
        atoms = bulk(symbol, "hcp", a=a, c=a * COA_IDEAL) * (3, 3, 3)
    atoms.calc = calc
    flt = FrechetCellFilter(atoms)
    LBFGS(flt, logfile=None).run(fmax=fmax, steps=steps)
    return float(atoms.get_potential_energy() / len(atoms))


def formation_energy(
    energy_per_atom: float,
    composition: dict[str, int],
    mu: dict[str, float],
) -> float:
    """ΔH_f per atom relative to pure-element references μ."""
    n = sum(composition.values())
    e_ref = sum(composition[el] * mu[el] for el in composition) / n
    return energy_per_atom - e_ref
