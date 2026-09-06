"""A placeholder label in property_sources is not a citation."""

import pytest

from app.tools.manufacturing.lpbf import tools as lpbf_tools
from app.tools.manufacturing.lpbf.tools import _THERMOPHYSICAL_PROPERTIES


@pytest.fixture
def run(monkeypatch):
    # No engine: the map itself is stubbed; only the evidence stamp is under test.
    monkeypatch.setattr(lpbf_tools, "check_lpbf_available", lambda: True)
    monkeypatch.setattr(
        lpbf_tools, "generate_printability_map", lambda **kw: {"grid": [], "summary": "ok"}
    )
    return lpbf_tools._run_printability_map


def test_placeholder_labels_do_not_count_as_sources(run):
    # The 2026-09-02 live run: the agent passed this label for every property
    # and the tool stamped the map orange.
    sources = {
        name: "IN625-class nominal value (UNVERIFIED)" for name in _THERMOPHYSICAL_PROPERTIES
    }
    out = run(property_sources=sources)
    assert out["evidence_class"] == "indeterminate", out.get("evidence_class")
    assert out["evidence_color"] == "red"
    assert sorted(out["unsourced_properties"]) == sorted(_THERMOPHYSICAL_PROPERTIES)
    assert out["placeholder_sources"] == sources
    assert out["property_sources"] == {}


def test_real_citations_count(run):
    sources = {name: "doi:10.6028/NIST.AMS.100-19" for name in _THERMOPHYSICAL_PROPERTIES}
    out = run(property_sources=sources)
    assert out["evidence_class"] == "research", out.get("evidence_class")
    assert out["evidence_color"] == "orange"
    assert out["unsourced_properties"] == []
    assert out["placeholder_sources"] == {}
    assert out["property_sources"] == sources


@pytest.mark.parametrize(
    ("value", "expected"),
    [
        ("https://doi.org/10.1000/xyz", True),
        ("ASTM G124-18", True),
        ("measured: in-house DSC 2026-08", True),
        ("NIST AMS 100-19", True),
        # What a metallurgist actually writes: a vendor document number, a
        # handbook volume. Neither has a DOI; both can be looked up.
        ("Special Metals INCONEL 718 datasheet SMC-045 (2007)", True),
        ("ASM Handbook Vol. 2", True),
        ("", False),
        # An alloy name is not a source, even with a digit in it.
        ("Inconel 718", False),
        ("nominal", False),
        ("IN625-class", False),
        ("assumed from Inconel", False),
        ("typical value", False),
        ("model memory", False),
    ],
)
def test_looks_like_citation_table(value, expected):
    assert lpbf_tools.looks_like_citation(value) is expected
