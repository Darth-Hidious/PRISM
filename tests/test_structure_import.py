"""`structure_import` must only advertise tools this install actually has.

Regression: the success payload listed the four ``mace_*`` consumers
unconditionally, and the "pyiron is absent" note told the agent to "use the
cache_ref with the mace_* tools instead". Those tools are registered only
when ``check_mace_available()`` is true (app/plugins/bootstrap.py), so on an
install without the ``[mace]`` extra — the default, since mace-torch is an
optional dependency — the tool reported ``imported: true`` and then pointed
the agent at four names that are not in the catalog at all.
"""

from __future__ import annotations

from unittest.mock import patch

import pytest

pytest.importorskip("ase")

from app.tools.structure_io import _structure_import


FCC_AL = {
    "lattice": [[4.05, 0, 0], [0, 4.05, 0], [0, 0, 4.05]],
    "species": ["Al", "Al", "Al", "Al"],
    "coords": [[0, 0, 0], [0, 0.5, 0.5], [0.5, 0, 0.5], [0.5, 0.5, 0]],
}

_MACE_TOOLS = (
    "mace_md_equilibrate",
    "mace_compute_elastic",
    "mace_phonon_harmonic",
    "mace_get_cached_structure",
)


def _import(**patches):
    with (
        patch(
            "app.tools.simulation.mace_bridge.check_mace_available",
            return_value=patches["mace"],
        ),
        patch(
            "app.tools.simulation.bridge.check_pyiron_available",
            return_value=False,
        ),
    ):
        return _structure_import(structure=FCC_AL, name="fcc Al")


def test_no_mace_tools_advertised_when_the_extra_is_missing():
    result = _import(mace=False)

    # The import itself really happened — the CIF is on disk under cache_ref.
    assert result["imported"] is True
    assert result["cache_ref"].startswith("cache://")

    named = " ".join(result["usable_by"])
    for tool in _MACE_TOOLS:
        assert tool not in named, f"{tool} is not registered without [mace]"
    # ...and the pyiron note must not send the agent there either.
    assert "mace" not in result["pyiron_note"]
    # The dead end has to be stated, with what would fix it.
    assert "[mace]" in result["mace_note"]
    assert "pip install" in result["mace_note"]


def test_mace_tools_are_advertised_when_the_extra_is_present():
    """The gate must not simply delete the advice — with [mace] installed the
    four consumers are registered and naming them is correct."""
    result = _import(mace=True)

    named = " ".join(result["usable_by"])
    for tool in _MACE_TOOLS:
        assert tool in named
    assert "mace_note" not in result
    assert "mace_* tools" in result["pyiron_note"]
