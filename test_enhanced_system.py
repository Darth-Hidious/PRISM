"""
Test the enhanced NOMAD connector with database integration
"""
import asyncio
import sys
import os

# Add the app directory to the path
sys.path.insert(0, os.path.dirname(__file__))

async def test_enhanced_nomad_system():
    """Test the enhanced NOMAD system with controlled material fetching"""
    print("Testing Enhanced NOMAD System with Database Integration")
    print("=" * 60)
    
    try:
        # Test 1: Database initialization
        print("\n1. Testing database initialization...")
        from app.db.database import init_db_sync
        init_db_sync()
        print("✅ Database initialized successfully")
        
        # Test 2: Materials service
        print("\n2. Testing materials service...")
        from app.services.materials_service import MaterialsService
        materials_service = MaterialsService()
        stats = materials_service.get_statistics()
        print(f"✅ Database contains {stats['total_materials']} materials")
        
        # Test 3: Enhanced NOMAD connector configuration
        print("\n3. Testing enhanced NOMAD connector...")
        from app.services.enhanced_nomad_connector import EnhancedNOMADConnector, create_progress_printer
        from app.cli import get_nomad_config
        
        config = get_nomad_config()
        config["batch_size"] = 5  # Small batch for testing
        
        enhanced_connector = EnhancedNOMADConnector(config, auto_store=True)
        
        # Test 4: Connection
        print("\n4. Testing NOMAD API connection...")
        success = await enhanced_connector.connect()
        if not success:
            print("❌ Failed to connect to NOMAD API")
            return
        print("✅ Connected to NOMAD API")
        
        # Test 5: Small controlled fetch
        print("\n5. Testing controlled material fetch (max 10 materials)...")
        progress_callback = create_progress_printer()
        
        query_params = {
            "elements": "Li",  # Lithium - smaller dataset than Silicon
        }
        
        stats = await enhanced_connector.search_and_store_materials(
            query_params=query_params,
            max_results=10,  # Very limited for testing
            progress_callback=progress_callback
        )
        
        print(f"\n✅ Fetch complete!")
        print(f"   - Total available: {stats['total_available']}")
        print(f"   - Total fetched: {stats['total_fetched']}")
        print(f"   - Total stored: {stats['total_stored']}")
        print(f"   - Total updated: {stats['total_updated']}")
        print(f"   - Total errors: {stats['total_errors']}")
        
        # Test 6: Database statistics after fetch
        db_stats = enhanced_connector.get_database_statistics()
        print(f"\n6. Updated database statistics:")
        print(f"   - Total materials in database: {db_stats['total_materials']}")
        
        # Test 7: Local search
        print(f"\n7. Testing local database search...")
        local_materials = enhanced_connector.search_local_materials(elements=["Li"], limit=5)
        print(f"   - Found {len(local_materials)} lithium materials in local database")
        
        for i, material in enumerate(local_materials[:3]):
            print(f"     {i+1}. {material.reduced_formula} (ID: {material.material_id[:12]}...)")
        
        # Disconnect
        await enhanced_connector.disconnect()
        print("\n✅ Disconnected from NOMAD API")
        
        print("\n" + "=" * 60)
        print("🎉 All tests passed! Enhanced NOMAD system is working correctly.")
        print("\nTo fetch materials with the CLI, use:")
        print("  ./prism fetch-and-store --elements Li --max-results 50")
        print("  ./prism fetch-and-store --formula LiCoO2 --max-results 20")
        print("  ./prism fetch-and-store --stats  # Show database statistics")
        
    except Exception as e:
        print(f"\n❌ Error: {e}")
        import traceback
        traceback.print_exc()

if __name__ == "__main__":
    # Run the test
    asyncio.run(test_enhanced_nomad_system())
