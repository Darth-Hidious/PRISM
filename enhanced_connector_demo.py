#!/usr/bin/env python3
"""
Enhanced Database Connector Framework Demonstration

This script demonstrates the enhanced abstract base class for database connectors
with comprehensive features including rate limiting, caching, metrics, and 
standardized data schemas.
"""

import asyncio
import json
import logging
from datetime import datetime
from typing import Dict, Any

# Setup logging
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

# Import the enhanced connector components
from app.services.connectors.base_connector import (
    DatabaseConnector,
    StandardizedMaterial,
    MaterialStructure, 
    MaterialProperties,
    MaterialMetadata,
    ConnectorStatus,
    ConnectorException
)
from app.services.connectors.jarvis_connector import JarvisConnector


async def demonstrate_enhanced_base_class():
    """Demonstrate the enhanced database connector framework."""
    
    print("=" * 80)
    print("Enhanced Database Connector Framework Demonstration")
    print("=" * 80)
    
    # Initialize JARVIS connector with enhanced features
    print("\n1. Initializing JARVIS Connector with Enhanced Features")
    print("-" * 50)
    
    connector = JarvisConnector(
        requests_per_second=1.0,  # Conservative rate limiting for demo
        burst_capacity=3,
        cache_ttl=1800,  # 30 minutes cache
        max_retries=2
    )
    
    print(f"✓ Connector initialized: {connector.__class__.__name__}")
    print(f"✓ Base URL: {connector.base_url}")
    print(f"✓ Rate limiting: {connector.rate_limiter.buckets['default'].capacity} burst, "
          f"{connector.rate_limiter.buckets['default'].refill_rate}/s")
    print(f"✓ Cache TTL: {connector.cache_ttl}s")
    print(f"✓ Max retries: {connector.max_retries}")
    print(f"✓ Status: {connector.status.value}")
    
    try:
        # Test connection
        print("\n2. Testing Connection and Health Check")
        print("-" * 50)
        
        # Connect to the database
        connected = await connector.connect()
        print(f"✓ Connection established: {connected}")
        print(f"✓ Status: {connector.status.value}")
        
        # Perform health check
        healthy = await connector.health_check()
        print(f"✓ Health check: {'Healthy' if healthy else 'Unhealthy'}")
        
        # Test standardized data schema
        print("\n3. Demonstrating Standardized Data Schema")
        print("-" * 50)
        
        # Search for a simple material to demonstrate data standardization
        print("Searching for 'Si' (Silicon) materials...")
        materials = await connector.search_materials({"formula": "Si"}, limit=2)
        
        if materials:
            material = materials[0]
            print(f"✓ Found {len(materials)} materials")
            print(f"✓ First material: {material.formula} (ID: {material.source_id})")
            
            # Demonstrate standardized material schema
            print("\n4. Standardized Material Data Structure")
            print("-" * 50)
            
            print(f"Source Database: {material.source_db}")
            print(f"Source ID: {material.source_id}")
            print(f"Formula: {material.formula}")
            
            # Structure information
            if material.structure:
                print(f"Crystal Structure:")
                print(f"  - Lattice vectors: {len(material.structure.lattice_parameters)}x{len(material.structure.lattice_parameters[0])}")
                print(f"  - Atomic positions: {len(material.structure.atomic_positions)} atoms")
                print(f"  - Species: {material.structure.atomic_species[:5]}...")  # First 5 species
                if material.structure.space_group:
                    print(f"  - Space group: {material.structure.space_group}")
                if material.structure.volume:
                    print(f"  - Volume: {material.structure.volume:.3f} Ų")
            
            # Properties information
            print(f"Properties:")
            props = material.properties
            if props.formation_energy is not None:
                print(f"  - Formation energy: {props.formation_energy:.3f} eV/atom")
            if props.energy_above_hull is not None:
                print(f"  - Energy above hull: {props.energy_above_hull:.3f} eV/atom")
            if props.band_gap is not None:
                print(f"  - Band gap: {props.band_gap:.3f} eV")
            if props.bulk_modulus is not None:
                print(f"  - Bulk modulus: {props.bulk_modulus:.3f} GPa")
            
            # Metadata information
            print(f"Metadata:")
            meta = material.metadata
            print(f"  - Fetched at: {meta.fetched_at.strftime('%Y-%m-%d %H:%M:%S')}")
            print(f"  - Version: {meta.version}")
            if meta.source_url:
                print(f"  - Source URL: {meta.source_url}")
            print(f"  - Experimental: {meta.experimental}")
            
            # Demonstrate JSON serialization
            print("\n5. JSON Serialization/Deserialization")
            print("-" * 50)
            
            material_dict = material.to_dict()
            json_str = json.dumps(material_dict, indent=2)
            print(f"✓ Material serialized to JSON ({len(json_str)} characters)")
            
            # Deserialize back
            restored_material = StandardizedMaterial.from_dict(material_dict)
            print(f"✓ Material deserialized: {restored_material.formula}")
            print(f"✓ Data integrity check: {restored_material.source_id == material.source_id}")
        
        # Test caching functionality
        print("\n6. Testing Caching Functionality")
        print("-" * 50)
        
        # Make the same search again to test caching
        print("Repeating the same search to test caching...")
        start_time = datetime.now()
        cached_materials = await connector.search_materials({"formula": "Si"}, limit=2)
        end_time = datetime.now()
        
        print(f"✓ Second search completed in {(end_time - start_time).total_seconds():.3f}s")
        print(f"✓ Results match: {len(cached_materials) == len(materials)}")
        
        # Test rate limiting
        print("\n7. Testing Rate Limiting")
        print("-" * 50)
        
        print("Making multiple rapid requests to test rate limiting...")
        request_times = []
        
        for i in range(3):
            start = datetime.now()
            await connector.search_materials({"formula": "C"}, limit=1)
            end = datetime.now()
            elapsed = (end - start).total_seconds()
            request_times.append(elapsed)
            print(f"  Request {i+1}: {elapsed:.3f}s")
        
        # Check if rate limiting is working (should see delays)
        avg_time = sum(request_times) / len(request_times)
        print(f"✓ Average request time: {avg_time:.3f}s")
        if avg_time > 0.5:  # Expecting some delay due to rate limiting
            print("✓ Rate limiting appears to be working")
        
        # Test metrics collection
        print("\n8. Performance Metrics")
        print("-" * 50)
        
        metrics = await connector.get_metrics()
        print(f"Connector: {metrics['connector']}")
        print(f"Status: {metrics['status']}")
        print(f"Total requests: {metrics['total_requests']}")
        print(f"Success rate: {metrics['success_rate']:.1f}%")
        print(f"Average latency: {metrics['average_latency']:.3f}s")
        print(f"Cache hit rate: {metrics['cache_hit_rate']:.1f}%")
        print(f"Cache size: {metrics['cache_size']} entries")
        
        if metrics['error_breakdown']:
            print(f"Error breakdown: {metrics['error_breakdown']}")
        
        # Test cache management
        print("\n9. Cache Management")
        print("-" * 50)
        
        cache_size_before = len(connector._cache)
        print(f"Cache entries before cleanup: {cache_size_before}")
        
        expired_count = await connector.cleanup_expired_cache()
        print(f"Expired entries removed: {expired_count}")
        
        cache_size_after = len(connector._cache)
        print(f"Cache entries after cleanup: {cache_size_after}")
        
        # Clear all cache
        await connector.clear_cache()
        print(f"Cache cleared, entries now: {len(connector._cache)}")
        
        print("\n10. Exception Handling")
        print("-" * 50)
        
        try:
            # Try to get a non-existent material
            await connector.get_material_by_id("invalid_id_12345")
        except ConnectorException as e:
            print(f"✓ Exception handling working: {e.__class__.__name__}")
            print(f"  Message: {str(e)}")
        
    except Exception as e:
        logger.error(f"Demonstration error: {e}")
        print(f"✗ Error occurred: {e}")
    
    finally:
        # Clean up
        print("\n11. Cleanup")
        print("-" * 50)
        
        await connector.disconnect()
        print(f"✓ Connector disconnected")
        print(f"✓ Final status: {connector.status.value}")
        
        final_metrics = await connector.get_metrics()
        print(f"✓ Final metrics: {final_metrics['total_requests']} total requests, "
              f"{final_metrics['success_rate']:.1f}% success rate")


