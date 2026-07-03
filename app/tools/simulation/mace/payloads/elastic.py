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
"""HF Job payload: compute_elastic."""

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

    from app.tools.simulation.mace.core.elastic import elastic_tensor, summarize_elastic
    from app.tools.simulation.mace.core.relax import relax

    atoms, _, _ = build_atoms(ip, seed)
    calc = make_calc_for(ip)
    # Pre-relax so reference stress is near zero.
    relax(atoms, calc, fmax=ip.get("relax_fmax_eV_per_A", 0.02), steps=200)
    import time

    t0 = time.time()
    C = elastic_tensor(atoms, calc, strain_amplitude=ip.get("strain_amplitude", 0.005))
    wall = time.time() - t0
    summary = summarize_elastic(C, wall_time_s=wall)

    result = {
        "C_GPa": summary.C_GPa,
        "K_VRH_GPa": summary.K_VRH_GPa,
        "G_VRH_GPa": summary.G_VRH_GPa,
        "E_Young_GPa": summary.E_Young_GPa,
        "nu_Poisson": summary.nu_Poisson,
        "pugh_G_over_B": summary.pugh_G_over_B,
        "cauchy_pressure_GPa": summary.cauchy_pressure_GPa,
        "pugh_verdict": summary.pugh_verdict,
        "am_manufacturability_passed": summary.am_manufacturability_passed,
        "wall_time_s": wall,
    }
    write_result(cache_key, result)
    repo = spec.get("results_repo")
    if repo:
        push_to_dataset(cache_key, repo)
    print(f"[compute_elastic] DONE cache_key={cache_key} G/B={summary.pugh_G_over_B:.3f}")


if __name__ == "__main__":
    main()
