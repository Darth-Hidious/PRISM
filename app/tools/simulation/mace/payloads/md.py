# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "mace-mcp>=0.1.0",
#   "mace-torch>=0.3.15",
#   "ase>=3.23",
#   "numpy>=1.24",
#   "torch>=2.1",
#   "huggingface_hub>=0.24",
# ]
# ///
"""HF Job payload: md_equilibrate."""

from __future__ import annotations

import sys


def main() -> None:
    from app.tools.simulation.mace.payloads._common import (
        build_atoms,
        cif_text,
        ensure_mace_core,
        make_calc_for,
        push_to_dataset,
        read_spec,
        write_cif,
        write_result,
        write_traj,
    )

    ensure_mace_core()
    spec = read_spec(sys.argv[1])
    cache_key = spec["cache_key"]
    ip = spec["input_payload"]
    seed = int(spec.get("seed", 20260506))

    from app.tools.simulation.mace.core.md import langevin_run
    from app.tools.simulation.mace.core.relax import relax

    atoms, _, _ = build_atoms(ip, seed)
    calc = make_calc_for(ip)
    relax(atoms, calc, fmax=0.05, steps=200)
    mdr = langevin_run(
        atoms,
        calc,
        T_K=ip["T_K"],
        n_steps=ip.get("n_steps", 1000),
        timestep_fs=ip.get("timestep_fs", 1.0),
        friction_per_fs=ip.get("friction_per_fs", 0.01),
        sample_every=ip.get("sample_every"),
        seed=seed,
    )

    result = {
        "mean_E_per_atom_eV": mdr.mean_E_per_atom_eV,
        "std_E_per_atom_eV": mdr.std_E_per_atom_eV,
        "mean_T_K": mdr.mean_T_K,
        "rdf_r_A": mdr.rdf_r_A,
        "rdf_g": mdr.rdf_g,
        "is_dynamically_stable": mdr.is_dynamically_stable,
        "wall_time_s": mdr.wall_time_s,
    }
    write_result(cache_key, result)
    write_cif(cache_key, cif_text(mdr.final_atoms))
    write_traj(
        cache_key,
        {"steps": mdr.steps, "energies": mdr.energies, "temperatures": mdr.temperatures},
    )
    repo = spec.get("results_repo")
    if repo:
        push_to_dataset(cache_key, repo)
    print(f"[md_equilibrate] DONE cache_key={cache_key} mean_E={mdr.mean_E_per_atom_eV:.4f} eV/atom")


if __name__ == "__main__":
    main()
