"""CALPHAD bridge layer — TDB management, equilibrium calculations, phase diagrams.

Physics: every number here comes out of pycalphad. PRISM parses no
thermodynamics of its own and converts no units.

Verified against known values (pycalphad 0.11.2, macOS arm64, py3.12):
  * Gibbs energy unit + temperature scale — a synthetic ideal binary whose
    end-member Gibbs energies are identically zero has, in closed form,
    GM(x=0.5) = -R*T*ln2. calculate_gibbs_energy returned -5763.172176
    J/mol-atom at T=1000; -R*T*ln2 with pycalphad's own R (8.3145, the SGTE
    convention) is -5763.172233 -> 1.1e-8 relative. This pins GM to
    J/mol-atom (not kJ, not per formula unit, not eV) and T to Kelvin.
  * Melting point — pure Al in pycalphad's bundled alfe.tdb via
    calculate_equilibrium: FCC_A1 at 933.00 K, LIQUID at 934.00 K, bracketing
    the literature Al melting point of 933.47 K.
"""

import shutil
import uuid
from pathlib import Path
from typing import Any, Dict, List, Optional

from app.tools import _provenance as prov

#: Units as pycalphad reports them, keyed by the RESULT keys these methods
#: actually emit. Listing a key nothing writes (`pressure`,
#: `composition_conditions`) makes the block look authoritative while saying
#: nothing about the numbers present, so it is kept honest to the output.
#: Input units live in the provenance `input` block, suffixed there
#: (`temperature_K`, `pressure_Pa`).
CALPHAD_UNITS = {
    "gibbs_energy": "J/mol-atom",
    "gibbs_energies": "J/mol-atom",
    "phase_fractions": "mole fraction of total moles of phase (dimensionless)",
    "temperature": "K",
}


def _pycalphad_version() -> str:
    try:
        import pycalphad

        return str(getattr(pycalphad, "__version__", "unknown"))
    except Exception:
        return "absent"


def check_calphad_available() -> bool:
    """Return True if pycalphad is importable."""
    try:
        import pycalphad  # noqa: F401
        return True
    except ImportError:
        return False


def _calphad_missing_error() -> dict:
    """Standard error dict when pycalphad is not installed."""
    return {
        "error": (
            "pycalphad is not installed. "
            "Install CALPHAD extras with: pip install prism-platform[calphad]"
        )
    }


class DatabaseStore:
    """Manages TDB thermodynamic database files in ~/.prism/databases/."""

    def __init__(self, base_dir: Optional[Path] = None):
        self._base_dir = base_dir or (Path.home() / ".prism" / "databases")
        self._base_dir.mkdir(parents=True, exist_ok=True)
        self._cache: Dict[str, Any] = {}

    @property
    def base_dir(self) -> Path:
        return self._base_dir

    def list_databases(self) -> List[dict]:
        """List all TDB files with name, path, and size."""
        databases = []
        for f in sorted(self._base_dir.glob("*.tdb")):
            databases.append({
                "name": f.stem,
                "path": str(f),
                "size_kb": round(f.stat().st_size / 1024, 1),
            })
        return databases

    def import_database(self, source_path: str, name: Optional[str] = None) -> dict:
        """Copy a TDB file into the managed directory."""
        src = Path(source_path)
        if not src.exists():
            return {"error": f"Source file not found: {source_path}"}
        if src.suffix.lower() != ".tdb":
            return {"error": f"Expected a .tdb file, got: {src.suffix}"}

        db_name = name or src.stem
        dest = self._base_dir / f"{db_name}.tdb"
        shutil.copy2(src, dest)
        return {
            "name": db_name,
            "path": str(dest),
            "imported": True,
        }

    def load(self, name: str) -> Any:
        """Load and cache a pycalphad Database object. Returns None if not found."""
        if name in self._cache:
            return self._cache[name]

        db_path = self._base_dir / f"{name}.tdb"
        if not db_path.exists():
            return None

        from pycalphad import Database
        db = Database(str(db_path))
        self._cache[name] = db
        return db

    def get_phases(self, name: str, components: Optional[List[str]] = None) -> Optional[List[str]]:
        """List phases in a database, optionally filtered by components."""
        db = self.load(name)
        if db is None:
            return None

        phases = list(db.phases.keys())

        if components:
            # Filter phases that contain at least one of the requested components
            filtered = []
            for phase_name in phases:
                phase = db.phases[phase_name]
                phase_constituents = set()
                for sublattice in phase.constituents:
                    phase_constituents.update(str(s) for s in sublattice)
                # Keep phase if any requested component is in its constituents
                if any(c in phase_constituents for c in components):
                    filtered.append(phase_name)
            return filtered

        return phases


