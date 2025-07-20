#!/usr/bin/env python3
"""
Example usage of JARVIS-DFT Database Connector.

This script demonstrates how to use the JARVIS connector to search for 
materials data and retrieve specific properties.
"""

import asyncio
import json
import logging
from typing import List, Dict, Any

from app.services.connectors.jarvis_connector import create_jarvis_connector


# Set up logging
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)


async def demonstrate_jarvis_connector():
    """Demonstrate JARVIS connector functionality."""
    
    # Create connector with custom settings
    connector = create_jarvis_connector(
        timeout=30,
        requests_per_second=2.0,  # Respectful rate limiting
        burst_capacity=5
    )
    
    try:
        # Connect to JARVIS
        logger.info("Connecting to JARVIS database...")
        connected = await connector.connect()
        
        if not connected:
            logger.error("Failed to connect to JARVIS database")
            return
        
        logger.info("Successfully connected to JARVIS database")
        
        # Example 1: Search for silicon materials
        logger.info("\n=== Example 1: Search for Silicon Materials ===")
        silicon_materials = await connector.search_materials(
            formula="Si",
            limit=5,
            properties=["bulk_modulus_kv", "shear_modulus_gv"]
        )
        
        logger.info(f"Found {len(silicon_materials)} silicon materials:")
        for material in silicon_materials:
            logger.info(f"  - {material['jid']}: {material['formula']}")
            logger.info(f"    Formation energy: {material.get('formation_energy_peratom')} eV/atom")
            logger.info(f"    E-hull: {material.get('ehull')} eV/atom")
            if 'bulk_modulus_kv' in material:
                logger.info(f"    Bulk modulus: {material['bulk_modulus_kv']} GPa")
        
        # Example 2: Search for binary compounds
        logger.info("\n=== Example 2: Search for Binary Compounds ===")
        binary_compounds = await connector.search_materials(
            n_elements=2,
            limit=3
        )
        
        logger.info(f"Found {len(binary_compounds)} binary compounds:")
        for material in binary_compounds:
            logger.info(f"  - {material['jid']}: {material['formula']}")
            if material.get('structure'):
                logger.info(f"    Atoms: {material['structure']['num_atoms']}")
        
        # Example 3: Get specific material by ID
        if silicon_materials:
            logger.info("\n=== Example 3: Get Material by ID ===")
            first_material_id = silicon_materials[0]['jid']
            specific_material = await connector.get_material_by_id(first_material_id)
            
            logger.info(f"Material details for {first_material_id}:")
            logger.info(f"  Formula: {specific_material['formula']}")
            logger.info(f"  Formation energy: {specific_material.get('formation_energy_peratom')} eV/atom")
            
            if specific_material.get('elastic_constants'):
                logger.info("  Elastic properties:")
                for prop, value in specific_material['elastic_constants'].items():
                    logger.info(f"    {prop}: {value}")
        
        # Example 4: Bulk fetch with pagination
        logger.info("\n=== Example 4: Bulk Fetch with Pagination ===")
        bulk_materials = await connector.fetch_bulk_materials(
            limit=5,
            offset=0
        )
        
        logger.info(f"Bulk fetched {len(bulk_materials)} materials:")
        for i, material in enumerate(bulk_materials, 1):
            logger.info(f"  {i}. {material['jid']}: {material['formula']}")
        
        # Example 5: Get dataset information
        logger.info("\n=== Example 5: Dataset Information ===")
        datasets = await connector.get_available_datasets()
        logger.info(f"Available datasets: {', '.join(datasets)}")
        
        # Get info for DFT 3D dataset
        dft_3d_info = await connector.get_dataset_info("dft_3d")
        logger.info(f"DFT 3D dataset info:")
        logger.info(f"  Total materials: {dft_3d_info['total_materials']}")
        logger.info(f"  Source file: {dft_3d_info['file']}")
        
        # Example 6: Search with multiple criteria
        logger.info("\n=== Example 6: Advanced Search ===")
        advanced_results = await connector.search_materials(
            formula="Ga",
            n_elements=2,
            properties=["bulk_modulus_kv", "formation_energy_peratom"],
            limit=3
        )
        
        logger.info(f"Advanced search results ({len(advanced_results)} materials):")
        for material in advanced_results:
            logger.info(f"  - {material['jid']}: {material['formula']}")
            logger.info(f"    Formation energy: {material.get('formation_energy_peratom')} eV/atom")
            logger.info(f"    Bulk modulus: {material.get('bulk_modulus_kv')} GPa")
    
    except Exception as e:
        logger.error(f"Error during demonstration: {e}")
        raise
    
    finally:
        # Clean up connection
        await connector.disconnect()
        logger.info("Disconnected from JARVIS database")


async def performance_test():
    """Test connector performance with rate limiting."""
    logger.info("\n=== Performance Test ===")
    
    connector = create_jarvis_connector(
        requests_per_second=1.0,  # Strict rate limiting for test
        burst_capacity=3
    )
    
    try:
        await connector.connect()
        
        import time
        start_time = time.time()
        
        # Make multiple requests to test rate limiting
        tasks = []
        for i in range(5):
            task = connector.search_materials(limit=1)
            tasks.append(task)
        
        # Execute all tasks concurrently
        results = await asyncio.gather(*tasks, return_exceptions=True)
        
        end_time = time.time()
        duration = end_time - start_time
        
        logger.info(f"Completed 5 requests in {duration:.2f} seconds")
        logger.info(f"Rate limiting working properly: {duration >= 2.0}")  # Should take at least 2 seconds
        
        successful_results = [r for r in results if not isinstance(r, Exception)]
        logger.info(f"Successful requests: {len(successful_results)}")
        
    except Exception as e:
        logger.error(f"Performance test error: {e}")
    
    finally:
        await connector.disconnect()


async def error_handling_demo():
    """Demonstrate error handling capabilities."""
    logger.info("\n=== Error Handling Demo ===")
    
    connector = create_jarvis_connector()
    
    try:
        await connector.connect()
        
        # Test 1: Search for non-existent material ID
        try:
            await connector.get_material_by_id("NONEXISTENT-ID")
        except Exception as e:
            logger.info(f"Expected error for non-existent ID: {type(e).__name__}: {e}")
        
        # Test 2: Invalid dataset
        try:
            await connector.get_dataset_info("invalid_dataset")
        except Exception as e:
            logger.info(f"Expected error for invalid dataset: {type(e).__name__}: {e}")
        
        logger.info("Error handling working correctly")
        
    except Exception as e:
        logger.error(f"Unexpected error in error handling demo: {e}")
    
    finally:
        await connector.disconnect()


def save_results_to_file(materials: List[Dict[str, Any]], filename: str):
    """Save materials data to JSON file."""
    try:
        with open(filename, 'w') as f:
            json.dump(materials, f, indent=2, default=str)
        logger.info(f"Results saved to {filename}")
    except Exception as e:
        logger.error(f"Failed to save results: {e}")


async def main():
    """Main demonstration function."""
    logger.info("Starting JARVIS-DFT Connector Demonstration")
    logger.info("=" * 50)
    
    try:
        # Run main demonstration
        await demonstrate_jarvis_connector()
        
        # Run performance test
        await performance_test()
        
        # Run error handling demo
        await error_handling_demo()
        
        logger.info("\n" + "=" * 50)
        logger.info("JARVIS connector demonstration completed successfully!")
        
    except Exception as e:
        logger.error(f"Demonstration failed: {e}")
        raise


if __name__ == "__main__":
    # Run the demonstration
    asyncio.run(main())
