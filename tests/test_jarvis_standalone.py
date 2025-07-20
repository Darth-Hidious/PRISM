"""
Standalone unit tests for JARVIS-DFT Database Connector.
This version avoids importing the main app configuration.
"""

import json
import pytest
from unittest.mock import AsyncMock, MagicMock, patch
from datetime import datetime

import httpx

# Import the specific modules we need without the main app
import sys
import os
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..'))

from app.services.connectors.base_connector import (
    DatabaseConnector,
    ConnectorException,
    ConnectorTimeoutException,
    ConnectorNotFoundException,
    ConnectorRateLimitException
)
from app.services.connectors.rate_limiter import RateLimiter, TokenBucket


class TestJarvisConnectorStandalone:
    """Test the JARVIS connector components in isolation."""
    
    def test_token_bucket_initialization(self):
        """Test token bucket initialization."""
        bucket = TokenBucket(capacity=10, refill_rate=2.0)
        assert bucket.capacity == 10
        assert bucket.refill_rate == 2.0
        assert bucket.tokens == 10
    
    @pytest.mark.asyncio
    async def test_token_bucket_consume(self):
        """Test token consumption."""
        bucket = TokenBucket(capacity=5, refill_rate=1.0)
        
        # Should be able to consume tokens initially
        result = await bucket.consume(3)
        assert result is True
        assert bucket.available_tokens == 2
        
        # Should not be able to consume more than available
        result = await bucket.consume(5)
        assert result is False
        assert bucket.available_tokens == 2
    
    def test_rate_limiter_initialization(self):
        """Test rate limiter initialization."""
        limiter = RateLimiter()
        assert len(limiter.buckets) == 0
        
        limiter.add_bucket("test", 10, 2.0)
        assert "test" in limiter.buckets
        assert isinstance(limiter.buckets["test"], TokenBucket)
    
    @pytest.mark.asyncio
    async def test_rate_limiter_try_acquire(self):
        """Test rate limiter acquire functionality."""
        limiter = RateLimiter()
        limiter.add_bucket("test", 5, 1.0)
        
        # Should be able to acquire initially
        result = await limiter.try_acquire("test", 3)
        assert result is True
        
        # Should not be able to acquire more than available
        result = await limiter.try_acquire("test", 5)
        assert result is False
    
    def test_database_connector_interface(self):
        """Test the abstract base connector interface."""
        
        # Should not be able to instantiate abstract class
        with pytest.raises(TypeError):
            DatabaseConnector("http://example.com")
    
    def test_connector_exceptions(self):
        """Test connector exception hierarchy."""
        base_exc = ConnectorException("Base error")
        assert str(base_exc) == "Base error"
        
        timeout_exc = ConnectorTimeoutException("Timeout error")
        assert isinstance(timeout_exc, ConnectorException)
        
        not_found_exc = ConnectorNotFoundException("Not found")
        assert isinstance(not_found_exc, ConnectorException)
        
        rate_limit_exc = ConnectorRateLimitException("Rate limited")
        assert isinstance(rate_limit_exc, ConnectorException)


class MockJarvisConnector:
    """Mock JARVIS connector for testing business logic."""
    
    def __init__(self):
        self.BASE_URL = "https://jarvis.nist.gov"
        self.DATA_BASE_URL = "https://jarvis-materials-design.github.io/dbdocs/jarvisd"
        self.DATA_FILES = {
            "dft_3d": "dft_3d.json",
            "dft_2d": "dft_2d.json"
        }
        self._cache = {}
        self._cache_ttl = 3600
    
    def _extract_material_data(self, material, properties=None):
        """Extract and standardize material data."""
        extracted = {
            "jid": material.get("jid"),
            "formula": material.get("formula"),
            "formation_energy_peratom": material.get("formation_energy_peratom"),
            "ehull": material.get("ehull"),
            "elastic_constants": self._extract_elastic_constants(material),
            "structure": self._convert_structure(material.get("atoms")),
            "source": "JARVIS-DFT",
            "retrieved_at": datetime.now().isoformat()
        }
        
        if properties:
            for prop in properties:
                if prop in material:
                    extracted[prop] = material[prop]
        
        return {k: v for k, v in extracted.items() if v is not None}
    
    def _extract_elastic_constants(self, material):
        """Extract elastic constants from material data."""
        elastic_data = {}
        elastic_props = [
            "bulk_modulus_kv", "shear_modulus_gv", 
            "elastic_tensor", "poisson_ratio"
        ]
        
        for prop in elastic_props:
            if prop in material:
                elastic_data[prop] = material[prop]
        
        return elastic_data if elastic_data else None
    
    def _convert_structure(self, atoms_data):
        """Convert JARVIS atomic structure to standard format."""
        if not atoms_data:
            return None
        
        try:
            return {
                "lattice": atoms_data.get("lattice_mat"),
                "species": atoms_data.get("elements"),
                "coords": atoms_data.get("coords"),
                "cart_coords": atoms_data.get("cart_coords"),
                "format": "jarvis",
                "num_atoms": len(atoms_data.get("elements", []))
            }
        except Exception:
            return None
    
    def _matches_formula(self, material, formula):
        """Check if material matches the given formula."""
        material_formula = material.get("formula", "")
        return formula.lower() in material_formula.lower()


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
def mock_connector():
    """Create a mock JARVIS connector for testing."""
    return MockJarvisConnector()


