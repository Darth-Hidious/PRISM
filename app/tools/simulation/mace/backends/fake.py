"""Deterministic in-memory calculator used by CI and unit tests.

No GPU, no MACE, no torch, no network. Energies come from a tiny lookup
table keyed by element + phase, with a small additive noise term seeded by
``BackendJob.seed`` so results are reproducible.

This is the only backend that runs in plain CPython without optional
dependencies; CI uses it exclusively.
"""

from __future__ import annotations

import hashlib
import math
import time
from typing import Any

from ..ids import canonical_json
from .base import Backend, BackendJob, ProgressCb


# Per-element pseudo-cohesive energies (eV/atom). Chosen to roughly preserve
# the relative ordering of refractories vs late-transition-metals vs
# Al/Fe/Ti, so tests can sanity-check "Mo binds tighter than Al" etc.
_E_REF: dict[str, float] = {
    "Mo": -10.8, "W": -12.8, "Ta": -11.7, "Nb": -10.0, "V": -9.0,
    "Fe": -8.4, "Ti": -7.9, "Hf": -9.0, "Zr": -8.5, "Al": -3.4,
}

# Per-phase scalar offsets (eV/atom).
_PHASE_OFFSET = {"bcc": 0.0, "fcc": 0.02, "hcp": 0.03, "c14_laves": -0.08}

# Per-element pseudo-atomic-volumes (Å³/atom).
_V_REF: dict[str, float] = {
    "Mo": 15.6, "W": 16.0, "Ta": 18.0, "Nb": 18.0, "V": 13.5,
    "Fe": 11.8, "Ti": 17.5, "Hf": 22.0, "Zr": 23.0, "Al": 16.6,
}


