"""Tests for data collector."""
import sys
import pytest
from unittest.mock import patch, MagicMock
from app.tools.data_collectors.base_collector import CollectorConfigError
from app.tools.data_collectors.collector import OPTIMADECollector


class TestOPTIMADECollector:
    def test_init(self):
        collector = OPTIMADECollector()
        assert collector.providers is not None
        assert len(collector.providers) > 0

    def test_collect_by_elements(self):
        mock_client_cls = MagicMock()
        mock_client = mock_client_cls.return_value
        # Response format: {endpoint: {filter: {url: {data: [entries]}}}}
        mock_client.get.return_value = {
            "structures": {
                'elements HAS "Si"': {
                    "https://optimade.materialsproject.org/": {
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

    def test_collect_raises_on_transport_error(self):
        """A transport failure is a FAILED search, not an empty source.

        The old oracle here was `assert isinstance(results, list)`, which the
        broken behaviour satisfied: `except Exception: return []` turned an
        outage into "OPTIMADE holds no such materials". That is the same lie
        MPCollector's C2 fix names, and `collect_all`/skills.acquisition
        already record CollectorConfigError as a NAMED skip, so the honest
        shape has somewhere to go.
        """
        mock_client_cls = MagicMock()
        mock_client_cls.return_value.get.side_effect = Exception("Network error")
        mock_optimade = MagicMock()
        mock_optimade.client.OptimadeClient = mock_client_cls
        with patch.dict(
            sys.modules,
            {"optimade": mock_optimade, "optimade.client": mock_optimade.client},
        ):
            collector = OPTIMADECollector()
            with pytest.raises(CollectorConfigError, match="Network error"):
                collector.collect(
                    filter_string='elements HAS "Zz"', max_per_provider=5
                )

    def test_collect_raises_when_every_provider_returned_only_errors(self):
        """OptimadeClient does NOT raise on a provider failure — it returns
        that provider's slot as {"data": [], "errors": [...]}. Reading only
        `data` therefore reported an unreachable federation as zero hits;
        measured against a dead base_url, collect() returned []."""
        mock_client_cls = MagicMock()
        mock_client_cls.return_value.get.return_value = {
            "structures": {
                'elements HAS "Si"': {
                    "https://example.org/optimade": {
                        "data": [],
                        "errors": ["ConnectError: All connection attempts failed"],
                    }
                }
            }
        }
        mock_optimade = MagicMock()
        mock_optimade.client.OptimadeClient = mock_client_cls
        with patch.dict(
            sys.modules,
            {"optimade": mock_optimade, "optimade.client": mock_optimade.client},
        ):
            collector = OPTIMADECollector(
                providers=[{"id": "dead", "name": "dead",
                            "base_url": "https://example.org/optimade"}]
            )
            with pytest.raises(CollectorConfigError, match="ConnectError"):
                collector.collect(
                    filter_string='elements HAS "Si"', max_per_provider=5
                )

    def test_collect_keeps_data_when_only_some_providers_failed(self):
        """One dead endpoint must not discard the endpoints that answered."""
        mock_client_cls = MagicMock()
        mock_client_cls.return_value.get.return_value = {
            "structures": {
                'elements HAS "Si"': {
                    "https://dead.example.org/optimade": {
                        "data": [], "errors": ["ConnectError: nope"],
                    },
                    "https://live.example.org/optimade": {
                        "data": [{
                            "id": "x-1",
                            "attributes": {
                                "chemical_formula_descriptive": "Si",
                                "elements": ["Si"],
                                "nelements": 1,
                            },
                        }],
                    },
                }
            }
        }
        mock_optimade = MagicMock()
        mock_optimade.client.OptimadeClient = mock_client_cls
        with patch.dict(
            sys.modules,
            {"optimade": mock_optimade, "optimade.client": mock_optimade.client},
        ):
            collector = OPTIMADECollector(providers=[
                {"id": "dead", "name": "dead",
                 "base_url": "https://dead.example.org/optimade"},
                {"id": "live", "name": "live",
                 "base_url": "https://live.example.org/optimade"},
            ])
            results = collector.collect(
                filter_string='elements HAS "Si"', max_per_provider=5
            )
        assert [r["provider"] for r in results] == ["live"]

    def test_collect_raises_when_the_optimade_package_is_absent(self):
        """A missing optional dependency means the source was NOT consulted.

        `return []` claimed the federation holds nothing — a statement about
        the world made by an uninstalled package.
        """
        with patch.dict(sys.modules, {"optimade.client": None}):
            collector = OPTIMADECollector(
                providers=[{"id": "x", "name": "x", "base_url": "https://x/optimade"}]
            )
            with pytest.raises(CollectorConfigError, match="pip install optimade"):
                collector.collect(
                    filter_string='elements HAS "Si"', max_per_provider=5
                )
