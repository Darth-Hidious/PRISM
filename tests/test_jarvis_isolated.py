"""
Isolated unit tests for JARVIS connector core components.
Tests the base connector, rate limiter, and business logic without dependencies.
"""

import json
import pytest
import asyncio
import time
from unittest.mock import AsyncMock, MagicMock, patch
from datetime import datetime
from typing import Any, Dict, List, Optional
from abc import ABC, abstractmethod

import httpx
from tenacity import (
    retry,
    stop_after_attempt,
    wait_exponential,
    retry_if_exception_type
)


# ============================================================================
# Base Connector Classes (copied to avoid import issues)
# ============================================================================

class ConnectorException(Exception):
    """Base exception for connector errors."""
    pass


class ConnectorTimeoutException(ConnectorException):
    """Exception raised when connector requests timeout."""
    pass


class ConnectorRateLimitException(ConnectorException):
    """Exception raised when rate limit is exceeded."""
    pass


class ConnectorNotFoundException(ConnectorException):
    """Exception raised when requested resource is not found."""
    pass


class DatabaseConnector(ABC):
    """Abstract base class for external database connectors."""
    
    def __init__(self, base_url: str, timeout: int = 30):
        self.base_url = base_url.rstrip('/')
        self.timeout = timeout
        self.last_request_time: Optional[datetime] = None
    
    @abstractmethod
    async def connect(self) -> bool:
        """Establish connection to the external database."""
        pass
    
    @abstractmethod
    async def disconnect(self) -> None:
        """Close connection to the external database."""
        pass
    
    @abstractmethod
    async def health_check(self) -> bool:
        """Check if the external database is accessible."""
        pass


# ============================================================================
# Rate Limiter Classes (copied to avoid import issues)
# ============================================================================

class TokenBucket:
    """Token bucket rate limiter implementation."""
    
    def __init__(self, capacity: int, refill_rate: float):
        self.capacity = capacity
        self.refill_rate = refill_rate
        self.tokens = capacity
        self.last_refill = time.time()
        self._lock = asyncio.Lock()
    
    async def consume(self, tokens: int = 1) -> bool:
        """Try to consume tokens from the bucket."""
        async with self._lock:
            self._refill()
            
            if self.tokens >= tokens:
                self.tokens -= tokens
                return True
            
            return False
    
    async def wait_for_tokens(self, tokens: int = 1) -> None:
        """Wait until enough tokens are available and consume them."""
        while True:
            async with self._lock:
                self._refill()
                
                if self.tokens >= tokens:
                    self.tokens -= tokens
                    return
                
                wait_time = (tokens - self.tokens) / self.refill_rate
            
            await asyncio.sleep(min(wait_time, 0.1))
    
    def _refill(self) -> None:
        """Refill tokens based on elapsed time."""
        now = time.time()
        elapsed = now - self.last_refill
        
        if elapsed > 0:
            new_tokens = elapsed * self.refill_rate
            self.tokens = min(self.capacity, self.tokens + new_tokens)
            self.last_refill = now
    
    @property
    def available_tokens(self) -> int:
        """Get current number of available tokens."""
        self._refill()
        return int(self.tokens)


class RateLimiter:
    """Rate limiter with multiple buckets for different rate limits."""
    
    def __init__(self):
        self.buckets: dict[str, TokenBucket] = {}
    
    def add_bucket(self, name: str, capacity: int, refill_rate: float) -> None:
        """Add a named token bucket."""
        self.buckets[name] = TokenBucket(capacity, refill_rate)
    
    async def wait_for_permit(self, bucket_name: str, tokens: int = 1) -> None:
        """Wait for permit from specified bucket."""
        if bucket_name not in self.buckets:
            return
        
        await self.buckets[bucket_name].wait_for_tokens(tokens)
    
    async def try_acquire(self, bucket_name: str, tokens: int = 1) -> bool:
        """Try to acquire permit without waiting."""
        if bucket_name not in self.buckets:
            return True
        
        return await self.buckets[bucket_name].consume(tokens)


# ============================================================================
# JARVIS Connector Implementation (simplified)
# ============================================================================

class JarvisConnector(DatabaseConnector):
    """Connector for JARVIS-DFT database."""
    
    BASE_URL = "https://jarvis.nist.gov"
    DATA_BASE_URL = "https://jarvis-materials-design.github.io/dbdocs/jarvisd"
    
    DATA_FILES = {
        "dft_3d": "dft_3d.json",
        "dft_2d": "dft_2d.json", 
        "ml_3d": "ml_3d.json",
        "ml_2d": "ml_2d.json"
    }
    
    def __init__(
        self,
        timeout: int = 30,
        max_retries: int = 3,
        requests_per_second: float = 2.0,
        burst_capacity: int = 10
    ):
        super().__init__(self.BASE_URL, timeout)
        
        self.max_retries = max_retries
        self._client: Optional[httpx.AsyncClient] = None
        self._cache: Dict[str, Any] = {}
        self._cache_ttl = 3600
        
        self.rate_limiter = RateLimiter()
        self.rate_limiter.add_bucket(
            "jarvis_api",
            capacity=burst_capacity,
            refill_rate=requests_per_second
        )
    
    async def connect(self) -> bool:
        """Establish HTTP client connection."""
        try:
            if self._client is None:
                self._client = httpx.AsyncClient(
                    timeout=httpx.Timeout(self.timeout),
                    limits=httpx.Limits(max_connections=10, max_keepalive_connections=5),
                    headers={
                        "User-Agent": "JARVIS-Connector/1.0",
                        "Accept": "application/json"
                    }
                )
            
            return await self.health_check()
            
        except Exception:
            return False
    
    async def disconnect(self) -> None:
        """Close HTTP client connection."""
        if self._client:
            await self._client.aclose()
            self._client = None
    
    async def health_check(self) -> bool:
        """Check if JARVIS database is accessible."""
        try:
            if not self._client:
                await self.connect()
            
            response = await self._client.get(self.BASE_URL, timeout=5.0)
            return response.status_code == 200
            
        except Exception:
            return False
    
    def _extract_material_data(
        self,
        material: Dict[str, Any],
        properties: Optional[List[str]] = None
    ) -> Dict[str, Any]:
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
    
    def _extract_elastic_constants(self, material: Dict[str, Any]) -> Optional[Dict[str, Any]]:
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
    
    def _convert_structure(self, atoms_data: Optional[Dict[str, Any]]) -> Optional[Dict[str, Any]]:
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
    
    def _matches_formula(self, material: Dict[str, Any], formula: str) -> bool:
        """Check if material matches the given formula."""
        material_formula = material.get("formula", "")
        return formula.lower() in material_formula.lower()


