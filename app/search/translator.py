"""Deterministic query translation -- MaterialSearchQuery to provider-native syntax."""
from __future__ import annotations

from app.search.query import MaterialSearchQuery


class QueryTranslator:
    """Converts MaterialSearchQuery into provider-specific query formats."""

    @staticmethod
    def to_optimade(query: MaterialSearchQuery) -> str:
        """MaterialSearchQuery -> OPTIMADE filter string."""
        parts: list[str] = []

        if query.elements:
            quoted = ",".join(f'"{e}"' for e in query.elements)
            parts.append(f"elements HAS ALL {quoted}")

        if query.elements_any:
            quoted = ",".join(f'"{e}"' for e in query.elements_any)
            parts.append(f"elements HAS ANY {quoted}")

        if query.exclude_elements:
            for e in query.exclude_elements:
                parts.append(f'NOT elements HAS "{e}"')

        if query.formula:
            parts.append(f'chemical_formula_reduced="{query.formula}"')

        if query.n_elements:
            if query.n_elements.min is not None:
                parts.append(f"nelements>={int(query.n_elements.min)}")
            if query.n_elements.max is not None:
                parts.append(f"nelements<={int(query.n_elements.max)}")

        if query.space_group:
            parts.append(f'space_group_symbol="{query.space_group}"')

        return " AND ".join(parts) if parts else ""

    @staticmethod
    def to_mp_kwargs(query: MaterialSearchQuery) -> dict:
        """MaterialSearchQuery -> MPRester.materials.summary.search() kwargs."""
        kwargs: dict = {}

        if query.elements:
            kwargs["elements"] = query.elements
        if query.formula:
            kwargs["formula"] = query.formula
        if query.band_gap:
            lo = query.band_gap.min if query.band_gap.min is not None else 0
            hi = query.band_gap.max if query.band_gap.max is not None else 100
            kwargs["band_gap"] = (lo, hi)
        if query.formation_energy:
            lo = query.formation_energy.min if query.formation_energy.min is not None else -10
            hi = query.formation_energy.max if query.formation_energy.max is not None else 10
            kwargs["formation_energy_per_atom"] = (lo, hi)
        if query.energy_above_hull:
            lo = query.energy_above_hull.min if query.energy_above_hull.min is not None else 0
            hi = query.energy_above_hull.max if query.energy_above_hull.max is not None else 10
            kwargs["energy_above_hull"] = (lo, hi)

        return kwargs
