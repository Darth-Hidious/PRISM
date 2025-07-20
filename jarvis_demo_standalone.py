#!/usr/bin/env python3
"""
Standalone JARVIS-DFT Database Connector Demo.

This script demonstrates the JARVIS connector functionality without 
depending on the main application configuration.
"""

import asyncio
import json
import logging
import time
from typing import List, Dict, Any, Optional
from datetime import datetime
from abc import ABC, abstractmethod

import httpx
from tenacity import (
    retry,
    stop_after_attempt,
    wait_exponential,
    retry_if_exception_type
)


# Set up logging
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)


# ============================================================================
# Standalone Implementation (copied from the main files)
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


class TokenBucket:
    """Token bucket rate limiter implementation."""
    
    def __init__(self, capacity: int, refill_rate: float):
        self.capacity = capacity
        self.refill_rate = refill_rate
        self.tokens = capacity
        self.last_refill = time.time()
        self._lock = asyncio.Lock()
    
    async def consume(self, tokens: int = 1) -> bool:
        async with self._lock:
            self._refill()
            
            if self.tokens >= tokens:
                self.tokens -= tokens
                return True
            
            return False
    
    async def wait_for_tokens(self, tokens: int = 1) -> None:
        while True:
            async with self._lock:
                self._refill()
                
                if self.tokens >= tokens:
                    self.tokens -= tokens
                    return
                
                wait_time = (tokens - self.tokens) / self.refill_rate
            
            await asyncio.sleep(min(wait_time, 0.1))
    
    def _refill(self) -> None:
        now = time.time()
        elapsed = now - self.last_refill
        
        if elapsed > 0:
            new_tokens = elapsed * self.refill_rate
            self.tokens = min(self.capacity, self.tokens + new_tokens)
            self.last_refill = now
    
    @property
    def available_tokens(self) -> int:
        self._refill()
        return int(self.tokens)


class RateLimiter:
    """Rate limiter with multiple buckets for different rate limits."""
    
    def __init__(self):
        self.buckets: dict[str, TokenBucket] = {}
    
    def add_bucket(self, name: str, capacity: int, refill_rate: float) -> None:
        self.buckets[name] = TokenBucket(capacity, refill_rate)
    
    async def wait_for_permit(self, bucket_name: str, tokens: int = 1) -> None:
        if bucket_name not in self.buckets:
            return
        
        await self.buckets[bucket_name].wait_for_tokens(tokens)


