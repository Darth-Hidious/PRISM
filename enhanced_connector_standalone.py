#!/usr/bin/env python3
"""
Standalone Enhanced Database Connector Framework Demonstration

This script demonstrates the enhanced abstract base class for database connectors
without dependencies on the main application configuration.
"""

import asyncio
import json
import logging
import sys
import os
from datetime import datetime
from typing import Dict, Any

# Add the project root to Python path
sys.path.insert(0, '/Users/siddharthayashkovid/PRISM')

# Setup logging
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

# Import the enhanced connector components directly
from app.services.connectors.base_connector import (
    DatabaseConnector,
    StandardizedMaterial,
    MaterialStructure, 
    MaterialProperties,
    MaterialMetadata,
    ConnectorStatus,
    ConnectorException,
    ConnectorTimeoutException,
    ConnectorNotFoundException
)

# Import rate limiter directly
from app.services.connectors.rate_limiter import RateLimiter, TokenBucket

# Import JARVIS connector implementation
import httpx
from tenacity import retry, stop_after_attempt, wait_exponential, retry_if_exception_type, RetryError


class StandaloneJarvisConnector(DatabaseConnector):
    """
    Standalone JARVIS connector for demonstration purposes.
    This version doesn't depend on app configuration.
    """
    
    def __init__(self, **kwargs):
        """Initialize standalone JARVIS connector."""
        base_url = "https://jarvis.nist.gov"
        super().__init__(base_url=base_url, **kwargs)
        self.datasets = ["dft_3d", "dft_2d"]
        self._client = None
    
    async def connect(self) -> bool:
        """Establish connection to JARVIS API."""
        try:
            self.status = ConnectorStatus.CONNECTING
            
            self._client = httpx.AsyncClient(
                timeout=httpx.Timeout(self.timeout),
                headers={
                    "User-Agent": "PRISM-DataIngestion/1.0",
                    "Accept": "application/json"
                }
            )
            
            # Test connection with a simple request
            response = await self._client.get(f"{self.base_url}/")
            if response.status_code == 200:
                self.status = ConnectorStatus.CONNECTED
                logger.info("Successfully connected to JARVIS database")
                return True
            else:
                self.status = ConnectorStatus.ERROR
                return False
                
        except Exception as e:
            self.status = ConnectorStatus.ERROR
            self.last_error = e
            logger.error(f"Failed to connect to JARVIS: {e}")
            return False
    
    async def disconnect(self) -> None:
        """Close connection to JARVIS."""
        if self._client:
            await self._client.aclose()
            self._client = None
        self.status = ConnectorStatus.DISCONNECTED
        logger.info("Disconnected from JARVIS database")
    
    async def search_materials(
        self,
        query: Dict[str, Any],
        limit: int = 100,
        offset: int = 0
    ) -> list[StandardizedMaterial]:
        """Search for materials in JARVIS database."""
        
        # Create cache key
        cache_key = f"search_{hash(str(sorted(query.items())))}_l{limit}_o{offset}"
        
        # Check cache first
        cached_result = await self._get_cached(cache_key)
        if cached_result is not None:
            logger.info(f"Returning cached search results for {query}")
            return cached_result
        
        try:
            # Simulate API call for demonstration
            materials = []
            
            # Create some demo materials based on query
            if query.get("formula") == "Si":
                materials = await self._create_demo_silicon_materials(limit)
            elif query.get("formula") == "C":
                materials = await self._create_demo_carbon_materials(limit)
            else:
                materials = await self._create_demo_generic_materials(limit)
            
            # Cache the results
            await self._set_cached(cache_key, materials)
            
            self._update_metrics(True, 0.5)  # Simulate 0.5s latency
            return materials
            
        except Exception as e:
            self._update_metrics(False, 0.0, e)
            raise ConnectorException(f"Search failed: {e}")
    
    async def get_material_by_id(self, material_id: str) -> StandardizedMaterial:
        """Get specific material by ID."""
        cache_key = f"material_{material_id}"
        
        # Check cache
        cached_result = await self._get_cached(cache_key)
        if cached_result is not None:
            return cached_result
        
        # For demo purposes, raise not found for invalid IDs
        if material_id == "invalid_id_12345":
            raise ConnectorNotFoundException(f"Material with ID {material_id} not found")
        
        # Create a demo material
        material = await self._create_demo_material(material_id, "Si2", "silicon")
        
        # Cache the result
        await self._set_cached(cache_key, material)
        
        self._update_metrics(True, 0.3)
        return material
    
    async def fetch_bulk_materials(
        self,
        limit: int = 100,
        offset: int = 0,
        filters: Dict[str, Any] = None
    ) -> list[StandardizedMaterial]:
        """Fetch materials in bulk."""
        # For demo, delegate to search
        query = filters or {}
        return await self.search_materials(query, limit, offset)
    
    async def validate_response(self, response: Dict[str, Any]) -> bool:
        """Validate response data."""
        # Basic validation for demo
        return isinstance(response, dict) and len(response) > 0
    
    async def standardize_data(self, raw_data: Dict[str, Any]) -> StandardizedMaterial:
        """Convert raw JARVIS data to standardized format."""
        # This is a simplified implementation for demo
        return await self._create_demo_material(
            raw_data.get("jid", "demo_001"),
            raw_data.get("formula", "Unknown"),
            raw_data.get("description", "Demo material")
        )
    
    async def _create_demo_silicon_materials(self, count: int) -> list[StandardizedMaterial]:
        """Create demo silicon materials."""
        materials = []
        for i in range(min(count, 3)):  # Limit to 3 for demo
            material = await self._create_demo_material(
                f"JARVIS-{1000 + i}",
                "Si" if i == 0 else f"Si{i+1}",
                f"Silicon structure {i+1}"
            )
            materials.append(material)
        return materials
    
    async def _create_demo_carbon_materials(self, count: int) -> list[StandardizedMaterial]:
        """Create demo carbon materials."""
        materials = []
        structures = ["diamond", "graphite", "graphene"]
        for i in range(min(count, 3)):
            material = await self._create_demo_material(
                f"JARVIS-{2000 + i}",
                "C",
                f"Carbon {structures[i % len(structures)]}"
            )
            materials.append(material)
        return materials
    
    async def _create_demo_generic_materials(self, count: int) -> list[StandardizedMaterial]:
        """Create demo generic materials."""
        materials = []
        formulas = ["Al2O3", "TiO2", "Fe2O3"]
        for i in range(min(count, 3)):
            material = await self._create_demo_material(
                f"JARVIS-{3000 + i}",
                formulas[i % len(formulas)],
                f"Oxide material {i+1}"
            )
            materials.append(material)
        return materials
    
    async def _create_demo_material(
        self, 
        jid: str, 
        formula: str, 
        description: str
    ) -> StandardizedMaterial:
        """Create a standardized demo material."""
        
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
            atomic_species=["Si", "Si"] if "Si" in formula else ["C", "C"],
            space_group="Fd-3m",
            crystal_system="cubic",
            volume=157.464
        )
        
        # Create properties
        properties = MaterialProperties(
            formation_energy=-5.425 + (hash(jid) % 100) / 1000.0,
            energy_above_hull=0.0,
            band_gap=1.14 if "Si" in formula else 0.0,
            bulk_modulus=97.8,
            shear_modulus=79.9
        )
        
        # Create metadata
        metadata = MaterialMetadata(
            fetched_at=datetime.now(),
            version="1.0",
            source_url=f"https://jarvis.nist.gov/material/{jid}",
            experimental=False,
            confidence_score=0.95
        )
        
        return StandardizedMaterial(
            source_db="JARVIS-DFT",
            source_id=jid,
            formula=formula,
            structure=structure,
            properties=properties,
            metadata=metadata
        )


