"""
NOMAD Laboratory database connector implementation.

This module provides a comprehensive connector for the NOMAD repository,
handling their specific API requirements, query syntax, and response structure.

NOMAD (Novel Materials Discovery) Laboratory: https://nomad-lab.eu/
API Documentation: https://nomad-lab.eu/prod/v1/docs/

Key Features:
- NOMAD-specific query syntax support
- Streaming for large datasets
- Pagination handling
- Multiple data sections support
- Material property extraction from NOMAD format
"""

import asyncio
import json
import logging
from datetime import datetime, timedelta, timezone
from typing import Any, Dict, List, Optional, Union, AsyncGenerator
from urllib.parse import urlencode

import httpx

from .base_connector import (
    DatabaseConnector, 
    StandardizedMaterial, 
    MaterialStructure, 
    MaterialProperties, 
    MaterialMetadata,
    ConnectorStatus
)


logger = logging.getLogger(__name__)


class NOMADQueryBuilder:
    """Query builder for NOMAD's specific query syntax."""
    
    def __init__(self):
        self.query_parts = []
        self.pagination = {}
        self.required_sections = []
    
    def elements(self, elements: List[str], operator: str = "HAS ANY") -> "NOMADQueryBuilder":
        """Add element filter using NOMAD syntax."""
        if operator.upper() == "HAS ANY":
            query = f"results.material.elements HAS ANY {json.dumps(elements)}"
        elif operator.upper() == "HAS ALL":
            query = f"results.material.elements HAS ALL {json.dumps(elements)}"
        else:
            raise ValueError(f"Unsupported element operator: {operator}")
        
        self.query_parts.append(query)
        return self
    
    def element_count(self, count: int, operator: str = "gte") -> "NOMADQueryBuilder":
        """Add element count filter."""
        valid_operators = ["gte", "lte", "eq", "gt", "lt"]
        if operator not in valid_operators:
            raise ValueError(f"Operator must be one of: {valid_operators}")
        
        query = f"results.material.n_elements:{operator}: {count}"
        self.query_parts.append(query)
        return self
    
    def formula(self, formula: str) -> "NOMADQueryBuilder":
        """Add chemical formula filter."""
        query = f'results.material.chemical_formula_reduced:"{formula}"'
        self.query_parts.append(query)
        return self
    
    def formula_contains(self, partial_formula: str) -> "NOMADQueryBuilder":
        """Add partial formula match."""
        query = f'results.material.chemical_formula_reduced:*{partial_formula}*'
        self.query_parts.append(query)
        return self
    
    def space_group(self, space_group: Union[str, int]) -> "NOMADQueryBuilder":
        """Add space group filter."""
        if isinstance(space_group, int):
            query = f"results.material.symmetry.space_group_number:{space_group}"
        else:
            query = f'results.material.symmetry.space_group_symbol:"{space_group}"'
        
        self.query_parts.append(query)
        return self
    
    def property_range(
        self, 
        property_path: str, 
        min_value: Optional[float] = None, 
        max_value: Optional[float] = None
    ) -> "NOMADQueryBuilder":
        """Add property range filter."""
        if min_value is not None:
            self.query_parts.append(f"{property_path}:gte:{min_value}")
        if max_value is not None:
            self.query_parts.append(f"{property_path}:lte:{max_value}")
        return self
    
    def band_gap_range(
        self, 
        min_gap: Optional[float] = None, 
        max_gap: Optional[float] = None
    ) -> "NOMADQueryBuilder":
        """Add band gap filter."""
        return self.property_range(
            "results.properties.electronic.band_gap.value",
            min_gap,
            max_gap
        )
    
    def formation_energy_range(
        self, 
        min_energy: Optional[float] = None, 
        max_energy: Optional[float] = None
    ) -> "NOMADQueryBuilder":
        """Add formation energy filter."""
        return self.property_range(
            "results.properties.thermodynamic.formation_energy_per_atom.value",
            min_energy,
            max_energy
        )
    
    def add_section(self, section: str) -> "NOMADQueryBuilder":
        """Add required data section."""
        valid_sections = [
            "run",
            "system",
            "calculation",
            "method",
            "results",
            "metadata"
        ]
        
        if section not in valid_sections:
            logger.warning(f"Unknown section '{section}'. Valid sections: {valid_sections}")
        
        if section not in self.required_sections:
            self.required_sections.append(section)
        return self
    
    def paginate(self, page_size: int = 100, page_offset: int = 0) -> "NOMADQueryBuilder":
        """Set pagination parameters."""
        self.pagination = {
            "page_size": min(page_size, 10000),  # NOMAD max limit
            "page_offset": page_offset
        }
        return self
    
    def build(self) -> Dict[str, Any]:
        """Build the final query for NOMAD API."""
        query = {}
        
        # Add query filter
        if self.query_parts:
            query["query"] = " AND ".join(self.query_parts)
        
        # Add pagination
        if self.pagination:
            query.update(self.pagination)
        else:
            query["page_size"] = 100  # Default
        
        # Add required sections
        if self.required_sections:
            query["required"] = ",".join(self.required_sections)
        
        return query