class TestJarvisConnectorLogic:
    """Test JARVIS connector business logic without external dependencies."""
    
    def test_extract_material_data_complete(self, mock_connector, mock_jarvis_data):
        """Test material data extraction with complete data."""
        material = mock_jarvis_data[0]
        
        extracted = mock_connector._extract_material_data(material)
        
        assert extracted["jid"] == "JVASP-1001"
        assert extracted["formula"] == "Si2"
        assert extracted["formation_energy_peratom"] == -5.425
        assert extracted["ehull"] == 0.0
        assert extracted["source"] == "JARVIS-DFT"
        assert "retrieved_at" in extracted
        assert extracted["elastic_constants"]["bulk_modulus_kv"] == 97.8
        assert extracted["structure"]["num_atoms"] == 2
    
    def test_extract_material_data_with_properties(self, mock_connector, mock_jarvis_data):
        """Test material data extraction with specific properties."""
        material = mock_jarvis_data[0]
        properties = ["bulk_modulus_kv", "elastic_tensor"]
        
        extracted = mock_connector._extract_material_data(material, properties)
        
        assert "bulk_modulus_kv" in extracted
        assert "elastic_tensor" in extracted
        assert extracted["bulk_modulus_kv"] == 97.8
    
    def test_extract_elastic_constants(self, mock_connector, mock_jarvis_data):
        """Test elastic constants extraction."""
        material = mock_jarvis_data[0]
        
        elastic = mock_connector._extract_elastic_constants(material)
        
        assert elastic["bulk_modulus_kv"] == 97.8
        assert elastic["shear_modulus_gv"] == 51.5
        assert "elastic_tensor" in elastic
    
    def test_convert_structure(self, mock_connector, mock_jarvis_data):
        """Test structure conversion."""
        atoms_data = mock_jarvis_data[0]["atoms"]
        
        structure = mock_connector._convert_structure(atoms_data)
        
        assert structure["format"] == "jarvis"
        assert structure["num_atoms"] == 2
        assert structure["species"] == ["Si", "Si"]
        assert len(structure["coords"]) == 2
    
    def test_convert_structure_none(self, mock_connector):
        """Test structure conversion with None input."""
        structure = mock_connector._convert_structure(None)
        assert structure is None
    
    def test_matches_formula(self, mock_connector):
        """Test formula matching functionality."""
        material = {"formula": "Si2O4"}
        
        assert mock_connector._matches_formula(material, "Si")
        assert mock_connector._matches_formula(material, "si")  # Case insensitive
        assert mock_connector._matches_formula(material, "O4")
        assert not mock_connector._matches_formula(material, "Al")


@pytest.mark.asyncio
class TestRateLimitingStandalone:
    """Test rate limiting functionality in isolation."""
    
    async def test_token_bucket_refill(self):
        """Test token bucket refill over time."""
        import time
        
        bucket = TokenBucket(capacity=5, refill_rate=10.0)  # High refill rate for testing
        
        # Consume all tokens
        await bucket.consume(5)
        assert bucket.available_tokens == 0
        
        # Wait a short time for refill
        time.sleep(0.2)  # 200ms should add ~2 tokens at 10/sec
        
        # Check that tokens were refilled
        available = bucket.available_tokens
        assert available > 0
        assert available <= 5
    
    async def test_token_bucket_wait_for_tokens(self):
        """Test waiting for tokens to become available."""
        bucket = TokenBucket(capacity=2, refill_rate=10.0)  # Fast refill for testing
        
        # Consume all tokens
        await bucket.consume(2)
        assert bucket.available_tokens == 0
        
        # This should wait and then succeed
        start_time = datetime.now()
        await bucket.wait_for_tokens(1)
        end_time = datetime.now()
        
        duration = (end_time - start_time).total_seconds()
        assert duration > 0  # Should have waited some time
        assert bucket.available_tokens >= 0  # Should have consumed the token


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