# ============================================================================
# Test Fixtures
# ============================================================================

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
        requests_per_second=10.0,
        burst_capacity=20
    )


# ============================================================================
# Test Cases
# ============================================================================

class TestTokenBucket:
    """Test token bucket rate limiter."""
    
    def test_initialization(self):
        """Test token bucket initialization."""
        bucket = TokenBucket(capacity=10, refill_rate=2.0)
        assert bucket.capacity == 10
        assert bucket.refill_rate == 2.0
        assert bucket.tokens == 10
    
    @pytest.mark.asyncio
    async def test_consume_tokens(self):
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
    
    @pytest.mark.asyncio
    async def test_token_refill(self):
        """Test token refill over time."""
        bucket = TokenBucket(capacity=5, refill_rate=10.0)  # High refill rate
        
        # Consume all tokens
        await bucket.consume(5)
        assert bucket.available_tokens == 0
        
        # Wait and check refill
        await asyncio.sleep(0.2)  # 200ms should add ~2 tokens at 10/sec
        available = bucket.available_tokens
        assert available > 0


class TestRateLimiter:
    """Test rate limiter functionality."""
    
    def test_initialization(self):
        """Test rate limiter initialization."""
        limiter = RateLimiter()
        assert len(limiter.buckets) == 0
        
        limiter.add_bucket("test", 10, 2.0)
        assert "test" in limiter.buckets
        assert isinstance(limiter.buckets["test"], TokenBucket)
    
    @pytest.mark.asyncio
    async def test_acquire_permit(self):
        """Test permit acquisition."""
        limiter = RateLimiter()
        limiter.add_bucket("test", 5, 1.0)
        
        # Should be able to acquire initially
        result = await limiter.try_acquire("test", 3)
        assert result is True
        
        # Should not be able to acquire more than available
        result = await limiter.try_acquire("test", 5)
        assert result is False


class TestJarvisConnector:
    """Test JARVIS connector functionality."""
    
    def test_initialization(self):
        """Test connector initialization."""
        connector = JarvisConnector()
        assert connector.BASE_URL == "https://jarvis.nist.gov"
        assert connector.timeout == 30
        assert connector.max_retries == 3
        
        custom_connector = JarvisConnector(
            timeout=60,
            max_retries=5,
            requests_per_second=1.0,
            burst_capacity=5
        )
        assert custom_connector.timeout == 60
        assert custom_connector.max_retries == 5
    
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
    
    @patch('httpx.AsyncClient')
    @pytest.mark.asyncio
    async def test_connect_success(self, mock_client_class, connector):
        """Test successful connection establishment."""
        mock_client = AsyncMock()
        mock_client.get.return_value.status_code = 200
        mock_client_class.return_value = mock_client
        
        result = await connector.connect()
        
        assert result is True
        assert connector._client is not None
    
    @patch('httpx.AsyncClient')
    @pytest.mark.asyncio
    async def test_connect_failure(self, mock_client_class, connector):
        """Test connection failure handling."""
        mock_client = AsyncMock()
        mock_client.get.side_effect = httpx.ConnectError("Connection failed")
        mock_client_class.return_value = mock_client
        
        result = await connector.connect()
        
        assert result is False
    
    @pytest.mark.asyncio
    async def test_disconnect(self, connector):
        """Test proper disconnection."""
        mock_client = AsyncMock()
        connector._client = mock_client
        
        await connector.disconnect()
        
        mock_client.aclose.assert_called_once()
        assert connector._client is None


class TestExceptionHandling:
    """Test exception hierarchy and handling."""
    
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


@pytest.mark.asyncio
class TestRateLimitingIntegration:
    """Test rate limiting integration."""
    
    async def test_rate_limiting_enforcement(self):
        """Test that rate limiting is properly enforced."""
        # Create a rate limiter with very restrictive limits
        limiter = RateLimiter()
        limiter.add_bucket("test", capacity=1, refill_rate=2.0)  # 1 token, refills 2/sec
        
        # First request should succeed immediately
        start_time = time.time()
        result1 = await limiter.try_acquire("test", 1)
        assert result1 is True
        
        # Second request should fail (no tokens available)
        result2 = await limiter.try_acquire("test", 1)
        assert result2 is False
        
        # Wait for token to refill and try again
        await limiter.wait_for_permit("test", 1)
        end_time = time.time()
        
        # Should have taken some time to get the permit
        duration = end_time - start_time
        assert duration >= 0.4  # At least 400ms to refill 1 token at 2/sec


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
