"""Tests for OMAT24Collector."""
import sys
import pytest
from unittest.mock import patch, MagicMock
from app.data.omat24_collector import OMAT24Collector


SAMPLE_ROWS = [
    {
        "id": "omat-001",
        "formula": "W2Rh",
        "elements": ["W", "Rh"],
        "energy": -12.5,
        "energy_per_atom": -4.17,
        "forces": [[0.1, 0.2, 0.3]],
        "stress": [1.0],
        "positions": [[0, 0, 0]],
        "cell": [[3, 0, 0], [0, 3, 0], [0, 0, 3]],
        "pbc": [True, True, True],
        "natoms": 3,
    },
    {
        "id": "omat-002",
        "formula": "Fe3O4",
        "elements": ["Fe", "O"],
        "energy": -20.0,
        "energy_per_atom": -2.86,
        "forces": None,
        "stress": None,
        "positions": None,
        "cell": None,
        "pbc": None,
        "natoms": 7,
    },
    {
        "id": "omat-003",
        "formula": "WO3",
        "elements": ["W", "O"],
        "energy": -15.0,
        "energy_per_atom": -3.75,
        "forces": None,
        "stress": None,
        "positions": None,
        "cell": None,
        "pbc": None,
        "natoms": 4,
    },
]


@pytest.fixture
def mock_datasets():
    """Inject a mock 'datasets' module into sys.modules."""
    mock_mod = MagicMock()
    with patch.dict(sys.modules, {"datasets": mock_mod}):
        yield mock_mod


class TestOMAT24Collector:
    def test_name(self):
        c = OMAT24Collector()
        assert c.name == "omat24"

    def test_supported_params(self):
        c = OMAT24Collector()
        assert set(c.supported_params()) == {"elements", "max_results", "formula"}

    def test_collect_all(self, mock_datasets):
        mock_datasets.load_dataset.return_value = iter(SAMPLE_ROWS)
        c = OMAT24Collector()
        results = c.collect(max_results=10)
        assert len(results) == 3
        assert results[0]["source"] == "omat24"
        assert results[0]["formula"] == "W2Rh"

    def test_collect_filter_elements(self, mock_datasets):
        mock_datasets.load_dataset.return_value = iter(SAMPLE_ROWS)
        c = OMAT24Collector()
        results = c.collect(elements=["W", "Rh"], max_results=10)
        assert len(results) == 1
        assert results[0]["formula"] == "W2Rh"

    def test_collect_filter_elements_partial(self, mock_datasets):
        mock_datasets.load_dataset.return_value = iter(SAMPLE_ROWS)
        c = OMAT24Collector()
        results = c.collect(elements=["W"], max_results=10)
        assert len(results) == 2  # W2Rh and WO3

    def test_collect_filter_formula(self, mock_datasets):
        mock_datasets.load_dataset.return_value = iter(SAMPLE_ROWS)
        c = OMAT24Collector()
        results = c.collect(formula="Fe3O4", max_results=10)
        assert len(results) == 1
        assert results[0]["formula"] == "Fe3O4"

    def test_collect_max_results(self, mock_datasets):
        mock_datasets.load_dataset.return_value = iter(SAMPLE_ROWS)
        c = OMAT24Collector()
        results = c.collect(max_results=2)
        assert len(results) == 2

    def test_collect_no_datasets_lib(self):
        """When datasets library is not installed, return empty."""
        import importlib
        # Block the datasets import by replacing with None in sys.modules
        saved = sys.modules.get("datasets")
        sys.modules["datasets"] = None  # type: ignore[assignment]
        try:
            # Re-import the collector module so it picks up the blocked import
            import app.data.omat24_collector as omat_mod
            importlib.reload(omat_mod)
            c = omat_mod.OMAT24Collector()
            results = c.collect(elements=["W"])
            assert results == []
        finally:
            if saved is not None:
                sys.modules["datasets"] = saved
            else:
                sys.modules.pop("datasets", None)
            # Reload again to restore normal behavior
            import app.data.omat24_collector as omat_mod
            importlib.reload(omat_mod)

    def test_parse_row(self):
        c = OMAT24Collector()
        record = c._parse_row(SAMPLE_ROWS[0])
        assert record["source"] == "omat24"
        assert record["source_id"] == "omat24:omat-001"
        assert record["formula"] == "W2Rh"
        assert record["elements"] == ["W", "Rh"]
        assert record["energy"] == -12.5

    def test_parse_row_composition_fallback(self):
        c = OMAT24Collector()
        row = {"id": "x", "composition": "NaCl"}
        record = c._parse_row(row)
        assert record["formula"] == "NaCl"

    def test_matches_elements_true(self):
        c = OMAT24Collector()
        record = {"elements": ["W", "Rh", "O"]}
        assert c._matches_elements(record, ["W", "Rh"]) is True

    def test_matches_elements_false(self):
        c = OMAT24Collector()
        record = {"elements": ["Fe", "O"]}
        assert c._matches_elements(record, ["W"]) is False

    def test_matches_elements_empty_record(self):
        c = OMAT24Collector()
        record = {"elements": []}
        assert c._matches_elements(record, ["W"]) is True  # Can't filter

    def test_collect_empty_dataset(self, mock_datasets):
        mock_datasets.load_dataset.return_value = iter([])
        c = OMAT24Collector()
        results = c.collect(max_results=10)
        assert results == []
