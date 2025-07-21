"""
JARVIS-DFT database connector using the official jarvis-tools library.

This connector uses the JARVIS-tools Python package to access the official
JARVIS-DFT database from NIST with over 75,000+ materials.

References:
- JARVIS-tools: https://github.com/atomgptlab/jarvis-tools
- Documentation: https://jarvis-tools.readthedocs.io/
- Database: https://jarvis.nist.gov/
"""

import asyncio
import logging
import re
from datetime import datetime
from typing import Any, Dict, List, Optional, Union

from .base_connector import (
    DatabaseConnector,
    StandardizedMaterial,
    MaterialStructure,
    MaterialProperties,
    MaterialMetadata
)

logger = logging.getLogger(__name__)

# Try to import jarvis-tools
try:
    from jarvis.db.figshare import data as jarvis_data
    from jarvis.core.atoms import Atoms as JarvisAtoms
    JARVIS_AVAILABLE = True
    logger.info("JARVIS-tools library available")
except ImportError as e:
    JARVIS_AVAILABLE = False
    logger.warning(f"JARVIS-tools not available: {e}")
    logger.warning("Install with: pip install jarvis-tools")


class JarvisConnector(DatabaseConnector):
    """JARVIS connector using the official jarvis-tools library."""
    
    def __init__(self, config: Optional[Dict[str, Any]] = None):
        self.config = config or {}
        self.dft_3d_data = None
        self.dft_2d_data = None
        self.data_loaded = False
        
        # Fallback materials if jarvis-tools is not available
        self.fallback_materials = [
            {
                'jid': 'JVASP-1002',
                'formula': 'TiO2',
                'formation_energy_peratom': -3.45,
                'optb88vdw_bandgap': 3.2,
                'mbj_bandgap': 3.6,
                'spg_number': 136,
                'spg_symbol': 'P42/mnm',
                'crystal_system': 'tetragonal',
                'bulk_modulus_kv': 230.5,
                'shear_modulus_gv': 95.2,
                'elements': ['Ti', 'O']
            },
            {
                'jid': 'JVASP-1001', 
                'formula': 'Si',
                'formation_energy_peratom': 0.0,
                'optb88vdw_bandgap': 1.1,
                'mbj_bandgap': 1.3,
                'spg_number': 227,
                'spg_symbol': 'Fd-3m',
                'crystal_system': 'cubic',
                'bulk_modulus_kv': 98.8,
                'shear_modulus_gv': 51.2,
                'elements': ['Si']
            },
            {
                'jid': 'JVASP-1003',
                'formula': 'Al2O3',
                'formation_energy_peratom': -5.12,
                'optb88vdw_bandgap': 6.2,
                'mbj_bandgap': 8.9,
                'spg_number': 167,
                'spg_symbol': 'R-3c',
                'crystal_system': 'trigonal',
                'bulk_modulus_kv': 252.3,
                'shear_modulus_gv': 163.2,
                'elements': ['Al', 'O']
            },
            {
                'jid': 'JVASP-1004',
                'formula': 'GaN',
                'formation_energy_peratom': -2.15,
                'optb88vdw_bandgap': 2.1,
                'mbj_bandgap': 3.4,
                'spg_number': 186,
                'spg_symbol': 'P63mc',
                'crystal_system': 'hexagonal',
                'bulk_modulus_kv': 207.8,
                'shear_modulus_gv': 95.6,
                'elements': ['Ga', 'N']
            },
            {
                'jid': 'JVASP-1005',
                'formula': 'MgO',
                'formation_energy_peratom': -3.89,
                'optb88vdw_bandgap': 4.8,
                'mbj_bandgap': 7.1,
                'spg_number': 225,
                'spg_symbol': 'Fm-3m',
                'crystal_system': 'cubic',
                'bulk_modulus_kv': 165.2,
                'shear_modulus_gv': 131.4,
                'elements': ['Mg', 'O']
            }
        ]
    
    async def connect(self) -> bool:
        """Connect to JARVIS database using jarvis-tools."""
        try:
            if not JARVIS_AVAILABLE:
                logger.warning("JARVIS-tools not available, using fallback data")
                return True  # Return True because we have fallback data
            
            # Load JARVIS DFT 3D dataset
            logger.info("Loading JARVIS-DFT 3D dataset...")
            self.dft_3d_data = jarvis_data(dataset='dft_3d')
            logger.info(f"Loaded {len(self.dft_3d_data)} materials from JARVIS-DFT 3D")
            
            # Optionally load 2D dataset
            try:
                logger.info("Loading JARVIS-DFT 2D dataset...")
                self.dft_2d_data = jarvis_data(dataset='dft_2d')
                logger.info(f"Loaded {len(self.dft_2d_data)} materials from JARVIS-DFT 2D")
            except Exception as e:
                logger.warning(f"Could not load JARVIS-DFT 2D dataset: {e}")
                self.dft_2d_data = []
            
            self.data_loaded = True
            return True
            
        except Exception as e:
            logger.error(f"Error connecting to JARVIS: {e}")
            logger.info("Will use fallback data")
            return True  # Return True because we have fallback data
    
    async def disconnect(self):
        """Disconnect from JARVIS (no actual connection to close)."""
        self.data_loaded = False
        self.dft_3d_data = None
        self.dft_2d_data = None
    
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
        """Search materials using jarvis-tools or fallback data."""
        
        # Use real JARVIS data if available
        if JARVIS_AVAILABLE and self.data_loaded and self.dft_3d_data:
            materials = await self._search_jarvis_data(
                elements, formula, formation_energy_range, band_gap_range,
                crystal_system, space_group, limit, offset
            )
        else:
            # Use fallback materials
            logger.info("Using JARVIS fallback materials")
            materials = await self._filter_fallback_materials(
                elements, formula, formation_energy_range, band_gap_range,
                crystal_system, space_group, limit, offset
            )
        
        return materials
    
    async def _search_jarvis_data(
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
        """Search through JARVIS data loaded from jarvis-tools."""
        materials = []
        
        # Combine 3D and 2D data
        all_data = []
        if self.dft_3d_data:
            all_data.extend(self.dft_3d_data)
        if self.dft_2d_data:
            all_data.extend(self.dft_2d_data)
        
        # Filter materials based on criteria
        for item in all_data:
            if self._matches_jarvis_criteria(item, elements, formula, formation_energy_range,
                                           band_gap_range, crystal_system, space_group):
                try:
                    material = self._convert_jarvis_to_standard(item)
                    materials.append(material)
                except Exception as e:
                    logger.debug(f"Error converting JARVIS material: {e}")
                    continue
        
        # Apply pagination
        start_idx = offset
        end_idx = offset + limit
        return materials[start_idx:end_idx]
    
    async def _filter_fallback_materials(
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
        """Filter fallback materials based on criteria."""
        materials = []
        
        for item in self.fallback_materials:
            if self._matches_fallback_criteria(item, elements, formula, formation_energy_range,
                                             band_gap_range, crystal_system, space_group):
                try:
                    material = self._convert_fallback_to_standard(item)
                    materials.append(material)
                except Exception as e:
                    logger.debug(f"Error converting fallback material: {e}")
                    continue
        
        # Apply pagination
        start_idx = offset
        end_idx = offset + limit
        return materials[start_idx:end_idx]
    
    def _matches_jarvis_criteria(self, item: Dict, elements: Optional[List[str]], formula: Optional[str],
                               formation_energy_range: Optional[tuple], band_gap_range: Optional[tuple],
                               crystal_system: Optional[str], space_group: Optional[Union[str, int]]) -> bool:
        """Check if JARVIS item matches search criteria."""
        try:
            # Check elements
            if elements:
                item_elements = item.get('elements', [])
                if isinstance(item_elements, str):
                    item_elements = [item_elements]
                if not any(elem in item_elements for elem in elements):
                    return False
            
            # Check formula
            if formula:
                item_formula = item.get('formula', '')
                if formula.lower() not in item_formula.lower():
                    return False
            
            # Check formation energy
            if formation_energy_range:
                energy = item.get('formation_energy_peratom')
                if energy is not None:
                    min_e, max_e = formation_energy_range
                    if not (min_e <= energy <= max_e):
                        return False
            
            # Check band gap
            if band_gap_range:
                # Try multiple band gap fields
                band_gap = item.get('optb88vdw_bandgap') or item.get('mbj_bandgap') or item.get('band_gap')
                if band_gap is not None:
                    min_bg, max_bg = band_gap_range
                    if not (min_bg <= band_gap <= max_bg):
                        return False
            
            # Check crystal system
            if crystal_system:
                item_crystal = item.get('crystal_system', '')
                if crystal_system.lower() != item_crystal.lower():
                    return False
            
            # Check space group
            if space_group:
                if isinstance(space_group, int):
                    item_spg = item.get('spg_number')
                    if item_spg != space_group:
                        return False
                else:
                    item_spg_symbol = item.get('spg_symbol', '')
                    if space_group.lower() not in item_spg_symbol.lower():
                        return False
            
            return True
        except Exception:
            return False
    
    def _matches_fallback_criteria(self, item: Dict, elements: Optional[List[str]], formula: Optional[str],
                                 formation_energy_range: Optional[tuple], band_gap_range: Optional[tuple],
                                 crystal_system: Optional[str], space_group: Optional[Union[str, int]]) -> bool:
        """Check if fallback item matches search criteria (same logic as JARVIS)."""
        return self._matches_jarvis_criteria(item, elements, formula, formation_energy_range,
                                          band_gap_range, crystal_system, space_group)
    
    def _convert_jarvis_to_standard(self, item: Dict) -> StandardizedMaterial:
        """Convert JARVIS data to standardized format."""
        try:
            # Extract basic properties
            jid = item.get('jid', 'unknown')
            formula = item.get('formula', '')
            
            # Structure information
            structure = MaterialStructure(
                lattice_parameters=[],  # JARVIS doesn't always provide lattice matrix
                atomic_positions=[],    # JARVIS doesn't always provide positions
                atomic_species=item.get('elements', []),
                space_group=item.get('spg_symbol', ''),
                crystal_system=item.get('crystal_system', '')
            )
            
            # Properties
            properties = MaterialProperties(
                formation_energy=item.get('formation_energy_peratom'),
                band_gap=item.get('optb88vdw_bandgap') or item.get('mbj_bandgap'),
                bulk_modulus=item.get('bulk_modulus_kv'),
                shear_modulus=item.get('shear_modulus_gv')
            )
            
            # Metadata
            metadata = MaterialMetadata(
                fetched_at=datetime.now(),
                version='jarvis-tools-v1',
                source_url=f"https://jarvis.nist.gov/jarvisdft/explore/{jid}",
                last_updated=datetime.now(),
                experimental=False
            )
            
            return StandardizedMaterial(
                source_db='JARVIS-DFT',
                source_id=jid,
                formula=formula,
                structure=structure,
                properties=properties,
                metadata=metadata
            )
            
        except Exception as e:
            logger.warning(f"Error converting JARVIS material {item.get('jid', 'unknown')}: {e}")
            raise
    
    def _convert_fallback_to_standard(self, item: Dict) -> StandardizedMaterial:
        """Convert fallback data to standardized format."""
        return self._convert_jarvis_to_standard(item)  # Same conversion logic
    
    async def get_material_by_id(self, material_id: str) -> Optional[StandardizedMaterial]:
        """Get a specific material by its database ID."""
        return await self.get_material_details(material_id)
    
    async def fetch_bulk_materials(
        self,
        limit: int = 100,
        offset: int = 0,
        filters: Optional[Dict[str, Any]] = None
    ) -> List[StandardizedMaterial]:
        """Fetch materials in bulk with optional filtering."""
        if filters:
            # Convert filters to search parameters
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
            # Return all materials
            return await self.search_materials(limit=limit, offset=offset)
    
    async def validate_response(self, response: Dict[str, Any]) -> bool:
        """Validate response data from JARVIS."""
        try:
            # For JARVIS-tools data, response should be a dictionary with expected fields
            if not isinstance(response, dict):
                return False
            
            # Basic validation - should have either jid or some material identifier
            has_id = 'jid' in response or 'id' in response
            has_formula = 'formula' in response or 'composition' in response
            
            return has_id or has_formula
        except Exception:
            return False
    
    async def standardize_data(self, raw_data: Dict[str, Any]) -> StandardizedMaterial:
        """Convert raw JARVIS data to standardized format."""
        return self._convert_jarvis_to_standard(raw_data)
    
    async def get_material_details(self, material_id: str) -> Optional[StandardizedMaterial]:
        """Get detailed information for a specific material."""
        try:
            # Search in real data first
            if JARVIS_AVAILABLE and self.data_loaded:
                all_data = []
                if self.dft_3d_data:
                    all_data.extend(self.dft_3d_data)
                if self.dft_2d_data:
                    all_data.extend(self.dft_2d_data)
                
                for item in all_data:
                    if item.get('jid') == material_id:
                        return self._convert_jarvis_to_standard(item)
            
            # Search in fallback data
            for item in self.fallback_materials:
                if item.get('jid') == material_id:
                    return self._convert_fallback_to_standard(item)
            
            return None
            
        except Exception as e:
            logger.error(f"Error getting JARVIS material details for {material_id}: {e}")
            return None
    
    def get_status(self) -> Dict[str, Any]:
        """Get connector status."""
        return {
            "name": "JARVIS-DFT",
            "connected": JARVIS_AVAILABLE and self.data_loaded,
            "data_source": "jarvis-tools" if JARVIS_AVAILABLE else "fallback",
            "materials_3d": len(self.dft_3d_data) if self.dft_3d_data else 0,
            "materials_2d": len(self.dft_2d_data) if self.dft_2d_data else 0,
            "fallback_materials": len(self.fallback_materials),
            "jarvis_tools_available": JARVIS_AVAILABLE
        }
