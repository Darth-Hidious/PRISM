"""Quantum ESPRESSO as a standard run.

Resolves the pw.x binary, the pseudopotential set and the run defaults, and
runs one calculation end to end — write, run, parse — with execution evidence
and full provenance. Resolution order everywhere is environment, then the
user's ``[qe]`` config table, then the PRISM home, then ``PATH``; nothing is
guessed, and "not found" is reported as such.

QE is provisioned into ``~/.prism/qe`` by ``prism provision qe`` (built from
source; there is no Homebrew formula and conda-forge ships no arm64 build),
and pseudopotentials into ``~/.prism/pseudopotentials/<set>`` with a
``MANIFEST.json`` naming the set, its URL, checksum and licence. That
manifest travels into every run's provenance.
"""
from __future__ import annotations

import json
import math
import os
import shutil
import subprocess
import time
from pathlib import Path
from typing import Any, Mapping, Optional

from app.tools.evidence import EvidenceSource, stamp_evidence
from app.tools.simulation.qe.input_writer import write_input
from app.tools.simulation.qe.output_parser import parse_output
from app.tools.simulation.qe.pseudos import (
    PseudopotentialNotFoundError,
    resolve_pseudopotentials,
)

RY_TO_EV = 13.605693122994

DEFAULT_PSEUDO_SET = "pseudo-dojo-nc-sr-04-pbe-standard"

# When no hint exists for a species and the caller gave no cutoff. Measured
# 2026-09-05: 60 Ry against PseudoDojo's Ni hint of 49 Ha (98 Ry) gave a
# -337 GPa stress at the experimental lattice constant. Hence a fallback that
# is always declared "unverified", never a silent default.
FALLBACK_ECUTWFC_RY = 60.0

DEFAULTS: dict[str, Any] = {
    # None = derive from the pseudopotential set's own per-element hints
    # (MANIFEST.json `hints_ha`, normal accuracy, Ha -> Ry). A number here or
    # per run is an explicit choice and wins.
    "ecutwfc_ry": None,
    "ecutrho_ratio": 4.0,
    "kspacing_inv_angstrom": 0.15,
    "smearing": "mv",
    "degauss_ry": 0.01,
}


def _home() -> Path:
    return Path(os.environ.get("HOME", str(Path.home())))


def find_pw_x(config: Mapping[str, Any]) -> Optional[Path]:
    """The pw.x binary: env, then config, then the PRISM home, then PATH."""
    env = os.environ.get("PRISM_QE_PW", "").strip()
    if env:
        p = Path(env)
        if p.is_file():
            return p
    configured = str(config.get("pw_path") or "").strip()
    if configured:
        p = Path(configured).expanduser()
        if p.is_file():
            return p
    home_pw = _home() / ".prism" / "qe" / "bin" / "pw.x"
    if home_pw.is_file():
        return home_pw
    on_path = shutil.which("pw.x")
    return Path(on_path) if on_path else None


def find_mpirun() -> Optional[Path]:
    env = os.environ.get("PRISM_QE_MPIRUN", "").strip()
    if env and Path(env).is_file():
        return Path(env)
    on_path = shutil.which("mpirun")
    return Path(on_path) if on_path else None


def find_pseudo_dir(config: Mapping[str, Any]) -> Optional[Path]:
    """The UPF directory: env, then config, then the PRISM home's default set."""
    env = os.environ.get("PRISM_QE_PSEUDO_DIR", "").strip()
    if env and Path(env).is_dir():
        return Path(env)
    configured = str(config.get("pseudo_dir") or "").strip()
    if configured and Path(configured).expanduser().is_dir():
        return Path(configured).expanduser()
    home_set = _home() / ".prism" / "pseudopotentials" / DEFAULT_PSEUDO_SET
    if home_set.is_dir():
        return home_set
    return None


