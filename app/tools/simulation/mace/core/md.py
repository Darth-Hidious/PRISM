"""NVT Langevin molecular dynamics and RDF computation."""

from __future__ import annotations

import time
from dataclasses import dataclass
from typing import TYPE_CHECKING, Callable

import numpy as np

if TYPE_CHECKING:
    from ase import Atoms


@dataclass
class MdResult:
    mean_E_per_atom_eV: float
    std_E_per_atom_eV: float
    mean_T_K: float
    final_atoms: object  # ase.Atoms (forward-declared)
    steps: list[int]
    energies: list[float]
    temperatures: list[float]
    rdf_r_A: list[float]
    rdf_g: list[float]
    is_dynamically_stable: bool
    wall_time_s: float


def langevin_run(
    atoms_seed: "Atoms",
    calc,
    T_K: float,
    n_steps: int = 1000,
    timestep_fs: float = 1.0,
    friction_per_fs: float = 0.01,
    sample_every: int | None = None,
    seed: int = 20260506,
    progress: Callable[[int, int], None] | None = None,
) -> MdResult:
    """Run NVT Langevin MD on a copy of ``atoms_seed``.

    Returns the time-series of E/atom and temperature, the final-frame RDF,
    and an energy-drift-based dynamic-stability flag (set True if the
    last-quarter mean energy differs from the first-quarter mean by less
    than 50 meV/atom, i.e. no runaway).
    """
    from ase import units
    from ase.md.langevin import Langevin
    from ase.md.velocitydistribution import MaxwellBoltzmannDistribution

    atoms = atoms_seed.copy()
    atoms.calc = calc

    MaxwellBoltzmannDistribution(
        atoms, temperature_K=T_K, rng=np.random.default_rng(seed)
    )
    dyn = Langevin(
        atoms,
        timestep=timestep_fs * units.fs,
        temperature_K=T_K,
        friction=friction_per_fs / units.fs,
    )

    if sample_every is None:
        sample_every = max(1, n_steps // 50)

    energies: list[float] = []
    temps: list[float] = []
    steps: list[int] = []

    t0 = time.time()
    for step in range(n_steps):
        dyn.run(1)
        if step % sample_every == 0:
            ep = atoms.get_potential_energy() / len(atoms)
            energies.append(float(ep))
            temps.append(float(atoms.get_temperature()))
            steps.append(step)
        if progress is not None and step % 50 == 49:
            progress(step + 1, n_steps)

    wall = time.time() - t0
    r_centers, g_r = compute_rdf(atoms)

    # Dynamic stability heuristic
    if len(energies) >= 4:
        q = len(energies) // 4
        e_first = float(np.mean(energies[:q]))
        e_last = float(np.mean(energies[-q:]))
        drift = abs(e_last - e_first)
        stable = drift < 0.05  # 50 meV/atom
    else:
        stable = True

    return MdResult(
        mean_E_per_atom_eV=float(np.mean(energies[-max(1, len(energies) // 4):])),
        std_E_per_atom_eV=float(np.std(energies[-max(1, len(energies) // 4):])),
        mean_T_K=float(np.mean(temps[-max(1, len(temps) // 4):])),
        final_atoms=atoms,
        steps=steps,
        energies=energies,
        temperatures=temps,
        rdf_r_A=r_centers.tolist(),
        rdf_g=g_r.tolist(),
        is_dynamically_stable=stable,
        wall_time_s=wall,
    )


def compute_rdf(
    atoms: "Atoms",
    rmax: float = 6.0,
    nbins: int = 80,
) -> tuple[np.ndarray, np.ndarray]:
    """Total (species-agnostic) radial distribution function via minimum image."""
    pos = atoms.get_positions()
    cell = atoms.cell.array
    n = len(atoms)
    inv = np.linalg.inv(cell)
    dists: list[np.ndarray] = []
    for i in range(n):
        d = pos - pos[i]
        f = d @ inv
        f -= np.round(f)
        d = f @ cell
        r = np.linalg.norm(d, axis=1)
        r = r[(r > 1e-3) & (r < rmax)]
        dists.append(r)
    all_dists = np.concatenate(dists) if dists else np.array([])
    edges = np.linspace(0.0, rmax, nbins + 1)
    hist, _ = np.histogram(all_dists, bins=edges)
    r_centers = 0.5 * (edges[:-1] + edges[1:])
    dr = edges[1] - edges[0]
    rho = n / atoms.get_volume()
    shell_vol = 4.0 * np.pi * r_centers ** 2 * dr
    with np.errstate(divide="ignore", invalid="ignore"):
        g = hist / (n * rho * shell_vol)
    return r_centers, np.nan_to_num(g)
