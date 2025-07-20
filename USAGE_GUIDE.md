# PRISM Materials Database Platform - User Guide

## 🚀 Quick Start

PRISM is a comprehensive materials science data platform that provides unified access to multiple materials databases including NOMAD, JARVIS, OQMD, and COD. This guide shows you how to use all the features effectively.

### Installation and Setup

```bash
# Clone and setup
git clone https://github.com/your-org/PRISM.git
cd PRISM

# Install dependencies
pip install -r requirements.txt

# Initialize database
python init_database.py

# Make executable
chmod +x prism
```

### Basic Usage

```bash
# Show all available commands
./prism --help

# Show comprehensive examples
./prism examples

# Test database connections
./prism test-database --database nomad
./prism test-database --database oqmd
```

## 🔍 Searching for Materials

### Simple Searches

```bash
# Search for Silicon materials across all databases
./prism search --elements Si --limit 10

# Search for a specific formula
./prism search --formula SiO2 --database nomad

# Search by elements with export
./prism search --elements Fe,Ni,Cr --export csv --limit 50
```

### Advanced Filtering

```bash
# Semiconductors with specific band gap range
./prism search --band-gap-min 1.0 --band-gap-max 3.0 --limit 20

# Stable materials only (using OQMD stability data)
./prism search --database oqmd --stability-max 0.1 --elements Si,O

# Materials with low formation energy
./prism search --formation-energy-max -1.0 --export json

# High Entropy Alloys (4+ elements)
./prism search --database cod --min-elements 4 --max-elements 8
```

### Database-Specific Features

#### NOMAD (19M+ DFT calculations)
```bash
# DFT-calculated properties
./prism search --database nomad --elements Ti,Al --formation-energy-max -0.5

# Large-scale materials discovery
./prism search --database nomad --band-gap-min 2.0 --limit 100 --export both
```

#### JARVIS (NIST Materials)
```bash
# NIST materials with mechanical properties
./prism search --database jarvis --space-group "Fm-3m"

# 2D materials search
./prism search --database jarvis --elements C,N --max-elements 2
```

#### OQMD (700K+ materials with stability)
```bash
# Stable compounds on convex hull
./prism search --database oqmd --stability-max 0.0 --elements Al,Fe

# Formation energy optimization
./prism search --database oqmd --formation-energy-max -2.0 --limit 20
```

#### COD (Crystal structures)
```bash
# Crystal structure search
./prism search --database cod --space-group "Fm-3m" --elements Cu

# High Entropy Alloy structures
./prism search --database cod --elements Nb,Mo,Ta,W --min-elements 4
```

## 📊 Data Visualization and Export

### Export Formats

```bash
# CSV export for spreadsheet analysis
./prism search --elements Si --export csv --limit 100

# JSON export with full metadata
./prism search --database nomad --elements Li --export json

# Both formats
./prism search --elements Fe,Ni --export both --plot
```

### Visualization

```bash
# Generate comprehensive plots and reports
./prism search --elements Si,O --plot --export both --limit 200

# This creates:
# - Formation energy distribution plots
# - Band gap vs formation energy correlations
# - Element frequency analysis
# - CSV and JSON data exports
# - Summary report
```

### Advanced Data Processing

```bash
# Export detailed data from previous search results
./prism export-from-csv --input-file search_results.csv

# Custom analysis pipeline
./prism search --elements Ti,Al,V --export csv --limit 500
# Then use the CSV in your analysis tools (Excel, Python, R, etc.)
```

## 🔧 Interactive Mode

```bash
# Guided search with prompts
./prism search --interactive

# This will prompt you for:
# - Database selection
# - Elements to search
# - Number of results
# - Export options
```

## 🏗️ Bulk Operations and Production Use

### Large-Scale Data Fetching

```bash
# Bulk fetch with database storage
./prism bulk-fetch --source nomad --elements Si --limit 1000 --store-db

# Enhanced NOMAD connector with progress tracking
./prism bulk-fetch --source enhanced-nomad --elements Fe,Ni --limit 500
```

### Production Monitoring

```bash
# Check system status
./prism queue-status

# Monitor performance
./prism monitor --duration 300

# List all available sources
./prism list-sources
```

## 🔌 Adding Custom Databases

### Custom Database Configuration

Create a JSON configuration file for your custom database:

```json
{
    "name": "MyMaterialsDB",
    "base_url": "https://api.mymaterialsdb.com",
    "api_key": "your-api-key-here",
    "timeout": 30.0,
    "endpoints": {
        "search": "/api/v1/search",
        "detail": "/api/v1/material/{id}"
    },
    "field_mappings": {
        "id": "material_id",
        "formula": "chemical_formula",
        "formation_energy": "formation_energy_per_atom",
        "band_gap": "electronic_band_gap"
    },
    "supported_filters": [
        "elements",
        "formation_energy_max",
        "band_gap_min",
        "band_gap_max"
    ]
}
```

```bash
# Add the custom database
./prism add-custom-database my_custom_db.json
```

### Creating Custom Connectors

For full integration, create a new connector class in `app/services/connectors/`:

```python
# app/services/connectors/my_custom_connector.py
from .base_connector import DatabaseConnector, StandardizedMaterial

class MyCustomConnector(DatabaseConnector):
    """Custom database connector."""
    
    def __init__(self, config):
        super().__init__(config)
        self.api_key = config.get('api_key')
        # ... implement your connector
        
    async def search_materials(self, **kwargs):
        # Implement search logic
        # Return List[StandardizedMaterial]
        pass
```

