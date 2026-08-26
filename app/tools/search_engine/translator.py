"""Deterministic query translation -- MaterialSearchQuery to provider-native syntax."""
from __future__ import annotations

import re
from math import gcd

from app.tools.search_engine.query import MaterialSearchQuery

_FORMULA_TOKEN = re.compile(r"([A-Z][a-z]?)(\d*)")


def optimade_reduced_formula(formula: str) -> str | None:
    """Canonicalise a written formula to OPTIMADE ``chemical_formula_reduced``.

    The spec requires element symbols in ALPHABETICAL order with proportions
    divided by their greatest common divisor and a proportion of 1 omitted.
    A human writes ``TiO2``; the canonical form is ``O2Ti``.

    This was not being done. ``chemical_formula_reduced="TiO2"`` was sent
    verbatim, and every spec-compliant provider answered HTTP 200 with zero
    hits — Materials Project included, which holds hundreds of TiO2 entries.
    The federation reported every provider as "success" while returning almost
    nothing, so the failure looked like an empty database rather than a bad
    query.

    Returns ``None`` when the string does not parse as a simple formula
    (parentheses, hydrates, charges, wildcards). The caller then sends the
    original text rather than a guess: a formula we cannot canonicalise is
    better handled by the provider than mangled here.
    """
    text = (formula or "").strip()
    if not text or not text[0].isupper():
        return None
    # Reject anything with structure this parser does not model.
    if not re.fullmatch(r"(?:[A-Z][a-z]?\d*)+", text):
        return None

    counts: dict[str, int] = {}
    for symbol, digits in _FORMULA_TOKEN.findall(text):
        if not symbol:
            continue
        counts[symbol] = counts.get(symbol, 0) + (int(digits) if digits else 1)
    if not counts:
        return None

    divisor = 0
    for n in counts.values():
        divisor = gcd(divisor, n)
    if divisor > 1:
        counts = {sym: n // divisor for sym, n in counts.items()}

    return "".join(
        f"{sym}{n if n > 1 else ''}" for sym, n in sorted(counts.items())
    )


def _it_number_for_symbol(symbol: str) -> int | None:
    """Hermann-Mauguin symbol -> International Tables number, or None.

    `space_group_symbol_hermann_mauguin` is an OPTIONAL OPTIMADE field.
    Measured 2026-08-25: sending it made cod, mpds, matterverse, nmd and tcod
    all return **400 Bad Request** — they reject the WHOLE query rather than
    ignoring one unsupported term, so a space-group filter silently cost every
    result from those providers. `space_group_it_number` is the widely
    supported form.

    The mapping comes from pymatgen (already a dependency of the `ml` extra),
    never from a hand-typed table: a wrong space group is worse than no filter.
    If pymatgen is absent or the symbol is unrecognised, return None and let the
    caller fall back to the symbol form — degrading to the old behaviour, never
    to a WRONG number.
    """
    try:
        from pymatgen.symmetry.groups import SpaceGroup
    except ImportError:
        return None
    try:
        return int(SpaceGroup(str(symbol).strip()).int_number)
    except Exception:
        return None


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
            # Canonicalise: the spec wants alphabetical, GCD-reduced. Falling
            # back to the raw text keeps formulas this parser cannot model
            # (hydrates, parentheses) working exactly as before.
            canonical = optimade_reduced_formula(query.formula) or query.formula
            parts.append(f'chemical_formula_reduced="{canonical}"')

        if query.n_elements:
            if query.n_elements.min is not None:
                parts.append(f"nelements>={int(query.n_elements.min)}")
            if query.n_elements.max is not None:
                parts.append(f"nelements<={int(query.n_elements.max)}")

        if query.space_group:
            # The OPTIMADE spec defines `space_group_it_number` (int, 1-230)
            # and `space_group_symbol_hermann_mauguin` (str). The previously
            # sent `space_group_symbol` is NOT a spec field and no live
            # provider filters on it.
            sg = query.space_group
            if isinstance(sg, int) or str(sg).strip().isdigit():
                parts.append(f"space_group_it_number={int(str(sg).strip())}")
            elif (it_number := _it_number_for_symbol(sg)) is not None:
                # Prefer the widely-supported numeric field; sending the
                # optional symbol field 400s most providers outright.
                parts.append(f"space_group_it_number={it_number}")
            else:
                parts.append(f'space_group_symbol_hermann_mauguin="{sg}"')

        return " AND ".join(parts) if parts else ""

    @staticmethod
    def to_mp_kwargs(query: MaterialSearchQuery) -> dict:
        """MaterialSearchQuery -> MPRester.materials.summary.search() kwargs."""
        kwargs: dict = {}

        if query.elements:
            kwargs["elements"] = query.elements
        if query.formula:
            kwargs["formula"] = query.formula
        if query.n_elements:
            # MPRester summary search takes num_elements as a (min, max) tuple.
            lo = int(query.n_elements.min) if query.n_elements.min is not None else 1
            hi = int(query.n_elements.max) if query.n_elements.max is not None else 20
            kwargs["num_elements"] = (lo, hi)
        if query.space_group:
            # MPRester supports both: spacegroup_number (International Tables
            # number) and spacegroup_symbol (Hermann-Mauguin).
            sg = query.space_group
            if isinstance(sg, int) or str(sg).strip().isdigit():
                kwargs["spacegroup_number"] = int(str(sg).strip())
            else:
                kwargs["spacegroup_symbol"] = str(sg)
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
        if query.bulk_modulus:
            # MPRester's k_vrh filter IS the Voigt-Reuss-Hill bulk modulus in
            # GPa -- the same number _parse_doc reads from the summary
            # `bulk_modulus` field. Previously this filter was silently
            # dropped AND the field never requested, so a bulk_modulus query
            # against mp_native was guaranteed zero results.
            lo = query.bulk_modulus.min if query.bulk_modulus.min is not None else 0
            hi = query.bulk_modulus.max if query.bulk_modulus.max is not None else 1000
            kwargs["k_vrh"] = (lo, hi)

        return kwargs