def _ensure_vacancy(components: List[str]) -> List[str]:
    """Add 'VA' (vacancy) to components if missing — pycalphad requires it."""
    if "VA" not in components:
        return list(components) + ["VA"]
    return list(components)


def _serialize_eq_result(eq_result) -> dict:
    """Convert pycalphad equilibrium xarray result to a JSON-safe dict."""
    import numpy as np

    data = {}
    try:
        # Extract phase names and fractions
        phases_present = []
        phase_fractions = {}
        compositions = {}

        phase_vals = eq_result.Phase.values.squeeze()
        np_vals = eq_result.NP.values.squeeze()

        if phase_vals.ndim == 0:
            phase_vals = phase_vals.reshape(1)
            np_vals = np_vals.reshape(1)

        for i, (phase, frac) in enumerate(zip(phase_vals.flat, np_vals.flat)):
            phase_str = str(phase).strip()
            if phase_str and phase_str != "" and not np.isnan(frac) and frac > 1e-10:
                phases_present.append(phase_str)
                phase_fractions[phase_str] = float(frac)

        data["phases_present"] = phases_present
        data["phase_fractions"] = phase_fractions

        # Extract Gibbs energy
        gm = eq_result.GM.values.squeeze()
        if hasattr(gm, "tolist"):
            data["gibbs_energy"] = float(gm) if gm.ndim == 0 else gm.tolist()
        else:
            data["gibbs_energy"] = float(gm)

    except Exception as e:
        # An "error" key, not a note: a half-extracted result with no
        # phases_present / gibbs_energy is a failure, and prov.attach() must
        # not dress it in provenance as though it were a measurement.
        data["error"] = f"Result extraction failed (partial): {e}"

    return data


def _serialize_calc_result(calc_result) -> dict:
    """Convert pycalphad calculate result to a JSON-safe dict."""
    data = {}
    try:
        gm = calc_result.GM.values.squeeze()
        if hasattr(gm, "tolist"):
            data["gibbs_energies"] = gm.tolist()
        else:
            data["gibbs_energies"] = float(gm)
    except Exception as e:
        # An "error" key, not a note: a half-extracted result with no
        # phases_present / gibbs_energy is a failure, and prov.attach() must
        # not dress it in provenance as though it were a measurement.
        data["error"] = f"Result extraction failed (partial): {e}"
    return data


