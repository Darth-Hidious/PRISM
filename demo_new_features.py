#!/usr/bin/env python3
"""
Demo script for new PRISM features: OQMD & COD connectors, data visualization, and enhanced CLI.

This script demonstrates:
1. OQMD connector with formation energy and stability filtering
2. COD connector with crystal structure and HEA searches
3. Data visualization and export capabilities
4. Enhanced CLI with interactive search modes

Usage: python demo_new_features.py
"""

import asyncio
import os
import sys
from datetime import datetime

# Add the app directory to the path
sys.path.insert(0, os.path.join(os.path.dirname(__file__), 'app'))

from app.services.connectors.oqmd_connector import OQMDConnector
from app.services.connectors.cod_connector import CODConnector
from app.services.data_viewer import MaterialsDataViewer


async def demo_oqmd_connector():
    """Demonstrate OQMD connector capabilities."""
    print("🧪 OQMD (Open Quantum Materials Database) Demo")
    print("=" * 60)
    
    config = {
        'base_url': 'http://oqmd.org/oqmdapi',
        'timeout': 30.0
    }
    
    connector = OQMDConnector(config)
    
    try:
        # Connect to OQMD
        success = await connector.connect()
        if not success:
            print("❌ Failed to connect to OQMD")
            return []
        
        print("✅ Connected to OQMD successfully")
        
        # Search for stable lithium battery materials
        print("\n🔍 Searching for stable Li-Co-O battery materials...")
        battery_materials = await connector.search_materials(
            elements=['Li', 'Co', 'O'],
            formation_energy_max=-1.0,  # Stable materials only
            stability_max=0.1,  # Very stable (close to convex hull)
            max_results=10
        )
        
        print(f"📊 Found {len(battery_materials)} stable battery materials")
        
        # Display top materials
        for i, material in enumerate(battery_materials[:5]):
            print(f"  {i+1}. {material.formula}")
            print(f"     Formation Energy: {material.properties.formation_energy:.3f} eV/atom")
            print(f"     Hull Distance: {material.properties.energy_above_hull:.3f} eV/atom")
            print(f"     Band Gap: {material.properties.band_gap:.3f} eV")
            print()
        
        # Search for wide bandgap semiconductors
        print("🔍 Searching for wide bandgap semiconductors...")
        semiconductors = await connector.search_materials(
            elements=['Ga', 'N'],
            band_gap_min=2.0,
            formation_energy_max=-0.5,
            max_results=5
        )
        
        print(f"📊 Found {len(semiconductors)} wide bandgap Ga-N semiconductors")
        
        await connector.disconnect()
        return battery_materials
        
    except Exception as e:
        print(f"❌ OQMD demo failed: {e}")
        return []


async def demo_cod_connector():
    """Demonstrate COD connector capabilities."""
    print("\n🔬 COD (Crystallography Open Database) Demo")
    print("=" * 60)
    
    config = {
        'base_url': 'https://www.crystallography.net/cod',
        'timeout': 30.0
    }
    
    connector = CODConnector(config)
    
    try:
        # Connect to COD
        success = await connector.connect()
        if not success:
            print("❌ Failed to connect to COD")
            return []
        
        print("✅ Connected to COD successfully")
        
        # Search for High Entropy Alloys
        print("\n🔍 Searching for High Entropy Alloys (HEAs)...")
        hea_materials = await connector.search_high_entropy_alloys(
            min_elements=4,
            element_set=['Nb', 'Mo', 'Ta', 'W', 'Re'],  # Refractory HEA elements
            limit=10
        )
        
        print(f"📊 Found {len(hea_materials)} High Entropy Alloy structures")
        
        # Display HEA materials
        for i, material in enumerate(hea_materials[:3]):
            print(f"  {i+1}. {material.formula}")
            print(f"     Elements: {', '.join(material.structure.atomic_species)}")
            print(f"     Crystal System: {material.structure.crystal_system or 'Unknown'}")
            print(f"     Space Group: {material.structure.space_group or 'Unknown'}")
            print()
        
        # Search for iron-based materials
        print("🔍 Searching for iron-based crystal structures...")
        iron_materials = await connector.search_materials(
            elements=['Fe'],
            max_results=5
        )
        
        print(f"📊 Found {len(iron_materials)} iron-based structures")
        
        await connector.disconnect()
        return hea_materials
        
    except Exception as e:
        print(f"❌ COD demo failed: {e}")
        return []


