"""Integration tests for Phase E-2 data sources."""
import sys
import pytest
from unittest.mock import patch, MagicMock


class TestGetDefaultCollectorRegistry:
    def test_returns_registry_with_builtin_collectors(self):
        from app.tools.data_collectors.base_collector import get_default_collector_registry
        reg = get_default_collector_registry()
        names = [c.name for c in reg.list_collectors()]
        # OPTIMADE and MP should always register (they have no import guards)
        assert "optimade" in names
        assert "mp" in names

    def test_includes_new_collectors(self):
        from app.tools.data_collectors.base_collector import get_default_collector_registry
        reg = get_default_collector_registry()
        names = [c.name for c in reg.list_collectors()]
        assert "omat24" in names
        # Literature retrieval moved to the Rust engine (`prism papers`);
        # it is deliberately NOT a Python collector anymore.
        assert "literature" not in names
        assert "patents" in names

    def test_at_least_five_collectors(self):
        from app.tools.data_collectors.base_collector import get_default_collector_registry
        reg = get_default_collector_registry()
        assert len(reg.list_collectors()) >= 5


class TestBuildFullRegistryIncludesSearchTools:
    def test_has_prior_art_search(self):
        """Round 6 collapse: literature_search + patent_search aliases
        were removed; prior_art_search is the canonical entry point."""
        from app.plugins.bootstrap import build_full_registry
        reg, _prov, _agents = build_full_registry(enable_mcp=False, enable_plugins=False)
        names = [t.name for t in reg.list_tools()]
        assert "prior_art_search" in names
        # Old aliases must be gone
        assert "literature_search" not in names
        assert "patent_search" not in names


class TestOMAT24Integration:
    def test_collect_with_element_filter(self):
        mock_mod = MagicMock()
        mock_mod.load_dataset.return_value = iter([
            {"id": "1", "formula": "WRh", "elements": ["W", "Rh"],
             "energy": -10, "energy_per_atom": -5, "forces": None,
             "stress": None, "positions": None, "cell": None,
             "pbc": None, "natoms": 2},
            {"id": "2", "formula": "FeO", "elements": ["Fe", "O"],
             "energy": -8, "energy_per_atom": -4, "forces": None,
             "stress": None, "positions": None, "cell": None,
             "pbc": None, "natoms": 2},
        ])
        with patch.dict(sys.modules, {"datasets": mock_mod}):
            from app.tools.data_collectors.omat24_collector import OMAT24Collector
            c = OMAT24Collector()
            results = c.collect(elements=["W"], max_results=10)
            assert len(results) == 1
            assert results[0]["formula"] == "WRh"


class TestPatentIntegration:
    @patch("app.tools.data_collectors.patent_collector.requests")
    @patch.dict("os.environ", {"LENS_API_TOKEN": "test-token"})
    def test_parsed_results(self, mock_requests):
        resp = MagicMock()
        resp.json.return_value = {
            "data": [
                {
                    "lens_id": "p1",
                    "title": "Alloy Patent",
                    "abstract": "Patent abstract.",
                    "date_published": "2024-03-01",
                    "inventor": [{"extracted_name": {"value": "Inv1"}}],
                    "applicant": [{"extracted_name": {"value": "Corp1"}}],
                    "jurisdiction": "US",
                }
            ]
        }
        resp.raise_for_status = MagicMock()
        mock_requests.post.return_value = resp

        from app.tools.data_collectors.patent_collector import PatentCollector
        c = PatentCollector()
        results = c.collect(query="alloy", max_results=10)
        assert len(results) == 1
        assert results[0]["type"] == "patent"
        assert results[0]["inventors"] == ["Inv1"]


class TestAcquisitionSkillWithNewSources:
    @patch("app.tools.data_collectors.normalizer.normalize_records")
    @patch("app.tools.data_collectors.store.DataStore")
    def test_omat24_source(self, MockStore, mock_normalize):
        import pandas as pd
        mock_mod = MagicMock()
        mock_mod.load_dataset.return_value = iter([
            {"id": "1", "formula": "WRh", "elements": ["W", "Rh"],
             "energy": -10, "energy_per_atom": -5, "forces": None,
             "stress": None, "positions": None, "cell": None,
             "pbc": None, "natoms": 2},
        ])
        mock_df = pd.DataFrame([{"source": "omat24", "formula": "WRh"}])
        mock_normalize.return_value = mock_df
        mock_store_inst = MockStore.return_value
        mock_store_inst.save = MagicMock()

        with patch.dict(sys.modules, {"datasets": mock_mod}):
            from app.tools.skills.acquisition import _acquire_materials
            result = _acquire_materials(
                elements=["W", "Rh"],
                sources=["omat24"],
                max_results=10,
                dataset_name="test_omat24",
            )
            assert result["total_records"] == 1
            assert "omat24" in result["sources_queried"]


class TestSupportedParams:
    def test_optimade_supported_params(self):
        from app.tools.data_collectors.collector import OPTIMADECollector
        c = OPTIMADECollector()
        assert "filter_string" in c.supported_params()
        assert "max_per_provider" in c.supported_params()

    def test_mp_supported_params(self):
        from app.tools.data_collectors.collector import MPCollector
        c = MPCollector()
        assert "formula" in c.supported_params()
        assert "elements" in c.supported_params()

    def test_omat24_supported_params(self):
        from app.tools.data_collectors.omat24_collector import OMAT24Collector
        c = OMAT24Collector()
        assert "elements" in c.supported_params()

    def test_patents_supported_params(self):
        from app.tools.data_collectors.patent_collector import PatentCollector
        c = PatentCollector()
        assert "query" in c.supported_params()


class TestMPCollectorProxyHonesty:
    """C2 honesty fix: an MP-proxy failure must RAISE CollectorConfigError
    (surfaced as a logged skip by collect_all), never return [] — an outage
    must be distinguishable from "source is genuinely empty"."""

    def test_proxy_error_raises(self):
        from app.tools.data_collectors.base_collector import CollectorConfigError
        from app.tools.data_collectors.collector import MPCollector

        c = MPCollector()
        with (
            patch.dict("os.environ", {}, clear=True),
            patch(
                "app.tools.data._query_materials_project",
                return_value={"error": "MP proxy unreachable"},
            ),
        ):
            with pytest.raises(CollectorConfigError, match="proxy"):
                c.collect(formula="Fe2O3")

    def test_proxy_empty_results_is_honest_empty(self):
        from app.tools.data_collectors.collector import MPCollector

        c = MPCollector()
        with (
            patch.dict("os.environ", {}, clear=True),
            patch(
                "app.tools.data._query_materials_project",
                return_value={"results": [], "count": 0},
            ),
        ):
            assert c.collect(formula="Xx99Zz") == []

    def test_proxy_success_returns_entries(self):
        from app.tools.data_collectors.collector import MPCollector

        c = MPCollector()
        fake = {
            "results": [
                {"material_id": "mp-1", "formula_pretty": "Fe2O3", "band_gap": 2.2}
            ],
            "count": 1,
        }
        with (
            patch.dict("os.environ", {}, clear=True),
            patch("app.tools.data._query_materials_project", return_value=fake),
        ):
            out = c.collect(formula="Fe2O3")
        assert out == [{"material_id": "mp-1", "formula_pretty": "Fe2O3", "band_gap": 2.2}]
