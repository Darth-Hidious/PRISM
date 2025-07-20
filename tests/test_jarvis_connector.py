"""
Unit tests for JARVIS-DFT Database Connector.
"""

import json
import pytest
from unittest.mock import AsyncMock, MagicMock, patch
from datetime import datetime

import httpx

from app.services.connectors.jarvis_connector import JarvisConnector, create_jarvis_connector
from app.services.connectors.base_connector import (
    ConnectorException,
    ConnectorTimeoutException,
    ConnectorNotFoundException,
    ConnectorRateLimitException
)


@pytest.fixture
def mock_jarvis_data():
    """Sample JARVIS materials data for testing."""
    return [
        {
            "jid": "JVASP-1001",
            "formula": "Si2",
            "formation_energy_peratom": -5.425,
            "ehull": 0.0,
            "bulk_modulus_kv": 97.8,
            "shear_modulus_gv": 51.5,
            "elastic_tensor": [[161.9, 63.9, 63.9], [63.9, 161.9, 63.9], [63.9, 63.9, 161.9]],
            "nelements": 1,
            "atoms": {
                "lattice_mat": [[5.43, 0.0, 0.0], [0.0, 5.43, 0.0], [0.0, 0.0, 5.43]],
                "elements": ["Si", "Si"],
                "coords": [[0.0, 0.0, 0.0], [0.25, 0.25, 0.25]],
                "cart_coords": [[0.0, 0.0, 0.0], [1.3575, 1.3575, 1.3575]]
            }
        },
        {
            "jid": "JVASP-1002", 
            "formula": "GaN",
            "formation_energy_peratom": -1.23,
            "ehull": 0.01,
            "bulk_modulus_kv": 207.0,
            "nelements": 2,
            "atoms": {
                "lattice_mat": [[3.19, 0.0, 0.0], [0.0, 3.19, 0.0], [0.0, 0.0, 5.18]],
                "elements": ["Ga", "N"],
                "coords": [[0.0, 0.0, 0.0], [0.33, 0.33, 0.5]]
            }
        }
    ]


@pytest.fixture
def connector():
    """Create a JARVIS connector instance for testing."""
    return JarvisConnector(
        timeout=10,
        max_retries=2,
        requests_per_second=10.0,  # Higher rate for testing
        burst_capacity=20
    )


@pytest.mark.asyncio
class TestJarvisConnector:
    """Test suite for JARVIS connector functionality."""

    async def test_connector_initialization(self):
        """Test connector initialization with default and custom parameters."""
        # Default initialization
        connector = JarvisConnector()
        assert connector.timeout == 30
        assert connector.max_retries == 3

        # Custom initialization
        custom_connector = JarvisConnector(
            timeout=60,
            max_retries=5,
            requests_per_second=1.0,
            burst_capacity=5
        )
        assert custom_connector.timeout == 60
        assert custom_connector.max_retries == 5

    async def test_factory_function(self):
        """Test factory function for creating connector."""
        connector = create_jarvis_connector(timeout=15)
        assert isinstance(connector, JarvisConnector)
        assert connector.timeout == 15

    async def test_connect_success(self, connector):
        """Test successful connection establishment."""
        result = await connector.connect()
        assert result is True

    async def test_disconnect(self, connector):
        """Test proper disconnection."""
        await connector.connect()
        await connector.disconnect()
        assert connector._client is None

    async def test_health_check_success(self, connector):
        """Test successful health check."""
        result = await connector.health_check()
        assert result is True

    @patch('app.services.connectors.jarvis_connector.jdata')
    async def test_search_materials_by_formula(self, mock_jdata, connector, mock_jarvis_data):
        """Test searching materials by chemical formula."""
        mock_jdata.return_value = mock_jarvis_data
        
        results = await connector.search_materials(formula="Si")
        
        assert len(results) == 1
        assert results[0]["formula"] == "Si2"
        assert results[0]["jid"] == "JVASP-1001"
        mock_jdata.assert_called_once_with(dataset="dft_3d")

    @patch('app.services.connectors.jarvis_connector.jdata')
    async def test_get_material_by_id_success(self, mock_jdata, connector, mock_jarvis_data):
        """Test retrieving material by JARVIS ID."""
        mock_jdata.return_value = mock_jarvis_data
        
        result = await connector.get_material_by_id("JVASP-1001")
        
        assert result["jid"] == "JVASP-1001"
        assert result["formula"] == "Si2"
        assert result["formation_energy_peratom"] == -5.425

    @patch('app.services.connectors.jarvis_connector.jdata')
    async def test_get_material_by_id_not_found(self, mock_jdata, connector, mock_jarvis_data):
        """Test handling of material not found."""
        mock_jdata.return_value = mock_jarvis_data
        
        with pytest.raises(ConnectorNotFoundException):
            await connector.get_material_by_id("NONEXISTENT-ID")

    @patch('app.services.connectors.jarvis_connector.jdata')
    async def test_fetch_bulk_materials(self, mock_jdata, connector, mock_jarvis_data):
        """Test bulk fetching with pagination."""
        # Create extended dataset
        extended_data = mock_jarvis_data * 10  # 20 materials
        mock_jdata.return_value = extended_data
        
        # Test first page
        results = await connector.fetch_bulk_materials(limit=5, offset=0)
        assert len(results) == 5
        
        # Test second page
        results = await connector.fetch_bulk_materials(limit=5, offset=5)
        assert len(results) == 5
        
        # Test beyond available data
        results = await connector.fetch_bulk_materials(limit=10, offset=15)
        assert len(results) == 5  # Only 5 remaining


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