def demo_data_visualization(materials):
    """Demonstrate data visualization capabilities."""
    print("\n📊 Data Visualization and Export Demo")
    print("=" * 60)
    
    if not materials:
        print("⚠️  No materials data available for visualization")
        return
    
    viewer = MaterialsDataViewer()
    
    try:
        # Create DataFrame
        df = viewer.create_dataframe(materials)
        print(f"✅ Created DataFrame with {len(df)} rows and {len(df.columns)} columns")
        print(f"📋 Columns: {', '.join(df.columns.tolist())}")
        
        # Display summary
        print("\n📈 Data Summary:")
        print(f"  • Total materials: {len(materials)}")
        print(f"  • Unique formulas: {df['Formula'].nunique()}")
        print(f"  • Formation energy range: {df['Formation_Energy'].min():.3f} to {df['Formation_Energy'].max():.3f} eV/atom")
        print(f"  • Band gap range: {df['Band_Gap'].min():.3f} to {df['Band_Gap'].max():.3f} eV")
        
        # Export to CSV
        timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
        csv_file = f"/tmp/materials_demo_{timestamp}.csv"
        viewer.export_to_csv(materials, csv_file)
        print(f"📁 Exported data to CSV: {csv_file}")
        
        # Export to JSON
        json_file = f"/tmp/materials_demo_{timestamp}.json"
        viewer.export_to_json(materials, json_file)
        print(f"📁 Exported data to JSON: {json_file}")
        
        # Try to create plots (works in GUI environments)
        try:
            import matplotlib
            matplotlib.use('Agg')  # Use non-GUI backend for demo
            
            plot_file = f"/tmp/formation_energy_demo_{timestamp}.png"
            viewer.plot_formation_energy_distribution(materials, save_path=plot_file)
            print(f"📊 Formation energy plot saved: {plot_file}")
            
        except Exception as e:
            print(f"⚠️  Plotting not available in this environment: {e}")
        
        print("✅ Data visualization demo completed")
        
    except Exception as e:
        print(f"❌ Visualization demo failed: {e}")


def demo_cli_features():
    """Demonstrate CLI features."""
    print("\n💻 Enhanced CLI Demo")
    print("=" * 60)
    
    print("🔧 New CLI commands added:")
    print("  • search: Advanced material search with filtering")
    print("  • test-database: Test database connections")
    print("  • examples: Show comprehensive usage examples")
    print("  • export-from-csv: Export detailed data from CSV")
    print("  • add-custom-database: Add custom database configurations")
    
    print("\n💡 Example commands:")
    print("  python -m app.cli search --database oqmd --elements Li,O --formation-energy-max -1.5")
    print("  python -m app.cli test-database --database cod")
    print("  python -m app.cli search --interactive")
    print("  python -m app.cli examples")
    
    print("\n🎯 Key features:")
    print("  • Multi-database support (NOMAD, JARVIS, OQMD, COD)")
    print("  • Advanced filtering (formation energy, band gap, stability)")
    print("  • Interactive search mode with prompts")
    print("  • Data export (CSV, JSON) and visualization")
    print("  • High Entropy Alloy (HEA) searches")
    print("  • Rich formatted output with progress indicators")


async def main():
    """Run the complete demo."""
    print("🚀 PRISM New Features Demonstration")
    print("=" * 80)
    print("This demo showcases new database connectors, visualization, and CLI features")
    print()
    
    # Demo OQMD connector
    oqmd_materials = await demo_oqmd_connector()
    
    # Demo COD connector
    cod_materials = await demo_cod_connector()
    
    # Demo data visualization (using OQMD materials if available)
    materials_for_viz = oqmd_materials if oqmd_materials else cod_materials
    demo_data_visualization(materials_for_viz)
    
    # Demo CLI features
    demo_cli_features()
    
    print("\n🎉 Demo completed!")
    print("\nNext steps:")
    print("1. Try interactive search: python -m app.cli search --interactive")
    print("2. Test database connections: python -m app.cli test-database --database oqmd")
    print("3. View all examples: python -m app.cli examples")
    print("4. Export and visualize your search results")
    print("\n📚 For more information, see the USAGE_GUIDE.md file")


if __name__ == "__main__":
    # Run the demo
    asyncio.run(main())