## 📈 Analysis Examples

### High Entropy Alloys (HEAs) Research

```bash
# Find HEA structures in COD
./prism search --database cod --min-elements 4 --max-elements 6 \\
  --elements Nb,Mo,Ta,W,Re --export csv --limit 100

# Analyze formation energies of multi-component systems
./prism search --database oqmd --min-elements 3 \\
  --formation-energy-max -0.5 --stability-max 0.2 --plot
```

### Semiconductor Discovery

```bash
# Find materials with optimal band gaps for solar cells
./prism search --band-gap-min 1.0 --band-gap-max 2.0 \\
  --formation-energy-max -0.5 --export both --plot

# Wide band gap semiconductors
./prism search --band-gap-min 3.0 --elements Ga,Al,In,N \\
  --database nomad --limit 50
```

### 2D Materials Research

```bash
# Search JARVIS for 2D materials
./prism search --database jarvis --max-elements 2 \\
  --elements C,N,B,Mo,W --export json

# Transition metal dichalcogenides
./prism search --elements Mo,W,S,Se,Te --min-elements 2 \\
  --max-elements 3 --band-gap-min 0.5
```

### Battery Materials

```bash
# Lithium-containing compounds
./prism search --elements Li --formation-energy-max -1.0 \\
  --database oqmd --stability-max 0.1 --limit 200 --export csv

# Sodium-ion battery materials
./prism search --elements Na --band-gap-max 5.0 \\
  --formation-energy-max -0.8 --export both
```

## 🛠️ Development and Testing

### Database Testing

```bash
# Test all database connections
./prism test-database --database nomad
./prism test-database --database jarvis
./prism test-database --database oqmd
./prism test-database --database cod
```

### Unit Testing

```bash
# Run comprehensive tests
python -m pytest test_enhanced_nomad_fixed.py -v
python -m pytest test_enhanced_jarvis.py -v

# Test specific functionality
python -m pytest tests/test_connectors.py::test_nomad_search -v
```

### Performance Monitoring

```bash
# Monitor API usage and performance
./prism monitor --duration 600 --log-level INFO

# Check queue status and failed jobs
./prism queue-status
./prism retry-failed-jobs
```

## 🎯 Best Practices

### API Usage Guidelines

1. **Rate Limiting**: Use appropriate `--limit` values to avoid overwhelming APIs
2. **Progressive Searches**: Start with small limits, then scale up
3. **Database Selection**: Choose appropriate databases for your research needs
4. **Export Early**: Save results frequently to avoid losing data

### Performance Optimization

```bash
# Use specific databases for targeted searches
./prism search --database oqmd --stability-max 0.1  # Stability data
./prism search --database cod --min-elements 4      # Crystal structures
./prism search --database nomad --band-gap-min 1.0  # Electronic properties

# Batch processing for large datasets
./prism bulk-fetch --source enhanced-nomad --elements Si \\
  --limit 10000 --store-db --batch-size 100
```

### Data Management

```bash
# Organize exports by date and purpose
./prism search --elements Ti,Al --export csv \\
  > "titanium_alloys_$(date +%Y%m%d).csv"

# Create comprehensive analysis reports
./prism search --elements Fe,Ni,Cr --plot --export both \\
  --limit 1000 --output-dir "stainless_steel_analysis"
```

## 📚 Database Information

| Database | Materials | Focus | Key Properties |
|----------|-----------|-------|----------------|
| **NOMAD** | 19M+ | DFT calculations | Formation energy, band gaps, electronic properties |
| **JARVIS** | 200K+ | NIST materials | Mechanical properties, 2D materials, defects |
| **OQMD** | 700K+ | Stability data | Hull distances, formation energies, phase stability |
| **COD** | 500K+ | Crystal structures | Space groups, lattice parameters, atomic positions |

## 🆘 Troubleshooting

### Common Issues

```bash
# Database connection timeout
./prism test-database --database nomad  # Check connectivity

# Missing dependencies
pip install matplotlib seaborn pandas  # For plotting
pip install psycopg2-binary  # For PostgreSQL

# API rate limiting
./prism search --elements Si --limit 10  # Reduce limit
```

### Getting Help

```bash
# Show all available commands
./prism --help

# Get help for specific commands
./prism search --help
./prism bulk-fetch --help

# Show usage examples
./prism examples
```

### Support and Community

- **Issues**: Report bugs and request features on GitHub
- **Documentation**: Check the README.md and IMPLEMENTATION_STATUS.md
- **Examples**: Use `./prism examples` for comprehensive usage examples

## 🔬 Research Applications

PRISM has been designed for a wide range of materials science research:

- **High Entropy Alloys**: Multi-component metallic systems
- **Semiconductor Discovery**: Band gap engineering and optimization
- **2D Materials**: Graphene, TMDCs, and novel layered structures
- **Battery Materials**: Li-ion, Na-ion, and solid-state electrolytes
- **Photovoltaics**: Solar cell materials and light absorption
- **Catalysis**: Surface properties and reaction energetics
- **Phase Stability**: Convex hull analysis and thermodynamics

Start exploring materials science data with PRISM today! 🚀
