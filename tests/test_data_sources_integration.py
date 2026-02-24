"""Integration tests for Phase E-2 data sources."""
import sys
import pytest
from unittest.mock import patch, MagicMock


class TestGetDefaultCollectorRegistry:
    def test_returns_registry_with_builtin_collectors(self):
        from app.data.base_collector import get_default_collector_registry
        reg = get_default_collector_registry()
        names = [c.name for c in reg.list_collectors()]
        # OPTIMADE and MP should always register (they have no import guards)
        assert "optimade" in names
        assert "mp" in names

    def test_includes_new_collectors(self):
        from app.data.base_collector import get_default_collector_registry
        reg = get_default_collector_registry()
        names = [c.name for c in reg.list_collectors()]
        assert "omat24" in names
        assert "literature" in names
        assert "patents" in names

    def test_at_least_five_collectors(self):
        from app.data.base_collector import get_default_collector_registry
        reg = get_default_collector_registry()
        assert len(reg.list_collectors()) >= 5


class TestBuildFullRegistryIncludesSearchTools:
    def test_has_literature_search(self):
        from app.plugins.bootstrap import build_full_registry
        reg = build_full_registry(enable_mcp=False, enable_plugins=False)
        names = [t.name for t in reg.list_tools()]
        assert "literature_search" in names

    def test_has_patent_search(self):
        from app.plugins.bootstrap import build_full_registry
        reg = build_full_registry(enable_mcp=False, enable_plugins=False)
        names = [t.name for t in reg.list_tools()]
        assert "patent_search" in names


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
            from app.data.omat24_collector import OMAT24Collector
            c = OMAT24Collector()
            results = c.collect(elements=["W"], max_results=10)
            assert len(results) == 1
            assert results[0]["formula"] == "WRh"


class TestLiteratureIntegration:
    @patch("app.data.literature_collector.requests")
    def test_combined_results(self, mock_requests):
        arxiv_resp = MagicMock()
        arxiv_resp.text = """<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <id>http://arxiv.org/abs/2401.00001</id>
    <title>Test Paper</title>
    <summary>Abstract text.</summary>
    <published>2024-01-01T00:00:00Z</published>
    <author><name>Author A</name></author>
  </entry>
</feed>"""
        arxiv_resp.raise_for_status = MagicMock()

        s2_resp = MagicMock()
        s2_resp.json.return_value = {
            "data": [
                {"paperId": "s2-1", "title": "S2 Paper", "authors": [{"name": "B"}],
                 "abstract": "S2 abstract", "year": 2024, "url": "http://s2.org",
                 "citationCount": 5}
            ]
        }
        s2_resp.raise_for_status = MagicMock()

        mock_requests.get.side_effect = [arxiv_resp, s2_resp]

        from app.data.literature_collector import LiteratureCollector
        c = LiteratureCollector()
        results = c.collect(query="tungsten alloy", max_results=20)
        assert len(results) == 2
        sources = {r["source"] for r in results}
        assert sources == {"arxiv", "semantic_scholar"}


class TestPatentIntegration:
    @patch("app.data.patent_collector.requests")
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

        from app.data.patent_collector import PatentCollector
        c = PatentCollector()
        results = c.collect(query="alloy", max_results=10)
        assert len(results) == 1
        assert results[0]["type"] == "patent"
        assert results[0]["inventors"] == ["Inv1"]


class TestAcquisitionSkillWithNewSources:
    @patch("app.data.normalizer.normalize_records")
    @patch("app.data.store.DataStore")
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
            from app.skills.acquisition import _acquire_materials
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
        from app.data.collector import OPTIMADECollector
        c = OPTIMADECollector()
        assert "filter_string" in c.supported_params()
        assert "max_per_provider" in c.supported_params()

    def test_mp_supported_params(self):
        from app.data.collector import MPCollector
        c = MPCollector()
        assert "formula" in c.supported_params()
        assert "elements" in c.supported_params()

    def test_omat24_supported_params(self):
        from app.data.omat24_collector import OMAT24Collector
        c = OMAT24Collector()
        assert "elements" in c.supported_params()

    def test_literature_supported_params(self):
        from app.data.literature_collector import LiteratureCollector
        c = LiteratureCollector()
        assert "query" in c.supported_params()

    def test_patents_supported_params(self):
        from app.data.patent_collector import PatentCollector
        c = PatentCollector()
        assert "query" in c.supported_params()
