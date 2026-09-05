"""Agent tools for the pyiron-free Quantum ESPRESSO path.

Three tools:

- ``qe_resolve_pseudopotentials`` — map elements to UPF files in a directory
- ``qe_write_input``              — write a pw.x .in file from a structure
- ``qe_parse_output``             — parse a pw.x .out file into results

None of these runs pw.x: this machine has no QE binary. They generate the
input a real allocation can consume and read the output it produces.
Registration is gated in ``app/plugins/bootstrap.py`` on ase + pymatgen
being importable; when they are not, these tools do not appear in the
catalog at all (no always-erroring ghost registrations).
"""
from __future__ import annotations

from pathlib import Path
from typing import Union

from app.tools.base import Tool, ToolRegistry

from app.tools.simulation.qe.input_writer import CALCULATION_TYPES, write_input
from app.tools.simulation.qe.output_parser import parse_output
from app.tools.simulation.qe.pseudos import resolve_pseudopotentials
from app.tools.simulation.qe import runtime as qe_runtime


STRUCTURE_FORMS = (
    "a path to a file pymatgen can read (CIF/POSCAR/xyz), a cache:// reference from "
    "structure_import or a MACE job, or a structure as a dict or JSON text — either "
    "{lattice: 3x3 Å, species: [...], coords: [...], cartesian?: bool} or a pymatgen as_dict()"
)


def _structure_from_cache(ref: str):
    """A `cache://` reference from structure_import / a MACE job, read from the
    shared structure cache. Raises with the reason when it cannot be."""
    from pymatgen.core import Structure

    from app.tools.simulation.mace.cache import CacheStore
    from app.tools.simulation.mace.cache.hashing import parse_cache_uri
    from app.tools.simulation.mace.jobs.runner import get_cache_dir

    key, _kind = parse_cache_uri(ref)
    cif = CacheStore(get_cache_dir()).read_structure_cif(key)
    if cif is None:
        raise ValueError(f"no cached structure for {ref!r}")
    return Structure.from_str(cif, fmt="cif")


def _load_structure(spec: Union[str, Path, dict, object]):
    """Build a pymatgen Structure from a tool argument, in every form the
    tool's schema names (STRUCTURE_FORMS). Never invents coordinates.

    Measured 2026-09-05: the schema said "string", so the model sent
    {lattice, species, coords} as JSON text and the loader treated it as a
    filename ("Unrecognized extension"); a bare formula got the same error.
    """
    import json

    from pymatgen.core import Lattice, Structure

    if isinstance(spec, Structure):
        return spec
    if isinstance(spec, Path):
        return Structure.from_file(str(spec))
    if isinstance(spec, str):
        text = spec.strip()
        if text.startswith("cache://"):
            return _structure_from_cache(text)
        if text.startswith("{"):
            try:
                return _load_structure(json.loads(text))
            except json.JSONDecodeError as exc:
                raise ValueError(f"structure looks like JSON but does not parse: {exc}") from exc
        if Path(text).is_file():
            return Structure.from_file(text)
        raise ValueError(
            f"structure {spec!r} is neither a file that exists nor a structure: a formula alone "
            f"does not define one. Pass {STRUCTURE_FORMS}."
        )
    if isinstance(spec, dict):
        if "sites" in spec and ("lattice" in spec or "@module" in spec):
            return Structure.from_dict(spec)
        missing = [key for key in ("lattice", "species", "coords") if key not in spec]
        if missing:
            raise ValueError(
                f"structure dict missing {missing}; pass {STRUCTURE_FORMS}"
            )
        return Structure(
            Lattice(spec["lattice"]),
            spec["species"],
            spec["coords"],
            coords_are_cartesian=bool(spec.get("cartesian", False)),
        )
    raise ValueError(f"structure must be {STRUCTURE_FORMS}; got {type(spec).__name__}")


def _qe_resolve(**kwargs) -> dict:
    elements = kwargs.get("elements")
    directory = kwargs.get("directory")
    if not elements:
        return {"error": "'elements' (list of element symbols) is required"}
    if not directory:
        return {"error": "'directory' containing .upf files is required"}
    mapping = resolve_pseudopotentials(elements, directory)
    return {
        "status": "ok",
        "directory": str(directory),
        "pseudopotentials": mapping,
    }


