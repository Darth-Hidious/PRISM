"""A count of zero must mean "searched and found nothing".

Measured 2026-08-20 on a live run: the agent called
`prior_art_search(source="papers")` and got back
`counts: {papers: 39, patents: 0, eastern: 0}`. Both zeros were for backends
that were never contacted, and nothing in the payload said so — so "I did not
look for patents" and "there are no relevant patents" were the same bytes.

That mattered in context: the model had just been told the patent backend was
unconfigured, and a plain `0` invites the conclusion that the prior art is
settled. Absence of evidence must not be encoded as evidence of absence.

A blank query is used throughout so no branch touches the network: the
literature impl returns early on an empty query, the patent branch refuses
before building a client, and eastern's collector gets nothing to fetch.
"""
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from app.tools.search import _prior_art_search


@pytest.mark.parametrize(
    ("source", "expected_searched"),
    [
        ("papers", {"papers"}),
        ("patents", {"patents"}),
        ("eastern", {"eastern"}),
        ("both", {"papers", "patents", "eastern"}),
    ],
)
def test_unsearched_sources_count_none_not_zero(source, expected_searched):
    out = _prior_art_search(query="", source=source, max_results=1)

    assert set(out["searched"]) == expected_searched

    for name in ("papers", "patents", "eastern"):
        count = out["counts"][name]
        if name in expected_searched:
            assert count == 0, (
                f"{name} WAS searched, so its count must be a real number"
            )
        else:
            assert count is None, (
                f"{name} was never searched; reporting 0 lets the caller read "
                f"'no {name} found' from a backend nobody asked"
            )


def test_the_uniform_shape_is_preserved():
    """Every key still exists for every source — callers must not null-check
    the arrays, which is why the counts (not the lists) carry the signal."""
    out = _prior_art_search(query="", source="papers", max_results=1)
    for key in ("papers", "patents", "eastern", "counts", "searched", "query"):
        assert key in out, f"the uniform result shape lost `{key}`"
    assert out["patents"] == [] and out["eastern"] == []
