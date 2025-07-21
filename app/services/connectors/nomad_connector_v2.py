"""
NOMAD Laboratory database connector implementation (v2).

This module provides a connector for the NOMAD repository using their proper v1 API.
Based on research of actual NOMAD API endpoints and schemas.

NOMAD (Novel Materials Discovery) Laboratory: https://nomad-lab.eu/
API Documentation: https://nomad-lab.eu/prod/v1/api/v1/
"""

import asyncio
import json
import logging
import time
from datetime import datetime, timezone
from typing import Any, Dict, List, Optional, Union

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


class NOMADConnectorV2(DatabaseConnector):
    """NOMAD Laboratory database connector using v1 API."""
    
    def __init__(self, rate_limit: float = 1.0, config: Optional[Dict[str, Any]] = None):
        self.base_url = "https://nomad-lab.eu/prod/v1/api/v1"
        self.rate_limit = rate_limit
        self.client: Optional[httpx.AsyncClient] = None
        self.config = config or {}
        
    async def connect(self) -> bool:
        """Connect to NOMAD API."""
        try:
            self.client = httpx.AsyncClient(timeout=30.0)
            
            # Test connection with a simple query
            test_response = await self.client.post(
                f"{self.base_url}/entries/query",
                json={
                    "pagination": {"page_size": 1},
                    "required": {"include": ["entry_id"]}
                }
            )
            
            if test_response.status_code == 200:
                logger.info("Successfully connected to NOMAD v1 API")
                return True
            else:
                logger.error(f"Failed to connect to NOMAD: {test_response.status_code}")
                return False
                
        except Exception as e:
            logger.error(f"Error connecting to NOMAD: {e}")
            return False
    
    async def disconnect(self):
        """Disconnect from NOMAD API."""
        if self.client:
            await self.client.aclose()
            self.client = None
    
    async def search_materials(
        self,
        elements: Optional[List[str]] = None,
        formula: Optional[str] = None,
        formation_energy_range: Optional[tuple] = None,
        band_gap_range: Optional[tuple] = None,
        crystal_system: Optional[str] = None,
        space_group: Optional[Union[str, int]] = None,
        limit: int = 100,
        offset: int = 0,
        **kwargs
    ) -> List[StandardizedMaterial]:
        """
        Search for materials using NOMAD v1 API.
        
        Args:
            elements: List of elements to search for
            formula: Specific chemical formula
            formation_energy_range: Tuple of (min, max) formation energy in eV/atom
            band_gap_range: Tuple of (min, max) band gap in eV
            crystal_system: Crystal system (cubic, tetragonal, etc.)
            space_group: Space group number or symbol
            limit: Maximum number of results to return
            offset: Number of results to skip (for pagination)
        
        Returns:
            List of StandardizedMaterial objects
        """
        
        if not self.client:
            await self.connect()
        
        try:
            # Build the NOMAD query according to their v1 API schema
            query = {}
            
            if elements:
                query['results.material.elements'] = {'all': elements}
            
            if formula:
                query['results.material.chemical_formula_hill'] = formula
                
            if formation_energy_range:
                min_energy, max_energy = formation_energy_range
                query['results.properties.thermodynamic.formation_energy_per_atom'] = {
                    'gte': min_energy,
                    'lte': max_energy
                }
                
            if band_gap_range:
                min_gap, max_gap = band_gap_range
                query['results.properties.electronic.band_gap'] = {
                    'gte': min_gap,
                    'lte': max_gap
                }
            
            if crystal_system:
                query['results.material.symmetry.crystal_system'] = crystal_system
                
            if space_group:
                if isinstance(space_group, int):
                    query['results.material.symmetry.space_group_number'] = space_group
                else:
                    query['results.material.symmetry.space_group_symbol'] = space_group
            
            # Construct the request body according to NOMAD v1 API
            # Start with a minimal working query
            request_body = {
                'pagination': {
                    'page_size': min(limit, 100),
                    'page_offset': offset
                }
            }
            
            # Only add query if we have criteria
            if query:
                request_body['query'] = query
            
            # Add required fields only if we have specific needs
            if elements or formula or formation_energy_range or band_gap_range:
                request_body['required'] = {
                    'include': [
                        'entry_id',
                        'results.material.chemical_formula_hill',
                        'results.material.elements'
                    ]
                }
            
            # Make the request with rate limiting
            await asyncio.sleep(self.rate_limit)
            
            logger.info(f"Making NOMAD API request to {self.base_url}/entries/query")
            response = await self.client.post(
                f"{self.base_url}/entries/query", 
                json=request_body
            )
            response.raise_for_status()
            
            data = response.json()
            materials = []
            
            if 'data' in data:
                for entry in data['data']:
                    try:
                        material = self._process_nomad_entry(entry)
                        if material:
                            materials.append(material)
                    except Exception as e:
                        logger.warning(f"Error processing NOMAD entry: {e}")
                        continue
            
            logger.info(f"Retrieved {len(materials)} materials from NOMAD")
            return materials
            
        except Exception as e:
            logger.error(f"Error searching NOMAD: {e}")
            return []
    
    def _process_nomad_entry(self, entry: Dict[str, Any]) -> Optional[StandardizedMaterial]:
        """Process a single NOMAD entry into standardized format."""
        try:
            # Extract basic information
            entry_id = entry.get('entry_id', '')
            
            # Extract material information from results
            results = entry.get('results', {})
            material_data = results.get('material', {})
            properties_data = results.get('properties', {})
            
            # Basic material info
            formula = material_data.get('chemical_formula_hill', '')
            elements = material_data.get('elements', [])
            
            # Structural information
            symmetry = material_data.get('symmetry', {})
            space_group = symmetry.get('space_group_number')
            crystal_system = symmetry.get('crystal_system')
            
            # Properties
            thermodynamic = properties_data.get('thermodynamic', {})
            electronic = properties_data.get('electronic', {})
            
            formation_energy = None
            if thermodynamic and 'formation_energy_per_atom' in thermodynamic:
                formation_energy = thermodynamic['formation_energy_per_atom']
            
            band_gap = None
            if electronic and 'band_gap' in electronic:
                band_gap = electronic['band_gap']
            
            # Create structure
            structure = MaterialStructure(
                lattice_parameters=[],
                atomic_positions=[],
                atomic_species=[],
                space_group=space_group,
                crystal_system=crystal_system
            )
            
            # Create properties
            properties = MaterialProperties(
                formation_energy=formation_energy,
                band_gap=band_gap,
                density=None,
                magnetic_moment=None
            )
            
            # Create metadata
            metadata = MaterialMetadata(
                source="nomad",
                entry_id=entry_id,
                calculated_properties=list(properties_data.keys()) if properties_data else [],
                last_updated=datetime.now().isoformat()
            )
            
            return StandardizedMaterial(
                id=entry_id,
                formula=formula,
                elements=elements,
                structure=structure,
                properties=properties,
                metadata=metadata
            )
            
        except Exception as e:
            logger.error(f"Error processing NOMAD entry: {e}")
            return None
    
    async def get_material_by_id(self, material_id: str) -> Optional[StandardizedMaterial]:
        """Get specific material by NOMAD entry ID."""
        if not self.client:
            await self.connect()
        
        try:
            # Use the archive endpoint for detailed material data
            response = await self.client.get(f"{self.base_url}/entries/{material_id}/archive")
            
            if response.status_code == 404:
                logger.warning(f"Material {material_id} not found in NOMAD")
                return None
            
            response.raise_for_status()
            data = response.json()
            
            return self._process_nomad_entry(data)
            
        except Exception as e:
            logger.error(f"Error fetching material {material_id} from NOMAD: {e}")
            return None
    
    async def health_check(self) -> bool:
        """Check if the NOMAD API is accessible."""
        try:
            if not self.client:
                await self.connect()
            
            response = await self.client.post(
                f"{self.base_url}/entries/query",
                json={
                    "pagination": {"page_size": 1},
                    "required": {"include": ["entry_id"]}
                }
            )
            
            return response.status_code == 200
            
        except Exception as e:
            logger.warning(f"NOMAD health check failed: {e}")
            return False
    
    def get_status(self) -> Dict[str, Any]:
        """Get the current status of the connector."""
        return {
            "name": "NOMAD",
            "connected": self.client is not None,
            "api_url": self.base_url,
            "last_query_time": None,
            "total_queries": 0,
            "error_count": 0
        }
    
    async def fetch_bulk_materials(
        self,
        limit: int = 100,
        offset: int = 0,
        filters: Optional[Dict[str, Any]] = None
    ) -> List[StandardizedMaterial]:
        """Fetch materials in bulk with optional filtering."""
        return await self.search_materials(limit=limit, offset=offset, **(filters or {}))
    
    async def validate_response(self, response: Dict[str, Any]) -> bool:
        """Validate response data from NOMAD."""
        if not isinstance(response, dict):
            return False
        return 'data' in response or 'entry_id' in response
    
    async def standardize_data(self, raw_data: Dict[str, Any]) -> Optional[StandardizedMaterial]:
        """Convert raw NOMAD data to standardized format."""
        return self._process_nomad_entry(raw_data)
