"""
Interactive Data Viewer for PRISM Materials Database.
Provides table views, plotting, and export capabilities for materials data.
"""

import pandas as pd
import matplotlib.pyplot as plt
import seaborn as sns
from typing import Dict, List, Any, Optional, Union
import json
import os
from datetime import datetime
import numpy as np

from app.services.connectors.base_connector import StandardizedMaterial
from app.services.materials_service import MaterialsService
from app.db.models import MaterialEntry


class MaterialsDataViewer:
    """
    Interactive viewer for materials data with visualization and export capabilities.
    """
    
    def __init__(self, materials_service: Optional[MaterialsService] = None):
        self.materials_service = materials_service or MaterialsService()
        
        # Configure plotting style
        plt.style.use('default')
        sns.set_palette("husl")
        
    def create_dataframe(self, materials: List[Union[StandardizedMaterial, MaterialEntry]]) -> pd.DataFrame:
        """Convert materials list to pandas DataFrame for easy viewing and analysis."""
        data = []
        
        for material in materials:
            if isinstance(material, StandardizedMaterial):
                row = {
                    'ID': material.source_id,
                    'Database': material.source_db,
                    'Formula': material.formula,
                    'Formation_Energy': material.properties.formation_energy if material.properties else None,
                    'Band_Gap': material.properties.band_gap if material.properties else None,
                    'Space_Group': material.structure.space_group if material.structure else None,
                    'Volume': material.structure.volume if material.structure else None,
                    'Elements': ','.join(material.structure.atomic_species) if material.structure and material.structure.atomic_species else '',
                    'Num_Elements': len(material.structure.atomic_species) if material.structure and material.structure.atomic_species else 0,
                    'Fetched_At': material.metadata.fetched_at if material.metadata else None
                }
            else:  # MaterialEntry from database
                row = {
                    'ID': material.source_id,
                    'Database': material.origin,
                    'Formula': material.reduced_formula,
                    'Formation_Energy': material.formation_energy_per_atom,
                    'Band_Gap': material.bandgap,
                    'Space_Group': material.space_group,
                    'Volume': material.volume,
                    'Elements': ','.join(material.elements) if material.elements else '',
                    'Num_Elements': len(material.elements) if material.elements else 0,
                    'Fetched_At': material.fetched_at
                }
            
            data.append(row)
        
        df = pd.DataFrame(data)
        
        # Convert numeric columns
        numeric_cols = ['Formation_Energy', 'Band_Gap', 'Volume', 'Num_Elements']
        for col in numeric_cols:
            if col in df.columns:
                df[col] = pd.to_numeric(df[col], errors='coerce')
        
        return df
    
    def display_summary_table(self, materials: List[Union[StandardizedMaterial, MaterialEntry]], 
                            max_rows: int = 20) -> None:
        """Display a formatted summary table of materials."""
        df = self.create_dataframe(materials)
        
        print(f"\\n📊 Materials Summary ({len(df)} materials)")
        print("=" * 80)
        
        # Basic statistics
        if not df.empty:
            print(f"Databases: {', '.join(df['Database'].unique())}")
            print(f"Elements represented: {len(set(','.join(df['Elements'].fillna('')).split(','))) - 1}")  # -1 for empty string
            
            if 'Formation_Energy' in df.columns:
                fe_stats = df['Formation_Energy'].describe()
                print(f"Formation Energy range: {fe_stats['min']:.3f} to {fe_stats['max']:.3f} eV/atom")
            
            if 'Band_Gap' in df.columns:
                bg_nonzero = df[df['Band_Gap'] > 0]['Band_Gap']
                if not bg_nonzero.empty:
                    print(f"Band Gap range: {bg_nonzero.min():.3f} to {bg_nonzero.max():.3f} eV")
            
            print()
        
        # Display table
        display_df = df.head(max_rows)
        
        # Format for better display
        pd.set_option('display.max_columns', None)
        pd.set_option('display.width', None)
        pd.set_option('display.max_colwidth', 20)
        
        print(display_df.to_string(index=False))
        
        if len(df) > max_rows:
            print(f"\\n... and {len(df) - max_rows} more materials")
        
        print("\\n" + "=" * 80)
    
    def plot_formation_energy_distribution(self, materials: List[Union[StandardizedMaterial, MaterialEntry]], 
                                         save_path: Optional[str] = None) -> None:
        """Plot formation energy distribution."""
        df = self.create_dataframe(materials)
        
        if 'Formation_Energy' in df.columns and not df['Formation_Energy'].isna().all():
            plt.figure(figsize=(10, 6))
            
            # Filter out NaN values
            fe_data = df['Formation_Energy'].dropna()
            
            # Create histogram
            plt.hist(fe_data, bins=30, alpha=0.7, edgecolor='black')
            plt.xlabel('Formation Energy (eV/atom)')
            plt.ylabel('Number of Materials')
            plt.title(f'Formation Energy Distribution ({len(fe_data)} materials)')
            plt.grid(True, alpha=0.3)
            
            # Add statistics
            mean_fe = fe_data.mean()
            plt.axvline(mean_fe, color='red', linestyle='--', 
                       label=f'Mean: {mean_fe:.3f} eV/atom')
            plt.legend()
            
            plt.tight_layout()
            
            if save_path:
                plt.savefig(save_path, dpi=300, bbox_inches='tight')
                print(f"Plot saved to: {save_path}")
            else:
                plt.show()
        else:
            print("No formation energy data available for plotting")
    
    def plot_band_gap_vs_formation_energy(self, materials: List[Union[StandardizedMaterial, MaterialEntry]], 
                                        save_path: Optional[str] = None) -> None:
        """Plot band gap vs formation energy scatter plot."""
        df = self.create_dataframe(materials)
        
        # Filter data with both properties
        plot_data = df.dropna(subset=['Formation_Energy', 'Band_Gap'])
        
        if not plot_data.empty:
            plt.figure(figsize=(10, 6))
            
            # Color by database
            databases = plot_data['Database'].unique()
            colors = plt.cm.Set1(np.linspace(0, 1, len(databases)))
            
            for db, color in zip(databases, colors):
                db_data = plot_data[plot_data['Database'] == db]
                plt.scatter(db_data['Formation_Energy'], db_data['Band_Gap'], 
                          label=db, alpha=0.7, c=[color])
            
            plt.xlabel('Formation Energy (eV/atom)')
            plt.ylabel('Band Gap (eV)')
            plt.title(f'Band Gap vs Formation Energy ({len(plot_data)} materials)')
            plt.grid(True, alpha=0.3)
            plt.legend()
            
            plt.tight_layout()
            
            if save_path:
                plt.savefig(save_path, dpi=300, bbox_inches='tight')
                print(f"Plot saved to: {save_path}")
            else:
                plt.show()
        else:
            print("Insufficient data for band gap vs formation energy plot")
    
    def plot_element_frequency(self, materials: List[Union[StandardizedMaterial, MaterialEntry]], 
                             top_n: int = 20, save_path: Optional[str] = None) -> None:
        """Plot frequency of elements in the dataset."""
        df = self.create_dataframe(materials)
        
        # Count element occurrences
        element_counts = {}
        for elements_str in df['Elements'].fillna(''):
            if elements_str:
                for element in elements_str.split(','):
                    element = element.strip()
                    if element:
                        element_counts[element] = element_counts.get(element, 0) + 1
        
        if element_counts:
            # Sort and take top N
            sorted_elements = sorted(element_counts.items(), key=lambda x: x[1], reverse=True)[:top_n]
            elements, counts = zip(*sorted_elements)
            
            plt.figure(figsize=(12, 6))
            bars = plt.bar(elements, counts)
            plt.xlabel('Element')
            plt.ylabel('Frequency')
            plt.title(f'Element Frequency in Dataset (Top {top_n})')
            plt.xticks(rotation=45)
            
            # Add count labels on bars
            for bar, count in zip(bars, counts):
                plt.text(bar.get_x() + bar.get_width()/2, bar.get_height() + max(counts)*0.01,
                        str(count), ha='center', va='bottom')
            
            plt.tight_layout()
            
            if save_path:
                plt.savefig(save_path, dpi=300, bbox_inches='tight')
                print(f"Plot saved to: {save_path}")
            else:
                plt.show()
        else:
            print("No element data available for plotting")
    
    def export_to_csv(self, materials: List[Union[StandardizedMaterial, MaterialEntry]], 
                     filename: str) -> None:
        """Export materials data to CSV file."""
        df = self.create_dataframe(materials)
        
        # Ensure directory exists
        os.makedirs(os.path.dirname(filename) if os.path.dirname(filename) else '.', exist_ok=True)
        
        df.to_csv(filename, index=False)
        print(f"✅ Data exported to CSV: {filename}")
        print(f"   {len(df)} materials, {len(df.columns)} columns")
    
    def export_to_json(self, materials: List[Union[StandardizedMaterial, MaterialEntry]], 
                      filename: str, pretty: bool = True) -> None:
        """Export materials data to JSON file."""
        df = self.create_dataframe(materials)
        
        # Convert DataFrame to JSON
        data = {
            'metadata': {
                'exported_at': datetime.utcnow().isoformat(),
                'total_materials': len(df),
                'databases': df['Database'].unique().tolist() if not df.empty else [],
                'exported_by': 'PRISM Materials Database Viewer'
            },
            'materials': df.to_dict('records')
        }
        
        # Ensure directory exists
        os.makedirs(os.path.dirname(filename) if os.path.dirname(filename) else '.', exist_ok=True)
        
        with open(filename, 'w') as f:
            if pretty:
                json.dump(data, f, indent=2, default=str)
            else:
                json.dump(data, f, default=str)
        
        print(f"✅ Data exported to JSON: {filename}")
        print(f"   {len(df)} materials with metadata")
    
    def filter_materials(self, materials: List[Union[StandardizedMaterial, MaterialEntry]], 
                        **filters) -> List[Union[StandardizedMaterial, MaterialEntry]]:
        """Filter materials based on criteria."""
        df = self.create_dataframe(materials)
        
        # Apply filters
        mask = pd.Series([True] * len(df))
        
        if 'database' in filters:
            mask &= df['Database'].isin(filters['database'] if isinstance(filters['database'], list) else [filters['database']])
        
        if 'min_formation_energy' in filters:
            mask &= df['Formation_Energy'] >= filters['min_formation_energy']
        
        if 'max_formation_energy' in filters:
            mask &= df['Formation_Energy'] <= filters['max_formation_energy']
        
        if 'min_band_gap' in filters:
            mask &= df['Band_Gap'] >= filters['min_band_gap']
        
        if 'max_band_gap' in filters:
            mask &= df['Band_Gap'] <= filters['max_band_gap']
        
        if 'elements' in filters:
            element_filter = filters['elements']
            if isinstance(element_filter, str):
                element_filter = [element_filter]
            
            element_mask = pd.Series([False] * len(df))
            for element in element_filter:
                element_mask |= df['Elements'].str.contains(element, na=False)
            mask &= element_mask
        
        if 'min_elements' in filters:
            mask &= df['Num_Elements'] >= filters['min_elements']
        
        if 'max_elements' in filters:
            mask &= df['Num_Elements'] <= filters['max_elements']
        
        # Return filtered materials
        filtered_indices = df[mask].index.tolist()
        return [materials[i] for i in filtered_indices]
    
    def generate_report(self, materials: List[Union[StandardizedMaterial, MaterialEntry]], 
                       output_dir: str = "materials_report") -> None:
        """Generate a comprehensive report with plots and data export."""
        os.makedirs(output_dir, exist_ok=True)
        
        print(f"🔍 Generating comprehensive materials report...")
        print(f"📁 Output directory: {output_dir}")
        
        # Create dataframe
        df = self.create_dataframe(materials)
        
        # Export data
        csv_path = os.path.join(output_dir, "materials_data.csv")
        json_path = os.path.join(output_dir, "materials_data.json")
        
        self.export_to_csv(materials, csv_path)
        self.export_to_json(materials, json_path)
        
        # Generate plots
        plots_dir = os.path.join(output_dir, "plots")
        os.makedirs(plots_dir, exist_ok=True)
        
        try:
            self.plot_formation_energy_distribution(
                materials, 
                os.path.join(plots_dir, "formation_energy_distribution.png")
            )
        except Exception as e:
            print(f"⚠️  Could not generate formation energy plot: {e}")
        
        try:
            self.plot_band_gap_vs_formation_energy(
                materials, 
                os.path.join(plots_dir, "bandgap_vs_formation_energy.png")
            )
        except Exception as e:
            print(f"⚠️  Could not generate band gap vs formation energy plot: {e}")
        
        try:
            self.plot_element_frequency(
                materials, 
                save_path=os.path.join(plots_dir, "element_frequency.png")
            )
        except Exception as e:
            print(f"⚠️  Could not generate element frequency plot: {e}")
        
        # Generate summary report
        report_path = os.path.join(output_dir, "summary_report.txt")
        with open(report_path, 'w') as f:
            f.write(f"PRISM Materials Database Report\\n")
            f.write(f"Generated: {datetime.utcnow().isoformat()}\\n")
            f.write(f"{'='*50}\\n\\n")
            
            f.write(f"Dataset Summary:\\n")
            f.write(f"- Total materials: {len(df)}\\n")
            
            if not df.empty:
                f.write(f"- Databases: {', '.join(df['Database'].unique())}\\n")
                f.write(f"- Unique formulas: {df['Formula'].nunique()}\\n")
                
                if 'Formation_Energy' in df.columns:
                    fe_stats = df['Formation_Energy'].describe()
                    f.write(f"- Formation energy range: {fe_stats['min']:.3f} to {fe_stats['max']:.3f} eV/atom\\n")
                
                if 'Band_Gap' in df.columns:
                    bg_nonzero = df[df['Band_Gap'] > 0]['Band_Gap']
                    if not bg_nonzero.empty:
                        f.write(f"- Band gap range: {bg_nonzero.min():.3f} to {bg_nonzero.max():.3f} eV\\n")
                
                # Element statistics
                element_counts = {}
                for elements_str in df['Elements'].fillna(''):
                    if elements_str:
                        for element in elements_str.split(','):
                            element = element.strip()
                            if element:
                                element_counts[element] = element_counts.get(element, 0) + 1
                
                if element_counts:
                    f.write(f"- Total unique elements: {len(element_counts)}\\n")
                    top_elements = sorted(element_counts.items(), key=lambda x: x[1], reverse=True)[:10]
                    f.write(f"- Most common elements: {', '.join([f'{e}({c})' for e, c in top_elements])}\\n")
            
            f.write(f"\\nFiles generated:\\n")
            f.write(f"- materials_data.csv: Tabular data export\\n")
            f.write(f"- materials_data.json: JSON data export with metadata\\n")
            f.write(f"- plots/: Visualization plots\\n")
        
        print(f"✅ Report generated successfully!")
        print(f"📋 Summary: {report_path}")
        print(f"📊 Data: {csv_path}")
        print(f"📈 Plots: {plots_dir}")


