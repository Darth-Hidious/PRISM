"""CALPHAD bridge layer — TDB management, equilibrium calculations, phase diagrams."""

import shutil
import uuid
from pathlib import Path
from typing import Any, Dict, List, Optional


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
        data["serialization_note"] = f"Partial extraction: {e}"

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
        data["serialization_note"] = f"Partial extraction: {e}"
    return data


class CalphadBridge:
    """Thin bridge between PRISM tools and pycalphad.

    Manages TDB databases and provides equilibrium/phase diagram calculations.
    """

    def __init__(self, base_dir: Optional[Path] = None):
        self.databases = DatabaseStore(base_dir=base_dir)

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
            return result
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

        return {
            "database": database_name,
            "components": comps,
            "phases": phase_list,
            "n_points": len(data_points),
            "data_points": data_points,
        }

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
            return result
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