class JarvisConnector:
    """Connector for JARVIS-DFT database."""
    
    BASE_URL = "https://jarvis.nist.gov"
    DATA_BASE_URL = "https://jarvis-materials-design.github.io/dbdocs/jarvisd"
    
    DATA_FILES = {
        "dft_3d": "dft_3d.json",
        "dft_2d": "dft_2d.json", 
        "ml_3d": "ml_3d.json",
        "ml_2d": "ml_2d.json",
        "cfid_3d": "cfid_3d.json",
        "cfid_2d": "cfid_2d.json",
        "qmof": "qmof.json",
        "hmof": "hmof.json"
    }
    
    def __init__(
        self,
        timeout: int = 30,
        max_retries: int = 3,
        requests_per_second: float = 2.0,
        burst_capacity: int = 10
    ):
        self.base_url = self.BASE_URL.rstrip('/')
        self.timeout = timeout
        self.max_retries = max_retries
        self._client: Optional[httpx.AsyncClient] = None
        self._cache: Dict[str, Any] = {}
        self._cache_ttl = 3600
        
        # Set up rate limiting
        self.rate_limiter = RateLimiter()
        self.rate_limiter.add_bucket(
            "jarvis_api",
            capacity=burst_capacity,
            refill_rate=requests_per_second
        )
        
        logger.info(f"JARVIS connector initialized with {requests_per_second} RPS limit")
    
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
            
        except Exception as e:
            logger.error(f"Failed to connect to JARVIS: {e}")
            return False
    
    async def disconnect(self) -> None:
        """Close HTTP client connection."""
        if self._client:
            await self._client.aclose()
            self._client = None
            logger.info("JARVIS connector disconnected")
    
    async def health_check(self) -> bool:
        """Check if JARVIS database is accessible."""
        try:
            if not self._client:
                await self.connect()
            
            response = await self._client.get(self.BASE_URL, timeout=5.0)
            return response.status_code == 200
            
        except Exception as e:
            logger.warning(f"JARVIS health check failed: {e}")
            return False
    
    async def search_materials(
        self,
        formula: Optional[str] = None,
        n_elements: Optional[int] = None,
        properties: Optional[List[str]] = None,
        dataset: str = "dft_3d",
        limit: int = 100
    ) -> List[Dict[str, Any]]:
        """Search for materials based on criteria."""
        try:
            materials = await self._load_dataset(dataset)
            
            results = []
            for material in materials:
                if len(results) >= limit:
                    break
                
                # Apply filters
                if formula and not self._matches_formula(material, formula):
                    continue
                
                if n_elements and material.get("nelements") != n_elements:
                    continue
                
                # Extract requested properties
                extracted = self._extract_material_data(material, properties)
                results.append(extracted)
            
            logger.info(f"Found {len(results)} materials matching search criteria")
            return results
            
        except Exception as e:
            logger.error(f"Error searching materials: {e}")
            raise ConnectorException(f"Search failed: {e}")
    
    async def get_material_by_id(self, jarvis_id: str, dataset: str = "dft_3d") -> Dict[str, Any]:
        """Get a specific material by its JARVIS ID."""
        try:
            materials = await self._load_dataset(dataset)
            
            for material in materials:
                if material.get("jid") == jarvis_id:
                    return self._extract_material_data(material)
            
            raise ConnectorNotFoundException(f"Material with ID {jarvis_id} not found")
            
        except ConnectorNotFoundException:
            raise
        except Exception as e:
            logger.error(f"Error getting material {jarvis_id}: {e}")
            raise ConnectorException(f"Failed to get material: {e}")
    
    async def fetch_bulk_materials(
        self,
        limit: int = 100,
        offset: int = 0,
        dataset: str = "dft_3d"
    ) -> List[Dict[str, Any]]:
        """Fetch materials in bulk with pagination."""
        try:
            materials = await self._load_dataset(dataset)
            
            # Apply pagination
            start_idx = offset
            end_idx = offset + limit
            paginated = materials[start_idx:end_idx]
            
            # Extract and format data
            results = [
                self._extract_material_data(material)
                for material in paginated
            ]
            
            logger.info(f"Fetched {len(results)} materials (offset: {offset}, limit: {limit})")
            return results
            
        except Exception as e:
            logger.error(f"Error fetching bulk materials: {e}")
            raise ConnectorException(f"Bulk fetch failed: {e}")
    
    @retry(
        stop=stop_after_attempt(3),
        wait=wait_exponential(multiplier=1, min=4, max=10),
        retry=retry_if_exception_type((httpx.TimeoutException, httpx.ConnectError))
    )
    async def _load_dataset(self, dataset: str) -> List[Dict[str, Any]]:
        """Load JARVIS dataset with caching and rate limiting."""
        # Check cache first
        cache_key = f"dataset:{dataset}"
        if cache_key in self._cache:
            cache_entry = self._cache[cache_key]
            if time.time() - cache_entry["timestamp"] < self._cache_ttl:
                logger.debug(f"Using cached dataset: {dataset}")
                return cache_entry["data"]
        
        # Rate limiting
        await self.rate_limiter.wait_for_permit("jarvis_api")
        
        if not self._client:
            await self.connect()
        
        # Get dataset file
        if dataset not in self.DATA_FILES:
            raise ConnectorException(f"Unknown dataset: {dataset}")
        
        filename = self.DATA_FILES[dataset]
        url = f"{self.DATA_BASE_URL}/{filename}"
        
        try:
            logger.info(f"Loading JARVIS dataset: {dataset} from {url}")
            response = await self._client.get(url)
            response.raise_for_status()
            
            data = response.json()
            
            # Cache the data
            self._cache[cache_key] = {
                "data": data,
                "timestamp": time.time()
            }
            
            logger.info(f"Loaded {len(data)} materials from {dataset}")
            return data
            
        except httpx.TimeoutException as e:
            logger.error(f"Timeout loading dataset {dataset}: {e}")
            raise ConnectorTimeoutException(f"Request timeout: {e}")
        
        except httpx.HTTPStatusError as e:
            if e.response.status_code == 429:
                raise ConnectorRateLimitException("Rate limit exceeded")
            elif e.response.status_code == 404:
                raise ConnectorNotFoundException(f"Dataset {dataset} not found")
            else:
                raise ConnectorException(f"HTTP error: {e}")
        
        except json.JSONDecodeError as e:
            logger.error(f"Invalid JSON in dataset {dataset}: {e}")
            raise ConnectorException(f"Invalid JSON response: {e}")
    
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
        
        # Add specific properties if requested
        if properties:
            for prop in properties:
                if prop in material:
                    extracted[prop] = material[prop]
        
        # Remove None values
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
        except Exception as e:
            logger.warning(f"Error converting structure: {e}")
            return None
    
    def _matches_formula(self, material: Dict[str, Any], formula: str) -> bool:
        """Check if material matches the given formula."""
        material_formula = material.get("formula", "")
        return formula.lower() in material_formula.lower()
    
    async def get_available_datasets(self) -> List[str]:
        """Get list of available JARVIS datasets."""
        return list(self.DATA_FILES.keys())
    
    async def get_dataset_info(self, dataset: str) -> Dict[str, Any]:
        """Get information about a specific dataset."""
        if dataset not in self.DATA_FILES:
            raise ConnectorException(f"Unknown dataset: {dataset}")
        
        try:
            materials = await self._load_dataset(dataset)
            
            return {
                "name": dataset,
                "total_materials": len(materials),
                "file": self.DATA_FILES[dataset],
                "url": f"{self.DATA_BASE_URL}/{self.DATA_FILES[dataset]}",
                "last_loaded": datetime.now().isoformat()
            }
            
        except Exception as e:
            logger.error(f"Error getting dataset info: {e}")
            raise ConnectorException(f"Failed to get dataset info: {e}")


