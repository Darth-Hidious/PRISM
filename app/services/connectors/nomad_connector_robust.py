"""
NOMAD Laboratory database connector (Schema-Aware Version)

Built using discovered schema information from actual NOMAD API responses.
This connector uses real property paths and provides robust fallback handling.
"""

import asyncio
import json
import logging
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


class NOMADConnectorRobust(DatabaseConnector):
    """NOMAD connector built on discovered schema information."""
    
    def __init__(self, config: Optional[Dict[str, Any]] = None):
        self.base_url = "https://nomad-lab.eu/prod/v1/api/v1"
        self.client: Optional[httpx.AsyncClient] = None
        self.config = config or {}
        
        # Schema discovered from actual NOMAD responses
        self.schema_mapping = {
            "formula": "results.material.chemical_formula_reduced_fragments",
            "space_group": "results.material.symmetry.space_group_symbol", 
            "elements": "results.material.topology.n_elements",
            "band_gap": "results.properties.electronic.band_gap.value",
            "crystal_system": "results.material.symmetry.crystal_system",
            "entry_id": "entry_id"
        }
        
        # Fallback mappings in case primary fields are missing
        self.fallback_mappings = {
            "formula": [
                "results.material.chemical_formula_reduced_fragments",
                "results.material.chemical_formula_reduced", 
                "results.material.chemical_formula_hill",
                "results.material.chemical_formula"
            ],
            "elements": [
                "results.material.topology.n_elements",
                "results.material.elements",
                "results.material.topology.elements"
            ],
            "band_gap": [
                "results.properties.electronic.band_gap.value",
                "results.properties.electronic.band_gap",
                "results.properties.band_gap"
            ]
        }
        
    async def connect(self) -> bool:
        """Connect to NOMAD API with robustness testing."""
        try:
            self.client = httpx.AsyncClient(timeout=30.0)
            
            # Test with minimal query that should always work
            test_response = await self.client.post(
                f"{self.base_url}/entries/query",
                json={"pagination": {"page_size": 1}}
            )
            
            if test_response.status_code == 200:
                logger.info("Successfully connected to NOMAD API")
                return True
            else:
                logger.error(f"NOMAD connection failed: {test_response.status_code}")
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
        """Search materials with robust query building and fallback handling."""
        
        if not self.client:
            await self.connect()
        
        try:
            # Build query progressively, testing what works
            query = {}
            
            # Start with the most basic query structure
            request_body = {
                "pagination": {
                    "page_size": min(limit, 100),
                    "page_offset": offset
                }
            }
            
            # Add query filters only if we have them and they're likely to work
            if formula:
                # Try different formula field variations
                for formula_field in self.fallback_mappings["formula"]:
                    try:
                        test_query = {formula_field: formula}
                        test_body = {
                            "query": test_query,
                            "pagination": {"page_size": 1}
                        }
                        
                        test_response = await self.client.post(
                            f"{self.base_url}/entries/query",
                            json=test_body
                        )
                        
                        if test_response.status_code == 200:
                            query[formula_field] = formula
                            logger.info(f"Using formula field: {formula_field}")
                            break
                            
                    except Exception:
                        continue
            
            # Only add query if we found working filters
            if query:
                request_body["query"] = query
            
            # Make the actual request
            await asyncio.sleep(0.5)  # Rate limiting
            
            response = await self.client.post(
                f"{self.base_url}/entries/query",
                json=request_body
            )
            
            if response.status_code != 200:
                logger.warning(f"NOMAD query failed: {response.status_code}")
                return []
            
            data = response.json()
            materials = []
            
            if 'data' in data:
                for entry in data['data']:
                    try:
                        material = self._process_entry_robust(entry)
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
    
    def _process_entry_robust(self, entry: Dict[str, Any]) -> Optional[StandardizedMaterial]:
        """Process NOMAD entry with robust field extraction."""
        try:
            # Extract basic information with fallbacks
            entry_id = self._extract_field(entry, ["entry_id", "id", "_id"], "unknown")
            
            # Extract formula with fallbacks
            formula = self._extract_field(entry, self.fallback_mappings["formula"], "Unknown")
            
            # Extract elements with fallbacks
            elements = self._extract_field(entry, self.fallback_mappings["elements"], [])
            if isinstance(elements, int):
                elements = []  # n_elements gives count, not list
            elif not isinstance(elements, list):
                elements = []
            
            # Extract properties with fallbacks
            band_gap = self._extract_field(entry, self.fallback_mappings["band_gap"], None)
            if isinstance(band_gap, dict) and "value" in band_gap:
                band_gap = band_gap["value"]
            
            # Extract structural info
            space_group = self._extract_field(entry, [self.schema_mapping["space_group"]], None)
            crystal_system = self._extract_field(entry, [self.schema_mapping["crystal_system"]], None)
            
            # Create structures with safe defaults
            structure = MaterialStructure(
                lattice_parameters=[],
                atomic_positions=[],
                atomic_species=elements if isinstance(elements, list) else [],
                space_group=space_group,
                crystal_system=crystal_system
            )
            
            properties = MaterialProperties(
                formation_energy=None,  # Would need to discover this field
                band_gap=band_gap if isinstance(band_gap, (int, float)) else None
            )
            
            metadata = MaterialMetadata(
                fetched_at=datetime.now(),
                version="nomad-robust-v1",
                source_url=f"https://nomad-lab.eu/prod/v1/gui/entry/id/{entry_id}",
                experimental=False
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
            logger.error(f"Error processing NOMAD entry: {e}")
            return None
    
    def _extract_field(self, obj: Dict[str, Any], field_paths: List[str], default: Any = None) -> Any:
        """Extract field value with multiple fallback paths."""
        for path in field_paths:
            try:
                value = obj
                for part in path.split('.'):
                    if isinstance(value, dict) and part in value:
                        value = value[part]
                    else:
                        value = None
                        break
                
                if value is not None:
                    return value
                    
            except Exception:
                continue
        
        return default
    
    async def get_material_by_id(self, material_id: str) -> Optional[StandardizedMaterial]:
        """Get material by ID with robust error handling."""
        if not self.client:
            await self.connect()
        
        try:
            response = await self.client.get(f"{self.base_url}/entries/{material_id}")
            
            if response.status_code == 404:
                return None
            
            response.raise_for_status()
            data = response.json()
            
            return self._process_entry_robust(data)
            
        except Exception as e:
            logger.error(f"Error fetching material {material_id}: {e}")
            return None
    
    async def health_check(self) -> bool:
        """Health check with basic connectivity test."""
        try:
            if not self.client:
                await self.connect()
            
            response = await self.client.post(
                f"{self.base_url}/entries/query",
                json={"pagination": {"page_size": 1}}
            )
            
            return response.status_code == 200
            
        except Exception:
            return False
    
    # Required abstract methods
    async def fetch_bulk_materials(self, limit: int = 100, offset: int = 0, 
                                 filters: Optional[Dict[str, Any]] = None) -> List[StandardizedMaterial]:
        return await self.search_materials(limit=limit, offset=offset, **(filters or {}))
    
    async def validate_response(self, response: Dict[str, Any]) -> bool:
        return isinstance(response, dict) and ('data' in response or 'entry_id' in response)
    
    async def standardize_data(self, raw_data: Dict[str, Any]) -> Optional[StandardizedMaterial]:
        return self._process_entry_robust(raw_data)
    
    def get_status(self) -> Dict[str, Any]:
        return {
            "name": "NOMAD-Robust",
            "connected": self.client is not None,
            "api_url": self.base_url,
            "schema_version": "discovered-v1"
        }