class FakeBackend(Backend):
    name = "fake"

    def __init__(self) -> None:
        self._cancelled: set[str] = set()

    # ------------------------------------------------------------------
    def execute(self, job: BackendJob, progress: ProgressCb | None = None) -> dict[str, Any]:
        # Dispatch on tool_name. Every branch yields a dict that matches the
        # corresponding *Result schema (plus 'cif_text' / 'traj_json' /
        # 'backend_details' that the runner consumes before writing cache).
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
        raise ValueError(f"FakeBackend does not implement tool {tool!r}")

    def cancel(self, job_id: str) -> None:
        self._cancelled.add(job_id)

    # ------------------------------------------------------------------
    # Per-tool deterministic stubs
    # ------------------------------------------------------------------
    def _e_v(self, composition: dict[str, int], phase: str, seed: int) -> tuple[float, float]:
        n = sum(composition.values())
        e_mix = sum(_E_REF[el] * c for el, c in composition.items()) / n
        v_mix = sum(_V_REF[el] * c for el, c in composition.items()) / n
        # Deterministic noise from canonical input
        noise_seed = hashlib.sha256(
            canonical_json({"comp": composition, "phase": phase, "seed": seed})
        ).digest()
        noise_e = (int.from_bytes(noise_seed[:4], "big") / 2**32 - 0.5) * 0.02  # ±10 meV
        noise_v = (int.from_bytes(noise_seed[4:8], "big") / 2**32 - 0.5) * 0.2  # ±0.1 Å³
        return e_mix + _PHASE_OFFSET[phase] + noise_e, v_mix + noise_v

    def _relax(self, job: BackendJob, progress: ProgressCb | None) -> dict[str, Any]:
        ip = job.input_payload
        comp = ip["composition"]["atoms"] if "composition" in ip else ip["matrix_composition"]["atoms"]
        phase = ip.get("phase", "bcc")
        e, v = self._e_v(comp, phase, job.seed)
        n_steps = 25
        if progress is not None:
            for s in range(0, n_steps + 1, 5):
                if job.cache_key in self._cancelled:
                    raise InterruptedError("cancelled")
                progress(100.0 * s / n_steps, f"step {s}/{n_steps}", s, n_steps)
                time.sleep(0.001)
        return {
            "energy_per_atom_eV": float(e),
            "volume_per_atom_A3": float(v),
            "lattice_a_eff_A": float((2.0 * v) ** (1 / 3)),
            "n_steps": int(n_steps),
            "fmax_final_eV_per_A": 0.04,
            "head": ip.get("options", {}).get("head", "omat_pbe"),
            "wall_time_s": 0.05,
            "cif_text": _stub_cif(comp, phase),
            "structure_summary": {
                "composition": comp,
                "phase": phase,
                "n_atoms": sum(comp.values()),
            },
            "backend_details": {"backend": "fake", "seed": job.seed},
        }

    def _elastic(self, job: BackendJob, progress: ProgressCb | None) -> dict[str, Any]:
        ip = job.input_payload
        # Use a plausible canonical BCC stiffness tensor as a baseline,
        # scaled by the mean cohesive energy magnitude per atom.
        comp = self._resolve_composition(ip)
        e, _ = self._e_v(comp, "bcc", job.seed)
        scale = abs(e) / 10.0  # ~1 for refractories, ~0.34 for Al-rich
        C = [
            [400 * scale, 150 * scale, 150 * scale, 0, 0, 0],
            [150 * scale, 400 * scale, 150 * scale, 0, 0, 0],
            [150 * scale, 150 * scale, 400 * scale, 0, 0, 0],
            [0, 0, 0, 120 * scale, 0, 0],
            [0, 0, 0, 0, 120 * scale, 0],
            [0, 0, 0, 0, 0, 120 * scale],
        ]
        if progress is not None:
            for s in range(0, 7):
                if job.cache_key in self._cancelled:
                    raise InterruptedError("cancelled")
                progress(100.0 * s / 6, f"strain {s}/6", s, 6)
        # VRH derived in elastic module would give:
        K = 233.33 * scale
        G = 119.0 * scale  # rough VRH for these constants
        E_Y = 9 * K * G / (3 * K + G) if (3 * K + G) else 0.0
        nu = (3 * K - 2 * G) / (2 * (3 * K + G)) if (3 * K + G) else 0.0
        pugh = G / K if K else 0.0
        return {
            "C_GPa": C,
            "K_VRH_GPa": float(K),
            "G_VRH_GPa": float(G),
            "E_Young_GPa": float(E_Y),
            "nu_Poisson": float(nu),
            "pugh_G_over_B": float(pugh),
            "cauchy_pressure_GPa": float(150 * scale - 120 * scale),
            "pugh_verdict": "ductile" if pugh < 0.57 else "brittle",
            "am_manufacturability_passed": pugh < 0.402,
            "wall_time_s": 0.05,
            "backend_details": {"backend": "fake"},
        }

    def _dilute(self, job: BackendJob, progress: ProgressCb | None) -> dict[str, Any]:
        ip = job.input_payload
        comp = ip["matrix_composition"]["atoms"]
        phase = ip.get("matrix_phase", "bcc")
        solute = ip["solute_element"]
        displaced = ip.get("displaced_element")
        if displaced is None:
            displaced = max((el for el in comp if el != solute), key=lambda el: comp[el])
        e_matrix, _ = self._e_v(comp, phase, job.seed)
        # Deterministic pseudo-mu: just use the element table.
        mu_sol = _E_REF.get(solute, -7.0)
        mu_disp = _E_REF.get(displaced, -7.0)
        # E_sub heuristic: difference of (alloy E w/ swap) - (alloy E) is
        # ~ (mu_disp - mu_sol) modulo an alloying chemical term.
        alloying = -0.15 * math.tanh((mu_sol - mu_disp) / 2.0)
        e_sub = (mu_sol - mu_disp) + alloying
        if progress is not None:
            for s in range(0, 11):
                if job.cache_key in self._cancelled:
                    raise InterruptedError("cancelled")
                progress(s * 10.0, f"step {s}/10", s, 10)
        return {
            "E_sub_eV": float(e_sub),
            "verdict": "favourable" if e_sub < 0 else "unfavourable",
            "matrix_E_per_atom_eV": float(e_matrix),
            "pure_solute_E_per_atom_eV": float(mu_sol),
            "pure_displaced_E_per_atom_eV": float(mu_disp),
            "solute_element": solute,
            "displaced_element": displaced,
            "wall_time_s": 0.05,
            "backend_details": {"backend": "fake"},
        }

    def _md(self, job: BackendJob, progress: ProgressCb | None) -> dict[str, Any]:
        ip = job.input_payload
        comp = self._resolve_composition(ip)
        phase = self._resolve_phase(ip)
        e, _ = self._e_v(comp, phase, job.seed)
        T = ip["T_K"]
        n_steps = ip.get("n_steps", 1000)
        if progress is not None:
            for s in range(0, 11):
                if job.cache_key in self._cancelled:
                    raise InterruptedError("cancelled")
                progress(s * 10.0, f"md step {s * n_steps // 10}/{n_steps}", s, 10)
        # Energy rises slightly with T (kT in eV/atom-ish)
        e_at_T = e + 3 * 8.617e-5 * T
        r_centers = [0.1 * i for i in range(80)]
        # Fake g(r): peak near a/√2 ≈ 2.5 Å for BCC
        g = [math.exp(-((r - 2.5) / 0.3) ** 2) for r in r_centers]
        return {
            "mean_E_per_atom_eV": float(e_at_T),
            "std_E_per_atom_eV": 0.005,
            "mean_T_K": float(T),
            "rdf_r_A": r_centers,
            "rdf_g": g,
            "is_dynamically_stable": True,
            "wall_time_s": 0.05,
            "cif_text": _stub_cif(comp, phase),
            "traj_json": {"steps": [0, n_steps], "energies": [e, e_at_T]},
            "backend_details": {"backend": "fake"},
        }

    def _phonon(self, job: BackendJob, progress: ProgressCb | None) -> dict[str, Any]:
        ip = job.input_payload
        temps = ip.get("temperatures_K", [0.0, 300.0, 1000.0, 1500.0])
        if progress is not None:
            for s in range(0, 11):
                if job.cache_key in self._cancelled:
                    raise InterruptedError("cancelled")
                progress(s * 10.0, "phonon displacement", s, 10)
        # F_vib(T) heuristic: -kT * 3 ln(T) per atom is roughly Debye-like
        f_vib = [-3 * 8.617e-5 * T * max(math.log(max(T, 1.0)), 0.0) for T in temps]
        return {
            "temperatures_K": list(temps),
            "F_vib_eV_per_atom": f_vib,
            "n_imaginary_modes": 0,
            "is_dynamically_stable": True,
            "phonon_dos_omega_THz": [i * 0.5 for i in range(20)],
            "phonon_dos_g": [math.sin(i * 0.3) ** 2 for i in range(20)],
            "quality_tier": "harmonic_supercell_identity_4q",
            "wall_time_s": 0.05,
            "backend_details": {"backend": "fake"},
        }

    # ------------------------------------------------------------------
    @staticmethod
    def _resolve_composition(ip: dict[str, Any]) -> dict[str, int]:
        if "composition" in ip:
            return ip["composition"]["atoms"]
        if "matrix_composition" in ip:
            return ip["matrix_composition"]["atoms"]
        if "structure" in ip and ip["structure"].get("composition"):
            return ip["structure"]["composition"]["atoms"]
        # Fallback for cache_ref-only structures: use a neutral default
        return {"Fe": 50, "Ti": 50}

    @staticmethod
    def _resolve_phase(ip: dict[str, Any]) -> str:
        if "phase" in ip:
            return ip["phase"]
        if "matrix_phase" in ip:
            return ip["matrix_phase"]
        if "structure" in ip and ip["structure"].get("phase"):
            return ip["structure"]["phase"]
        return "bcc"


def _stub_cif(composition: dict[str, int], phase: str) -> str:
    """Minimal placeholder CIF the fake backend writes to cache."""
    n = sum(composition.values())
    parts = ", ".join(f"{k}{v}" for k, v in sorted(composition.items()))
    return (
        f"# Stub CIF generated by FakeBackend (no actual coords)\n"
        f"# phase: {phase}\n"
        f"# composition: {parts}\n"
        f"# n_atoms: {n}\n"
        "data_fake\n"
        f"_chemical_formula_sum '{parts}'\n"
        "_symmetry_space_group_name_H-M 'P 1'\n"
        "_cell_length_a 10.0\n_cell_length_b 10.0\n_cell_length_c 10.0\n"
        "_cell_angle_alpha 90\n_cell_angle_beta 90\n_cell_angle_gamma 90\n"
    )
