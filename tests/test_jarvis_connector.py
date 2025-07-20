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
        assert connector.BASE_URL == "https://jarvis.nist.gov"
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
    
    @patch('httpx.AsyncClient')
    async def test_connect_success(self, mock_client_class, connector):
        """Test successful connection establishment."""
        mock_client = AsyncMock()
        mock_client.get.return_value.status_code = 200
        mock_client_class.return_value = mock_client
        
        result = await connector.connect()
        
        assert result is True
        assert connector._client is not None
        mock_client.get.assert_called_once()
    
    @patch('httpx.AsyncClient')
    async def test_connect_failure(self, mock_client_class, connector):
        """Test connection failure handling."""
        mock_client = AsyncMock()
        mock_client.get.side_effect = httpx.ConnectError("Connection failed")
        mock_client_class.return_value = mock_client
        
        result = await connector.connect()
        
        assert result is False
        assert connector._client is not None  # Client created but connection failed
    
    async def test_disconnect(self, connector):
        """Test proper disconnection."""
        # Set up a mock client
        connector._client = AsyncMock()
        
        await connector.disconnect()
        
        connector._client.aclose.assert_called_once()
        assert connector._client is None
    
    @patch('httpx.AsyncClient')
    async def test_health_check_success(self, mock_client_class, connector):
        """Test successful health check."""
        mock_client = AsyncMock()
        mock_client.get.return_value.status_code = 200
        mock_client_class.return_value = mock_client
        
        result = await connector.health_check()
        
        assert result is True
    
    @patch('httpx.AsyncClient')
    async def test_health_check_failure(self, mock_client_class, connector):
        """Test health check failure."""
        mock_client = AsyncMock()
        mock_client.get.side_effect = httpx.TimeoutException("Timeout")
        mock_client_class.return_value = mock_client
        
        result = await connector.health_check()
        
        assert result is False
    
    @patch.object(JarvisConnector, '_load_dataset')
    async def test_search_materials_by_formula(self, mock_load, connector, mock_jarvis_data):
        """Test searching materials by chemical formula."""
        mock_load.return_value = mock_jarvis_data
        
        results = await connector.search_materials(formula="Si")
        
        assert len(results) == 1
        assert results[0]["formula"] == "Si2"
        assert results[0]["jid"] == "JVASP-1001"
        mock_load.assert_called_once_with("dft_3d")
    
    @patch.object(JarvisConnector, '_load_dataset')
    async def test_search_materials_by_elements(self, mock_load, connector, mock_jarvis_data):
        """Test searching materials by number of elements."""
        mock_load.return_value = mock_jarvis_data
        
        results = await connector.search_materials(n_elements=2)
        
        assert len(results) == 1
        assert results[0]["formula"] == "GaN"
        assert results[0]["jid"] == "JVASP-1002"
    
    @patch.object(JarvisConnector, '_load_dataset')
    async def test_search_materials_with_properties(self, mock_load, connector, mock_jarvis_data):
        """Test searching materials with specific properties."""
        mock_load.return_value = mock_jarvis_data
        
        results = await connector.search_materials(
            formula="Si",
            properties=["bulk_modulus_kv", "shear_modulus_gv"]
        )
        
        assert len(results) == 1
        assert "bulk_modulus_kv" in results[0]
        assert "shear_modulus_gv" in results[0]
        assert results[0]["bulk_modulus_kv"] == 97.8
    
    @patch.object(JarvisConnector, '_load_dataset')
    async def test_search_materials_limit(self, mock_load, connector, mock_jarvis_data):
        """Test search results limit."""
        # Create more test data
        extended_data = mock_jarvis_data * 10  # 20 materials
        mock_load.return_value = extended_data
        
        results = await connector.search_materials(limit=5)
        
        assert len(results) == 5
    
    @patch.object(JarvisConnector, '_load_dataset')
    async def test_get_material_by_id_success(self, mock_load, connector, mock_jarvis_data):
        """Test retrieving material by JARVIS ID."""
        mock_load.return_value = mock_jarvis_data
        
        result = await connector.get_material_by_id("JVASP-1001")
        
        assert result["jid"] == "JVASP-1001"
        assert result["formula"] == "Si2"
        assert result["formation_energy_peratom"] == -5.425
    
    @patch.object(JarvisConnector, '_load_dataset')
    async def test_get_material_by_id_not_found(self, mock_load, connector, mock_jarvis_data):
        """Test handling of material not found."""
        mock_load.return_value = mock_jarvis_data
        
        with pytest.raises(ConnectorNotFoundException):
            await connector.get_material_by_id("NONEXISTENT-ID")
    
    @patch.object(JarvisConnector, '_load_dataset')
    async def test_fetch_bulk_materials(self, mock_load, connector, mock_jarvis_data):
        """Test bulk fetching with pagination."""
        # Create extended dataset
        extended_data = mock_jarvis_data * 10  # 20 materials
        mock_load.return_value = extended_data
        
        # Test first page
        results = await connector.fetch_bulk_materials(limit=5, offset=0)
        assert len(results) == 5
        
        # Test second page
        results = await connector.fetch_bulk_materials(limit=5, offset=5)
        assert len(results) == 5
        
        # Test beyond available data
        results = await connector.fetch_bulk_materials(limit=10, offset=15)
        assert len(results) == 5  # Only 5 remaining
    
    @patch('httpx.AsyncClient')
    async def test_load_dataset_success(self, mock_client_class, connector, mock_jarvis_data):
        """Test successful dataset loading."""
        mock_client = AsyncMock()
        mock_response = MagicMock()
        mock_response.json.return_value = mock_jarvis_data
        mock_response.raise_for_status.return_value = None
        mock_client.get.return_value = mock_response
        mock_client_class.return_value = mock_client
        
        connector._client = mock_client
        
        result = await connector._load_dataset("dft_3d")
        
        assert result == mock_jarvis_data
        assert len(result) == 2
    
    @patch('httpx.AsyncClient')
    async def test_load_dataset_caching(self, mock_client_class, connector, mock_jarvis_data):
        """Test dataset caching functionality."""
        mock_client = AsyncMock()
        mock_response = MagicMock()
        mock_response.json.return_value = mock_jarvis_data
        mock_response.raise_for_status.return_value = None
        mock_client.get.return_value = mock_response
        mock_client_class.return_value = mock_client
        
        connector._client = mock_client
        
        # First call should hit the API
        result1 = await connector._load_dataset("dft_3d")
        
        # Second call should use cache
        result2 = await connector._load_dataset("dft_3d")
        
        assert result1 == result2
        # API should only be called once due to caching
        mock_client.get.assert_called_once()
    
    @patch('httpx.AsyncClient')
    async def test_load_dataset_timeout(self, mock_client_class, connector):
        """Test dataset loading timeout handling."""
        mock_client = AsyncMock()
        mock_client.get.side_effect = httpx.TimeoutException("Timeout")
        mock_client_class.return_value = mock_client
        
        connector._client = mock_client
        
        with pytest.raises(ConnectorTimeoutException):
            await connector._load_dataset("dft_3d")
    
    @patch('httpx.AsyncClient')
    async def test_load_dataset_rate_limit(self, mock_client_class, connector):
        """Test rate limit error handling."""
        mock_client = AsyncMock()
        mock_response = MagicMock()
        mock_response.status_code = 429
        mock_error = httpx.HTTPStatusError("Rate limited", request=MagicMock(), response=mock_response)
        mock_client.get.side_effect = mock_error
        mock_client_class.return_value = mock_client
        
        connector._client = mock_client
        
        with pytest.raises(ConnectorRateLimitException):
            await connector._load_dataset("dft_3d")
    
    @patch('httpx.AsyncClient')
    async def test_load_dataset_not_found(self, mock_client_class, connector):
        """Test dataset not found error handling."""
        mock_client = AsyncMock()
        mock_response = MagicMock()
        mock_response.status_code = 404
        mock_error = httpx.HTTPStatusError("Not found", request=MagicMock(), response=mock_response)
        mock_client.get.side_effect = mock_error
        mock_client_class.return_value = mock_client
        
        connector._client = mock_client
        
        with pytest.raises(ConnectorNotFoundException):
            await connector._load_dataset("dft_3d")
    
    async def test_load_dataset_unknown_dataset(self, connector):
        """Test loading unknown dataset."""
        with pytest.raises(ConnectorException):
            await connector._load_dataset("unknown_dataset")
    
    def test_extract_material_data_complete(self, connector, mock_jarvis_data):
        """Test material data extraction with complete data."""
        material = mock_jarvis_data[0]
        
        extracted = connector._extract_material_data(material)
        
        assert extracted["jid"] == "JVASP-1001"
        assert extracted["formula"] == "Si2"
        assert extracted["formation_energy_peratom"] == -5.425
        assert extracted["ehull"] == 0.0
        assert extracted["source"] == "JARVIS-DFT"
        assert "retrieved_at" in extracted
        assert extracted["elastic_constants"]["bulk_modulus_kv"] == 97.8
        assert extracted["structure"]["num_atoms"] == 2
    
    def test_extract_material_data_with_properties(self, connector, mock_jarvis_data):
        """Test material data extraction with specific properties."""
        material = mock_jarvis_data[0]
        properties = ["bulk_modulus_kv", "elastic_tensor"]
        
        extracted = connector._extract_material_data(material, properties)
        
        assert "bulk_modulus_kv" in extracted
        assert "elastic_tensor" in extracted
        assert extracted["bulk_modulus_kv"] == 97.8
    
    def test_extract_elastic_constants(self, connector, mock_jarvis_data):
        """Test elastic constants extraction."""
        material = mock_jarvis_data[0]
        
        elastic = connector._extract_elastic_constants(material)
        
        assert elastic["bulk_modulus_kv"] == 97.8
        assert elastic["shear_modulus_gv"] == 51.5
        assert "elastic_tensor" in elastic
    
    def test_convert_structure(self, connector, mock_jarvis_data):
        """Test structure conversion."""
        atoms_data = mock_jarvis_data[0]["atoms"]
        
        structure = connector._convert_structure(atoms_data)
        
        assert structure["format"] == "jarvis"
        assert structure["num_atoms"] == 2
        assert structure["species"] == ["Si", "Si"]
        assert len(structure["coords"]) == 2
    
    def test_convert_structure_none(self, connector):
        """Test structure conversion with None input."""
        structure = connector._convert_structure(None)
        assert structure is None
    
    def test_matches_formula(self, connector):
        """Test formula matching functionality."""
        material = {"formula": "Si2O4"}
        
        assert connector._matches_formula(material, "Si")
        assert connector._matches_formula(material, "si")  # Case insensitive
        assert connector._matches_formula(material, "O4")
        assert not connector._matches_formula(material, "Al")
    
    async def test_get_available_datasets(self, connector):
        """Test getting available datasets."""
        datasets = await connector.get_available_datasets()
        
        assert isinstance(datasets, list)
        assert "dft_3d" in datasets
        assert "dft_2d" in datasets
        assert "ml_3d" in datasets
    
    @patch.object(JarvisConnector, '_load_dataset')
    async def test_get_dataset_info(self, mock_load, connector, mock_jarvis_data):
        """Test getting dataset information."""
        mock_load.return_value = mock_jarvis_data
        
        info = await connector.get_dataset_info("dft_3d")
        
        assert info["name"] == "dft_3d"
        assert info["total_materials"] == 2
        assert info["file"] == "dft_3d.json"
        assert "jarvis-materials-design.github.io" in info["url"]
        assert "last_loaded" in info
    
    async def test_get_dataset_info_unknown(self, connector):
        """Test getting info for unknown dataset."""
        with pytest.raises(ConnectorException):
            await connector.get_dataset_info("unknown_dataset")
    
    @patch.object(JarvisConnector, '_load_dataset')
    async def test_generic_interfaces(self, mock_load, connector, mock_jarvis_data):
        """Test generic connector interfaces."""
        mock_load.return_value = mock_jarvis_data
        
        # Test search interface
        results = await connector.search(formula="Si")
        assert len(results) == 1
        
        # Test get_by_id interface
        result = await connector.get_by_id("JVASP-1001")
        assert result["jid"] == "JVASP-1001"
        
        # Test fetch_bulk interface
        results = await connector.fetch_bulk(limit=1)
        assert len(results) == 1


