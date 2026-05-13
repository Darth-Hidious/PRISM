"""Elastic-tensor finite-difference fit + Voigt-Reuss-Hill averaging.

Six Voigt strain modes, central difference at ±strain_amplitude (default
0.005). Forces internal coordinates frozen at the relaxed cell (standard
convention for clamped-ion elastic constants from foundation MLIPs).

Output convention:
    K_VRH, G_VRH, E_Young, ν_Poisson are in GPa.
    Pugh G/B = G_VRH / K_VRH.   Pugh 1954: G/B < 0.57 = ductile.
    Cauchy pressure P_c = C12 - C44 (GPa, only meaningful for cubic).
    JOM-2025 RHEA-AM manufacturability threshold: G/B < 0.402.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import TYPE_CHECKING

import numpy as np

if TYPE_CHECKING:
    from ase import Atoms


EV_PER_A3_TO_GPA = 160.21766208
N_VOIGT = 6
PUGH_THRESHOLD_DUCTILE = 0.57
PUGH_THRESHOLD_AM_MANUFACTURABILITY = 0.402  # JOM 2025 RHEA-AM cracking


@dataclass
class ElasticResult:
    C_GPa: list[list[float]]  # 6×6
    K_VRH_GPa: float
    G_VRH_GPa: float
    E_Young_GPa: float
    nu_Poisson: float
    pugh_G_over_B: float
    cauchy_pressure_GPa: float
    pugh_verdict: str  # "ductile" | "brittle"
    am_manufacturability_passed: bool
    wall_time_s: float = 0.0
    extras: dict = field(default_factory=dict)


def voigt_strain_tensor(component: int, eps: float) -> np.ndarray:
    """Symmetric 3×3 strain tensor for one Voigt component (0..5)."""
    s = np.zeros((3, 3))
    if component < 3:
        s[component, component] = eps
    elif component == 3:  # yz
        s[1, 2] = s[2, 1] = eps / 2
    elif component == 4:  # xz
        s[0, 2] = s[2, 0] = eps / 2
    elif component == 5:  # xy
        s[0, 1] = s[1, 0] = eps / 2
    else:
        raise ValueError(component)
    return s


def apply_strain(atoms: "Atoms", strain: np.ndarray) -> "Atoms":
    """Return a copy of ``atoms`` with the strain applied (scale_atoms=True)."""
    new = atoms.copy()
    deformation = np.eye(3) + strain
    new.set_cell(atoms.cell.array @ deformation, scale_atoms=True)
    return new


def _strained_stress(atoms: "Atoms", calc, comp: int, eps: float) -> np.ndarray:
    a = apply_strain(atoms, voigt_strain_tensor(comp, eps))
    a.calc = calc
    a.get_potential_energy()  # trigger stress computation
    return a.get_stress()


def elastic_tensor(
    atoms_relaxed: "Atoms",
    calc,
    strain_amplitude: float = 0.005,
) -> np.ndarray:
    """Compute the 6×6 stiffness tensor in GPa by central differences.

    The reference cell is assumed to already be relaxed. Reference stress
    is reported in the caller's logs but not subtracted (its absolute value
    is a sanity check on the reference state).
    """
    C = np.zeros((N_VOIGT, N_VOIGT))
    for j in range(N_VOIGT):
        sigma_plus = _strained_stress(atoms_relaxed, calc, j, +strain_amplitude)
        sigma_minus = _strained_stress(atoms_relaxed, calc, j, -strain_amplitude)
        dC = (sigma_plus - sigma_minus) / (2.0 * strain_amplitude)
        C[:, j] = dC
    # Symmetrise (small residual from numerical stress noise)
    C = 0.5 * (C + C.T)
    # Convert ASE stress units (eV/Å³) to GPa
    C *= EV_PER_A3_TO_GPA
    return C


def voigt_reuss_hill(C: np.ndarray) -> tuple[float, float, float, float, float]:
    """Return ``(K_VRH, G_VRH, E_Young, ν_Poisson, Pugh)`` in GPa from a 6×6 C.

    All formulas in Hill 1952; ν_Poisson and E_Young from isotropic relations.
    Returns NaN tuple if compliance inversion fails.
    """
    C11, C22, C33 = C[0, 0], C[1, 1], C[2, 2]
    C12, C13, C23 = C[0, 1], C[0, 2], C[1, 2]
    C44, C55, C66 = C[3, 3], C[4, 4], C[5, 5]

    K_V = ((C11 + C22 + C33) + 2 * (C12 + C13 + C23)) / 9.0
    G_V = ((C11 + C22 + C33) - (C12 + C13 + C23) + 3 * (C44 + C55 + C66)) / 15.0

    try:
        S = np.linalg.inv(C)
    except np.linalg.LinAlgError:
        nan = float("nan")
        return nan, nan, nan, nan, nan

    S11, S22, S33 = S[0, 0], S[1, 1], S[2, 2]
    S12, S13, S23 = S[0, 1], S[0, 2], S[1, 2]
    S44, S55, S66 = S[3, 3], S[4, 4], S[5, 5]
    K_R_inv = (S11 + S22 + S33) + 2 * (S12 + S13 + S23)
    G_R_inv = (4 * (S11 + S22 + S33) - 4 * (S12 + S13 + S23) + 3 * (S44 + S55 + S66)) / 15.0

    K_R = 1.0 / K_R_inv if K_R_inv > 0 else float("nan")
    G_R = 1.0 / G_R_inv if G_R_inv > 0 else float("nan")
    K_VRH = 0.5 * (K_V + K_R)
    G_VRH = 0.5 * (G_V + G_R)
    pugh = G_VRH / K_VRH if K_VRH > 0 else float("nan")
    # Isotropic Poisson / Young from K and G
    nu = (3.0 * K_VRH - 2.0 * G_VRH) / (2.0 * (3.0 * K_VRH + G_VRH)) if (3 * K_VRH + G_VRH) != 0 else float("nan")
    E_Y = 9.0 * K_VRH * G_VRH / (3.0 * K_VRH + G_VRH) if (3 * K_VRH + G_VRH) != 0 else float("nan")
    return float(K_VRH), float(G_VRH), float(E_Y), float(nu), float(pugh)


def cauchy_pressure(C: np.ndarray) -> float:
    """Pettifor Cauchy pressure C12 - C44 in GPa.

    Only strictly meaningful for cubic crystals; for general anisotropy we
    return the average of the three cubic-symmetric pairs.
    """
    P = ((C[0, 1] - C[3, 3]) + (C[0, 2] - C[4, 4]) + (C[1, 2] - C[5, 5])) / 3.0
    return float(P)


def summarize_elastic(C_GPa: np.ndarray, wall_time_s: float = 0.0) -> ElasticResult:
    """Compute the full structured summary from a 6×6 stiffness tensor."""
    K, G, E, nu, pugh = voigt_reuss_hill(C_GPa)
    P_c = cauchy_pressure(C_GPa)
    verdict = "ductile" if pugh < PUGH_THRESHOLD_DUCTILE else "brittle"
    am_pass = pugh < PUGH_THRESHOLD_AM_MANUFACTURABILITY
    return ElasticResult(
        C_GPa=C_GPa.tolist(),
        K_VRH_GPa=K,
        G_VRH_GPa=G,
        E_Young_GPa=E,
        nu_Poisson=nu,
        pugh_G_over_B=pugh,
        cauchy_pressure_GPa=P_c,
        pugh_verdict=verdict,
        am_manufacturability_passed=am_pass,
        wall_time_s=wall_time_s,
        extras={
            "pugh_threshold_ductile": PUGH_THRESHOLD_DUCTILE,
            "pugh_threshold_am_manufacturability": PUGH_THRESHOLD_AM_MANUFACTURABILITY,
        },
    )
