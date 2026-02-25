import pytest
from pydantic import ValidationError


def test_empty_query_is_valid():
    from app.search.query import MaterialSearchQuery
    q = MaterialSearchQuery()
    assert q.elements is None
    assert q.limit == 100


def test_elements_validation_accepts_valid():
    from app.search.query import MaterialSearchQuery
    q = MaterialSearchQuery(elements=["Fe", "O"])
    assert q.elements == ["Fe", "O"]


def test_elements_validation_rejects_invalid():
    from app.search.query import MaterialSearchQuery
    with pytest.raises(ValidationError):
        MaterialSearchQuery(elements=["Fe", "Unobtainium"])


def test_property_range():
    from app.search.query import MaterialSearchQuery, PropertyRange
    q = MaterialSearchQuery(band_gap=PropertyRange(min=1.0, max=3.0))
    assert q.band_gap.min == 1.0
    assert q.band_gap.max == 3.0


def test_property_range_rejects_min_gt_max():
    from app.search.query import MaterialSearchQuery, PropertyRange
    with pytest.raises(ValidationError):
        MaterialSearchQuery(band_gap=PropertyRange(min=5.0, max=2.0))


def test_limit_bounds():
    from app.search.query import MaterialSearchQuery
    with pytest.raises(ValidationError):
        MaterialSearchQuery(limit=0)
    with pytest.raises(ValidationError):
        MaterialSearchQuery(limit=20000)


def test_crystal_system_literal():
    from app.search.query import MaterialSearchQuery
    q = MaterialSearchQuery(crystal_system="cubic")
    assert q.crystal_system == "cubic"
    with pytest.raises(ValidationError):
        MaterialSearchQuery(crystal_system="invalid")


def test_n_elements_range():
    from app.search.query import MaterialSearchQuery, PropertyRange
    q = MaterialSearchQuery(n_elements=PropertyRange(min=2, max=4))
    assert q.n_elements.min == 2


def test_query_hash_stable():
    from app.search.query import MaterialSearchQuery
    q1 = MaterialSearchQuery(elements=["Fe", "O"], limit=50)
    q2 = MaterialSearchQuery(elements=["Fe", "O"], limit=50)
    assert q1.query_hash() == q2.query_hash()


def test_query_hash_different():
    from app.search.query import MaterialSearchQuery
    q1 = MaterialSearchQuery(elements=["Fe", "O"])
    q2 = MaterialSearchQuery(elements=["Fe", "Si"])
    assert q1.query_hash() != q2.query_hash()
