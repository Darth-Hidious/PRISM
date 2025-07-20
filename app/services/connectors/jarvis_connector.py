"""
JARVIS-DFT Database Connector for materials data.

This connector interfaces with the JARVIS (Joint Automated Repository for 
Various Integrated Simulations) database to retrieve materials science data.
"""

import asyncio
import json
import logging
from typing import Any, Dict, List, Optional, Union
from datetime import datetime

import httpx
from tenacity import (
    retry,
    stop_after_attempt,
    wait_exponential,
    retry_if_exception_type
)

from .base_connector import (
    DatabaseConnector,
    ConnectorException,
    ConnectorTimeoutException,
    ConnectorRateLimitException,
    ConnectorNotFoundException
)
from .rate_limiter import RateLimiter


logger = logging.getLogger(__name__)


class JarvisConnector(DatabaseConnector):
    def standardize_data(self, data: Any) -> Any:
        pass

    def validate_response(self, response: Any) -> bool:
        pass

    """
    Connector for JARVIS-DFT database.
    
    Provides access to materials data including formation energies,
    elastic constants, electronic properties, and crystal structures.
    """
    
    # JARVIS API endpoints
    BASE_URL = "https://jarvis.nist.gov"
    DATA_BASE_URL = "https://raw.githubusercontent.com/usnistgov/jarvis-materials-design/main/dbdocs/jarvisd"
    
    # Common data files available in JARVIS
    DATA_FILES = {
        "jarvis_dft_3d": "dft_3d.json",
        "jarvis_dft_2d": "dft_2d.json",
        "jarvis_ml_3d": "ml_3d.json",
        "jarvis_ml_2d": "ml_2d.json",
        "jarvis_cfid_3d": "cfid_3d.json",
        "jarvis_cfid_2d": "cfid_2d.json",
        "jarvis_qmof": "qmof.json",
        "jarvis_hmof": "hmof.json"
    }
    
    def __init__(
        self,
        timeout: int = 30,
        max_retries: int = 3,
        requests_per_second: float = 2.0,
        burst_capacity: int = 10
    ):
        """
        Initialize JARVIS connector.
        
        Args:
            timeout: Request timeout in seconds
            max_retries: Maximum number of retry attempts
            requests_per_second: Rate limit for API requests
            burst_capacity: Maximum burst requests allowed
        """
        super().__init__(self.BASE_URL, timeout)
        
        self.max_retries = max_retries
        self._client: Optional[httpx.AsyncClient] = None
        self._cache: Dict[str, Any] = {}
        self._cache_ttl = 3600  # 1 hour cache
        
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
            
            # Test connection with health check
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
            
            # Test with a simple request to the base URL
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
        """
        Search for materials based on criteria.
        
        Args:
            formula: Chemical formula to search for
            n_elements: Number of elements in the compound
            properties: List of properties to include in results
            dataset: JARVIS dataset to search in
            limit: Maximum number of results
            
        Returns:
            List of matching materials
        """
        try:
            # Load dataset
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
        """
        Get a specific material by its JARVIS ID.
        
        Args:
            jarvis_id: JARVIS ID (jid) of the material
            dataset: JARVIS dataset to search in
            
        Returns:
            Material data dictionary
        """
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
        """
        Fetch materials in bulk with pagination.
        
        Args:
            limit: Maximum number of materials to fetch
            offset: Number of materials to skip
            dataset: JARVIS dataset to fetch from
            
        Returns:
            List of materials
        """
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
    
    async def search(self, **kwargs) -> List[Dict[str, Any]]:
        """Generic search interface."""
        return await self.search_materials(**kwargs)
    
    async def get_by_id(self, record_id: str) -> Dict[str, Any]:
        """Generic get by ID interface."""
        return await self.get_material_by_id(record_id)
    
    async def fetch_bulk(self, limit: int = 100, offset: int = 0) -> List[Dict[str, Any]]:
        """Generic bulk fetch interface."""
        return await self.fetch_bulk_materials(limit, offset)
    
    @retry(
        stop=stop_after_attempt(3),
        wait=wait_exponential(multiplier=1, min=4, max=10),
        retry=retry_if_exception_type((httpx.TimeoutException, httpx.ConnectError))
    )
    async def _load_dataset(self, dataset: str) -> List[Dict[str, Any]]:
        """
        Load JARVIS dataset with caching and rate limiting.
        
        Args:
            dataset: Name of the dataset to load
            
        Returns:
            List of materials from the dataset
        """
        # Check cache first
        cache_key = f"dataset:{dataset}"
        if cache_key in self._cache:
            cache_entry = self._cache[cache_key]
            if datetime.now().timestamp() - cache_entry["timestamp"] < self._cache_ttl:
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
                "timestamp": datetime.now().timestamp()
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
        """
        Extract and standardize material data.
        
        Args:
            material: Raw material data from JARVIS
            properties: Specific properties to extract
            
        Returns:
            Standardized material data
        """
        # Default extraction
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
        
        # Common elastic properties in JARVIS
        elastic_props = [
            "bulk_modulus_kv", "shear_modulus_gv", 
            "elastic_tensor", "poisson_ratio"
        ]
        
        for prop in elastic_props:
            if prop in material:
                elastic_data[prop] = material[prop]
        
        return elastic_data if elastic_data else None
    
    def _convert_structure(self, atoms_data: Optional[Dict[str, Any]]) -> Optional[Dict[str, Any]]:
        """
        Convert JARVIS atomic structure to standard format.
        
        Args:
            atoms_data: Raw atomic structure data
            
        Returns:
            Standardized structure data
        """
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
        
        # Simple string matching - could be enhanced with chemical formula parsing
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


# Factory function for easy instantiation
def create_jarvis_connector(**kwargs) -> JarvisConnector:
    """Create a JARVIS connector with default settings."""
    return JarvisConnector(**kwargs)