# ============================================================================
# Demo Functions
# ============================================================================

async def demonstrate_jarvis_connector():
    """Demonstrate JARVIS connector functionality."""
    
    # Create connector with respectful rate limiting
    connector = JarvisConnector(
        timeout=30,
        requests_per_second=1.0,  # Be respectful to JARVIS servers
        burst_capacity=3
    )
    
    try:
        # Connect to JARVIS
        logger.info("Connecting to JARVIS database...")
        connected = await connector.connect()
        
        if not connected:
            logger.error("Failed to connect to JARVIS database")
            return
        
        logger.info("Successfully connected to JARVIS database")
        
        # Example 1: Get dataset information
        logger.info("\n=== Example 1: Dataset Information ===")
        datasets = await connector.get_available_datasets()
        logger.info(f"Available datasets: {', '.join(datasets)}")
        
        # Get info for DFT 3D dataset (this will load the data)
        try:
            dft_3d_info = await connector.get_dataset_info("dft_3d")
            logger.info(f"DFT 3D dataset info:")
            logger.info(f"  Total materials: {dft_3d_info['total_materials']}")
            logger.info(f"  Source file: {dft_3d_info['file']}")
        except Exception as e:
            logger.error(f"Failed to load DFT 3D dataset: {e}")
            logger.info("This might be due to the dataset being very large or network issues")
            
            # Try a smaller dataset
            logger.info("Trying 2D dataset instead...")
            try:
                dft_2d_info = await connector.get_dataset_info("dft_2d")
                logger.info(f"DFT 2D dataset info:")
                logger.info(f"  Total materials: {dft_2d_info['total_materials']}")
                logger.info(f"  Source file: {dft_2d_info['file']}")
                
                # Example 2: Search for materials in 2D dataset
                logger.info("\n=== Example 2: Search for Materials ===")
                materials = await connector.search_materials(
                    dataset="dft_2d",
                    limit=3,
                    properties=["bulk_modulus_kv", "formation_energy_peratom"]
                )
                
                logger.info(f"Found {len(materials)} materials:")
                for i, material in enumerate(materials, 1):
                    logger.info(f"  {i}. {material['jid']}: {material['formula']}")
                    if 'formation_energy_peratom' in material:
                        logger.info(f"     Formation energy: {material['formation_energy_peratom']} eV/atom")
                
                # Example 3: Get specific material by ID
                if materials:
                    logger.info("\n=== Example 3: Get Material by ID ===")
                    first_material_id = materials[0]['jid']
                    specific_material = await connector.get_material_by_id(
                        first_material_id, 
                        dataset="dft_2d"
                    )
                    
                    logger.info(f"Material details for {first_material_id}:")
                    logger.info(f"  Formula: {specific_material['formula']}")
                    if 'formation_energy_peratom' in specific_material:
                        logger.info(f"  Formation energy: {specific_material['formation_energy_peratom']} eV/atom")
                    if specific_material.get('structure'):
                        logger.info(f"  Structure atoms: {specific_material['structure']['num_atoms']}")
                
            except Exception as e:
                logger.error(f"Failed to load 2D dataset: {e}")
                logger.info("Demonstrating with mock data instead...")
                await demonstrate_with_mock_data(connector)
    
    except Exception as e:
        logger.error(f"Error during demonstration: {e}")
        await demonstrate_with_mock_data(connector)
    
    finally:
        # Clean up connection
        await connector.disconnect()
        logger.info("Disconnected from JARVIS database")


