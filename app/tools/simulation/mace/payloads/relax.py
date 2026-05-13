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
"""HF Job payload: relax_structure."""

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
    )

    ensure_mace_core()
    spec = read_spec(sys.argv[1])
    cache_key = spec["cache_key"]
    ip = spec["input_payload"]
    seed = int(spec.get("seed", 20260506))

    from app.tools.simulation.mace.core.relax import relax

    atoms, comp, phase = build_atoms(ip, seed)
    calc = make_calc_for(ip)
    fmax = ip.get("fmax_eV_per_A", 0.05)
    max_steps = ip.get("max_steps", 200)
    rr = relax(atoms, calc, fmax=fmax, steps=max_steps)

    head = ip.get("options", {}).get("head", "omat_pbe")
    result = {
        "energy_per_atom_eV": rr.energy_per_atom_eV,
        "volume_per_atom_A3": rr.volume_per_atom_A3,
        "lattice_a_eff_A": rr.lattice_a_eff_A,
        "n_steps": rr.n_steps,
        "fmax_final_eV_per_A": rr.fmax_final_eV_per_A,
        "head": head,
        "wall_time_s": rr.wall_time_s,
        "structure_summary": {
            "composition": comp,
            "phase": phase,
            "n_atoms": len(atoms),
        },
    }
    write_result(cache_key, result)
    write_cif(cache_key, cif_text(atoms))
    repo = spec.get("results_repo")
    if repo:
        push_to_dataset(cache_key, repo)
    print(f"[relax_structure] DONE cache_key={cache_key} E={rr.energy_per_atom_eV:.5f} eV/atom")


if __name__ == "__main__":
    main()