def pseudo_manifest(pseudo_dir: Optional[Path]) -> dict[str, Any]:
    """The set's MANIFEST.json (name, URL, checksum, licence), or an honest note."""
    if pseudo_dir is None:
        return {"note": "no pseudopotential directory resolved"}
    path = pseudo_dir / "MANIFEST.json"
    if not path.is_file():
        return {"note": f"{pseudo_dir} carries no MANIFEST.json — provenance of these UPF files is unknown"}
    try:
        return json.loads(path.read_text())
    except Exception as exc:  # pragma: no cover — a corrupt manifest is still reported
        return {"note": f"MANIFEST.json unreadable: {exc}"}


SETTINGS_KEYS = ("pw_path", "pseudo_dir", "ecutwfc_ry", "ecutrho_ratio", "kspacing_inv_angstrom", "smearing", "degauss_ry", "nproc")


def settings_path() -> Path:
    """`~/.prism/qe/settings.toml` — QE's own file, so editing it never
    rewrites the user's main config and loses its comments."""
    return _home() / ".prism" / "qe" / "settings.toml"


def load_settings_file() -> dict[str, Any]:
    """The persisted QE settings, or {} when none were saved."""
    path = settings_path()
    if not path.is_file():
        return {}
    import tomllib

    try:
        return dict(tomllib.loads(path.read_text()))
    except Exception as exc:
        return {"_error": f"{path}: {exc}"}


def save_settings_file(values: Mapping[str, Any]) -> Path:
    """Write the flat QE table. Unknown keys are refused by name; the file is
    the only thing rewritten."""
    unknown = sorted(set(values) - set(SETTINGS_KEYS))
    if unknown:
        raise ValueError(f"unknown QE settings {unknown}; known: {list(SETTINGS_KEYS)}")
    path = settings_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    lines = ["# Quantum ESPRESSO run settings — edited by `prism qe settings` and the palette."]
    for key in SETTINGS_KEYS:
        if key not in values or values[key] is None:
            continue
        v = values[key]
        if isinstance(v, bool):
            lines.append(f"{key} = {'true' if v else 'false'}")
        elif isinstance(v, (int, float)):
            lines.append(f"{key} = {v}")
        else:
            lines.append(f'{key} = "{str(v).replace(chr(34), chr(39))}"')
    path.write_text("\n".join(lines) + "\n")
    return path


def coerce_setting(key: str, raw: str) -> Any:
    """`--set key=value` strings into the setting's type."""
    if key not in SETTINGS_KEYS:
        raise ValueError(f"unknown QE setting {key!r}; known: {list(SETTINGS_KEYS)}")
    if key in ("pw_path", "pseudo_dir", "smearing"):
        return str(raw)
    if key == "nproc":
        return int(raw)
    return float(raw)


def settings(config: Mapping[str, Any]) -> dict[str, Any]:
    """Effective run settings: defaults, then the saved settings file, then the
    caller's config (a `[qe]` table or per-run overrides)."""
    saved = {k: v for k, v in load_settings_file().items() if not k.startswith("_")}
    merged: dict[str, Any] = {**saved, **{k: v for k, v in config.items() if v is not None}}
    config = merged
    out: dict[str, Any] = dict(DEFAULTS)
    for key in DEFAULTS:
        if key in config and config[key] is not None:
            if DEFAULTS[key] is None:
                out[key] = float(config[key])
            else:
                out[key] = type(DEFAULTS[key])(config[key]) if not isinstance(DEFAULTS[key], str) else str(config[key])
    nproc = config.get("nproc")
    out["nproc"] = int(nproc) if nproc else max(1, os.cpu_count() or 1)
    pw = find_pw_x(config)
    pseudo = find_pseudo_dir(config)
    out["pw_path"] = str(pw) if pw else None
    out["pseudo_dir"] = str(pseudo) if pseudo else None
    out["pseudopotential_set"] = pseudo_manifest(pseudo)
    mpirun = find_mpirun()
    out["mpirun"] = str(mpirun) if mpirun else None
    return out


