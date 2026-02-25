from app.search.query import MaterialSearchQuery, PropertyRange


def test_to_optimade_elements():
    from app.search.translator import QueryTranslator
    q = MaterialSearchQuery(elements=["Fe", "O"])
    f = QueryTranslator.to_optimade(q)
    assert f == 'elements HAS ALL "Fe","O"'


def test_to_optimade_exclude_elements():
    from app.search.translator import QueryTranslator
    q = MaterialSearchQuery(exclude_elements=["C"])
    f = QueryTranslator.to_optimade(q)
    assert 'NOT elements HAS "C"' in f


def test_to_optimade_formula():
    from app.search.translator import QueryTranslator
    q = MaterialSearchQuery(formula="SiO2")
    f = QueryTranslator.to_optimade(q)
    assert 'chemical_formula_reduced="SiO2"' in f


def test_to_optimade_n_elements():
    from app.search.translator import QueryTranslator
    q = MaterialSearchQuery(n_elements=PropertyRange(min=2, max=4))
    f = QueryTranslator.to_optimade(q)
    assert "nelements>=2" in f
    assert "nelements<=4" in f


def test_to_optimade_combined():
    from app.search.translator import QueryTranslator
    q = MaterialSearchQuery(elements=["Fe", "O"], n_elements=PropertyRange(max=3))
    f = QueryTranslator.to_optimade(q)
    assert "AND" in f
    assert 'elements HAS ALL "Fe","O"' in f
    assert "nelements<=3" in f


def test_to_optimade_space_group():
    from app.search.translator import QueryTranslator
    q = MaterialSearchQuery(space_group="Fm-3m")
    f = QueryTranslator.to_optimade(q)
    assert 'space_group_symbol="Fm-3m"' in f


def test_to_optimade_empty_query():
    from app.search.translator import QueryTranslator
    q = MaterialSearchQuery()
    f = QueryTranslator.to_optimade(q)
    assert f == ""


def test_to_mp_kwargs_elements():
    from app.search.translator import QueryTranslator
    q = MaterialSearchQuery(elements=["Fe", "O"])
    kw = QueryTranslator.to_mp_kwargs(q)
    assert kw["elements"] == ["Fe", "O"]


def test_to_mp_kwargs_band_gap():
    from app.search.translator import QueryTranslator
    q = MaterialSearchQuery(band_gap=PropertyRange(min=1.0, max=3.0))
    kw = QueryTranslator.to_mp_kwargs(q)
    assert kw["band_gap"] == (1.0, 3.0)


def test_to_mp_kwargs_empty():
    from app.search.translator import QueryTranslator
    q = MaterialSearchQuery()
    kw = QueryTranslator.to_mp_kwargs(q)
    assert kw == {}
