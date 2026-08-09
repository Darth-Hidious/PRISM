from app.tools.search_engine.query import MaterialSearchQuery, PropertyRange


def test_to_optimade_elements():
    from app.tools.search_engine.translator import QueryTranslator
    q = MaterialSearchQuery(elements=["Fe", "O"])
    f = QueryTranslator.to_optimade(q)
    assert f == 'elements HAS ALL "Fe","O"'


def test_to_optimade_exclude_elements():
    from app.tools.search_engine.translator import QueryTranslator
    q = MaterialSearchQuery(exclude_elements=["C"])
    f = QueryTranslator.to_optimade(q)
    assert 'NOT elements HAS "C"' in f


def test_to_optimade_formula():
    """`chemical_formula_reduced` is alphabetical and GCD-reduced per the spec.

    A user typing "SiO2" must go on the wire as "O2Si". Sending the input
    spelling matched almost nothing: providers store the canonical form, so
    only one of nine responding databases had data for a query like this.
    """
    from app.tools.search_engine.translator import QueryTranslator
    q = MaterialSearchQuery(formula="SiO2")
    f = QueryTranslator.to_optimade(q)
    assert 'chemical_formula_reduced="O2Si"' in f


def test_to_optimade_n_elements():
    from app.tools.search_engine.translator import QueryTranslator
    q = MaterialSearchQuery(n_elements=PropertyRange(min=2, max=4))
    f = QueryTranslator.to_optimade(q)
    assert "nelements>=2" in f
    assert "nelements<=4" in f


def test_to_optimade_combined():
    from app.tools.search_engine.translator import QueryTranslator
    q = MaterialSearchQuery(elements=["Fe", "O"], n_elements=PropertyRange(max=3))
    f = QueryTranslator.to_optimade(q)
    assert "AND" in f
    assert 'elements HAS ALL "Fe","O"' in f
    assert "nelements<=3" in f


def test_to_optimade_space_group_symbol_uses_spec_field():
    """`space_group_symbol` is NOT an OPTIMADE field; the spec defines
    `space_group_symbol_hermann_mauguin` (and `space_group_it_number`)."""
    from app.tools.search_engine.translator import QueryTranslator
    q = MaterialSearchQuery(space_group="Fm-3m")
    f = QueryTranslator.to_optimade(q)
    assert 'space_group_symbol_hermann_mauguin="Fm-3m"' in f


def test_to_optimade_space_group_number_uses_it_number():
    from app.tools.search_engine.translator import QueryTranslator
    q = MaterialSearchQuery(space_group=225)
    f = QueryTranslator.to_optimade(q)
    assert "space_group_it_number=225" in f


def test_to_optimade_empty_query():
    from app.tools.search_engine.translator import QueryTranslator
    q = MaterialSearchQuery()
    f = QueryTranslator.to_optimade(q)
    assert f == ""


def test_to_mp_kwargs_elements():
    from app.tools.search_engine.translator import QueryTranslator
    q = MaterialSearchQuery(elements=["Fe", "O"])
    kw = QueryTranslator.to_mp_kwargs(q)
    assert kw["elements"] == ["Fe", "O"]


def test_to_mp_kwargs_band_gap():
    from app.tools.search_engine.translator import QueryTranslator
    q = MaterialSearchQuery(band_gap=PropertyRange(min=1.0, max=3.0))
    kw = QueryTranslator.to_mp_kwargs(q)
    assert kw["band_gap"] == (1.0, 3.0)


def test_to_mp_kwargs_empty():
    from app.tools.search_engine.translator import QueryTranslator
    q = MaterialSearchQuery()
    kw = QueryTranslator.to_mp_kwargs(q)
    assert kw == {}


def test_to_mp_kwargs_n_elements():
    """`nelements` is advertised as filterable on mp_native; the translator
    must actually send it (MPRester's num_elements tuple), not drop it."""
    from app.tools.search_engine.translator import QueryTranslator
    q = MaterialSearchQuery(n_elements=PropertyRange(min=2, max=3))
    kw = QueryTranslator.to_mp_kwargs(q)
    assert kw["num_elements"] == (2, 3)


def test_to_mp_kwargs_space_group_symbol():
    from app.tools.search_engine.translator import QueryTranslator
    q = MaterialSearchQuery(space_group="Fm-3m")
    kw = QueryTranslator.to_mp_kwargs(q)
    assert kw["spacegroup_symbol"] == "Fm-3m"
    assert "spacegroup_number" not in kw


def test_to_mp_kwargs_space_group_number():
    from app.tools.search_engine.translator import QueryTranslator
    q = MaterialSearchQuery(space_group=225)
    kw = QueryTranslator.to_mp_kwargs(q)
    assert kw["spacegroup_number"] == 225
    assert "spacegroup_symbol" not in kw


def test_to_mp_kwargs_bulk_modulus_maps_to_k_vrh():
    """A declared-filterable bulk_modulus used to be dropped AND the field
    never requested -- a capability that guaranteed zero results."""
    from app.tools.search_engine.translator import QueryTranslator
    q = MaterialSearchQuery(bulk_modulus=PropertyRange(min=100.0, max=300.0))
    kw = QueryTranslator.to_mp_kwargs(q)
    assert kw["k_vrh"] == (100.0, 300.0)
