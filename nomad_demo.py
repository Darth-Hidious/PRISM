#!/usr/bin/env python3
"""
NOMAD Connector Demo Script

This script demonstrates the capabilities of the NOMAD Laboratory database connector,
including query building, material searching, and data streaming.

Usage:
    python nomad_demo.py
"""

import asyncio
import json
from datetime import datetime
from typing import List

from app.services.connectors.nomad_connector import (
    NOMADConnector, 
    NOMADQueryBuilder,
    create_nomad_query
)
from app.services.connectors.base_connector import StandardizedMaterial


class NOMADDemo:
    """Demonstration of NOMAD connector capabilities."""
    
    def __init__(self):
        """Initialize the demo with NOMAD connector."""
        self.config = {
            "base_url": "https://nomad-lab.eu/prod/v1/api/v1",
            "timeout": 30.0,
            "stream_threshold": 100,
            "max_retries": 3
        }
        self.connector = NOMADConnector(self.config)
    
    async def run_demo(self):
        """Run the complete NOMAD connector demonstration."""
        print("🚀 NOMAD Laboratory Database Connector Demo")
        print("=" * 50)
        
        try:
            # Connect to NOMAD
            await self.demo_connection()
            
            # Demonstrate query building
            await self.demo_query_builder()
            
            # Demonstrate material searches
            await self.demo_material_search()
            
            # Demonstrate specific material lookup
            await self.demo_material_by_id()
            
            # Demonstrate bulk operations
            await self.demo_bulk_operations()
            
            # Demonstrate streaming for large datasets
            await self.demo_streaming()
            
        except Exception as e:
            print(f"❌ Demo failed: {e}")
        finally:
            await self.connector.disconnect()
            print("\n✅ Demo completed!")
    
    async def demo_connection(self):
        """Demonstrate connection to NOMAD API."""
        print("\n📡 Testing NOMAD API Connection")
        print("-" * 30)
        
        connected = await self.connector.connect()
        if connected:
            print("✅ Successfully connected to NOMAD API")
            print(f"📊 Base URL: {self.config['base_url']}")
        else:
            print("❌ Failed to connect to NOMAD API")
            raise Exception("Connection failed")
    
    async def demo_query_builder(self):
        """Demonstrate NOMAD query builder capabilities."""
        print("\n🔍 NOMAD Query Builder Demo")
        print("-" * 30)
        
        # Basic element search
        print("1. Basic Element Search:")
        builder1 = NOMADQueryBuilder()
        query1 = builder1.elements(["Fe", "O"], "HAS ANY").build()
        print(f"   Query: {query1['query']}")
        
        # Complex materials search
        print("\n2. Complex Materials Search:")
        builder2 = (create_nomad_query()
                   .elements(["Fe", "O"])
                   .element_count(3, "gte")
                   .band_gap_range(1.0, 3.0)
                   .formation_energy_range(-3.0, 0.0)
                   .add_section("results")
                   .add_section("run")
                   .paginate(page_size=50))
        
        query2 = builder2.build()
        print(f"   Query parts: {len(query2['query'].split(' AND '))}")
        print(f"   Sections: {query2['required']}")
        print(f"   Page size: {query2['page_size']}")
        
        # Specific formula search
        print("\n3. Specific Formula Search:")
        builder3 = NOMADQueryBuilder()
        query3 = builder3.formula("Fe2O3").space_group("R-3c").build()
        print(f"   Query: {query3['query']}")
        
        print("✅ Query builder demo completed")
    
    async def demo_material_search(self):
        """Demonstrate material search capabilities."""
        print("\n🔬 Material Search Demo")
        print("-" * 30)
        
        try:
            # Search for iron oxides
            print("1. Searching for Iron Oxides (Fe-O compounds):")
            materials = await self.connector.search_materials(
                elements=["Fe", "O"],
                limit=10
            )
            
            print(f"   Found {len(materials)} materials")
            if materials:
                for i, material in enumerate(materials[:3], 1):
                    print(f"   {i}. {material.formula} (ID: {material.source_id})")
                    if material.properties.band_gap:
                        print(f"      Band Gap: {material.properties.band_gap:.2f} eV")
            
            # Search with query builder
            print("\n2. Advanced Search with Query Builder:")
            query_builder = (NOMADQueryBuilder()
                           .elements(["Ti", "O"])
                           .band_gap_range(2.0, 4.0)
                           .add_section("results"))
            
            materials = await self.connector.search_materials(
                query_builder=query_builder,
                limit=5
            )
            
            print(f"   Found {len(materials)} Ti-O materials with band gap 2-4 eV")
            for material in materials:
                print(f"   - {material.formula}: {material.properties.band_gap:.2f} eV")
                
        except Exception as e:
            print(f"   ⚠️ Search error: {e}")
        
        print("✅ Material search demo completed")
    
    async def demo_material_by_id(self):
        """Demonstrate specific material lookup by ID."""
        print("\n🎯 Material Lookup by ID Demo")
        print("-" * 30)
        
        try:
            # First get some material IDs from a search
            materials = await self.connector.search_materials(
                formula="TiO2",
                limit=3
            )
            
            if materials:
                material_id = materials[0].source_id
                print(f"Looking up material: {material_id}")
                
                # Get detailed material information
                detailed_material = await self.connector.get_material_by_id(material_id)
                
                if detailed_material:
                    print(f"✅ Found material: {detailed_material.formula}")
                    print(f"   Space Group: {detailed_material.structure.space_group}")
                    print(f"   Crystal System: {detailed_material.structure.crystal_system}")
                    print(f"   Volume: {detailed_material.structure.volume:.2f} Ų")
                    
                    if detailed_material.properties.band_gap:
                        print(f"   Band Gap: {detailed_material.properties.band_gap:.2f} eV")
                    if detailed_material.properties.formation_energy:
                        print(f"   Formation Energy: {detailed_material.properties.formation_energy:.2f} eV/atom")
                else:
                    print(f"❌ Material {material_id} not found")
            else:
                print("⚠️ No TiO2 materials found for ID demo")
                
        except Exception as e:
            print(f"   ⚠️ Lookup error: {e}")
        
        print("✅ Material lookup demo completed")
    
    async def demo_bulk_operations(self):
        """Demonstrate bulk material fetching."""
        print("\n📦 Bulk Operations Demo")
        print("-" * 30)
        
        try:
            print("Fetching bulk materials for perovskite-related compounds...")
            
            # Fetch materials with specific characteristics
            materials = await self.connector.fetch_bulk_materials(
                elements=["Ba", "Ti", "O"],
                min_elements=3,
                limit=20
            )
            
            print(f"✅ Fetched {len(materials)} materials")
            
            # Analyze the results
            formulas = [m.formula for m in materials]
            unique_formulas = set(formulas)
            
            print(f"   Unique formulas: {len(unique_formulas)}")
            print(f"   Common formulas: {list(unique_formulas)[:5]}")
            
            # Property statistics
            band_gaps = [m.properties.band_gap for m in materials if m.properties.band_gap]
            if band_gaps:
                avg_gap = sum(band_gaps) / len(band_gaps)
                print(f"   Average band gap: {avg_gap:.2f} eV")
                
        except Exception as e:
            print(f"   ⚠️ Bulk operation error: {e}")
        
        print("✅ Bulk operations demo completed")
    
    async def demo_streaming(self):
        """Demonstrate streaming for large datasets."""
        print("\n🌊 Streaming Demo")
        print("-" * 30)
        
        try:
            print("Testing streaming capability with large dataset query...")
            
            # Set a low streaming threshold for demonstration
            original_threshold = self.connector.config.get("stream_threshold", 100)
            self.connector.config["stream_threshold"] = 10
            
            # Search for common elements to get a large dataset
            print("Searching for materials containing oxygen (large dataset)...")
            materials = await self.connector.search_materials(
                elements=["O"],
                limit=50  # This should trigger streaming
            )
            
            print(f"✅ Streamed {len(materials)} materials")
            print("   Streaming allows processing large datasets efficiently")
            
            # Restore original threshold
            self.connector.config["stream_threshold"] = original_threshold
            
            # Show some statistics
            if materials:
                crystal_systems = [m.structure.crystal_system for m in materials if m.structure.crystal_system]
                unique_systems = set(crystal_systems)
                print(f"   Crystal systems found: {len(unique_systems)}")
                
        except Exception as e:
            print(f"   ⚠️ Streaming error: {e}")
        
        print("✅ Streaming demo completed")
    
    def print_material_summary(self, material: StandardizedMaterial):
        """Print a summary of a material."""
        print(f"📋 Material Summary: {material.formula}")
        print(f"   Source: {material.source_db} (ID: {material.source_id})")
        print(f"   Space Group: {material.structure.space_group}")
        print(f"   Crystal System: {material.structure.crystal_system}")
        
        if material.properties.band_gap:
            print(f"   Band Gap: {material.properties.band_gap:.2f} eV")
        if material.properties.formation_energy:
            print(f"   Formation Energy: {material.properties.formation_energy:.2f} eV/atom")
        
        print(f"   Elements: {', '.join(material.structure.atomic_species)}")
        print(f"   Last Updated: {material.metadata.date_added}")


def main():
    """Main function to run the NOMAD demo."""
    demo = NOMADDemo()
    
    print("Starting NOMAD Connector Demo...")
    print("This demo will showcase the NOMAD Laboratory database connector.")
    print("The demo includes query building, material searching, and streaming.")
    print("\nNote: This demo requires internet access to connect to NOMAD API.")
    
    try:
        asyncio.run(demo.run_demo())
    except KeyboardInterrupt:
        print("\n⚠️ Demo interrupted by user")
    except Exception as e:
        print(f"\n❌ Demo failed with error: {e}")


if __name__ == "__main__":
    main()
