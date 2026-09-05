"""Pure-element starting lattice parameters for supercell construction.

Superset of the values used across the original phase_diagrams scripts:
  - dilution_hf_job.py        (9 elements, BCC)
  - extra_baselines_pugh.py   (adds W)
  - generate_rhea_mace.py     (BCC + FCC + HCP tables)

Used only as starting guesses; subsequent cell relaxation finds the actual
minimum. Composition-weighted averages are taken for multi-element cells.

All values in angstroms.
"""

from __future__ import annotations

# BCC lattice parameter (cubic conventional cell).
A_BCC: dict[str, float] = {
    "Al": 3.20,
    "Fe": 2.87,
    "Hf": 3.55,
    "Mo": 3.15,
    "Nb": 3.30,
    "Ta": 3.30,
    "Ti": 3.27,
    "V": 3.03,
    "W": 3.16,
    "Zr": 3.57,
}

# FCC lattice parameter (cubic conventional cell).
A_FCC: dict[str, float] = {
    "Al": 4.05,
    "Fe": 3.65,
    "Hf": 4.50,
    "Mo": 4.00,
    "Nb": 4.20,
    "Ta": 4.20,
    "Ti": 4.13,
    "V": 3.83,
    "W": 4.00,
    "Zr": 4.50,
}

# HCP lattice parameter a (c uses ideal c/a = 1.633 unless overridden).
A_HCP: dict[str, float] = {
    "Al": 2.86,
    "Fe": 2.51,
    "Hf": 3.20,
    "Mo": 2.74,
    "Nb": 2.85,
    "Ta": 2.85,
    "Ti": 2.95,
    "V": 2.62,
    "W": 2.74,
    "Zr": 3.23,
}

# Ideal HCP c/a ratio.
COA_IDEAL: float = 1.633

# Pure-element conventional ground-state phase (used for ΔH_f references).
GROUND_STATE_PHASE: dict[str, str] = {
    "Al": "fcc",
    "Fe": "bcc",
    "Hf": "hcp",
    "Mo": "bcc",
    "Nb": "bcc",
    "Ta": "bcc",
    "Ti": "hcp",
    "V": "bcc",
    "W": "bcc",
    "Zr": "hcp",
}

PHASES = ("bcc", "fcc", "hcp")


def _extend_from_ase_reference_states() -> None:
    """Widen the tables to every element whose ground state is a simple metal
    lattice, from ASE's experimental reference states.

    Measured 2026-09-02 in a live research session: the MACE tier refused
    Ni/Co/Cr, and a superalloy proxy had to be relaxed by hand in the notebook
    kernel. The gate was never the model — MACE-MP-0 and MACE-MH-1 are trained
    on the whole periodic table — but these ten-row hand tables.

    The element's own structure takes ASE's lattice constant; a phase it does
    not adopt in nature is estimated at EQUAL ATOMIC VOLUME (the hand values
    above follow the same rule: Fe bcc 2.87 → fcc 3.62 vs 3.65 listed). These
    are starting guesses for relaxation, never reported properties. Elements
    with complex ground states (α-Mn, diamond Si, bct Sn, …) are not added; a
    request for them fails with the unsupported-element message, which is the
    honest answer. Hand values stay authoritative where present.
    """
    try:
        from ase.data import atomic_numbers, chemical_symbols, reference_states
    except Exception:  # pragma: no cover — ASE is a hard dependency of this tier
        return
    for symbol in chemical_symbols[1:]:
        ref = reference_states[atomic_numbers[symbol]]
        if not ref:
            continue
        symmetry = ref.get("symmetry")
        a = ref.get("a")
        if symmetry not in PHASES or not a:
            continue
        if symmetry == "hcp":
            coa = float(ref.get("c/a") or COA_IDEAL)
            v_atom = (3**0.5 / 4.0) * a * a * (coa * a)
        elif symmetry == "fcc":
            v_atom = a**3 / 4.0
        else:  # bcc
            v_atom = a**3 / 2.0
        derived = {
            "bcc": (2.0 * v_atom) ** (1.0 / 3.0),
            "fcc": (4.0 * v_atom) ** (1.0 / 3.0),
            "hcp": (4.0 * v_atom / (3**0.5 * COA_IDEAL)) ** (1.0 / 3.0),
        }
        derived[symmetry] = float(a)
        A_BCC.setdefault(symbol, round(derived["bcc"], 3))
        A_FCC.setdefault(symbol, round(derived["fcc"], 3))
        A_HCP.setdefault(symbol, round(derived["hcp"], 3))
        GROUND_STATE_PHASE.setdefault(symbol, symmetry)


_extend_from_ase_reference_states()


def lookup_a(element: str, phase: str) -> float:
    """Look up starting lattice parameter for one element in one phase."""
    table = {"bcc": A_BCC, "fcc": A_FCC, "hcp": A_HCP}[phase]
    return table[element]


def avg_a(composition: dict[str, int], phase: str) -> float:
    """Composition-weighted average lattice parameter."""
    table = {"bcc": A_BCC, "fcc": A_FCC, "hcp": A_HCP}[phase]
    n = sum(composition.values())
    return sum(table[el] * c for el, c in composition.items()) / n


def supported_elements() -> set[str]:
    """All elements with a starting lattice parameter in at least BCC."""
    return set(A_BCC) | set(A_FCC) | set(A_HCP)