def _qe_write_input(**kwargs) -> dict:
    structure = _load_structure(kwargs["structure"])
    species = sorted({str(el) for el in structure.composition.elements})

    pseudo_dir = kwargs.get("pseudo_dir")
    pseudopotentials = kwargs.get("pseudopotentials")
    if pseudopotentials is None:
        if not pseudo_dir:
            return {
                "error": "Provide either 'pseudopotentials' "
                         "({element: upf_filename}) or 'pseudo_dir' to "
                         "resolve UPF files from"
            }
        pseudopotentials = resolve_pseudopotentials(species, pseudo_dir)

    cutoffs = kwargs.get("cutoffs")
    if not cutoffs or not isinstance(cutoffs, dict):
        return {"error": "'cutoffs' dict with at least ecutwfc (Ry) is required"}

    kpoints = kwargs.get("kpoints")
    out_path = kwargs.get("output_path") or "qe_input.in"

    path = write_input(
        structure=structure,
        pseudopotentials=pseudopotentials,
        cutoffs=cutoffs,
        kpoints=kpoints,
        calculation_type=kwargs.get("calculation_type", "scf"),
        path=out_path,
        pseudo_dir=kwargs.get("pseudo_dir") or "./pseudo",
        prefix=kwargs.get("prefix", "prism"),
        extra_input=kwargs.get("extra_input"),
    )
    return {
        "status": "ok",
        "path": str(path),
        "n_atoms": len(structure),
        "formula": structure.composition.reduced_formula,
        "calculation_type": kwargs.get("calculation_type", "scf"),
        "pseudopotentials": dict(pseudopotentials),
    }


def _qe_parse_output(**kwargs) -> dict:
    path = kwargs.get("path")
    if not path:
        return {"error": "'path' to the pw.x output file is required"}
    result = parse_output(path)
    if result["status"] != "ok":
        # Uniform error contract: surface the failure as an error dict so the
        # model sees an explicit failure, never an unconverged number.
        return {
            "error": f"QE results unavailable: {result['reason']}",
            "converged": False,
        }
    return result


def _validate_parse_result(output: dict) -> str:
    """Scientific-validity gate (Tool contract, backlog D2): exit-0 parsing
    is not the same as usable results. A converged parse missing forces or
    stress is downgraded to 'warn' so downstream steps can branch."""
    if not output.get("converged"):
        return "invalid"
    if output.get("forces_ev_per_angstrom") is None or \
            output.get("stress_ev_per_angstrom3") is None:
        return "warn"
    return "ok"


_RESOLVE_DESCRIPTION = (
    "Map chemical elements to Quantum ESPRESSO pseudopotential (.upf) files "
    "found in a LOCAL directory. Returns {element: filename}. Errors "
    "explicitly when any element has no UPF file (and lists what is "
    "present). Never downloads anything — stage UPF files on disk first. "
    "Use before qe_write_input when you only know the pseudo directory."
)

_WRITE_DESCRIPTION = (
    "Write a Quantum ESPRESSO pw.x input (.in) file from a crystal "
    "structure: real &control/&system/&electrons namelists plus "
    "ATOMIC_SPECIES / ATOMIC_POSITIONS / K_POINTS cards, via ASE's "
    "espresso-in writer. Does NOT run pw.x (no QE binary here) — the file "
    "is what you submit to an HPC allocation. Inputs: `structure` (path to "
    "CIF/POSCAR/..., or a dict {lattice, species, coords, cartesian?}), "
    "`cutoffs` {ecutwfc (Ry), ecutrho (Ry, optional)}, `kpoints` "
    "[n1,n2,n3] Monkhorst-Pack grid (omit for Gamma-only), "
    "`calculation_type` scf|relax|vc-relax (default scf), and EITHER "
    "`pseudopotentials` {element: upf_filename} OR `pseudo_dir` to resolve "
    "them from. Output: `output_path` (default qe_input.in)."
)

_PARSE_DESCRIPTION = (
    "Parse a Quantum ESPRESSO pw.x output (.out) file into structured "
    "results: total_energy_ev, forces_ev_per_angstrom, "
    "stress_ev_per_angstrom3, wall_time_seconds, convergence flag. "
    "FAILS EXPLICITLY (no numbers at all) when the run did not converge, "
    "crashed, or was killed — an unconverged energy is never returned. "
    "Does not require a QE installation."
)


