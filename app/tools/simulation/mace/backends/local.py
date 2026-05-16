"""LocalBackend — real MACE in-process.

Used for small cells (N≤30) when GPU is unavailable. All five primitives
are dispatched to ``mace_core`` directly.

mace-torch / ase / phonopy are imported lazily — instantiating this class
does not require them, but calling :meth:`LocalBackend.execute` does.
"""

from __future__ import annotations

import io
import time
from typing import Any

import numpy as np

from .base import Backend, BackendJob, ProgressCb


class LocalBackend(Backend):
    name = "local"

    def __init__(self) -> None:
        self._cancelled: set[str] = set()

    def cancel(self, job_id: str) -> None:
        self._cancelled.add(job_id)

    # ------------------------------------------------------------------
    def execute(self, job: BackendJob, progress: ProgressCb | None = None) -> dict[str, Any]:
        tool = job.tool_name
        if tool == "relax_structure":
            return self._relax(job, progress)
        if tool == "compute_elastic":
            return self._elastic(job, progress)
        if tool == "compute_dilute_solute":
            return self._dilute(job, progress)
        if tool == "md_equilibrate":
            return self._md(job, progress)
        if tool == "phonon_harmonic":
            return self._phonon(job, progress)
        raise ValueError(f"LocalBackend does not implement tool {tool!r}")

    # ------------------------------------------------------------------
    def _make_calc(self, ip: dict[str, Any]):
        from app.tools.simulation.mace.core.calculator import make_calc

        opts = ip.get("options", {})
        head = opts.get("head", "omat_pbe")
        dtype = opts.get("dtype", "float64")
        dev_pref = opts.get("device_preference", "auto")
        device = None if dev_pref == "auto" else dev_pref
        return make_calc(head=head, device=device, dtype=dtype)

    def _build_supercell(self, comp: dict[str, int], phase: str, seed: int):
        from app.tools.simulation.mace.core.builders import build_supercell, build_c14_laves

        if phase == "c14_laves":
            # composition for c14_laves is expected to encode {big, small}
            # — but in the MCP schema we pass a real composition map. As a
            # convention, the most-abundant element is the "small" (Fe-like)
            # and the next is the "big" (Nb / Ta). This is a heuristic; the
            # caller can also pass the structure via cache_ref to bypass.
            sorted_els = sorted(comp.items(), key=lambda kv: -kv[1])
            small = sorted_els[0][0]
            big = sorted_els[1][0] if len(sorted_els) > 1 else "Nb"
            return build_c14_laves(big_atom=big, small_atom=small)
        rng = np.random.default_rng(seed)
        return build_supercell(comp, phase, rng=rng)

    def _atoms_from_input(self, ip: dict[str, Any], seed: int):
        if "composition" in ip:
            comp = ip["composition"]["atoms"]
            phase = ip.get("phase", "bcc")
            return self._build_supercell(comp, phase, seed), comp, phase
        if "matrix_composition" in ip:
            comp = ip["matrix_composition"]["atoms"]
            phase = ip.get("matrix_phase", "bcc")
            return self._build_supercell(comp, phase, seed), comp, phase
        if "structure" in ip:
            s = ip["structure"]
            if s.get("composition"):
                comp = s["composition"]["atoms"]
                phase = s.get("phase", "bcc")
                return self._build_supercell(comp, phase, seed), comp, phase
            raise NotImplementedError(
                "LocalBackend cannot yet hydrate structures from cache_ref; "
                "pass an inline composition + phase or use HfJobsBackend."
            )
        raise ValueError("input has no composition / matrix_composition / structure")

    # ------------------------------------------------------------------
    def _cif_text(self, atoms) -> str:
        from ase.io import write as ase_write

        atoms_clean = atoms.copy()
        atoms_clean.calc = None
        # ASE's CIF writer is text in some versions, bytes in others
        # (e.g. fd.write(loop.tostring()) emits bytes -> a StringIO
        # rejects it with "string argument expected, got 'bytes'").
        # Be version-robust: prefer a text buffer, fall back to binary.
        try:
            buf = io.StringIO()
            ase_write(buf, atoms_clean, format="cif")
            return buf.getvalue()
        except TypeError:
            bbuf = io.BytesIO()
            ase_write(bbuf, atoms_clean, format="cif")
            return bbuf.getvalue().decode("utf-8")

    # ------------------------------------------------------------------
    def _relax(self, job: BackendJob, progress: ProgressCb | None) -> dict[str, Any]:
        from app.tools.simulation.mace.core.relax import relax

        ip = job.input_payload
        atoms, comp, phase = self._atoms_from_input(ip, job.seed)
        calc = self._make_calc(ip)
        fmax = ip.get("fmax_eV_per_A", 0.05)
        max_steps = ip.get("max_steps", 200)

        prog_cb = None
        if progress is not None:
            def prog_cb(step: int, total: int, fmax_now: float):
                if job.cache_key in self._cancelled:
                    raise InterruptedError("cancelled")
                pct = min(99.0, 100.0 * step / max(total, 1))
                progress(pct, f"step {step}/{total} fmax={fmax_now:.3f}", step, total)

        t0 = time.time()
        rr = relax(atoms, calc, fmax=fmax, steps=max_steps, progress=prog_cb)
        wall = time.time() - t0
        head = ip.get("options", {}).get("head", "omat_pbe")
        return {
            "energy_per_atom_eV": rr.energy_per_atom_eV,
            "volume_per_atom_A3": rr.volume_per_atom_A3,
            "lattice_a_eff_A": rr.lattice_a_eff_A,
            "n_steps": rr.n_steps,
            "fmax_final_eV_per_A": rr.fmax_final_eV_per_A,
            "head": head,
            "wall_time_s": wall,
            "cif_text": self._cif_text(atoms),
            "structure_summary": {"composition": comp, "phase": phase, "n_atoms": len(atoms)},
            "backend_details": {"backend": "local", "n_atoms": len(atoms)},
        }

    # ------------------------------------------------------------------
    def _elastic(self, job: BackendJob, progress: ProgressCb | None) -> dict[str, Any]:
        from app.tools.simulation.mace.core.elastic import elastic_tensor, summarize_elastic

        ip = job.input_payload
        atoms, _, _ = self._atoms_from_input(ip, job.seed)
        calc = self._make_calc(ip)
        atoms.calc = calc
        # First relax (cheap) so reference stress is near zero.
        from app.tools.simulation.mace.core.relax import relax as _relax_fn

        _relax_fn(atoms, calc, fmax=ip.get("relax_fmax_eV_per_A", 0.02), steps=200)
        t0 = time.time()
        C = elastic_tensor(atoms, calc, strain_amplitude=ip.get("strain_amplitude", 0.005))
        wall = time.time() - t0
        if progress is not None:
            progress(100.0, "elastic done", 6, 6)
        result = summarize_elastic(C, wall_time_s=wall)
        return {
            "C_GPa": result.C_GPa,
            "K_VRH_GPa": result.K_VRH_GPa,
            "G_VRH_GPa": result.G_VRH_GPa,
            "E_Young_GPa": result.E_Young_GPa,
            "nu_Poisson": result.nu_Poisson,
            "pugh_G_over_B": result.pugh_G_over_B,
            "cauchy_pressure_GPa": result.cauchy_pressure_GPa,
            "pugh_verdict": result.pugh_verdict,
            "am_manufacturability_passed": result.am_manufacturability_passed,
            "wall_time_s": wall,
            "backend_details": {"backend": "local"},
        }

    # ------------------------------------------------------------------
    def _dilute(self, job: BackendJob, progress: ProgressCb | None) -> dict[str, Any]:
        from app.tools.simulation.mace.core.relax import relax
        from app.tools.simulation.mace.core.compositions import pure_element_energy

        ip = job.input_payload
        atoms, comp, phase = self._atoms_from_input(ip, job.seed)
        calc = self._make_calc(ip)
        atoms.calc = calc
        relax(atoms, calc, fmax=0.05, steps=200)
        e_matrix = float(atoms.get_potential_energy() / len(atoms))
        solute = ip["solute_element"]
        displaced = ip.get("displaced_element")
        if displaced is None:
            displaced = max((el for el in comp if el != solute), key=lambda el: comp[el])

        # Substitute the first atom of `displaced` with `solute`
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

        return {
            "E_sub_eV": float(e_sub),
            "verdict": "favourable" if e_sub < 0 else "unfavourable",
            "matrix_E_per_atom_eV": e_matrix,
            "pure_solute_E_per_atom_eV": mu_sol,
            "pure_displaced_E_per_atom_eV": mu_disp,
            "solute_element": solute,
            "displaced_element": displaced,
            "wall_time_s": 0.0,
            "backend_details": {"backend": "local"},
        }

    # ------------------------------------------------------------------
    def _md(self, job: BackendJob, progress: ProgressCb | None) -> dict[str, Any]:
        from app.tools.simulation.mace.core.md import langevin_run

        ip = job.input_payload
        atoms, comp, phase = self._atoms_from_input(ip, job.seed)
        calc = self._make_calc(ip)
        # Pre-relax
        from app.tools.simulation.mace.core.relax import relax as _relax_fn

        _relax_fn(atoms, calc, fmax=0.05, steps=200)
        prog_cb = None
        if progress is not None:
            def prog_cb(step: int, total: int):
                if job.cache_key in self._cancelled:
                    raise InterruptedError("cancelled")
                progress(min(99.0, 100.0 * step / max(total, 1)), f"md {step}/{total}", step, total)

        mdr = langevin_run(
            atoms,
            calc,
            T_K=ip["T_K"],
            n_steps=ip.get("n_steps", 1000),
            timestep_fs=ip.get("timestep_fs", 1.0),
            friction_per_fs=ip.get("friction_per_fs", 0.01),
            sample_every=ip.get("sample_every"),
            seed=job.seed,
            progress=prog_cb,
        )
        return {
            "mean_E_per_atom_eV": mdr.mean_E_per_atom_eV,
            "std_E_per_atom_eV": mdr.std_E_per_atom_eV,
            "mean_T_K": mdr.mean_T_K,
            "rdf_r_A": mdr.rdf_r_A,
            "rdf_g": mdr.rdf_g,
            "is_dynamically_stable": mdr.is_dynamically_stable,
            "wall_time_s": mdr.wall_time_s,
            "cif_text": self._cif_text(mdr.final_atoms),
            "traj_json": {"steps": mdr.steps, "energies": mdr.energies, "temperatures": mdr.temperatures},
            "backend_details": {"backend": "local"},
        }

    # ------------------------------------------------------------------
    def _phonon(self, job: BackendJob, progress: ProgressCb | None) -> dict[str, Any]:
        from app.tools.simulation.mace.core.phonons import harmonic_free_energy

        ip = job.input_payload
        atoms, _, _ = self._atoms_from_input(ip, job.seed)
        calc = self._make_calc(ip)
        # Pre-relax
        from app.tools.simulation.mace.core.relax import relax as _relax_fn

        _relax_fn(atoms, calc, fmax=0.02, steps=200)
        prog_cb = None
        if progress is not None:
            def prog_cb(step: int, total: int):
                if job.cache_key in self._cancelled:
                    raise InterruptedError("cancelled")
                progress(min(99.0, 100.0 * step / max(total, 1)), f"displacement {step}/{total}", step, total)
        pr = harmonic_free_energy(
            atoms,
            calc,
            temperatures_K=ip.get("temperatures_K", [0.0, 300.0, 1000.0, 1500.0]),
            displacement_A=ip.get("displacement_A", 0.01),
            q_mesh=tuple(ip.get("q_mesh", (4, 4, 4))),
            progress=prog_cb,
        )
        return {
            "temperatures_K": pr.temperatures_K,
            "F_vib_eV_per_atom": pr.F_vib_eV_per_atom,
            "n_imaginary_modes": pr.n_imaginary_modes,
            "is_dynamically_stable": pr.is_dynamically_stable,
            "phonon_dos_omega_THz": pr.phonon_dos_omega_THz,
            "phonon_dos_g": pr.phonon_dos_g,
            "quality_tier": pr.quality_tier,
            "wall_time_s": pr.wall_time_s,
            "backend_details": {"backend": "local"},
        }
