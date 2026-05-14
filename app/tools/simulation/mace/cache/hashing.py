"""Cache-key construction with canonical inputs.

Goals:
  - Same logical input → same SHA-256 key (cache hits).
  - Dict ordering, float formatting, and CIF whitespace are normalised.
  - Cosmetic source code edits to mace_core force cache invalidation via
    the embedded ``mace_core_git_sha`` field, which is set by the caller.

The key fields:
    {
      "tool_name":         <str>,
      "tool_version":      <str>,
      "structure":         <canonical structure dict>,
      "head":              <enum>,
      "calc_params":       <dict, only the params that affect physics>,
      "mace_core_git_sha": <str>,
      "recipe_git_sha":    <str>,
    }
"""

from __future__ import annotations

import hashlib
from typing import Any

from ..ids import canonical_json


def _round_float(x: float, sig_figs: int = 12) -> float:
    """Round a float to ``sig_figs`` significant figures; used to canonicalise."""
    if x == 0:
        return 0.0
    from math import floor, log10

    digits = sig_figs - int(floor(log10(abs(x)))) - 1
    return round(x, digits)


def _canon_value(v: Any) -> Any:
    if isinstance(v, float):
        return _round_float(v)
    if isinstance(v, dict):
        return {k: _canon_value(v[k]) for k in sorted(v)}
    if isinstance(v, (list, tuple)):
        return [_canon_value(x) for x in v]
    return v


def canonical_structure_repr(
    composition: dict[str, int],
    phase: str,
    n_atoms: int,
    seed: int,
) -> dict[str, Any]:
    """Canonical structure descriptor for cache-key hashing.

    Note: this is *not* a full CIF — random-substitution supercells from a
    seeded RNG are reproducible from (composition, phase, n_atoms, seed),
    so we hash the spec rather than the realised atomic positions. This
    makes cache hits robust to ASE version changes that might alter the
    bit-exact CIF representation.
    """
    return {
        "composition": {k: int(composition[k]) for k in sorted(composition)},
        "phase": phase,
        "n_atoms": int(n_atoms),
        "seed": int(seed),
    }


def cache_key(
    *,
    tool_name: str,
    tool_version: str,
    structure: dict[str, Any],
    head: str,
    calc_params: dict[str, Any],
    mace_core_git_sha: str,
    recipe_git_sha: str = "",
) -> str:
    """Return the SHA-256 hex digest of the canonicalised key blob."""
    blob = {
        "tool_name": tool_name,
        "tool_version": tool_version,
        "structure": _canon_value(structure),
        "head": head,
        "calc_params": _canon_value(calc_params),
        "mace_core_git_sha": mace_core_git_sha,
        "recipe_git_sha": recipe_git_sha,
    }
    return hashlib.sha256(canonical_json(blob)).hexdigest()


def cache_uri(key: str, kind: str = "structure") -> str:
    """Build a ``cache://<key>/<kind>`` URI."""
    return f"cache://{key}/{kind}"


def parse_cache_uri(uri: str) -> tuple[str, str]:
    """Reverse of :func:`cache_uri`: returns ``(key, kind)``."""
    if not uri.startswith("cache://"):
        raise ValueError(f"not a cache URI: {uri!r}")
    rest = uri[len("cache://"):]
    if "/" in rest:
        key, kind = rest.split("/", 1)
    else:
        key, kind = rest, "structure"
    return key, kind
