"""Cache-key invariance and canonical-structure tests."""

from __future__ import annotations

from app.tools.simulation.mace.cache.hashing import (
    cache_key,
    cache_uri,
    canonical_structure_repr,
    parse_cache_uri,
)


def _key(comp, **overrides):
    return cache_key(
        tool_name=overrides.get("tool_name", "relax_structure"),
        tool_version=overrides.get("tool_version", "0.1.0"),
        structure=canonical_structure_repr(comp, "bcc", 100, 20260506),
        head=overrides.get("head", "omat_pbe"),
        calc_params=overrides.get("calc_params", {"dtype": "float64", "fmax_eV_per_A": 0.05}),
        mace_core_git_sha=overrides.get("mace_core_git_sha", "abc123"),
    )


def test_same_composition_same_key() -> None:
    k1 = _key({"Fe": 50, "Ti": 50})
    k2 = _key({"Ti": 50, "Fe": 50})  # different dict order
    assert k1 == k2


def test_different_composition_different_key() -> None:
    k1 = _key({"Fe": 50, "Ti": 50})
    k2 = _key({"Fe": 49, "Ti": 51})  # one-atom swap
    assert k1 != k2


def test_different_head_different_key() -> None:
    k1 = _key({"Fe": 50, "Ti": 50}, head="omat_pbe")
    k2 = _key({"Fe": 50, "Ti": 50}, head="matpes_r2scan")
    assert k1 != k2


def test_different_phase_different_key() -> None:
    base_args = dict(
        tool_name="relax_structure",
        tool_version="0.1.0",
        head="omat_pbe",
        calc_params={"dtype": "float64"},
        mace_core_git_sha="abc",
    )
    k1 = cache_key(
        structure=canonical_structure_repr({"Fe": 50, "Ti": 50}, "bcc", 100, 1), **base_args
    )
    k2 = cache_key(
        structure=canonical_structure_repr({"Fe": 50, "Ti": 50}, "fcc", 100, 1), **base_args
    )
    assert k1 != k2


def test_different_seed_different_key() -> None:
    base_args = dict(
        tool_name="relax_structure",
        tool_version="0.1.0",
        head="omat_pbe",
        calc_params={"dtype": "float64"},
        mace_core_git_sha="abc",
    )
    k1 = cache_key(
        structure=canonical_structure_repr({"Fe": 50, "Ti": 50}, "bcc", 100, 1), **base_args
    )
    k2 = cache_key(
        structure=canonical_structure_repr({"Fe": 50, "Ti": 50}, "bcc", 100, 2), **base_args
    )
    assert k1 != k2


def test_float_canonicalisation_in_calc_params() -> None:
    """Tiny cosmetic float differences should NOT change the cache key."""
    # Within 12 sig-figs, 0.05 == 0.05000000000005
    k1 = _key({"Fe": 50, "Ti": 50}, calc_params={"fmax_eV_per_A": 0.05, "dtype": "float64"})
    k2 = _key(
        {"Fe": 50, "Ti": 50},
        calc_params={"fmax_eV_per_A": 0.05000000000005, "dtype": "float64"},
    )
    assert k1 == k2


def test_cache_uri_roundtrip() -> None:
    uri = cache_uri("abcd1234", "structure.cif")
    key, kind = parse_cache_uri(uri)
    assert key == "abcd1234"
    assert kind == "structure.cif"


def test_cache_uri_default_kind() -> None:
    key, kind = parse_cache_uri("cache://abc")
    assert key == "abc"
    assert kind == "structure"