class CalphadBridge:
    """Thin bridge between PRISM tools and pycalphad.

    Manages TDB databases and provides equilibrium/phase diagram calculations.
    """

    def __init__(self, base_dir: Optional[Path] = None):
        self.databases = DatabaseStore(base_dir=base_dir)

    def _provenance(
        self,
        *,
        activity: str,
        database_name: str,
        inputs: Dict[str, Any],
        reproduce: str,
    ) -> Dict[str, Any]:
        """Provenance for one pycalphad call.

        The TDB is hashed: the same database name can name different
        thermodynamics after an edit, and a number is only reproducible if
        the reader can tell which bytes produced it.
        """
        tdb_path = self.databases.base_dir / f"{database_name}.tdb"
        return prov.build(
            tool_name="calphad_compute",
            engine="pycalphad",
            engine_version=_pycalphad_version(),
            activity=activity,
            inputs=inputs,
            units=CALPHAD_UNITS,
            derived_from=[prov.file_ref(tdb_path, role="thermodynamic_database")],
            reproduce=reproduce,
        )

    def calculate_equilibrium(
        self,
        database_name: str,
        components: List[str],
        phases: Optional[List[str]],
        conditions: Dict[str, Any],
    ) -> dict:
        """Calculate thermodynamic equilibrium at specific conditions."""
        db = self.databases.load(database_name)
        if db is None:
            return {"error": f"Database '{database_name}' not found"}

        from pycalphad import equilibrium, variables as v

        comps = _ensure_vacancy(components)
        if phases is None:
            phase_list = self.databases.get_phases(database_name, comps)
        else:
            phase_list = list(phases)

        # Build condition dict with pycalphad variables
        cond = {}
        for key, val in conditions.items():
            if key == "T":
                cond[v.T] = val
            elif key == "P":
                cond[v.P] = val
            elif key.startswith("X(") and key.endswith(")"):
                element = key[2:-1]
                cond[v.X(element)] = val
            else:
                cond[key] = val

        try:
            eq_result = equilibrium(db, comps, phase_list, cond)
            result = _serialize_eq_result(eq_result)
            result["database"] = database_name
            result["components"] = comps
            return prov.attach(result, self._provenance(
                activity="pycalphad.equilibrium",
                database_name=database_name,
                inputs={
                    "components_requested": list(components),
                    "components_used": comps,
                    "vacancy_added": "VA" not in components,
                    "phases": phase_list,
                    "conditions": conditions,
                },
                reproduce=(
                    f"calphad_compute(action='equilibrium', "
                    f"database_name={database_name!r}, "
                    f"components={list(components)!r}, "
                    f"phases={phases!r}, conditions={conditions!r})"
                ),
            ))
        except Exception as e:
            return {"error": f"Equilibrium calculation failed: {e}"}

    def calculate_phase_diagram(
        self,
        database_name: str,
        components: List[str],
        phases: Optional[List[str]] = None,
        temperature_range: Optional[List[float]] = None,
        pressure: float = 101325,
    ) -> dict:
        """Compute equilibrium across a temperature range for phase diagram data."""
        import numpy as np

        db = self.databases.load(database_name)
        if db is None:
            return {"error": f"Database '{database_name}' not found"}

        from pycalphad import equilibrium, variables as v

        comps = _ensure_vacancy(components)
        if phases is None:
            phase_list = self.databases.get_phases(database_name, comps)
        else:
            phase_list = list(phases)

        if temperature_range is None:
            temperature_range = [300, 2000, 50]

        t_start, t_stop, t_step = temperature_range
        temperatures = np.arange(t_start, t_stop + t_step, t_step)

        data_points = []
        for t in temperatures:
            cond = {v.T: float(t), v.P: pressure}
            try:
                eq_result = equilibrium(db, comps, phase_list, cond)
                point = _serialize_eq_result(eq_result)
                point["temperature"] = float(t)
                data_points.append(point)
            except Exception:
                data_points.append({"temperature": float(t), "error": "calculation_failed"})

        n_failed = sum(1 for p in data_points if "error" in p)
        result = {
            "database": database_name,
            "components": comps,
            "phases": phase_list,
            "n_points": len(data_points),
            # A per-temperature solve that did not converge is a defect to
            # surface, not something to average away — count it up front so
            # the caller cannot miss it.
            "n_failed_points": n_failed,
            "data_points": data_points,
        }
        return prov.attach(result, self._provenance(
            activity="pycalphad.equilibrium (temperature scan)",
            database_name=database_name,
            inputs={
                "components_requested": list(components),
                "components_used": comps,
                "vacancy_added": "VA" not in components,
                "phases": phase_list,
                "temperature_range_K": list(temperature_range),
                "pressure_Pa": pressure,
            },
            reproduce=(
                f"calphad_compute(action='phase_diagram', "
                f"database_name={database_name!r}, components={list(components)!r}, "
                f"phases={phases!r}, temperature_range={list(temperature_range)!r}, "
                f"pressure={pressure!r})"
            ),
        ))

    def calculate_gibbs_energy(
        self,
        database_name: str,
        components: List[str],
        phases: List[str],
        temperature: float,
        pressure: float = 101325,
    ) -> dict:
        """Calculate Gibbs energy surface for given phases."""
        db = self.databases.load(database_name)
        if db is None:
            return {"error": f"Database '{database_name}' not found"}

        from pycalphad import calculate, variables as v

        comps = _ensure_vacancy(components)

        try:
            calc_result = calculate(db, comps, phases, T=temperature, P=pressure)
            result = _serialize_calc_result(calc_result)
            result["phases"] = phases
            result["temperature"] = temperature
            result["database"] = database_name
            return prov.attach(result, self._provenance(
                activity="pycalphad.calculate",
                database_name=database_name,
                inputs={
                    "components_requested": list(components),
                    "components_used": comps,
                    "vacancy_added": "VA" not in components,
                    "phases": list(phases),
                    "temperature_K": temperature,
                    "pressure_Pa": pressure,
                },
                reproduce=(
                    f"calphad_compute(action='gibbs', "
                    f"database_name={database_name!r}, "
                    f"components={list(components)!r}, "
                    f"phases={list(phases)!r}, temperature={temperature!r}, "
                    f"pressure={pressure!r})"
                ),
            ))
        except Exception as e:
            return {"error": f"Gibbs energy calculation failed: {e}"}


# Module-level singleton so all tools share the same bridge.
_bridge: Optional[CalphadBridge] = None


def get_calphad_bridge() -> CalphadBridge:
    """Return the module-level CalphadBridge singleton."""
    global _bridge
    if _bridge is None:
        _bridge = CalphadBridge()
    return _bridge
