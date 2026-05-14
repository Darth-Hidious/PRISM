"""Harmonic phonon free energy via Phonopy finite-displacement.

Treats the supplied relaxed supercell as the phonon "primitive" cell
(supercell_matrix = identity). This is adequate for free-energy comparisons
between competing phases of the same alloy and matches the protocol used
throughout the WAMS paper. It is NOT publication-grade phonon DOS — for
that, callers must run on a true primitive cell with a larger
supercell_matrix.

Quality-tier flag is exposed in the result for downstream provenance.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Callable

import numpy as np

if TYPE_CHECKING:
    from ase import Atoms


KJ_MOL_TO_EV = 1.0 / 96.485
IMAGINARY_TOL_THZ = 0.05  # treat |ω| < 0.05 THz as essentially zero (numerical noise)


@dataclass
class PhononResult:
    temperatures_K: list[float]
    F_vib_eV_per_atom: list[float]
    n_imaginary_modes: int
    is_dynamically_stable: bool
    phonon_dos_omega_THz: list[float] = field(default_factory=list)
    phonon_dos_g: list[float] = field(default_factory=list)
    quality_tier: str = "harmonic_supercell_identity_4q"
    wall_time_s: float = 0.0


def harmonic_free_energy(
    atoms_relaxed: "Atoms",
    calc,
    temperatures_K: list[float] | np.ndarray,
    displacement_A: float = 0.01,
    q_mesh: tuple[int, int, int] = (4, 4, 4),
    progress: Callable[[int, int], None] | None = None,
) -> PhononResult:
    """Compute F_vib(T) per atom on the supplied relaxed supercell."""
    import time

    from ase import Atoms
    from phonopy import Phonopy
    from phonopy.structure.atoms import PhonopyAtoms

    temps = np.asarray(temperatures_K, dtype=float)
    n_atoms = len(atoms_relaxed)

    pa = PhonopyAtoms(
        symbols=atoms_relaxed.get_chemical_symbols(),
        cell=atoms_relaxed.cell.array,
        scaled_positions=atoms_relaxed.get_scaled_positions(),
    )
    phonon = Phonopy(
        pa,
        supercell_matrix=np.eye(3, dtype=int),
        primitive_matrix=np.eye(3),
    )
    phonon.generate_displacements(distance=displacement_A)
    n_disp = len(phonon.supercells_with_displacements)

    t0 = time.time()
    forces_set: list[np.ndarray] = []
    for i, sc in enumerate(phonon.supercells_with_displacements):
        if sc is None:
            forces_set.append(np.zeros((n_atoms, 3)))
            continue
        a = Atoms(
            symbols=sc.symbols,
            cell=sc.cell,
            scaled_positions=sc.scaled_positions,
            pbc=True,
        )
        a.calc = calc
        forces_set.append(a.get_forces())
        if progress is not None:
            progress(i + 1, n_disp)

    phonon.forces = forces_set
    phonon.produce_force_constants()

    phonon.run_mesh(list(q_mesh))
    phonon.run_thermal_properties(temperatures=temps.tolist())
    tp = phonon.get_thermal_properties_dict()
    F_eV_per_cell = np.array(tp["free_energy"]) * KJ_MOL_TO_EV
    F_per_atom = F_eV_per_cell / n_atoms

    freqs = phonon.get_mesh_dict()["frequencies"]  # THz
    n_imag = int(np.sum(freqs < -IMAGINARY_TOL_THZ))

    # Phonon DOS (optional; cheap to compute on the existing mesh)
    try:
        phonon.run_total_dos()
        dos = phonon.get_total_dos_dict()
        dos_omega = dos["frequency_points"].tolist()
        dos_g = dos["total_dos"].tolist()
    except Exception:
        dos_omega, dos_g = [], []

    return PhononResult(
        temperatures_K=temps.tolist(),
        F_vib_eV_per_atom=F_per_atom.tolist(),
        n_imaginary_modes=n_imag,
        is_dynamically_stable=(n_imag == 0),
        phonon_dos_omega_THz=dos_omega,
        phonon_dos_g=dos_g,
        wall_time_s=time.time() - t0,
    )