# Interactive CLI functions
def interactive_search_and_view():
    """Interactive CLI for searching and viewing materials data."""
    viewer = MaterialsDataViewer()
    
    print("🔬 PRISM Interactive Materials Viewer")
    print("=" * 40)
    
    # Database selection
    databases = ['NOMAD', 'JARVIS', 'OQMD', 'COD']
    print("Available databases:")
    for i, db in enumerate(databases, 1):
        print(f"  {i}. {db}")
    
    try:
        db_choice = input("\\nSelect database (1-4) or press Enter for all: ").strip()
        if db_choice:
            selected_db = databases[int(db_choice) - 1]
            print(f"Selected: {selected_db}")
        else:
            selected_db = None
            print("Selected: All databases")
        
        # Search parameters
        elements = input("\\nEnter elements (comma-separated, e.g., Si,O) or press Enter to skip: ").strip()
        if not elements:
            elements = None
        
        max_results = input("Maximum results (default 20): ").strip()
        max_results = int(max_results) if max_results else 20
        
        print(f"\\n🔍 Searching for materials...")
        
        # Here you would integrate with your actual search functionality
        # For now, this is a placeholder
        print("⚠️  Search functionality requires integration with active connectors")
        print("💡 Use the CLI commands: ./prism search --help")
        
    except (ValueError, IndexError) as e:
        print(f"❌ Invalid input: {e}")
        return
    except KeyboardInterrupt:
        print("\\n👋 Search cancelled by user")
        return


if __name__ == "__main__":
    # Example usage
    interactive_search_and_view()
