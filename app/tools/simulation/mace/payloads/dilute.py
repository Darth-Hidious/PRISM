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
"""HF Job payload: compute_dilute_solute."""

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

    from app.tools.simulation.mace.core.compositions import pure_element_energy
    from app.tools.simulation.mace.core.relax import relax

    atoms, comp, phase = build_atoms(ip, seed)
    calc = make_calc_for(ip)
    atoms.calc = calc
    relax(atoms, calc, fmax=0.05, steps=200)
    e_matrix = float(atoms.get_potential_energy() / len(atoms))

    solute = ip["solute_element"]
    displaced = ip.get("displaced_element")
    if displaced is None:
        displaced = max((el for el in comp if el != solute), key=lambda el: comp[el])

    test = atoms.copy()
    symbols = list(test.get_chemical_symbols())
    idx = symbols.index(displaced)
    symbols[idx] = solute
    test.set_chemical_symbols(symbols)
    test.calc = calc
    relax(test, calc, fmax=0.05, steps=120)
    e_sub_total = float(test.get_potential_energy())
    e_alloy_total = e_matrix * len(atoms)

    mu_sol = pure_element_energy(solute, calc)
    mu_disp = pure_element_energy(displaced, calc)
    e_sub = (e_sub_total - e_alloy_total) - mu_sol + mu_disp

    result = {
        "E_sub_eV": float(e_sub),
        "verdict": "favourable" if e_sub < 0 else "unfavourable",
        "matrix_E_per_atom_eV": e_matrix,
        "pure_solute_E_per_atom_eV": mu_sol,
        "pure_displaced_E_per_atom_eV": mu_disp,
        "solute_element": solute,
        "displaced_element": displaced,
        "wall_time_s": 0.0,
    }
    write_result(cache_key, result)
    repo = spec.get("results_repo")
    if repo:
        push_to_dataset(cache_key, repo)
    print(f"[compute_dilute_solute] DONE cache_key={cache_key} E_sub={e_sub:+.3f} eV")


if __name__ == "__main__":
    main()
