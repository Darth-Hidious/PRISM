"""
COD (Crystallography Open Database) connector.
Provides access to crystallographic data including crystal structures, 
lattice parameters, and space group information.

API Documentation: https://wiki.crystallography.net/RESTful_API/
Base URL: https://www.crystallography.net/cod/
"""

import asyncio
import aiohttp
import logging
from typing import Dict, List, Any, Optional, Union
from datetime import datetime
from urllib.parse import urlencode, quote

from .base_connector import (
    DatabaseConnector, 
    StandardizedMaterial, 
    MaterialStructure, 
    MaterialProperties, 
    MaterialMetadata
)

logger = logging.getLogger(__name__)


class CODConnector(DatabaseConnector):
    """
    Connector for COD (Crystallography Open Database).
    
    Key features:
    - Crystallographic structures
    - Lattice parameters
    - Space group information
    - Unit cell data
    - Primarily inorganic and metal-organic compounds
    """
    
    def __init__(self, config: Dict[str, Any]):
        base_url = config.get('base_url', 'https://www.crystallography.net/cod')
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
        
    async def connect(self) -> bool:
        """Establish connection to COD API."""
        try:
            timeout = aiohttp.ClientTimeout(total=self.timeout)
            self.session = aiohttp.ClientSession(timeout=timeout)
            
            # Test connection
            test_url = f"{self.base_url}/result"
            async with self.session.get(f"{test_url}?limit=1&format=json") as response:
                if response.status == 200:
                    logger.info("Successfully connected to COD database")
                    return True
                else:
                    logger.error(f"COD connection failed with status: {response.status}")
                    return False
                    
        except Exception as e:
            logger.error(f"Failed to connect to COD: {e}")
            return False
    
    async def disconnect(self) -> bool:
        """Close connection to COD API."""
        if self.session:
            await self.session.close()
            self.session = None
        return True
    
    async def health_check(self) -> bool:
        """Check if COD API is accessible."""
        try:
            if not self.session:
                return False
                
            test_url = f"{self.base_url}/result"
            async with self.session.get(f"{test_url}?limit=1&format=json") as response:
                return response.status == 200
                
        except Exception as e:
            logger.error(f"COD health check failed: {e}")
            return False
    
    async def search_materials(
        self,
        elements: Optional[Union[str, List[str]]] = None,
        formula: Optional[str] = None,
        space_group: Optional[str] = None,
        crystal_system: Optional[str] = None,
        strictmin: Optional[int] = None,
        strictmax: Optional[int] = None,
        limit: int = 50,
        offset: int = 0,
        **kwargs
    ) -> List[StandardizedMaterial]:
        """
        Search for materials in COD database.
        
        Args:
            elements: List of elements or comma-separated string
            formula: Specific chemical formula
            space_group: Crystal space group
            crystal_system: Crystal system (cubic, tetragonal, etc.)
            strictmin: Minimum number of elements (for HEAs)
            strictmax: Maximum number of elements
            limit: Maximum number of results
            offset: Offset for pagination
            
        Returns:
            List of StandardizedMaterial objects
        """
        try:
            # Build query parameters
            params = {
                'format': 'json',
                'limit': limit,
                'offset': offset
            }
            
            # Handle elements - COD uses el1, el2, etc.
            if elements:
                if isinstance(elements, str):
                    element_list = elements.replace(',', ' ').split()
                else:
                    element_list = elements
                
                for i, element in enumerate(element_list[:10]):  # COD supports up to el10
                    params[f'el{i+1}'] = element.strip()
            
            if formula:
                params['formula'] = formula
                
            if space_group:
                params['spacegroup'] = space_group
                
            if crystal_system:
                params['crystalsystem'] = crystal_system
                
            if strictmin is not None:
                params['strictmin'] = strictmin
                
            if strictmax is not None:
                params['strictmax'] = strictmax
            
            # Make API request
            url = f"{self.base_url}/result"
            query_string = urlencode(params)
            full_url = f"{url}?{query_string}"
            
            logger.info(f"COD query: {full_url}")
            
            async with self.session.get(full_url) as response:
                if response.status != 200:
                    logger.error(f"COD API request failed: {response.status}")
                    return []
                
                # COD returns JSON array directly
                data = await response.json()
                materials = []
                
                if isinstance(data, list):
                    for item in data:
                        try:
                            material = self._convert_to_standard_format(item)
                            materials.append(material)
                        except Exception as e:
                            logger.warning(f"Failed to convert COD material {item.get('cod_id', 'unknown')}: {e}")
                
                logger.info(f"Retrieved {len(materials)} materials from COD")
                return materials
                
        except Exception as e:
            logger.error(f"COD search failed: {e}")
            return []
    
    def _convert_to_standard_format(self, cod_data: Dict[str, Any]) -> StandardizedMaterial:
        """Convert COD data to StandardizedMaterial format."""
        
        # Extract basic information
        cod_id = str(cod_data.get('cod_id', 'unknown'))
        formula = cod_data.get('formula', 'Unknown')
        
        # Structure information
        lattice_params = []
        if all(key in cod_data for key in ['a', 'b', 'c', 'alpha', 'beta', 'gamma']):
            try:
                a = float(cod_data['a'])
                b = float(cod_data['b']) 
                c = float(cod_data['c'])
                alpha = float(cod_data['alpha'])
                beta = float(cod_data['beta'])
                gamma = float(cod_data['gamma'])
                
                # Convert to lattice matrix (simplified orthogonal case)
                lattice_params = [
                    [a, 0, 0],
                    [0, b, 0], 
                    [0, 0, c]
                ]
            except (ValueError, TypeError):
                lattice_params = []
        
        structure = MaterialStructure(
            lattice_parameters=lattice_params,
            atomic_positions=[],  # Would need CIF file parsing
            atomic_species=self._extract_elements_from_formula(formula),
            space_group=cod_data.get('spacegroup'),
            crystal_system=cod_data.get('crystalsystem'),
            volume=cod_data.get('volume')
        )
        
        # Properties (COD is primarily structural)
        properties = MaterialProperties()
        
        # Add COD-specific structural properties
        calculated_properties = {}
        for key in ['a', 'b', 'c', 'alpha', 'beta', 'gamma', 'volume', 'z']:
            if key in cod_data and cod_data[key]:
                calculated_properties[f'cell_{key}'] = cod_data[key]
        
        if 'density' in cod_data:
            calculated_properties['density'] = cod_data['density']
            
        if calculated_properties:
            properties.calculated_properties = calculated_properties
        
        # Metadata
        metadata = MaterialMetadata(
            fetched_at=datetime.utcnow(),
            version="2024",
            source_url=f"https://www.crystallography.net/cod/{cod_id}.html",
            experimental=True  # COD contains experimental crystal structures
        )
        
        # Add bibliographic information if available
        if 'journal' in cod_data or 'year' in cod_data:
            bib_info = {}
            for key in ['journal', 'year', 'authors', 'title']:
                if key in cod_data:
                    bib_info[key] = cod_data[key]
            if bib_info:
                metadata.confidence_score = 0.9  # High confidence for published structures
        
        return StandardizedMaterial(
            source_db="COD",
            source_id=cod_id,
            formula=formula,
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
        """Get specific material by COD ID."""
        try:
            url = f"{self.base_url}/result"
            params = {
                'cod_id': material_id,
                'format': 'json'
            }
            
            query_string = urlencode(params)
            full_url = f"{url}?{query_string}"
            
            async with self.session.get(full_url) as response:
                if response.status != 200:
                    return None
                
                data = await response.json()
                
                if isinstance(data, list) and len(data) > 0:
                    return self._convert_to_standard_format(data[0])
                
                return None
                
        except Exception as e:
            logger.error(f"Failed to get COD material {material_id}: {e}")
            return None
    
    async def search_high_entropy_alloys(
        self,
        min_elements: int = 4,
        max_elements: int = 10,
        element_set: Optional[List[str]] = None,
        limit: int = 50
    ) -> List[StandardizedMaterial]:
        """
        Search for High Entropy Alloys (HEAs) in COD.
        
        Args:
            min_elements: Minimum number of elements (typically 4+ for HEAs)
            max_elements: Maximum number of elements
            element_set: Specific elements to search for
            limit: Maximum number of results
            
        Returns:
            List of StandardizedMaterial objects representing HEAs
        """
        params = {
            'strictmin': min_elements,
            'strictmax': max_elements,
            'limit': limit,
            'format': 'json'
        }
        
        # Add specific elements if provided
        if element_set:
            for i, element in enumerate(element_set[:10]):
                params[f'el{i+1}'] = element
        
        try:
            url = f"{self.base_url}/result"
            query_string = urlencode(params)
            full_url = f"{url}?{query_string}"
            
            logger.info(f"COD HEA search: {full_url}")
            
            async with self.session.get(full_url) as response:
                if response.status != 200:
                    logger.error(f"COD HEA search failed: {response.status}")
                    return []
                
                data = await response.json()
                materials = []
                
                if isinstance(data, list):
                    for item in data:
                        try:
                            material = self._convert_to_standard_format(item)
                            # Verify it's actually a HEA
                            if len(material.structure.atomic_species) >= min_elements:
                                materials.append(material)
                        except Exception as e:
                            logger.warning(f"Failed to convert COD HEA {item.get('cod_id', 'unknown')}: {e}")
                
                logger.info(f"Found {len(materials)} HEA materials from COD")
                return materials
                
        except Exception as e:
            logger.error(f"COD HEA search failed: {e}")
            return []
    
    async def get_database_info(self) -> Dict[str, Any]:
        """Get information about the COD database."""
        return {
            "name": "COD",
            "full_name": "Crystallography Open Database",
            "description": "Open access collection of crystal structures",
            "base_url": self.base_url,
            "total_entries": "~500,000",
            "data_types": [
                "Crystal structures",
                "Lattice parameters",
                "Space groups",
                "Unit cell data",
                "Atomic positions",
                "Crystallographic data"
            ],
            "supported_formats": ["JSON", "CIF", "XML"],
            "focus": "Experimental crystal structures",
            "coverage": "Inorganic, metal-organic, and small organic molecules"
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
            max_results=limit
        )
    
    async def validate_response(self, response: Dict[str, Any]) -> bool:
        """Validate response data from COD."""
        if not isinstance(response, (list, dict)):
            return False
            
        # COD returns either a list or dict
        if isinstance(response, list):
            # Validate each entry in the list
            for item in response:
                if not isinstance(item, dict):
                    return False
                # Check for basic COD fields
                if 'cod_id' not in item and 'id' not in item:
                    return False
        elif isinstance(response, dict):
            # Single entry response
            if 'cod_id' not in response and 'id' not in response:
                return False
                
        return True
    
    async def standardize_data(self, raw_data: Dict[str, Any]) -> StandardizedMaterial:
        """Convert raw COD data to standardized format."""
        return self._convert_to_standard_format(raw_data)


# Example usage and testing
if __name__ == "__main__":
    async def test_cod():
        config = {
            'base_url': 'https://www.crystallography.net/cod',
            'timeout': 30.0
        }
        
        connector = CODConnector(config)
        
        try:
            # Connect
            success = await connector.connect()
            print(f"Connection success: {success}")
            
            if success:
                # Test search for high entropy alloys
                print("\\nSearching for High Entropy Alloys (4+ elements):")
                hea_materials = await connector.search_high_entropy_alloys(
                    min_elements=4,
                    element_set=["Nb", "Mo", "Ta", "W"],  # Common refractory HEA elements
                    limit=5
                )
                
                print(f"Found {len(hea_materials)} HEA materials:")
                for material in hea_materials:
                    print(f"  - {material.formula} (ID: {material.source_id})")
                    print(f"    Elements: {material.structure.atomic_species}")
                    print(f"    Space group: {material.structure.space_group}")
                
                # Test regular search
                print("\\nSearching for Silicon compounds:")
                si_materials = await connector.search_materials(
                    elements=["Si"],
                    limit=3
                )
                
                print(f"Found {len(si_materials)} Silicon materials:")
                for material in si_materials:
                    print(f"  - {material.formula} (ID: {material.source_id})")
                    print(f"    Space group: {material.structure.space_group}")
                
        finally:
            await connector.disconnect()
    
    # Run test
    asyncio.run(test_cod())
