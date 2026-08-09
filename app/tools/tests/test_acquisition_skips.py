"""An unknown data source must be reported, never silently dropped.

`acquire_materials` used to `continue` past a source the registry did not
know, while every other failure path recorded a skip. `literature` was
advertised in the tool's own schema and has no collector at all, so asking
for it produced fewer records with no explanation.
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

from app.tools.data_collectors.base_collector import CollectorRegistry  # noqa: E402


def test_registry_has_no_literature_collector():
    """The premise: the advertised source genuinely does not exist."""
    from app.tools.data_collectors.base_collector import get_default_collector_registry

    names = {c.name for c in get_default_collector_registry().list_collectors()}
    assert "literature" not in names, (
        "if a literature collector is added, restore it to the tool's schema"
    )
    # the ones that DO exist, so a rename breaks this loudly
    assert {"optimade", "mp", "omat24", "patents"} <= names, names


def test_acquire_materials_no_longer_advertises_a_source_it_cannot_query():
    import app.tools.skills.acquisition as acq

    src = Path(acq.__file__).read_text()
    schema_line = next(
        line for line in src.splitlines() if "Data sources to query" in line
    )
    assert "literature," not in schema_line, (
        "the schema must not advertise a source with no collector: " + schema_line
    )
    assert "eastern_literature" in schema_line, "the real one must stay"


def test_unknown_source_is_recorded_as_a_skip_not_dropped():
    """The behaviour itself: an unregistered name reaches `skipped`."""
    import app.tools.skills.acquisition as acq

    src = Path(acq.__file__).read_text()
    # the KeyError arm must append a skip before continuing
    # NB: split on the statement, not the word — the explanatory comment in
    # that arm contains "continue" and an earlier version of this test matched
    # the comment instead of the code.
    arm = src.split("except KeyError:")[1].split("\n            continue")[0]
    assert "skipped.append" in arm, (
        "an unknown source must be reported, not silently skipped"
    )
    assert "no collector named" in arm
