"""Structure import: bring external structures into PRISM's tool chain.

Closes the workflow gap "found TiAl in Materials Project / a paper → run a
MACE calculation on it". Accepts a CIF string (or a plain lattice+species
dict), validates it through ase, and stores it:

  1. ALWAYS into the MACE disk cache (same directory the MACE tools use),
     returning a ``cache://<key>/structure.cif`` URI usable as `cache_ref`
     by mace_md_equilibrate / mace_phonon_harmonic / mace_compute_elastic
     and readable via mace_get_cached_structure.
  2. ADDITIONALLY into the pyiron bridge StructureStore when pyiron is
     importable in this interpreter, returning a `structure_id` usable by
     the `structure` / `sim_run` tools. (When sim tools run in the science
     sidecar, this in-process store is not shared — the tool says so
     honestly instead of pretending.)
"""

from __future__ import annotations

import hashlib
import io
import logging
from collections import Counter

from app.tools.base import Tool, ToolRegistry

logger = logging.getLogger(__name__)


def _atoms_from_inputs(kwargs: dict):
    """Build an ase.Atoms from either `cif` or `structure`. Raises ValueError
    with an agent-actionable message on bad input."""
    from ase import Atoms
    from ase.io import read as ase_read

    cif = kwargs.get("cif")
    structure = kwargs.get("structure")
    if cif:
        try:
            atoms = ase_read(io.StringIO(cif), format="cif")
        except Exception as e:
            raise ValueError(f"could not parse CIF: {e}") from e
        return atoms
    if structure:
        lattice = structure.get("lattice")
        species = structure.get("species")
        coords = structure.get("coords")
        if not (lattice and species and coords):
            raise ValueError(
                "`structure` needs `lattice` (3×3), `species` (list of "
                "element symbols), and `coords` (one xyz triple per atom)"
            )
        if len(species) != len(coords):
            raise ValueError(
                f"species has {len(species)} entries but coords has "
                f"{len(coords)} — one coordinate triple per atom required"
            )
        kw = {"symbols": species, "cell": lattice, "pbc": True}
        if structure.get("cartesian"):
            kw["positions"] = coords
        else:
            kw["scaled_positions"] = coords
        return Atoms(**kw)
    raise ValueError("provide either `cif` (CIF text) or `structure` (lattice+species+coords)")


def _structure_import(**kwargs) -> dict:
    try:
        import ase  # noqa: F401
    except ImportError:
        return {
            "error": "structure_import needs ase (Atomic Simulation Environment)",
            "install_hint": "pip install ase  (or the 'prism-platform[mace]' extra)",
        }

    try:
        from ase.io import write as ase_write

        atoms = _atoms_from_inputs(kwargs)

        # Canonical CIF text (round-tripped through ase so downstream
        # readers see exactly what we validated). ase's CIF writer wants
        # a binary stream (it handles its own encoding).
        buf = io.BytesIO()
        ase_write(buf, atoms, format="cif")
        cif_text = buf.getvalue().decode("utf-8")

        symbols = atoms.get_chemical_symbols()
        composition = {el: int(n) for el, n in sorted(Counter(symbols).items())}
        formula = atoms.get_chemical_formula()

        # 1. MACE cache (content-addressed by the CIF itself).
        from app.tools.simulation.mace.auth import get_cache_dir
        from app.tools.simulation.mace.cache.store import CacheStore

        key = hashlib.sha256(cif_text.encode("utf-8")).hexdigest()
        cache = CacheStore(get_cache_dir())
        cache.write_structure_cif(key, cif_text)
        cache.write_meta(key, {
            "tool": "structure_import",
            "name": kwargs.get("name"),
            "formula": formula,
            "n_atoms": len(atoms),
            "composition": composition,
            "source": "user_import",
        })
        cache_ref = f"cache://{key}/structure.cif"

        # 2. pyiron StructureStore, when available in this interpreter.
        pyiron_structure_id = None
        pyiron_note = None
        try:
            from app.tools.simulation.bridge import check_pyiron_available, get_bridge

            if check_pyiron_available():
                pyiron_structure_id = get_bridge().structures.store(atoms)
            else:
                pyiron_note = (
                    "pyiron not importable here — sim tools run in the "
                    "science sidecar and cannot see this in-process store; "
                    "use the cache_ref with the mace_* tools instead"
                )
        except Exception as e:
            pyiron_note = f"pyiron StructureStore unavailable: {e}"

        return {
            "imported": True,
            "cache_ref": cache_ref,
            "formula": formula,
            "n_atoms": len(atoms),
            "composition": composition,
            "pyiron_structure_id": pyiron_structure_id,
            **({"pyiron_note": pyiron_note} if pyiron_note else {}),
            "usable_by": [
                "mace_md_equilibrate(cache_ref=...)",
                "mace_compute_elastic(cache_ref=...)",
                "mace_phonon_harmonic(cache_ref=...)",
                "mace_get_cached_structure(cache_uri=...)",
            ] + (["structure(action='info', structure_id=...)"] if pyiron_structure_id else []),
        }
    except ValueError as e:
        return {"error": str(e)}
    except Exception as e:
        logger.exception("structure_import failed")
        return {"error": f"{type(e).__name__}: {e}"}


_DESCRIPTION = (
    "Import an external crystal structure into PRISM so simulation tools can "
    "use it. Use this when you have a CIF (from Materials Project, a paper, "
    "or the user) or an explicit lattice+species+coords and want to run MACE "
    "calculations on it. Validates via ase, stores the structure in the MACE "
    "cache, and returns a cache_ref accepted by mace_md_equilibrate / "
    "mace_compute_elastic / mace_phonon_harmonic (and a pyiron structure_id "
    "when pyiron runs in-process). NOT for building bulk structures from "
    "scratch (use structure(action='create') or the mace composition+phase "
    "inputs) and NOT for dataset files (use dataset(action='import'))."
)

_SCHEMA = {
    "type": "object",
    "properties": {
        "cif": {
            "type": "string",
            "description": "CIF file content as a string. Either this or `structure` is required.",
        },
        "structure": {
            "type": "object",
            "description": (
                "Explicit structure: `lattice` (3×3 Å matrix), `species` "
                "(element symbol per atom), `coords` (one [x,y,z] per atom, "
                "fractional unless `cartesian` is true)."
            ),
            "properties": {
                "lattice": {"type": "array", "description": "3×3 lattice matrix in Angstroms."},
                "species": {"type": "array", "items": {"type": "string"},
                            "description": "Element symbol for every atom."},
                "coords": {"type": "array", "description": "One [x,y,z] triple per atom."},
                "cartesian": {"type": "boolean",
                              "description": "Set true if coords are Cartesian Å (default: fractional)."},
            },
            "required": ["lattice", "species", "coords"],
        },
        "name": {
            "type": "string",
            "description": "Optional label stored with the structure (e.g. 'mp-1823 TiAl').",
        },
    },
    "required": [],
    "additionalProperties": False,
}


def create_structure_io_tools(registry: ToolRegistry) -> None:
    """Register the structure_import tool."""
    registry.register(Tool(
        name="structure_import",
        description=_DESCRIPTION,
        input_schema=_SCHEMA,
        func=_structure_import,
    ))
