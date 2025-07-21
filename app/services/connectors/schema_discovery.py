"""
Database Schema Discovery Tool

Discovers actual available properties and fields from NOMAD and JARVIS APIs
to build robust connectors based on real data structures.
"""

import asyncio
import json
import logging
from typing import Dict, List, Set, Any, Optional
import httpx

logger = logging.getLogger(__name__)


class SchemaDiscovery:
    """Discovers database schemas and available properties."""
    
    def __init__(self):
        self.client = httpx.AsyncClient(timeout=30.0)
    
    async def discover_nomad_schema(self) -> Dict[str, Any]:
        """Discover NOMAD API schema and available properties."""
        print("🔍 Discovering NOMAD schema...")
        
        schema_info = {
            "base_url": "https://nomad-lab.eu/prod/v1/api/v1",
            "available_endpoints": [],
            "sample_entries": [],
            "property_fields": set(),
            "query_examples": []
        }
        
        try:
            # 1. Test basic connection and get a few entries
            basic_query = {
                "pagination": {"page_size": 3}
            }
            
            response = await self.client.post(
                f"{schema_info['base_url']}/entries/query",
                json=basic_query
            )
            
            if response.status_code == 200:
                data = response.json()
                schema_info["sample_entries"] = data.get("data", [])
                
                # Extract all property paths from sample entries
                for entry in schema_info["sample_entries"]:
                    self._extract_property_paths(entry, schema_info["property_fields"])
                
                print(f"✅ Found {len(schema_info['sample_entries'])} sample entries")
                print(f"📊 Discovered {len(schema_info['property_fields'])} property fields")
                
            else:
                print(f"❌ NOMAD basic query failed: {response.status_code}")
        
        except Exception as e:
            print(f"❌ Error discovering NOMAD schema: {e}")
        
        return schema_info
    
    async def discover_jarvis_schema(self) -> Dict[str, Any]:
        """Discover JARVIS API schema and available properties."""
        print("🔍 Discovering JARVIS schema...")
        
        schema_info = {
            "base_url": "https://jarvis.nist.gov",
            "available_endpoints": [],
            "sample_entries": [],
            "property_fields": set(),
            "working_endpoints": []
        }
        
        # Test multiple possible JARVIS endpoints
        test_endpoints = [
            "https://jarvis.nist.gov/api/",
            "https://jarvis.nist.gov/jarvisdft/api/",
            "https://jarvis.nist.gov/jarvis_dft/",
            "https://www.ctcms.nist.gov/~knc6/jarvisdft.html",
            "https://jarvis.nist.gov/",
            "https://figshare.com/collections/JARVIS_DFT_3D_Dataset/3723694"
        ]
        
        for endpoint in test_endpoints:
            try:
                print(f"  Testing: {endpoint}")
                response = await self.client.get(endpoint)
                
                if response.status_code == 200:
                    schema_info["working_endpoints"].append(endpoint)
                    print(f"  ✅ Working endpoint: {endpoint}")
                    
                    # Try to get JSON data
                    try:
                        data = response.json()
                        if isinstance(data, list) and len(data) > 0:
                            # Take first few entries as samples
                            schema_info["sample_entries"] = data[:3]
                            for entry in schema_info["sample_entries"]:
                                self._extract_property_paths(entry, schema_info["property_fields"])
                            break
                        elif isinstance(data, dict):
                            schema_info["sample_entries"] = [data]
                            self._extract_property_paths(data, schema_info["property_fields"])
                            break
                    except:
                        # Not JSON, might be HTML documentation
                        continue
                        
            except Exception as e:
                print(f"  ❌ Failed: {endpoint} - {e}")
                continue
        
        if schema_info["sample_entries"]:
            print(f"✅ Found {len(schema_info['sample_entries'])} sample entries")
            print(f"📊 Discovered {len(schema_info['property_fields'])} property fields")
        else:
            print("⚠️  No sample data found, will use fallback approach")
        
        return schema_info
    
    def _extract_property_paths(self, obj: Any, paths: Set[str], prefix: str = "") -> None:
        """Recursively extract all property paths from a nested object."""
        if isinstance(obj, dict):
            for key, value in obj.items():
                current_path = f"{prefix}.{key}" if prefix else key
                paths.add(current_path)
                
                if isinstance(value, (dict, list)):
                    self._extract_property_paths(value, paths, current_path)
        
        elif isinstance(obj, list) and obj:
            # For lists, explore the first item to get structure
            self._extract_property_paths(obj[0], paths, prefix)
    
    async def get_robust_property_mapping(self) -> Dict[str, Dict[str, str]]:
        """Get robust property mappings for both databases."""
        print("🔍 Discovering database schemas for robust integration...")
        
        nomad_schema = await self.discover_nomad_schema()
        jarvis_schema = await self.discover_jarvis_schema()
        
        # Create robust mappings based on discovered properties
        property_mappings = {
            "nomad": self._create_nomad_mapping(nomad_schema),
            "jarvis": self._create_jarvis_mapping(jarvis_schema)
        }
        
        return property_mappings
    
    def _create_nomad_mapping(self, schema: Dict[str, Any]) -> Dict[str, str]:
        """Create property mapping for NOMAD based on discovered schema."""
        mapping = {}
        
        # Look for common property patterns in discovered fields
        for field in schema["property_fields"]:
            field_lower = field.lower()
            
            # Map formation energy variations
            if "formation" in field_lower and "energy" in field_lower:
                mapping["formation_energy"] = field
            
            # Map band gap variations
            elif "band" in field_lower and "gap" in field_lower:
                mapping["band_gap"] = field
            
            # Map chemical formula variations
            elif "formula" in field_lower and ("chemical" in field_lower or "hill" in field_lower):
                mapping["formula"] = field
            
            # Map elements
            elif field_lower == "elements" or "elements" in field_lower:
                mapping["elements"] = field
            
            # Map space group
            elif "space" in field_lower and "group" in field_lower:
                mapping["space_group"] = field
            
            # Map crystal system
            elif "crystal" in field_lower and "system" in field_lower:
                mapping["crystal_system"] = field
        
        # Set safe defaults if not found
        mapping.setdefault("entry_id", "entry_id")
        mapping.setdefault("formula", "results.material.chemical_formula_hill")
        mapping.setdefault("elements", "results.material.elements")
        
        return mapping
    
    def _create_jarvis_mapping(self, schema: Dict[str, Any]) -> Dict[str, str]:
        """Create property mapping for JARVIS based on discovered schema."""
        mapping = {}
        
        # Look for common property patterns in discovered fields
        for field in schema["property_fields"]:
            field_lower = field.lower()
            
            # Map formation energy variations
            if "formation" in field_lower and "energy" in field_lower:
                mapping["formation_energy"] = field
            elif "form_enp" in field_lower or "ehull" in field_lower:
                mapping["formation_energy"] = field
            
            # Map band gap variations
            elif "bandgap" in field_lower or ("band" in field_lower and "gap" in field_lower):
                mapping["band_gap"] = field
            
            # Map formula variations
            elif "formula" in field_lower:
                mapping["formula"] = field
            
            # Map JARVIS ID
            elif field_lower == "jid":
                mapping["jid"] = field
            
            # Map space group
            elif "spg" in field_lower or ("space" in field_lower and "group" in field_lower):
                mapping["space_group"] = field
        
        # Set safe defaults
        mapping.setdefault("jid", "jid")
        mapping.setdefault("formula", "formula")
        
        return mapping
    
    async def save_schema_info(self, filename: str = "discovered_schemas.json"):
        """Save discovered schema information to file."""
        mappings = await self.get_robust_property_mapping()
        
        with open(filename, 'w') as f:
            json.dump(mappings, f, indent=2, default=str)
        
        print(f"💾 Schema information saved to {filename}")
        return mappings
    
    async def close(self):
        """Close the HTTP client."""
        await self.client.aclose()


async def discover_schemas():
    """Convenience function to run schema discovery."""
    discovery = SchemaDiscovery()
    try:
        mappings = await discovery.save_schema_info()
        return mappings
    finally:
        await discovery.close()


if __name__ == "__main__":
    # Run schema discovery
    asyncio.run(discover_schemas())