async def demonstrate_data_standardization():
    """Demonstrate creating standardized materials manually."""
    
    print("\n" + "=" * 80)
    print("Data Standardization Schema Demonstration")
    print("=" * 80)
    
    # Create a standardized material manually
    print("\n1. Creating Standardized Material Data Structures")
    print("-" * 50)
    
    # Create structure
    structure = MaterialStructure(
        lattice_parameters=[
            [5.4, 0.0, 0.0],
            [0.0, 5.4, 0.0], 
            [0.0, 0.0, 5.4]
        ],
        atomic_positions=[
            [0.0, 0.0, 0.0],
            [0.25, 0.25, 0.25]
        ],
        atomic_species=["Si", "Si"],
        space_group="Fd-3m",
        crystal_system="cubic",
        volume=157.464
    )
    
    # Create properties
    properties = MaterialProperties(
        formation_energy=-5.425,
        energy_above_hull=0.0,
        band_gap=1.14,
        bulk_modulus=97.8,
        shear_modulus=79.9
    )
    
    # Create metadata
    metadata = MaterialMetadata(
        fetched_at=datetime.now(),
        version="1.0",
        source_url="https://example.com/material/si",
        experimental=False,
        confidence_score=0.95
    )
    
    # Create standardized material
    material = StandardizedMaterial(
        source_db="demo_db",
        source_id="demo_si_001",
        formula="Si2",
        structure=structure,
        properties=properties,
        metadata=metadata
    )
    
    print(f"✓ Created standardized material: {material.formula}")
    print(f"✓ Source: {material.source_db} (ID: {material.source_id})")
    print(f"✓ Structure: {len(material.structure.atomic_positions)} atoms")
    print(f"✓ Properties: {len([p for p in material.properties.to_dict().values() if p is not None])} defined")
    
    # Test serialization
    print("\n2. Testing Serialization/Deserialization")
    print("-" * 50)
    
    # Convert to dict and JSON
    material_dict = material.to_dict()
    json_string = json.dumps(material_dict, indent=2)
    
    print(f"✓ Serialized to JSON ({len(json_string)} characters)")
    
    # Deserialize
    restored_material = StandardizedMaterial.from_dict(material_dict)
    
    print(f"✓ Deserialized material: {restored_material.formula}")
    print(f"✓ Structure preserved: {len(restored_material.structure.atomic_positions)} atoms")
    print(f"✓ Properties preserved: {restored_material.properties.formation_energy} eV/atom")
    print(f"✓ Metadata preserved: {restored_material.metadata.confidence_score}")
    
    # Verify data integrity
    integrity_checks = [
        material.source_id == restored_material.source_id,
        material.formula == restored_material.formula,
        material.structure.space_group == restored_material.structure.space_group,
        material.properties.formation_energy == restored_material.properties.formation_energy,
        material.metadata.version == restored_material.metadata.version
    ]
    
    print(f"✓ Data integrity: {all(integrity_checks)} ({sum(integrity_checks)}/{len(integrity_checks)} checks passed)")


async def main():
    """Main demonstration function."""
    
    print("Enhanced Database Connector Framework")
    print("=====================================")
    print("This demonstration shows the comprehensive features of the enhanced")
    print("abstract base class for materials database connectors.")
    print()
    
    try:
        # Demonstrate the enhanced connector framework
        await demonstrate_enhanced_base_class()
        
        # Demonstrate data standardization
        await demonstrate_data_standardization()
        
        print("\n" + "=" * 80)
        print("✅ DEMONSTRATION COMPLETED SUCCESSFULLY")
        print("=" * 80)
        print()
        print("Key Features Demonstrated:")
        print("• Abstract base class with comprehensive functionality")
        print("• Rate limiting with token bucket algorithm")
        print("• Response caching with TTL")
        print("• Performance metrics collection")
        print("• Standardized data schema for materials")
        print("• Error handling and recovery")
        print("• JSON serialization/deserialization")
        print("• Health monitoring and connection management")
        print()
        
    except Exception as e:
        logger.error(f"Demonstration failed: {e}")
        print(f"\n❌ Demonstration failed: {e}")


if __name__ == "__main__":
    asyncio.run(main())
