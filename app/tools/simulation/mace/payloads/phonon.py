# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "mace-mcp>=0.1.0",
#   "mace-torch>=0.3.15",
#   "ase>=3.23",
#   "phonopy>=2.20",
#   "numpy>=1.24",
#   "scipy>=1.10",
#   "torch>=2.1",
#   "huggingface_hub>=0.24",
# ]
# ///
"""HF Job payload: phonon_harmonic."""

from __future__ import annotations

import sys


def main() -> None:
    from app.tools.simulation.mace.payloads._common import (
        build_atoms,
        ensure_mace_core,
        make_calc_for,
        push_to_dataset,
        read_spec,
        write_result,
    )

    ensure_mace_core()
    spec = read_spec(sys.argv[1])
    cache_key = spec["cache_key"]
    ip = spec["input_payload"]
    seed = int(spec.get("seed", 20260506))

    from app.tools.simulation.mace.core.phonons import harmonic_free_energy
    from app.tools.simulation.mace.core.relax import relax

    atoms, _, _ = build_atoms(ip, seed)
    calc = make_calc_for(ip)
    relax(atoms, calc, fmax=0.02, steps=200)
    pr = harmonic_free_energy(
        atoms,
        calc,
        temperatures_K=ip.get("temperatures_K", [0.0, 300.0, 1000.0, 1500.0]),
        displacement_A=ip.get("displacement_A", 0.01),
        q_mesh=tuple(ip.get("q_mesh", (4, 4, 4))),
    )

    result = {
        "temperatures_K": pr.temperatures_K,
        "F_vib_eV_per_atom": pr.F_vib_eV_per_atom,
        "n_imaginary_modes": pr.n_imaginary_modes,
        "is_dynamically_stable": pr.is_dynamically_stable,
        "phonon_dos_omega_THz": pr.phonon_dos_omega_THz,
        "phonon_dos_g": pr.phonon_dos_g,
        "quality_tier": pr.quality_tier,
        "wall_time_s": pr.wall_time_s,
    }
    write_result(cache_key, result)
    repo = spec.get("results_repo")
    if repo:
        push_to_dataset(cache_key, repo)
    print(
        f"[phonon_harmonic] DONE cache_key={cache_key} "
        f"n_imag={pr.n_imaginary_modes} stable={pr.is_dynamically_stable}"
    )


if __name__ == "__main__":
    main()
