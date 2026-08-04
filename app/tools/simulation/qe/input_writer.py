"""Quantum ESPRESSO (pw.x) input-file writer built on ASE's espresso writer.

This is the pyiron-free QE path: ``ase.io.write(..., format="espresso-in")``
is the tested primitive, and we own the namelist construction on top of it.

The writer always emits real ``&CONTROL``/``&SYSTEM``/``&ELECTRONS``
namelists plus the ``ATOMIC_SPECIES``, ``ATOMIC_POSITIONS``, ``K_POINTS``
and ``CELL_PARAMETERS`` cards (ASE writes all known namelist sections; empty
ones such as ``&IONS`` in an scf run are valid QE syntax). There are no
placeholders: every pseudopotential name comes from a file resolved on disk
(see :mod:`app.tools.simulation.qe.pseudos`).
"""
from __future__ import annotations

from pathlib import Path
from typing import Dict, Mapping, Optional, Sequence, Tuple, Union

from ase import Atoms
from ase.io import write
from pymatgen.core import Structure
from pymatgen.io.ase import AseAtomsAdaptor

#: QE ``calculation`` values this writer supports. Everything else is
#: rejected up front — pw.x would die on an unknown value anyway, and
#: failing here keeps the error attached to the input we generated.
CALCULATION_TYPES: Tuple[str, ...] = ("scf", "relax", "vc-relax")

StructureLike = Union[Structure, Atoms]


def _to_atoms(structure: StructureLike) -> Atoms:
    """Accept a pymatgen Structure or ASE Atoms; return ASE Atoms."""
    if isinstance(structure, Atoms):
        return structure
    if isinstance(structure, Structure):
        return AseAtomsAdaptor.get_atoms(structure)
    raise TypeError(
        "structure must be a pymatgen Structure or an ASE Atoms object, "
        f"got {type(structure).__name__}"
    )


def write_input(
    structure: StructureLike,
    pseudopotentials: Mapping[str, str],
    cutoffs: Mapping[str, float],
    kpoints: Optional[Sequence[int]],
    calculation_type: str,
    path: Union[str, Path],
    pseudo_dir: Union[str, Path] = "./pseudo",
    prefix: str = "prism",
    extra_input: Optional[Mapping[str, Mapping]] = None,
) -> Path:
    """Write a complete pw.x ``.in`` file and return its path.

    Parameters
    ----------
    structure:
        pymatgen ``Structure`` or ASE ``Atoms``.
    pseudopotentials:
        ``{element: upf_filename}`` covering EVERY element in the structure
        (see ``resolve_pseudopotentials``). Raises ``ValueError`` listing
        uncovered elements otherwise.
    cutoffs:
        ``{"ecutwfc": <Ry>}`` with optional ``"ecutrho": <Ry>`` (QE units;
        Rydberg, not eV). When ``ecutrho`` is omitted, QE defaults to
        4x ecutwfc for norm-conserving pseudos — we still write ecutwfc
        only and let QE apply its own rule rather than guessing.
    kpoints:
        Monkhorst-Pack grid as 3 positive ints, e.g. ``(8, 8, 8)``.
        ``None`` writes a Gamma-only ``K_POINTS`` card.
    calculation_type:
        One of ``scf``, ``relax``, ``vc-relax`` (QE's own values).
    path:
        Destination ``.in`` file; parent directories are created.
    pseudo_dir:
        Directory pw.x will read the UPF files from at run time
        (``&control / pseudo_dir``). Not validated here — it only needs to
        exist on the machine that runs pw.x.
    prefix:
        QE job prefix (``&control / prefix``).
    extra_input:
        Optional overrides/extra namelist entries, e.g.
        ``{"system": {"nspin": 2}, "electrons": {"mixing_beta": 0.4}}``.
        User overrides win over the defaults built here.

    Returns
    -------
    Path
        The written file.

    Raises
    ------
    ValueError
        For unsupported calculation types, non-positive cutoffs/kpoints, or
        pseudopotentials that do not cover every element.
    """
    calc = str(calculation_type).strip().lower()
    if calc not in CALCULATION_TYPES:
        raise ValueError(
            f"Unsupported calculation_type {calculation_type!r}; "
            f"supported: {', '.join(CALCULATION_TYPES)}"
        )

    ecutwfc = cutoffs.get("ecutwfc")
    if not isinstance(ecutwfc, (int, float)) or ecutwfc <= 0:
        raise ValueError(f"cutoffs['ecutwfc'] must be a positive number (Ry), got {ecutwfc!r}")
    ecutrho = cutoffs.get("ecutrho")
    if ecutrho is not None and (not isinstance(ecutrho, (int, float)) or ecutrho <= 0):
        raise ValueError(f"cutoffs['ecutrho'] must be a positive number (Ry), got {ecutrho!r}")

    atoms = _to_atoms(structure)
    species = sorted(set(atoms.get_chemical_symbols()))
    uncovered = [el for el in species if el not in pseudopotentials]
    if uncovered:
        raise ValueError(
            f"No pseudopotential supplied for element(s): {', '.join(uncovered)}. "
            f"Resolve them from a UPF directory with resolve_pseudopotentials() "
            f"before calling write_input()."
        )

    if kpoints is not None:
        kpoints = tuple(int(k) for k in kpoints)
        if len(kpoints) != 3 or any(k < 1 for k in kpoints):
            raise ValueError(
                f"kpoints must be 3 positive ints (Monkhorst-Pack grid) or None, "
                f"got {kpoints!r}"
            )

    control: Dict = {
        "calculation": calc,
        "prefix": prefix,
        "pseudo_dir": str(pseudo_dir),
        # Always ask for forces/stress so parse_output has something to read
        # back, even for plain scf runs.
        "tprnfor": True,
        "tstress": True,
    }
    system: Dict = {
        "ecutwfc": float(ecutwfc),
        # Metallic smearing is the safe default for arbitrary inputs;
        # override via extra_input for insulators ("fixed") or metals
        # ("tetrahedra").
        "occupations": "smearing",
        "degauss": 0.02,
    }
    if ecutrho is not None:
        system["ecutrho"] = float(ecutrho)
    electrons: Dict = {"conv_thr": 1e-8}
    ions: Dict = {}
    cell: Dict = {}
    if calc in ("relax", "vc-relax"):
        ions = {"ion_dynamics": "bfgs"}
    if calc == "vc-relax":
        cell = {"cell_dynamics": "bfgs"}

    input_data = {
        "control": control,
        "system": system,
        "electrons": electrons,
        "ions": ions,
        "cell": cell,
    }
    if extra_input:
        for section, entries in extra_input.items():
            section = str(section).lower()
            if section not in input_data:
                raise ValueError(
                    f"extra_input section {section!r} unknown; "
                    f"valid: {', '.join(input_data)}"
                )
            input_data[section].update(entries)

    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    write(
        path,
        atoms,
        format="espresso-in",
        input_data=input_data,
        pseudopotentials=dict(pseudopotentials),
        kpts=kpoints,
    )
    return path
