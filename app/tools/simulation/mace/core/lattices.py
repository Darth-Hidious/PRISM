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


def _radius_estimate_a(element: str, phase: str) -> float:
    """Generalisable starting lattice parameter for ANY element.

    These tables only seed the optimizer — this module's own docstring:
    "Used only as starting guesses; subsequent cell relaxation finds the
    actual minimum." So instead of a hardcoded ~10-element refractory dict
    (which made the tool a one-example artefact), derive a physically sane
    `a` from the element's covalent radius `r` via close-packing geometry.
    MACE-MH-1 is a periodic-table-wide foundation MLIP; the only real
    reason Cu/Ni/Si "weren't supported" was a missing dict entry.

        bcc: nn on body diagonal, 2r = a·√3/2  -> a = 4r/√3
        fcc: nn on face diagonal, 2r = a/√2    -> a = 2√2·r
        hcp: basal nn,            a = 2r       (c = COA_IDEAL·a)
    """
    from ase.data import atomic_numbers, covalent_radii

    z = atomic_numbers.get(element)
    if not z:
        raise KeyError(f"{element!r} is not a real chemical element")
    r = float(covalent_radii[z])  # angstroms, defined for every element
    if phase == "bcc":
        return 4.0 * r / (3.0**0.5)
    if phase == "fcc":
        return 2.0 * (2.0**0.5) * r
    if phase == "hcp":
        return 2.0 * r
    raise KeyError(f"unknown phase {phase!r}; expected one of {PHASES}")


def lookup_a(element: str, phase: str) -> float:
    """Starting lattice parameter for one element in one phase.

    Curated value if we have one (preserves tuned refractory accuracy);
    otherwise a covalent-radius estimate so the tool works for the whole
    periodic table, not one alloy family.
    """
    table = {"bcc": A_BCC, "fcc": A_FCC, "hcp": A_HCP}[phase]
    a = table.get(element)
    return a if a is not None else _radius_estimate_a(element, phase)


def avg_a(composition: dict[str, int], phase: str) -> float:
    """Composition-weighted average starting lattice parameter (any elements)."""
    n = sum(composition.values())
    return sum(lookup_a(el, phase) * c for el, c in composition.items()) / n


def supported_elements() -> set[str]:
    """Every real chemical element — curated or radius-derived. The tool is
    no longer gated to the refractory subset; MACE-MH-1 spans the periodic
    table and the starting lattice is now derivable for any element."""
    from ase.data import chemical_symbols

    return {s for s in chemical_symbols if s and s != "X"}
