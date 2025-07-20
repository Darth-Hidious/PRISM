"""
OQMD (Open Quantum Materials Database) connector.
Provides access to over 700,000 DFT-calculated materials with formation energies, 
stability data, and crystal structures.

API Documentation: https://static.oqmd.org/static/docs/restful.html
Base URL: http://oqmd.org/oqmdapi/
"""

import asyncio
import aiohttp
import logging
from typing import Dict, List, Any, Optional, Union
from datetime import datetime
from urllib.parse import urlencode

from .base_connector import (
    DatabaseConnector, 
    StandardizedMaterial, 
    MaterialStructure, 
    MaterialProperties, 
    MaterialMetadata
)

logger = logging.getLogger(__name__)


class OQMDConnector(DatabaseConnector):
    """
    Connector for OQMD (Open Quantum Materials Database).
    
    Key features:
    - Over 700,000 DFT-calculated materials
    - Formation energies (delta_e)
    - Stability data (hull distance)
    - Band gaps
    - Crystal structures and space groups
    - OPTiMaDe compatible
    """
    
    def __init__(self, config: Dict[str, Any]):
        base_url = config.get('base_url', 'http://oqmd.org/oqmdapi')
        timeout = config.get('timeout', 30.0)
        requests_per_second = config.get('requests_per_second', 2.0)
        burst_capacity = config.get('burst_capacity', 10)
        cache_ttl = config.get('cache_ttl', 3600)
        max_retries = config.get('max_retries', 3)
        
        super().__init__(
            base_url=base_url,
            timeout=timeout,
            requests_per_second=requests_per_second,
            burst_capacity=burst_capacity,
            cache_ttl=cache_ttl,
            max_retries=max_retries
        )
        
        self.base_url = base_url
        self.timeout = timeout
        self.max_retries = max_retries
        self.session = None
        
        # OQMD-specific settings
        self.default_fields = [
            'name', 'entry_id', 'spacegroup', 'ntypes', 'natoms', 
            'volume', 'delta_e', 'band_gap', 'stability', 'prototype'
        ]
        
    async def connect(self) -> bool:
        """Establish connection to OQMD API."""
        try:
            timeout = aiohttp.ClientTimeout(total=self.timeout)
            self.session = aiohttp.ClientSession(timeout=timeout)
            
            # Test connection
            test_url = f"{self.base_url}/formationenergy"
            async with self.session.get(f"{test_url}?limit=1") as response:
                if response.status == 200:
                    logger.info("Successfully connected to OQMD database")
                    return True
                else:
                    logger.error(f"OQMD connection failed with status: {response.status}")
                    return False
                    
        except Exception as e:
            logger.error(f"Failed to connect to OQMD: {e}")
            return False
    
    async def disconnect(self) -> bool:
        """Close connection to OQMD API."""
        if self.session:
            await self.session.close()
            self.session = None
        return True
    
    async def health_check(self) -> bool:
        """Check if OQMD API is accessible."""
        try:
            if not self.session:
                return False
                
            test_url = f"{self.base_url}/formationenergy"
            async with self.session.get(f"{test_url}?limit=1") as response:
                return response.status == 200
                
        except Exception as e:
            logger.error(f"OQMD health check failed: {e}")
            return False
    
    async def search_materials(
        self,
        elements: Optional[Union[str, List[str]]] = None,
        formula: Optional[str] = None,
        space_group: Optional[str] = None,
        formation_energy_max: Optional[float] = None,
        stability_max: Optional[float] = None,
        band_gap_min: Optional[float] = None,
        band_gap_max: Optional[float] = None,
        limit: int = 50,
        offset: int = 0,
        **kwargs
    ) -> List[StandardizedMaterial]:
        """
        Search for materials in OQMD database.
        
        Args:
            elements: List of elements or comma-separated string (e.g., "Al,Fe" or ["Al", "Fe"])
            formula: Specific chemical formula
            space_group: Crystal space group (e.g., "Fm-3m")
            formation_energy_max: Maximum formation energy (delta_e)
            stability_max: Maximum hull distance (stability)
            band_gap_min: Minimum band gap
            band_gap_max: Maximum band gap
            limit: Maximum number of results
            offset: Offset for pagination
            
        Returns:
            List of StandardizedMaterial objects
        """
        try:
            # Build query parameters
            params = {
                'fields': ','.join(self.default_fields),
                'limit': limit,
                'offset': offset,
                'format': 'json'
            }
            
            # Build filter conditions
            filters = []
            
            if elements:
                if isinstance(elements, list):
                    elements_str = ','.join(elements)
                else:
                    elements_str = elements
                filters.append(f"element_set={elements_str}")
            
            if formula:
                filters.append(f"name={formula}")
                
            if space_group:
                filters.append(f'spacegroup="{space_group}"')
                
            if formation_energy_max is not None:
                filters.append(f"delta_e<{formation_energy_max}")
                
            if stability_max is not None:
                filters.append(f"stability<{stability_max}")
                
            if band_gap_min is not None:
                filters.append(f"band_gap>{band_gap_min}")
                
            if band_gap_max is not None:
                filters.append(f"band_gap<{band_gap_max}")
            
            if filters:
                params['filter'] = ' AND '.join(filters)
            
            # Make API request
            url = f"{self.base_url}/formationenergy"
            query_string = urlencode(params, safe='(),"')
            full_url = f"{url}?{query_string}"
            
            logger.info(f"OQMD query: {full_url}")
            
            async with self.session.get(full_url) as response:
                if response.status != 200:
                    logger.error(f"OQMD API request failed: {response.status}")
                    return []
                
                data = await response.json()
                materials = []
                
                if 'data' in data:
                    for item in data['data']:
                        try:
                            material = self._convert_to_standard_format(item)
                            materials.append(material)
                        except Exception as e:
                            logger.warning(f"Failed to convert OQMD material {item.get('entry_id', 'unknown')}: {e}")
                
                logger.info(f"Retrieved {len(materials)} materials from OQMD")
                return materials
                
        except Exception as e:
            logger.error(f"OQMD search failed: {e}")
            return []
    
    def _convert_to_standard_format(self, oqmd_data: Dict[str, Any]) -> StandardizedMaterial:
        """Convert OQMD data to StandardizedMaterial format."""
        
        # Extract basic information
        entry_id = str(oqmd_data.get('entry_id', 'unknown'))
        name = oqmd_data.get('name', 'Unknown')
        
        # Structure information
        structure = MaterialStructure(
            lattice_parameters=[],  # Not available in formation energy endpoint
            atomic_positions=[],    # Not available in formation energy endpoint
            atomic_species=self._extract_elements_from_formula(name),
            space_group=oqmd_data.get('spacegroup'),
            volume=oqmd_data.get('volume'),
            crystal_system=None  # Would need additional API call
        )
        
        # Properties
        properties = MaterialProperties(
            formation_energy=oqmd_data.get('delta_e'),
            band_gap=oqmd_data.get('band_gap'),
            energy_above_hull=oqmd_data.get('stability'),
        )
        
        # Add OQMD-specific properties
        calculated_properties = {}
        if 'prototype' in oqmd_data:
            calculated_properties['prototype'] = oqmd_data['prototype']
        if 'ntypes' in oqmd_data:
            calculated_properties['ntypes'] = oqmd_data['ntypes']
        if 'natoms' in oqmd_data:
            calculated_properties['natoms'] = oqmd_data['natoms']
            
        if calculated_properties:
            properties.calculated_properties = calculated_properties
        
        # Metadata
        metadata = MaterialMetadata(
            fetched_at=datetime.utcnow(),
            version="1.0",
            source_url=f"http://oqmd.org/materials/entry/{entry_id}",
            experimental=False  # OQMD is computational
        )
        
        return StandardizedMaterial(
            source_db="OQMD",
            source_id=entry_id,
            formula=name,
            structure=structure,
            properties=properties,
            metadata=metadata
        )
    
    def _extract_elements_from_formula(self, formula: str) -> List[str]:
        """Extract element symbols from chemical formula."""
        import re
        
        if not formula or formula == "Unknown":
            return []
        
        # Find all capital letters followed by optional lowercase letters
        elements = re.findall(r'[A-Z][a-z]?', formula)
        return list(set(elements))  # Remove duplicates
    
    async def get_material_by_id(self, material_id: str) -> Optional[StandardizedMaterial]:
        """Get specific material by OQMD entry ID."""
        try:
            params = {
                'fields': ','.join(self.default_fields),
                'filter': f'entry_id={material_id}',
                'format': 'json'
            }
            
            url = f"{self.base_url}/formationenergy"
            query_string = urlencode(params)
            full_url = f"{url}?{query_string}"
            
            async with self.session.get(full_url) as response:
                if response.status != 200:
                    return None
                
                data = await response.json()
                
                if 'data' in data and len(data['data']) > 0:
                    return self._convert_to_standard_format(data['data'][0])
                
                return None
                
        except Exception as e:
            logger.error(f"Failed to get OQMD material {material_id}: {e}")
            return None
    
    async def get_database_info(self) -> Dict[str, Any]:
        """Get information about the OQMD database."""
        return {
            "name": "OQMD",
            "full_name": "Open Quantum Materials Database",
            "description": "DFT-calculated formation energies, stability data, and properties",
            "base_url": self.base_url,
            "total_entries": "~700,000",
            "data_types": [
                "Formation energies (delta_e)",
                "Hull distances (stability)",
                "Band gaps",
                "Crystal structures",
                "Space groups",
                "Prototype structures"
            ],
            "supported_formats": ["JSON", "XML", "YAML"],
            "api_version": "1.0",
            "optimade_compatible": True
        }
    
    # Required abstract methods from base connector
    async def fetch_bulk_materials(
        self,
        limit: int = 100,
        offset: int = 0,
        filters: Optional[Dict[str, Any]] = None
    ) -> List[StandardizedMaterial]:
        """Fetch materials in bulk with optional filtering."""
        # Extract elements from filters if provided
        elements = None
        if filters and 'elements' in filters:
            elements = filters['elements']
        
        return await self.search_materials(
            elements=elements,
            max_results=limit,
            offset=offset
        )
    
    async def validate_response(self, response: Dict[str, Any]) -> bool:
        """Validate response data from OQMD."""
        if not isinstance(response, dict):
            return False
        
        # Check for required OQMD response structure
        if 'data' not in response:
            return False
            
        if not isinstance(response['data'], list):
            return False
            
        # Validate each material entry
        for item in response['data']:
            if not isinstance(item, dict):
                return False
            # Check for required fields
            if 'composition' not in item:
                return False
                
        return True
    
    async def standardize_data(self, raw_data: Dict[str, Any]) -> StandardizedMaterial:
        """Convert raw OQMD data to standardized format."""
        return self._convert_to_standard_format(raw_data)


# Example usage and testing
if __name__ == "__main__":
    async def test_oqmd():
        config = {
            'base_url': 'http://oqmd.org/oqmdapi',
            'timeout': 30.0
        }
        
        connector = OQMDConnector(config)
        
        try:
            # Connect
            success = await connector.connect()
            print(f"Connection success: {success}")
            
            if success:
                # Test search
                materials = await connector.search_materials(
                    elements="Si",
                    stability_max=0.1,  # Only stable materials
                    limit=5
                )
                
                print(f"Found {len(materials)} stable Silicon materials:")
                for material in materials:
                    print(f"  - {material.formula} (ID: {material.source_id})")
                    print(f"    Formation energy: {material.properties.formation_energy} eV/atom")
                    print(f"    Stability: {material.properties.energy_above_hull} eV/atom")
                    print(f"    Band gap: {material.properties.band_gap} eV")
                
        finally:
            await connector.disconnect()
    
    # Run test
    asyncio.run(test_oqmd())