async def demonstrate_enhanced_base_class():
    """Demonstrate the enhanced database connector framework."""
    
    print("=" * 80)
    print("Enhanced Database Connector Framework Demonstration")
    print("=" * 80)
    
    # Initialize JARVIS connector with enhanced features
    print("\n1. Initializing JARVIS Connector with Enhanced Features")
    print("-" * 50)
    
    connector = StandaloneJarvisConnector(
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
                print(f"  - Species: {material.structure.atomic_species}")
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
    
    # Show a sample of the JSON
    print("\nJSON Sample (first 300 characters):")
    print(json_string[:300] + "..." if len(json_string) > 300 else json_string)
    
    # Deserialize
    restored_material = StandardizedMaterial.from_dict(material_dict)
    
    print(f"\n✓ Deserialized material: {restored_material.formula}")
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


async def demonstrate_rate_limiter():
    """Demonstrate the token bucket rate limiter."""
    
    print("\n" + "=" * 80)
    print("Rate Limiter Demonstration")
    print("=" * 80)
    
    print("\n1. Creating Token Bucket Rate Limiter")
    print("-" * 50)
    
    # Create a rate limiter with low limits for demonstration
    rate_limiter = RateLimiter()
    rate_limiter.add_bucket("demo", capacity=2, refill_rate=1.0)  # 2 tokens, refill 1/sec
    
    print("✓ Rate limiter created with:")
    print("  - Capacity: 2 tokens")
    print("  - Refill rate: 1 token/second")
    
    print("\n2. Testing Token Consumption")
    print("-" * 50)
    
    # Consume tokens rapidly
    for i in range(5):
        start_time = datetime.now()
        await rate_limiter.wait_for_permit("demo")
        end_time = datetime.now()
        elapsed = (end_time - start_time).total_seconds()
        
        print(f"Request {i+1}: waited {elapsed:.3f}s")
        
        if i < 2:
            print("  ✓ Should be immediate (using initial tokens)")
        else:
            print("  ✓ Should be delayed (waiting for refill)")
    
    print("\n✓ Rate limiting demonstration completed")


async def main():
    """Main demonstration function."""
    
    print("Enhanced Database Connector Framework")
    print("=====================================")
    print("This demonstration shows the comprehensive features of the enhanced")
    print("abstract base class for materials database connectors.")
    print()
    
    try:
        # Demonstrate the rate limiter first
        await demonstrate_rate_limiter()
        
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