class NOMADConnector(DatabaseConnector):
    """
    NOMAD Laboratory database connector.
    
    Provides access to the NOMAD repository with support for:
    - NOMAD-specific query syntax
    - Streaming large datasets
    - Multiple data sections
    - Comprehensive material property extraction
    """
    
    def __init__(self, config: Dict[str, Any], rate_limiter=None):
        """
        Initialize NOMAD connector.
        
        Args:
            config: Configuration dictionary containing NOMAD API settings
            rate_limiter: Optional rate limiter instance
        """
        # Extract config values with defaults
        base_url = config.get("base_url", "https://nomad-lab.eu/prod/v1/api/v1")
        timeout = config.get("timeout", 30.0)
        max_retries = config.get("max_retries", 3)
        requests_per_second = config.get("requests_per_second", 2.0)
        burst_capacity = config.get("burst_capacity", 10)
        cache_ttl = config.get("cache_ttl", 3600)
        
        # Initialize base connector
        super().__init__(
            base_url=base_url,
            timeout=int(timeout),
            requests_per_second=requests_per_second,
            burst_capacity=burst_capacity,
            cache_ttl=cache_ttl,
            max_retries=max_retries,
            redis_client=rate_limiter
        )
        
        # Store full config for reference
        self.config = config
        
        # NOMAD-specific settings
        self.stream_threshold = config.get("stream_threshold", 1000)
        self.default_page_size = config.get("default_page_size", 100)
        self.max_page_size = config.get("max_page_size", 10000)
        
        # NOMAD-specific endpoints
        self.endpoints = {
            "entries": f"{self.base_url}/entries",
            "materials": f"{self.base_url}/materials", 
            "entry": f"{self.base_url}/entries/{{entry_id}}",
            "raw": f"{self.base_url}/entries/{{entry_id}}/raw",
            "archive": f"{self.base_url}/entries/{{entry_id}}/archive"
        }
        
        self.client: Optional[httpx.AsyncClient] = None
        self.status = ConnectorStatus.DISCONNECTED
    
    async def connect(self) -> bool:
        """Establish connection to NOMAD API."""
        try:
            self.status = ConnectorStatus.CONNECTING
            
            self.client = httpx.AsyncClient(
                timeout=httpx.Timeout(self.timeout),
                follow_redirects=True,
                headers={
                    "Accept": "application/json",
                    "User-Agent": "PRISM-DataIngestion/1.0"
                }
            )
            
            # Test connection with a minimal query
            test_params = {"page_size": 1}
            response = await self.client.get(self.endpoints["entries"], params=test_params)
            response.raise_for_status()
            
            self.status = ConnectorStatus.CONNECTED
            logger.info("Successfully connected to NOMAD API")
            return True
            
        except Exception as e:
            self.status = ConnectorStatus.ERROR
            logger.error(f"Failed to connect to NOMAD API: {e}")
            return False
    
    async def disconnect(self) -> bool:
        """Close connection to NOMAD API."""
        try:
            if self.client:
                await self.client.aclose()
                self.client = None
            
            self.status = ConnectorStatus.DISCONNECTED
            logger.info("Disconnected from NOMAD API")
            return True
            
        except Exception as e:
            logger.error(f"Error disconnecting from NOMAD API: {e}")
            return False
    
    async def search_materials(
        self,
        query_builder: Optional[NOMADQueryBuilder] = None,
        **kwargs
    ) -> List[StandardizedMaterial]:
        """
        Search materials using NOMAD query syntax.
        
        Args:
            query_builder: NOMADQueryBuilder instance for complex queries
            **kwargs: Simple search parameters (formula, elements, etc.)
        
        Returns:
            List of standardized materials
        """
        if not self.client:
            raise RuntimeError("Connector not connected. Call connect() first.")
        
        # Build query
        if query_builder:
            query_params = query_builder.build()
        else:
            query_params = self._build_simple_query(**kwargs)
        
        # Add required sections for complete material data
        if "required" not in query_params:
            query_params["required"] = "results,run,system"
        
        materials = []
        
        try:
            # Check if we should use streaming
            total_count = await self._get_total_count(query_params)
            
            if total_count > self.stream_threshold:
                logger.info(f"Large dataset detected ({total_count} entries). Using streaming mode.")
                async for material in self._stream_materials(query_params):
                    materials.append(material)
            else:
                # Use regular pagination for smaller datasets
                materials = await self._fetch_paginated_materials(query_params)
            
            logger.info(f"Retrieved {len(materials)} materials from NOMAD")
            return materials
            
        except Exception as e:
            logger.error(f"Error searching NOMAD materials: {e}")
            raise
    
    async def get_material_by_id(self, material_id: str) -> Optional[StandardizedMaterial]:
        """
        Get specific material by NOMAD entry ID.
        
        Args:
            material_id: NOMAD entry ID
        
        Returns:
            Standardized material or None if not found
        """
        if not self.client:
            raise RuntimeError("Connector not connected. Call connect() first.")
        
        try:
            # Get entry with full archive data
            url = self.endpoints["archive"].format(entry_id=material_id)
            
            await self.rate_limiter.wait_for_permit("default")
            response = await self.client.get(url)
            
            if response.status_code == 404:
                logger.warning(f"Material {material_id} not found in NOMAD")
                return None
            
            response.raise_for_status()
            data = response.json()
            
            # Convert to standardized format
            material = self._convert_to_standard_material(data, material_id)
            
            self.metrics.successful_requests += 1
            logger.debug(f"Retrieved material {material_id} from NOMAD")
            return material
            
        except httpx.HTTPStatusError as e:
            self.metrics.failed_requests += 1
            logger.error(f"HTTP error getting material {material_id}: {e}")
            return None
        except Exception as e:
            self.metrics.failed_requests += 1
            logger.error(f"Error getting material {material_id}: {e}")
            return None
    
    async def fetch_bulk_materials(
        self,
        dataset: Optional[str] = None,
        limit: Optional[int] = None,
        **kwargs
    ) -> List[StandardizedMaterial]:
        """
        Fetch materials in bulk with streaming support.
        
        Args:
            dataset: Dataset filter (not directly applicable to NOMAD)
            limit: Maximum number of materials to fetch
            **kwargs: Additional search parameters
        
        Returns:
            List of standardized materials
        """
        # Build query for bulk fetch
        query_builder = NOMADQueryBuilder()
        
        # Add sections for complete data
        query_builder.add_section("results").add_section("run").add_section("system")
        
        # Apply limit if specified
        if limit:
            query_builder.paginate(page_size=min(limit, 10000))
        
        # Add any additional filters
        if "elements" in kwargs:
            query_builder.elements(kwargs["elements"])
        
        if "min_elements" in kwargs:
            query_builder.element_count(kwargs["min_elements"], "gte")
        
        if "max_elements" in kwargs:
            query_builder.element_count(kwargs["max_elements"], "lte")
        
        return await self.search_materials(query_builder, **kwargs)
    
    async def validate_response(self, response_data: Dict[str, Any]) -> bool:
        """Validate NOMAD API response structure."""
        try:
            # Check for required top-level fields
            if "data" not in response_data:
                logger.error("NOMAD response missing 'data' field")
                return False
            
            # For pagination info
            if "pagination" not in response_data:
                logger.warning("NOMAD response missing pagination info")
            
            return True
            
        except Exception as e:
            logger.error(f"Error validating NOMAD response: {e}")
            return False
    
    def standardize_data(self, raw_data: Dict[str, Any], entry_id: str) -> StandardizedMaterial:
        """Convert NOMAD data to standardized format."""
        return self._convert_to_standard_material(raw_data, entry_id)
    
    async def _get_total_count(self, query_params: Dict[str, Any]) -> int:
        """Get total count of results for a query."""
        try:
            # Make a minimal query to get count
            count_params = query_params.copy()
            count_params["page_size"] = 1
            count_params.pop("page_offset", None)
            
            await self.rate_limiter.wait_for_permit("default")
            response = await self.client.get(self.endpoints["entries"], params=count_params)
            response.raise_for_status()
            
            data = response.json()
            return data.get("pagination", {}).get("total", 0)
            
        except Exception as e:
            logger.warning(f"Could not get total count: {e}")
            return 0
    
    async def _stream_materials(
        self, 
        query_params: Dict[str, Any]
    ) -> AsyncGenerator[StandardizedMaterial, None]:
        """Stream materials for large datasets."""
        page_size = min(query_params.get("page_size", 1000), 10000)
        page_offset = 0
        
        while True:
            # Update pagination for current page
            current_params = query_params.copy()
            current_params.update({
                "page_size": page_size,
                "page_offset": page_offset
            })
            
            try:
                await self.rate_limiter.wait_for_permit("default")
                response = await self.client.get(self.endpoints["entries"], params=current_params)
                response.raise_for_status()
                
                data = response.json()
                entries = data.get("data", [])
                
                if not entries:
                    break
                
                # Convert each entry to standardized format
                for entry in entries:
                    try:
                        material = self._convert_to_standard_material(
                            entry, 
                            entry.get("entry_id", f"unknown_{page_offset}")
                        )
                        yield material
                    except Exception as e:
                        logger.warning(f"Error converting entry to standard format: {e}")
                        continue
                
                # Check if we've reached the end
                pagination = data.get("pagination", {})
                if len(entries) < page_size or page_offset + page_size >= pagination.get("total", 0):
                    break
                
                page_offset += page_size
                
                # Small delay between pages to be respectful
                await asyncio.sleep(0.1)
                
            except Exception as e:
                logger.error(f"Error streaming materials from NOMAD: {e}")
                break
    
    async def _fetch_paginated_materials(
        self, 
        query_params: Dict[str, Any]
    ) -> List[StandardizedMaterial]:
        """Fetch materials using pagination for smaller datasets."""
        materials = []
        page_size = min(query_params.get("page_size", 1000), 10000)
        page_offset = 0
        
        while True:
            current_params = query_params.copy()
            current_params.update({
                "page_size": page_size,
                "page_offset": page_offset
            })
            
            try:
                await self.rate_limiter.wait_for_permit("default")
                response = await self.client.get(self.endpoints["entries"], params=current_params)
                response.raise_for_status()
                
                data = response.json()
                entries = data.get("data", [])
                
                if not entries:
                    break
                
                # Convert entries to standardized format
                for entry in entries:
                    try:
                        material = self._convert_to_standard_material(
                            entry,
                            entry.get("entry_id", f"unknown_{page_offset}")
                        )
                        materials.append(material)
                    except Exception as e:
                        logger.warning(f"Error converting entry: {e}")
                        continue
                
                # Check if we've reached the end
                pagination = data.get("pagination", {})
                if len(entries) < page_size or page_offset + page_size >= pagination.get("total", 0):
                    break
                
                page_offset += page_size
                
            except Exception as e:
                logger.error(f"Error fetching paginated materials: {e}")
                break
        
        return materials
    
    def _build_simple_query(self, **kwargs) -> Dict[str, Any]:
        """Build simple query from keyword arguments."""
        query_builder = NOMADQueryBuilder()
        
        if "formula" in kwargs:
            query_builder.formula(kwargs["formula"])
        
        if "elements" in kwargs:
            elements = kwargs["elements"]
            if isinstance(elements, str):
                elements = [elements]
            query_builder.elements(elements)
        
        if "min_elements" in kwargs:
            query_builder.element_count(kwargs["min_elements"], "gte")
        
        if "max_elements" in kwargs:
            query_builder.element_count(kwargs["max_elements"], "lte")
        
        if "space_group" in kwargs:
            query_builder.space_group(kwargs["space_group"])
        
        # Handle property ranges
        if "band_gap_min" in kwargs or "band_gap_max" in kwargs:
            query_builder.band_gap_range(
                kwargs.get("band_gap_min"),
                kwargs.get("band_gap_max")
            )
        
        if "formation_energy_min" in kwargs or "formation_energy_max" in kwargs:
            query_builder.formation_energy_range(
                kwargs.get("formation_energy_min"),
                kwargs.get("formation_energy_max")
            )
        
        # Set default pagination
        page_size = kwargs.get("limit", 100)
        query_builder.paginate(page_size=page_size)
        
        return query_builder.build()
    
    def _convert_to_standard_material(
        self, 
        nomad_data: Dict[str, Any], 
        entry_id: str
    ) -> StandardizedMaterial:
        """Convert NOMAD entry data to standardized material format."""
        try:
            # Extract basic material info
            results = nomad_data.get("results", {})
            material_info = results.get("material", {})
            run_info = nomad_data.get("run", [{}])[0] if nomad_data.get("run") else {}
            system_info = nomad_data.get("system", [{}])[0] if nomad_data.get("system") else {}
            
            # Get chemical formula
            formula = material_info.get("chemical_formula_reduced", "Unknown")
            
            # Extract structure information
            structure = self._extract_structure(system_info, material_info)
            
            # Extract properties
            properties = self._extract_properties(results)
            
            # Create metadata
            metadata = MaterialMetadata(
                fetched_at=datetime.now(timezone.utc),
                version="nomad-api-v1",
                source_url=f"https://nomad-lab.eu/prod/v1/gui/entry/id/{entry_id}",
                last_updated=self._parse_nomad_date(run_info.get("time_run")),
                experimental=self._is_experimental(nomad_data)
            )
            
            return StandardizedMaterial(
                source_db="nomad",
                source_id=entry_id,
                formula=formula,
                structure=structure,
                properties=properties,
                metadata=metadata
            )
            
        except Exception as e:
            logger.error(f"Error converting NOMAD data to standard format: {e}")
            # Return minimal material data
            return StandardizedMaterial(
                source_db="nomad",
                source_id=entry_id,
                formula="Unknown",
                structure=MaterialStructure([], [], []),
                properties=MaterialProperties(),
                metadata=MaterialMetadata(
                    fetched_at=datetime.now(timezone.utc),
                    version="nomad-api-v1"
                )
            )
    
    def _extract_structure(
        self, 
        system_info: Dict[str, Any], 
        material_info: Dict[str, Any]
    ) -> MaterialStructure:
        """Extract structure information from NOMAD data."""
        try:
            # Get lattice vectors
            lattice = system_info.get("atoms", {}).get("lattice_vectors")
            if lattice is None:
                lattice = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
            
            # Get atomic positions (convert to fractional if needed)
            positions = system_info.get("atoms", {}).get("positions", [])
            if not positions:
                positions = [[0.0, 0.0, 0.0]]
            
            # Get atomic species
            labels = system_info.get("atoms", {}).get("labels", [])
            if not labels:
                # Try to extract from chemical formula
                formula = material_info.get("chemical_formula_reduced", "H")
                labels = [formula.split()[0] if formula else "H"]
            
            # Get symmetry information
            symmetry = material_info.get("symmetry", {})
            space_group = symmetry.get("space_group_symbol")
            crystal_system = symmetry.get("crystal_system")
            
            # Calculate volume if available
            volume = system_info.get("atoms", {}).get("cell", {}).get("volume")
            
            return MaterialStructure(
                lattice_parameters=lattice,
                atomic_positions=positions,
                atomic_species=labels,
                space_group=space_group,
                crystal_system=crystal_system,
                volume=volume
            )
            
        except Exception as e:
            logger.warning(f"Error extracting structure from NOMAD data: {e}")
            return MaterialStructure(
                lattice_parameters=[[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                atomic_positions=[[0.0, 0.0, 0.0]],
                atomic_species=["H"]
            )
    
    def _extract_properties(self, results: Dict[str, Any]) -> MaterialProperties:
        """Extract material properties from NOMAD results."""
        try:
            properties = MaterialProperties()
            
            # Extract electronic properties
            electronic = results.get("properties", {}).get("electronic", {})
            if electronic:
                band_gap_info = electronic.get("band_gap")
                if band_gap_info:
                    properties.band_gap = band_gap_info.get("value")
            
            # Extract thermodynamic properties
            thermo = results.get("properties", {}).get("thermodynamic", {})
            if thermo:
                formation_energy = thermo.get("formation_energy_per_atom")
                if formation_energy:
                    properties.formation_energy = formation_energy.get("value")
            
            # Extract mechanical properties
            mechanical = results.get("properties", {}).get("mechanical", {})
            if mechanical:
                bulk_modulus = mechanical.get("bulk_modulus")
                if bulk_modulus:
                    properties.bulk_modulus = bulk_modulus.get("value")
                
                shear_modulus = mechanical.get("shear_modulus")
                if shear_modulus:
                    properties.shear_modulus = shear_modulus.get("value")
                
                elastic_tensor = mechanical.get("elastic_tensor")
                if elastic_tensor:
                    properties.elastic_tensor = elastic_tensor.get("value")
            
            # Extract magnetic properties
            magnetic = results.get("properties", {}).get("magnetic", {})
            if magnetic:
                magnetic_moment = magnetic.get("total_magnetization")
                if magnetic_moment:
                    properties.magnetic_moment = magnetic_moment.get("value")
            
            return properties
            
        except Exception as e:
            logger.warning(f"Error extracting properties from NOMAD data: {e}")
            return MaterialProperties()
    
    def _parse_nomad_date(self, date_str: Optional[str]) -> Optional[datetime]:
        """Parse NOMAD date string to datetime object."""
        if not date_str:
            return None
        
        try:
            # NOMAD typically uses ISO format
            return datetime.fromisoformat(date_str.replace('Z', '+00:00'))
        except Exception:
            try:
                # Try common formats
                return datetime.strptime(date_str, "%Y-%m-%dT%H:%M:%S")
            except Exception:
                logger.warning(f"Could not parse NOMAD date: {date_str}")
                return None
    
    def _is_experimental(self, nomad_data: Dict[str, Any]) -> bool:
        """Determine if the data is experimental."""
        # Check method information to determine if experimental
        run_info = nomad_data.get("run", [{}])[0] if nomad_data.get("run") else {}
        program = run_info.get("program", {}).get("name", "").lower()
        
        # Most NOMAD data is computational, but some experimental data exists
        experimental_indicators = ["exp", "experimental", "measurement", "xrd", "neutron"]
        return any(indicator in program for indicator in experimental_indicators)


# Convenience function to create NOMAD query builder
def create_nomad_query() -> NOMADQueryBuilder:
    """Create a new NOMAD query builder instance."""
    return NOMADQueryBuilder()