def cutoff_for(species: list[str], pseudo_dir: Path, explicit: Optional[float]) -> tuple[float, str]:
    """The wavefunction cutoff (Ry) for these species and where it came from:
    the caller's number; else the set's own hints (the largest 'normal'
    hint over the species, Ha -> Ry, rounded up); else a fallback that is
    declared unverified."""
    if explicit is not None:
        return float(explicit), "caller"
    hints = pseudo_manifest(Path(pseudo_dir)).get("hints_ha") or {}
    missing = [el for el in species if not (hints.get(el) or {}).get("normal")]
    if missing:
        return FALLBACK_ECUTWFC_RY, (
            f"fallback {FALLBACK_ECUTWFC_RY:g} Ry — no cutoff hint for {', '.join(missing)} in the set's "
            "manifest; convergence unverified (run_convergence_test, or pass ecutwfc_ry)"
        )
    per = {el: float(hints[el]["normal"]) for el in species}
    ecut_ry = float(math.ceil(max(per.values()) * 2.0))
    detail = ", ".join(f"{el} {ha:g} Ha" for el, ha in sorted(per.items()))
    return ecut_ry, f"the set's hints, normal accuracy ({detail}); largest, in Ry"


def kpoints_for(structure, kspacing_inv_angstrom: float) -> tuple[int, int, int]:
    """Monkhorst-Pack grid from a reciprocal spacing (Å⁻¹): ceil(|b_i| / spacing), at least 1.

    ``|b_i| = 2π / d_i`` with ``d_i`` the real-space plane spacing, so a
    cubic a = 3.52 Å cell at 0.15 Å⁻¹ gets 12 divisions.
    """
    lattice = structure.lattice
    spacing = max(float(kspacing_inv_angstrom), 1e-9)
    recip = lattice.reciprocal_lattice  # includes the 2π factor
    return tuple(max(1, int(math.ceil(length / spacing))) for length in recip.abc)  # type: ignore[return-value]


def status(config: Mapping[str, Any]) -> dict[str, Any]:
    """What is provisioned and what is not — the answer to `prism qe status`."""
    s = settings(config)
    ready = bool(s["pw_path"]) and bool(s["pseudo_dir"])
    return {
        "ready": ready,
        "pw_x": s["pw_path"],
        "mpirun": s["mpirun"],
        "pseudo_dir": s["pseudo_dir"],
        "pseudopotential_set": s["pseudopotential_set"],
        "defaults": {k: s[k] for k in ("ecutwfc_ry", "ecutrho_ratio", "kspacing_inv_angstrom", "smearing", "degauss_ry", "nproc")},
        "remedy": None if ready else "prism provision qe (builds pw.x into ~/.prism/qe and fetches a pseudopotential set with a manifest)",
    }


