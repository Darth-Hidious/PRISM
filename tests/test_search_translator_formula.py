"""OPTIMADE `chemical_formula_reduced` canonicalisation.

The spec requires alphabetical element order with GCD-reduced proportions.
A human writes `TiO2`; the canonical form is `O2Ti`. Sending the raw text made
every spec-compliant provider answer HTTP 200 with zero hits — Materials
Project included, which holds hundreds of TiO2 entries — so the federation
reported success while returning almost nothing.
"""

from app.tools.search_engine.query import MaterialSearchQuery
from app.tools.search_engine.translator import QueryTranslator, optimade_reduced_formula


def test_elements_are_alphabetised():
    # The regression this exists for.
    assert optimade_reduced_formula("TiO2") == "O2Ti"
    assert optimade_reduced_formula("SiC") == "CSi"
    assert optimade_reduced_formula("NbMoTaW") == "MoNbTaW"


def test_already_alphabetical_is_unchanged():
    assert optimade_reduced_formula("Fe2O3") == "Fe2O3"
    assert optimade_reduced_formula("H2O") == "H2O"
    assert optimade_reduced_formula("Al") == "Al"


def test_proportions_are_reduced_by_gcd():
    assert optimade_reduced_formula("Ti2O4") == "O2Ti"
    assert optimade_reduced_formula("Fe4O6") == "Fe2O3"
    # A count of 1 is omitted, never written as "1".
    assert optimade_reduced_formula("Ti2O2") == "OTi"


def test_unparseable_formulas_fall_back_rather_than_guess():
    # Structures this parser does not model must return None so the caller
    # sends the original text — a provider handles it better than a mangling.
    for bad in ["Ca(OH)2", "TiO2·H2O", "", "  ", "tio2", "*"]:
        assert optimade_reduced_formula(bad) is None


def test_translator_emits_the_canonical_form():
    got = QueryTranslator.to_optimade(MaterialSearchQuery(formula="TiO2"))
    assert 'chemical_formula_reduced="O2Ti"' in got
    assert "TiO2" not in got


def test_translator_passes_through_what_it_cannot_canonicalise():
    got = QueryTranslator.to_optimade(MaterialSearchQuery(formula="Ca(OH)2"))
    assert 'chemical_formula_reduced="Ca(OH)2"' in got