async def demonstrate_with_mock_data(connector):
    """Demonstrate functionality with mock data when API is unavailable."""
    logger.info("\n=== Mock Data Demonstration ===")
    
    # Create mock material data
    mock_materials = [
        {
            "jid": "JVASP-1001",
            "formula": "Si2",
            "formation_energy_peratom": -5.425,
            "ehull": 0.0,
            "bulk_modulus_kv": 97.8,
            "atoms": {
                "elements": ["Si", "Si"],
                "coords": [[0.0, 0.0, 0.0], [0.25, 0.25, 0.25]]
            }
        },
        {
            "jid": "JVASP-1002",
            "formula": "GaN",
            "formation_energy_peratom": -1.23,
            "ehull": 0.01,
            "bulk_modulus_kv": 207.0,
            "atoms": {
                "elements": ["Ga", "N"],
                "coords": [[0.0, 0.0, 0.0], [0.33, 0.33, 0.5]]
            }
        }
    ]
    
    # Test data extraction
    for material in mock_materials:
        extracted = connector._extract_material_data(material)
        logger.info(f"Extracted material: {extracted['jid']} - {extracted['formula']}")
        logger.info(f"  Formation energy: {extracted.get('formation_energy_peratom')} eV/atom")
        if extracted.get('structure'):
            logger.info(f"  Atoms: {extracted['structure']['num_atoms']}")
    
    # Test formula matching
    logger.info("\nTesting formula matching:")
    test_material = {"formula": "Si2O4"}
    logger.info(f"Material: {test_material['formula']}")
    logger.info(f"Matches 'Si': {connector._matches_formula(test_material, 'Si')}")
    logger.info(f"Matches 'O': {connector._matches_formula(test_material, 'O')}")
    logger.info(f"Matches 'Al': {connector._matches_formula(test_material, 'Al')}")


async def test_rate_limiting():
    """Test rate limiting functionality."""
    logger.info("\n=== Rate Limiting Test ===")
    
    connector = JarvisConnector(
        requests_per_second=2.0,  # 2 requests per second
        burst_capacity=3
    )
    
    start_time = time.time()
    
    # Test that rate limiting works
    bucket = connector.rate_limiter.buckets["jarvis_api"]
    
    logger.info("Testing token bucket rate limiting...")
    logger.info(f"Initial tokens: {bucket.available_tokens}")
    
    # Consume all burst capacity
    for i in range(3):
        success = await bucket.consume(1)
        logger.info(f"Request {i+1}: {success}, remaining tokens: {bucket.available_tokens}")
    
    # Next request should wait
    logger.info("Next request should wait for token refill...")
    await bucket.wait_for_tokens(1)
    
    end_time = time.time()
    duration = end_time - start_time
    
    logger.info(f"Total time for rate-limited requests: {duration:.2f} seconds")
    logger.info(f"Rate limiting working properly: {duration >= 0.5}")  # Should take at least 0.5s
    
    await connector.disconnect()


async def main():
    """Main demonstration function."""
    logger.info("Starting JARVIS-DFT Connector Demonstration")
    logger.info("=" * 50)
    
    try:
        # Run main demonstration
        await demonstrate_jarvis_connector()
        
        # Test rate limiting
        await test_rate_limiting()
        
        logger.info("\n" + "=" * 50)
        logger.info("JARVIS connector demonstration completed successfully!")
        
    except Exception as e:
        logger.error(f"Demonstration failed: {e}")
        raise


if __name__ == "__main__":
    # Run the demonstration
    asyncio.run(main())
