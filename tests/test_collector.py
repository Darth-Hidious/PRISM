"""Tests for data collector."""
import sys
import pytest
from unittest.mock import patch, MagicMock
from app.data.collector import OPTIMADECollector


class TestOPTIMADECollector:
    def test_init(self):
        collector = OPTIMADECollector()
        assert collector.providers is not None
        assert len(collector.providers) > 0

    def test_collect_by_elements(self):
        mock_client_cls = MagicMock()
        mock_client = mock_client_cls.return_value
        mock_client.get.return_value = {
            "mp": {
                "data": [
                    {
                        "id": "mp-1",
                        "attributes": {
                            "chemical_formula_descriptive": "Si",
                            "elements": ["Si"],
                            "nelements": 1,
                        },
                    }
                ]
            }
        }
        mock_optimade = MagicMock()
        mock_optimade.client.OptimadeClient = mock_client_cls
        with patch.dict(
            sys.modules,
            {"optimade": mock_optimade, "optimade.client": mock_optimade.client},
        ):
            collector = OPTIMADECollector()
            results = collector.collect(
                filter_string='elements HAS "Si"', max_per_provider=5
            )
            assert len(results) > 0
            assert "formula" in results[0]

    def test_collect_handles_errors(self):
        mock_client_cls = MagicMock()
        mock_client_cls.return_value.get.side_effect = Exception("Network error")
        mock_optimade = MagicMock()
        mock_optimade.client.OptimadeClient = mock_client_cls
        with patch.dict(
            sys.modules,
            {"optimade": mock_optimade, "optimade.client": mock_optimade.client},
        ):
            collector = OPTIMADECollector()
            results = collector.collect(
                filter_string='elements HAS "Zz"', max_per_provider=5
            )
            assert isinstance(results, list)
