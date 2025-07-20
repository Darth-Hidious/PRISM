"""
Test the fixed NOMAD connector with simple material searches
"""
import asyncio
import sys
import os

# Add the app directory to the path
sys.path.insert(0, os.path.join(os.path.dirname(__file__), 'app'))

from app.services.connectors.nomad_connector import NOMADConnector

async def test_nomad_connector():
    """Test the NOMAD connector with silicon search"""
    print("Testing fixed NOMAD connector...")
    
    # Create connector config
    config = {
        "base_url": "https://nomad-lab.eu/prod/rae/api/v1",
        "timeout": 30.0,
        "max_retries": 3,
        "requests_per_second": 2.0
    }
    
    # Initialize connector
    connector = NOMADConnector(config)
    
    try:
        # Connect
        print("Connecting to NOMAD API...")
        success = await connector.connect()
        if not success:
            print("❌ Failed to connect to NOMAD API")
            return
        
        print("✅ Connected to NOMAD API")
        
        # Test health check
        print("Testing health check...")
        health = await connector.health_check()
        print(f"Health check: {health}")
        
        # Test simple element search
        print("\nSearching for Silicon (Si) materials...")
        materials = await connector.search_materials(elements="Si", limit=5)
        
        print(f"✅ Found {len(materials)} materials")
        
        # Print details of first few materials
        for i, material in enumerate(materials[:3]):
            print(f"\nMaterial {i+1}:")
            print(f"  ID: {material.id}")
            print(f"  Formula: {material.chemical_formula}")
            print(f"  Elements: {material.structure.elements if material.structure else 'N/A'}")
            print(f"  Database: {material.metadata.database}")
        
        # Test chemical formula search
        print(f"\nSearching for SiO2 materials...")
        sio2_materials = await connector.search_materials(formula="SiO2", limit=3)
        print(f"✅ Found {len(sio2_materials)} SiO2 materials")
        
        for i, material in enumerate(sio2_materials):
            print(f"  {i+1}. {material.chemical_formula} (ID: {material.id})")
        
    except Exception as e:
        print(f"❌ Error: {e}")
        import traceback
        traceback.print_exc()
    
    finally:
        # Disconnect
        await connector.disconnect()
        print("\n✅ Disconnected from NOMAD API")

if __name__ == "__main__":
    asyncio.run(test_nomad_connector())