def qe_run(
    structure,
    *,
    calculation: str,
    settings: Mapping[str, Any],
    workdir: Path,
    mpirun: Optional[str] = "auto",
    extra_input: Optional[Mapping[str, Mapping]] = None,
) -> dict[str, Any]:
    """Write a pw.x input for `structure`, run it, parse the output.

    Returns the parsed result with `evidence_class` (execution) and a
    `provenance` block: binary, pseudopotentials and their set manifest,
    cutoffs, k-points, smearing, processes, wall time, file paths. A missing
    binary or pseudopotential is a named `unavailable` result with a remedy,
    never a guess.
    """
    pw = settings.get("pw_path")
    if not pw or not Path(pw).is_file():
        return {
            "status": "unavailable",
            "reason": f"pw.x not found ({pw or 'nothing resolved'}); Quantum ESPRESSO is not provisioned on this machine",
            "remedy": "prism provision qe",
        }
    pseudo_dir = settings.get("pseudo_dir")
    if not pseudo_dir or not Path(pseudo_dir).is_dir():
        return {
            "status": "unavailable",
            "reason": "no pseudopotential directory resolved",
            "remedy": "prism provision qe (fetches a set with a manifest) or set [qe].pseudo_dir",
        }
    species = sorted({str(el) for el in structure.composition.elements})
    try:
        pseudos = resolve_pseudopotentials(species, pseudo_dir)
    except PseudopotentialNotFoundError as exc:
        return {"status": "unavailable", "reason": str(exc), "remedy": "add the missing UPF files to the set, or choose a set that covers these elements"}

    workdir = Path(workdir)
    workdir.mkdir(parents=True, exist_ok=True)
    ecutwfc, cutoff_source = cutoff_for(species, Path(pseudo_dir), settings.get("ecutwfc_ry"))
    cutoffs = {"ecutwfc": ecutwfc, "ecutrho": ecutwfc * float(settings["ecutrho_ratio"]), "source": cutoff_source}
    kpts = kpoints_for(structure, float(settings["kspacing_inv_angstrom"]))
    system_extra = {"occupations": "smearing", "smearing": str(settings["smearing"]), "degauss": float(settings["degauss_ry"])}
    merged_extra: dict[str, dict] = {"system": system_extra}
    for section, values in (extra_input or {}).items():
        merged_extra.setdefault(section, {}).update(dict(values))
    in_path = write_input(
        structure=structure,
        pseudopotentials=pseudos,
        cutoffs=cutoffs,
        kpoints=kpts,
        calculation_type=calculation,
        path=workdir / "pw.in",
        pseudo_dir=pseudo_dir,
        prefix="prism",
        extra_input=merged_extra,
    )
    out_path = workdir / "pw.out"
    nproc = int(settings.get("nproc") or 1)
    launcher = settings.get("mpirun") if mpirun == "auto" else mpirun
    cmd = [str(pw), "-in", str(in_path)]
    if launcher and nproc > 1:
        cmd = [str(launcher), "-np", str(nproc)] + cmd
    started = time.monotonic()
    with open(out_path, "w") as fh:
        proc = subprocess.run(cmd, stdout=fh, stderr=subprocess.PIPE, text=True, cwd=str(workdir))
    elapsed = time.monotonic() - started
    parsed = parse_output(out_path)
    result: dict[str, Any] = dict(parsed)
    stderr_tail = (proc.stderr or "").strip()[-400:]
    if proc.returncode != 0:
        # A non-zero exit is the diagnosis; the parser's reason for an empty
        # or truncated output is only its consequence. Measured 2026-09-05:
        # mpirun died in 54 ms, the parser said "no convergence marker", and
        # the exit code and stderr never reached the caller.
        why = f"pw.x exited {proc.returncode}: {stderr_tail or 'no stderr'}"
        if parsed.get("status") == "failed" and parsed.get("reason"):
            why = f"{why} — {parsed['reason']}"
        result = {"status": "failed", "converged": False, "reason": why}
    if result.get("status") == "failed":
        result["status"] = "failed"
    else:
        result["status"] = "ok"
        if "total_energy_ev" in result and result["total_energy_ev"] is not None:
            # parse_output reports the Ry total energy converted to eV already
            # when it can; guard the case where a parser variant returns Ry.
            pass
    result["provenance"] = {
        "pw_x": str(pw),
        "command": cmd,
        "pseudopotentials": dict(pseudos),
        "pseudo_dir": str(pseudo_dir),
        "pseudopotential_set": pseudo_manifest(Path(pseudo_dir)),
        "cutoffs": cutoffs,
        "kpoints": list(kpts),
        "smearing": system_extra,
        "calculation": calculation,
        "nproc": nproc if launcher and nproc > 1 else 1,
        "returncode": proc.returncode,
        "stderr_tail": stderr_tail,
        "wall_seconds": round(elapsed, 3),
        "input_path": str(in_path),
        "output_path": str(out_path),
    }
    stamp_evidence(result, EvidenceSource.EXECUTION if result["status"] == "ok" else EvidenceSource.MODEL_ASSERTION)
    return result