def _qe_status(**kwargs) -> dict:
    return qe_runtime.status(config={})


def _qe_run(**kwargs) -> dict:
    structure = _load_structure(kwargs["structure"])
    overrides = {k: kwargs[k] for k in qe_runtime.SETTINGS_KEYS if kwargs.get(k) is not None}
    settings = qe_runtime.settings(config=overrides)
    workdir = Path(kwargs.get("workdir") or (Path.home() / ".prism" / "qe" / "runs" / _run_stamp()))
    return qe_runtime.qe_run(
        structure,
        calculation=kwargs.get("calculation", "scf"),
        settings=settings,
        workdir=workdir,
        extra_input=kwargs.get("extra_input"),
    )


def _run_stamp() -> str:
    import datetime as _dt

    return _dt.datetime.now(_dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")


_RUN_DESCRIPTION = (
    "Run Quantum ESPRESSO pw.x on a structure — write the input, run it on the "
    "provisioned binary, parse the output. Returns total energy (eV), forces, "
    "convergence, wall time, and a provenance block naming the binary, the "
    "pseudopotential set and its manifest, cutoffs, k-points and smearing. "
    "Evidence class is execution when the run converged. Defaults come from "
    "`prism qe settings` (also in the command palette: Compute → QE settings); "
    "any of them can be overridden per call. Unavailable when QE is not "
    "provisioned — the result then names the remedy (`prism provision qe`), "
    "never a guess."
)


def create_qe_tools(registry: ToolRegistry) -> None:
    """Register the QE tools. Callers must have verified ase + pymatgen are
    importable first (see check_qe_available)."""
    registry.register(Tool(
        name="qe_status",
        description=(
            "Is Quantum ESPRESSO provisioned here? Binary, MPI launcher, "
            "pseudopotential set (with its manifest) and the current run defaults."
        ),
        input_schema={"type": "object", "properties": {}, "additionalProperties": False},
        func=_qe_status,
    ))
    registry.register(Tool(
        name="qe_run",
        description=_RUN_DESCRIPTION,
        input_schema={
            "type": "object",
            "properties": {
                "structure": {
                    "type": ["string", "object"],
                    "description": "Structure to compute: " + STRUCTURE_FORMS + ". A formula alone is refused.",
                },
                "calculation": {"type": "string", "enum": sorted(CALCULATION_TYPES), "description": "scf (default), relax, vc-relax, …"},
                "ecutwfc_ry": {"type": "number", "exclusiveMinimum": 0, "description": "Wavefunction cutoff, Ry (default from settings)."},
                "ecutrho_ratio": {"type": "number", "exclusiveMinimum": 0, "description": "ecutrho / ecutwfc (default 4)."},
                "kspacing_inv_angstrom": {"type": "number", "exclusiveMinimum": 0, "description": "Reciprocal k-point spacing in 1/Å (default 0.15)."},
                "smearing": {"type": "string", "description": "QE smearing: mv, mp, gaussian, fd."},
                "degauss_ry": {"type": "number", "exclusiveMinimum": 0},
                "nproc": {"type": "integer", "minimum": 1, "description": "MPI processes (default: all cores)."},
                "workdir": {"type": "string", "description": "Where to write pw.in/pw.out (default ~/.prism/qe/runs/<stamp>)."},
                "extra_input": {"type": "object", "description": "Extra pw.x namelist entries, {section: {key: value}}."},
            },
            "required": ["structure"],
            "additionalProperties": False,
        },
        func=_qe_run,
    ))
    registry.register(Tool(
        name="qe_resolve_pseudopotentials",
        description=_RESOLVE_DESCRIPTION,
        input_schema={
            "type": "object",
            "properties": {
                "elements": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Element symbols to resolve, e.g. ['Si', 'O'].",
                },
                "directory": {
                    "type": "string",
                    "description": "Path to a directory containing .upf files.",
                },
            },
            "required": ["elements", "directory"],
            "additionalProperties": False,
        },
        func=_qe_resolve,
    ))

    registry.register(Tool(
        name="qe_write_input",
        description=_WRITE_DESCRIPTION,
        input_schema={
            "type": "object",
            "properties": {
                "structure": {
                    "type": ["string", "object"],
                    "description": (
                        "Path to a structure file pymatgen can read "
                        "(CIF/POSCAR/xyz), or a dict {lattice: 3x3, "
                        "species: [...], coords: [...], cartesian?: bool}."
                    ),
                },
                "cutoffs": {
                    "type": "object",
                    "description": "Plane-wave cutoffs in Rydberg: "
                                   "{ecutwfc: <float>, ecutrho?: <float>}.",
                },
                "kpoints": {
                    "type": "array",
                    "items": {"type": "integer"},
                    "minItems": 3,
                    "maxItems": 3,
                    "description": "Monkhorst-Pack grid [n1, n2, n3]. "
                                   "Omit for Gamma-only.",
                },
                "calculation_type": {
                    "type": "string",
                    "enum": list(CALCULATION_TYPES),
                    "description": "QE calculation type. Default: scf.",
                },
                "pseudopotentials": {
                    "type": "object",
                    "description": "Explicit {element: upf_filename} map. "
                                   "Alternative to pseudo_dir.",
                },
                "pseudo_dir": {
                    "type": "string",
                    "description": "Directory of UPF files to resolve from, "
                                   "and the value written to &control/pseudo_dir.",
                },
                "output_path": {
                    "type": "string",
                    "description": "Where to write the .in file. Default qe_input.in.",
                },
                "prefix": {
                    "type": "string",
                    "description": "QE job prefix (&control/prefix). Default 'prism'.",
                },
                "extra_input": {
                    "type": "object",
                    "description": "Extra/override namelist entries, e.g. "
                                   "{\"system\": {\"nspin\": 2}}.",
                },
            },
            "required": ["structure", "cutoffs"],
            "additionalProperties": False,
        },
        func=_qe_write_input,
        examples=[{
            "input": {
                "structure": {"lattice": [[5.43, 0, 0], [0, 5.43, 0], [0, 0, 5.43]],
                              "species": ["Si", "Si"],
                              "coords": [[0, 0, 0], [0.25, 0.25, 0.25]]},
                "cutoffs": {"ecutwfc": 60.0, "ecutrho": 480.0},
                "kpoints": [8, 8, 8],
                "pseudo_dir": "/data/pseudos",
                "output_path": "si_scf.in",
            },
            "output": {"status": "ok", "path": "si_scf.in", "n_atoms": 2,
                       "formula": "Si", "calculation_type": "scf"},
        }],
    ))

    registry.register(Tool(
        name="qe_parse_output",
        description=_PARSE_DESCRIPTION,
        input_schema={
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the pw.x stdout (.out) file.",
                },
            },
            "required": ["path"],
            "additionalProperties": False,
        },
        func=_qe_parse_output,
        output_schema={
            "type": "object",
            # `status` and `converged` are the two keys BOTH shapes carry, so
            # they are what may be required. `total_energy_ev` is absent on the
            # failure path and must not be promised.
            "required": ["status", "converged"],
            "properties": {
                # The parser returns `{"status": "failed", "converged": false,
                # "reason": ...}` for an unparseable or unconverged run. The
                # enum here admitted only "ok", so the tool's own failure shape
                # violated its own schema — and a consumer validating against it
                # would reject the very result that tells it what went wrong.
                "status": {"type": "string", "enum": ["ok", "failed"]},
                "converged": {"type": "boolean"},
                "reason": {
                    "type": "string",
                    "description": "Why the parse failed. Present only when status is failed.",
                },
                "total_energy_ev": {"type": "number"},
                "forces_ev_per_angstrom": {"type": ["array", "null"]},
                "stress_ev_per_angstrom3": {"type": ["array", "null"]},
                "wall_time_seconds": {"type": ["number", "null"]},
            },
        },
        units={
            "total_energy_ev": "EMMO:eV",
            "forces_ev_per_angstrom": "EMMO:eV-per-angstrom",
            "stress_ev_per_angstrom3": "EMMO:eV-per-cubic-angstrom",
            "wall_time_seconds": "EMMO:second",
        },
        validate=_validate_parse_result,
    ))
