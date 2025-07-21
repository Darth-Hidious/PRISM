"""
JARVIS-OPTIMADE database connector using the official OPTIMADE API.

This connector uses the OPTIMADE API standard to access JARVIS-DFT data
from NIST following international protocols for materials database interoperability.

References:
- JARVIS-OPTIMADE: https://jarvis.nist.gov/optimade/jarvisdft
- OPTIMADE specification: https://www.optimade.org/
- JARVIS database: https://jarvis.nist.gov/
"""

import asyncio
import logging
import re
from datetime import datetime
from typing import Any, Dict, List, Optional, Union
import httpx

from .base_connector import (
    DatabaseConnector,
    StandardizedMaterial,
    MaterialStructure,
    MaterialProperties,
    MaterialMetadata
)

logger = logging.getLogger(__name__)


class JarvisConnector(DatabaseConnector):
    """JARVIS connector using the OPTIMADE API."""
    
    def __init__(self, config: Optional[Dict[str, Any]] = None):
        self.config = config or {}
        self.base_url = "https://jarvis.nist.gov/optimade/jarvisdft"
        self.client = None
        self.connected = False
    
    async def connect(self) -> bool:
        """Connect to JARVIS OPTIMADE API."""
        try:
            self.client = httpx.AsyncClient(timeout=30.0)
            
            # Test connection to OPTIMADE info endpoint
            response = await self.client.get(f"{self.base_url}/info")
            if response.status_code == 200:
                info = response.json()
                logger.info(f"Connected to JARVIS-OPTIMADE: {info.get('data', {}).get('description', 'JARVIS-DFT')}")
                self.connected = True
                return True
            else:
                logger.error(f"JARVIS OPTIMADE API returned status {response.status_code}")
                return False
                
        except Exception as e:
            logger.error(f"Error connecting to JARVIS OPTIMADE: {e}")
            return False
    
    async def disconnect(self):
        """Disconnect from JARVIS OPTIMADE API."""
        if self.client:
            await self.client.aclose()
            self.client = None
        self.connected = False
    
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
        """Search materials using OPTIMADE API."""
        
        if not self.connected or not self.client:
            raise ConnectionError("Not connected to JARVIS OPTIMADE API")
        
        return await self._search_optimade_api(
            elements, formula, formation_energy_range, band_gap_range,
            crystal_system, space_group, limit, offset
        )
    
    async def _search_optimade_api(
        self,
        elements: Optional[List[str]] = None,
        formula: Optional[str] = None,
        formation_energy_range: Optional[tuple] = None,
        band_gap_range: Optional[tuple] = None,
        crystal_system: Optional[str] = None,
        space_group: Optional[Union[str, int]] = None,
        limit: int = 100,
        offset: int = 0
    ) -> List[StandardizedMaterial]:
        """Search using JARVIS OPTIMADE API."""
        materials = []
        
        # Build OPTIMADE filter
        filters = []
        
        if elements:
            if len(elements) == 1:
                filters.append(f'elements HAS "{elements[0]}"')
            else:
                element_list = ','.join(elements)
                filters.append(f'elements HAS ANY {element_list}')
        
        if formula:
            filters.append(f'chemical_formula_reduced="{formula}"')
        
        # Combine filters
        filter_str = ' AND '.join(filters) if filters else None
        
        # Build query parameters
        params = {
            'page_limit': min(limit, 1000),  # OPTIMADE typically limits page size
            'page_offset': offset
        }
        
        if filter_str:
            params['filter'] = filter_str
        
        try:
            response = await self.client.get(f"{self.base_url}/structures", params=params)
            
            if response.status_code == 200:
                data = response.json()
                structures = data.get('data', [])
                
                for structure in structures:
                    try:
                        material = self._convert_optimade_to_standard(structure)
                        
                        # Apply additional filtering that OPTIMADE doesn't support
                        if self._matches_additional_criteria(material, formation_energy_range, 
                                                           band_gap_range, crystal_system, space_group):
                            materials.append(material)
                    except Exception as e:
                        logger.debug(f"Error converting OPTIMADE structure: {e}")
                        continue
            else:
                logger.error(f"OPTIMADE API request failed with status {response.status_code}")
                
        except Exception as e:
            logger.error(f"OPTIMADE API request failed: {e}")
            raise
        
        return materials
    
    def _matches_additional_criteria(self, material: StandardizedMaterial, 
                                   formation_energy_range: Optional[tuple],
                                   band_gap_range: Optional[tuple],
                                   crystal_system: Optional[str],
                                   space_group: Optional[Union[str, int]]) -> bool:
        """Apply additional filtering criteria not supported by OPTIMADE."""
        try:
            # Check formation energy
            if formation_energy_range and material.properties.formation_energy is not None:
                min_e, max_e = formation_energy_range
                if not (min_e <= material.properties.formation_energy <= max_e):
                    return False
            
            # Check band gap
            if band_gap_range and material.properties.band_gap is not None:
                min_bg, max_bg = band_gap_range
                if not (min_bg <= material.properties.band_gap <= max_bg):
                    return False
            
            # Check crystal system
            if crystal_system and material.structure.crystal_system:
                if crystal_system.lower() != material.structure.crystal_system.lower():
                    return False
            
            # Check space group
            if space_group and material.structure.space_group:
                if isinstance(space_group, int):
                    # Extract space group number from symbol
                    spg_num = self._extract_space_group_number(material.structure.space_group)
                    if spg_num != space_group:
                        return False
                else:
                    if space_group.lower() not in material.structure.space_group.lower():
                        return False
            
            return True
        except Exception:
            return False
    
    def _extract_space_group_number(self, space_group_symbol: str) -> Optional[int]:
        """Extract space group number from symbol."""
        try:
            numbers = re.findall(r'\d+', space_group_symbol)
            if numbers:
                return int(numbers[0])
        except Exception:
            pass
        return None
    
    def _convert_optimade_to_standard(self, structure: Dict) -> StandardizedMaterial:
        """Convert OPTIMADE structure to standardized format."""
        try:
            # Extract basic properties
            structure_id = structure.get('id', 'unknown')
            formula = structure.get('chemical_formula_reduced', structure.get('chemical_formula_descriptive', ''))
            elements = structure.get('elements', [])
            
            # Extract attributes (JARVIS-specific data)
            attributes = structure.get('attributes', {})
            
            # Structure information
            structure_obj = MaterialStructure(
                lattice_parameters=[],  # Would need to parse from structure data
                atomic_positions=[],    # Would need to parse from structure data  
                atomic_species=elements,
                space_group=attributes.get('spg_symbol', ''),
                crystal_system=self._get_crystal_system_from_spg(attributes.get('spg_number'))
            )
            
            # Properties
            properties = MaterialProperties(
                formation_energy=attributes.get('formation_energy_peratom'),
                band_gap=attributes.get('optb88vdw_bandgap') or attributes.get('mbj_bandgap'),
                bulk_modulus=attributes.get('bulk_modulus_kv'),
                shear_modulus=attributes.get('shear_modulus_gv')
            )
            
            # Metadata
            metadata = MaterialMetadata(
                fetched_at=datetime.now(),
                version='optimade-v1',
                source_url=attributes.get('_jarvis_reference', f"https://jarvis.nist.gov/jarvisdft/explore/{structure_id.replace('jarvisdft-', '')}"),
                last_updated=datetime.now(),
                experimental=False
            )
            
            return StandardizedMaterial(
                source_db='JARVIS-DFT',
                source_id=structure_id,
                formula=formula,
                structure=structure_obj,
                properties=properties,
                metadata=metadata
            )
            
        except Exception as e:
            logger.warning(f"Error converting OPTIMADE structure {structure.get('id', 'unknown')}: {e}")
            raise
    
    def _get_crystal_system_from_spg(self, spg_number: Optional[int]) -> str:
        """Get crystal system from space group number."""
        if not spg_number:
            return ''
        
        # Standard space group to crystal system mapping
        if 1 <= spg_number <= 2:
            return 'triclinic'
        elif 3 <= spg_number <= 15:
            return 'monoclinic'
        elif 16 <= spg_number <= 74:
            return 'orthorhombic'
        elif 75 <= spg_number <= 142:
            return 'tetragonal'
        elif 143 <= spg_number <= 167:
            return 'trigonal'
        elif 168 <= spg_number <= 194:
            return 'hexagonal'
        elif 195 <= spg_number <= 230:
            return 'cubic'
        else:
            return ''
    
    async def get_material_by_id(self, material_id: str) -> Optional[StandardizedMaterial]:
        """Get a specific material by its database ID."""
        try:
            if not self.connected or not self.client:
                raise ConnectionError("Not connected to JARVIS OPTIMADE API")
                
            # Try to get from OPTIMADE API
            response = await self.client.get(f"{self.base_url}/structures/{material_id}")
            if response.status_code == 200:
                data = response.json()
                structure_data = data.get('data')
                if structure_data:
                    return self._convert_optimade_to_standard(structure_data)
            
            return None
            
        except Exception as e:
            logger.error(f"Error getting JARVIS material details for {material_id}: {e}")
            return None
    
    async def fetch_bulk_materials(
        self,
        limit: int = 100,
        offset: int = 0,
        filters: Optional[Dict[str, Any]] = None
    ) -> List[StandardizedMaterial]:
        """Fetch materials in bulk with optional filtering."""
        if filters:
            return await self.search_materials(
                elements=filters.get('elements'),
                formula=filters.get('formula'),
                formation_energy_range=filters.get('formation_energy_range'),
                band_gap_range=filters.get('band_gap_range'),
                crystal_system=filters.get('crystal_system'),
                space_group=filters.get('space_group'),
                limit=limit,
                offset=offset
            )
        else:
            return await self.search_materials(limit=limit, offset=offset)
    
    async def validate_response(self, response: Dict[str, Any]) -> bool:
        """Validate response data from OPTIMADE."""
        try:
            if not isinstance(response, dict):
                return False
            
            # OPTIMADE response should have data field
            if 'data' not in response:
                return False
            
            # Check if it's a structure response
            data = response['data']
            if isinstance(data, list):
                # Multiple structures
                return len(data) > 0
            elif isinstance(data, dict):
                # Single structure
                return 'id' in data or 'chemical_formula_reduced' in data
            
            return False
        except Exception:
            return False
    
    async def standardize_data(self, raw_data: Dict[str, Any]) -> StandardizedMaterial:
        """Convert raw OPTIMADE data to standardized format."""
        return self._convert_optimade_to_standard(raw_data)
    
    def get_status(self) -> Dict[str, Any]:
        """Get connector status."""
        return {
            "name": "JARVIS-DFT",
            "connected": self.connected,
            "data_source": "OPTIMADE API",
            "api_url": self.base_url,
            "optimade_compatible": True
        }