@pytest.mark.asyncio
class TestRateLimitingIntegration:
    """Test rate limiting integration with JARVIS connector."""
    
    async def test_rate_limiting_respected(self):
        """Test that rate limiting is properly enforced."""
        connector = JarvisConnector(
            requests_per_second=1.0,  # Very low rate for testing
            burst_capacity=2
        )
        
        # Mock the HTTP client to avoid actual requests
        mock_client = AsyncMock()
        mock_response = MagicMock()
        mock_response.json.return_value = []
        mock_response.raise_for_status.return_value = None
        mock_client.get.return_value = mock_response
        connector._client = mock_client
        
        start_time = datetime.now()
        
        # Make 3 requests (exceeds burst capacity)
        for i in range(3):
            await connector._load_dataset("dft_3d")
        
        end_time = datetime.now()
        duration = (end_time - start_time).total_seconds()
        
        # Should take at least 1 second due to rate limiting
        assert duration >= 1.0
    
    async def test_rate_limiting_burst(self):
        """Test burst capacity allows immediate requests."""
        connector = JarvisConnector(
            requests_per_second=1.0,
            burst_capacity=5  # Allow 5 immediate requests
        )
        
        # Mock the HTTP client
        mock_client = AsyncMock()
        mock_response = MagicMock()
        mock_response.json.return_value = []
        mock_response.raise_for_status.return_value = None
        mock_client.get.return_value = mock_response
        connector._client = mock_client
        
        start_time = datetime.now()
        
        # Make 3 requests (within burst capacity)
        for i in range(3):
            await connector._load_dataset(f"dft_{i}")  # Different datasets to avoid cache
        
        end_time = datetime.now()
        duration = (end_time - start_time).total_seconds()
        
        # Should complete quickly due to burst capacity
        assert duration < 1.0


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
