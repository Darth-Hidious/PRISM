"""Configurational entropy + Gibbs-energy assembly helpers."""

from __future__ import annotations

import numpy as np

KB_EV: float = 8.617333262e-5  # Boltzmann constant in eV/K


def s_config_random_mixing(composition: dict[str, int]) -> float:
    """Bragg-Williams ideal-mixing config entropy per atom.

    S_config = -k_B Σ x_i ln x_i.

    Returns the value in eV/K per atom.
    """
    n = sum(composition.values())
    s = 0.0
    for el, c in composition.items():
        if c <= 0:
            continue
        x = c / n
        s -= x * np.log(x)
    return float(KB_EV * s)


def gibbs_energy(
    E_per_atom_eV: float,
    F_vib_eV_per_atom: float,
    S_config_eV_per_K: float,
    T_K: float,
) -> float:
    """G(T) ≈ E_0 + F_vib(T) - T · S_config (eV / atom).

    Ignores pV (negligible at the ambient lunar-surface conditions of
    interest) and ignores non-configurational electronic entropy.
    """
    return E_per_atom_eV + F_vib_eV_per_atom - T_K * S_config_eV_per_K
