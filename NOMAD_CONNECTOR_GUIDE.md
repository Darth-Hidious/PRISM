# NOMAD Connector Documentation

## Overview

The NOMAD Connector provides comprehensive access to the NOMAD Laboratory database, the world's largest open materials database containing millions of calculated and experimental materials data entries. This connector implements specialized query building, streaming capabilities, and standardized data formats for seamless integration with the PRISM system.

## Features

### 🔍 Advanced Query Building
- **NOMADQueryBuilder**: Specialized query builder for NOMAD's unique syntax
- **Element Filtering**: Search by elements with HAS ANY/HAS ALL operators
- **Property Ranges**: Filter by band gap, formation energy, and other properties
- **Structure Filters**: Search by space group, crystal system, and symmetry
- **Complex Queries**: Combine multiple criteria with logical operators

### 🌊 Streaming Support
- **Large Dataset Handling**: Automatic streaming for datasets above threshold
- **Memory Efficient**: Process millions of materials without memory issues
- **Pagination Management**: Intelligent pagination with NOMAD API limits
- **Progress Tracking**: Monitor streaming progress for large operations

### 📊 Standardized Data Format
- **Unified Interface**: Consistent material representation across all connectors
- **Structure Information**: Lattice parameters, atomic positions, space groups
- **Properties**: Electronic, thermodynamic, and mechanical properties
- **Metadata**: Source information, calculation details, experimental flags

### ⚡ Performance Optimization
- **Rate Limiting**: Built-in rate limiting to respect API constraints
- **Async Operations**: Fully asynchronous for high performance
- **Caching Ready**: Compatible with caching layers for repeated queries
- **Bulk Operations**: Efficient bulk material fetching

## Installation and Setup

### Prerequisites
```bash
pip install httpx asyncio
```

### Configuration
```python
nomad_config = {
    "base_url": "https://nomad-lab.eu/prod/v1/api/v1",
    "timeout": 30.0,
    "stream_threshold": 1000,  # Stream if more than 1000 results
    "max_retries": 3,
    "retry_delay": 1.0
}
```

## Usage Examples

### 1. Basic Connection and Search

```python
import asyncio
from app.services.connectors.nomad_connector import NOMADConnector

async def basic_search():
    config = {"base_url": "https://nomad-lab.eu/prod/v1/api/v1"}
    connector = NOMADConnector(config)
    
    # Connect to NOMAD API
    await connector.connect()
    
    # Search for iron oxides
    materials = await connector.search_materials(
        elements=["Fe", "O"],
        limit=10
    )
    
    for material in materials:
        print(f"{material.formula}: {material.properties.band_gap} eV")
    
    await connector.disconnect()

asyncio.run(basic_search())
```

### 2. Advanced Query Building

```python
from app.services.connectors.nomad_connector import NOMADQueryBuilder, create_nomad_query

async def advanced_search():
    connector = NOMADConnector(config)
    await connector.connect()
    
    # Build complex query
    query = (create_nomad_query()
             .elements(["Ti", "O"], "HAS ALL")          # Must contain both Ti and O
             .element_count(3, "lte")                   # Max 3 elements
             .band_gap_range(2.0, 4.0)                 # Band gap 2-4 eV
             .formation_energy_range(-3.0, 0.0)        # Stable compounds
             .space_group("P4/mmm")                     # Specific space group
             .add_section("results")                    # Include results section
             .add_section("run")                        # Include calculation details
             .paginate(page_size=100))                  # 100 results per page
    
    # Execute search
    materials = await connector.search_materials(query_builder=query)
    
    print(f"Found {len(materials)} materials matching criteria")
    
    await connector.disconnect()
```

### 3. Specific Query Types

#### Element-Based Searches
```python
# Materials containing any of the specified elements
query = NOMADQueryBuilder().elements(["Fe", "Co", "Ni"], "HAS ANY")

# Materials containing all specified elements
query = NOMADQueryBuilder().elements(["Ba", "Ti", "O"], "HAS ALL")

# Materials with specific element count
query = NOMADQueryBuilder().element_count(4, "gte")  # 4 or more elements
```

#### Property-Based Searches
```python
# Band gap range
query = NOMADQueryBuilder().band_gap_range(1.0, 3.0)

# Formation energy range
query = NOMADQueryBuilder().formation_energy_range(-2.0, 0.0)

# Custom property range
query = NOMADQueryBuilder().property_range(
    "results.properties.mechanical.bulk_modulus.value", 
    100.0, 300.0
)
```

#### Structure-Based Searches
```python
# Specific space group number
query = NOMADQueryBuilder().space_group(225)

# Specific space group symbol
query = NOMADQueryBuilder().space_group("Fm-3m")

# Formula searches
query = NOMADQueryBuilder().formula("Fe2O3")              # Exact formula
query = NOMADQueryBuilder().formula_contains("Fe")        # Contains Fe
```

### 4. Streaming Large Datasets

```python
async def stream_large_dataset():
    config = {"stream_threshold": 100}  # Stream if >100 results
    connector = NOMADConnector(config)
    await connector.connect()
    
    # This will automatically use streaming for large results
    materials = await connector.search_materials(
        elements=["O"],  # Very common element = large dataset
        limit=10000
    )
    
    print(f"Processed {len(materials)} materials via streaming")
    
    # Process in chunks
    for i in range(0, len(materials), 100):
        chunk = materials[i:i+100]
        # Process chunk
        print(f"Processing chunk {i//100 + 1}: {len(chunk)} materials")
    
    await connector.disconnect()
```

### 5. Material Details and Analysis

```python
async def analyze_materials():
    connector = NOMADConnector(config)
    await connector.connect()
    
    # Get specific material by ID
    material = await connector.get_material_by_id("entry-id-123")
    
    if material:
        # Structure information
        structure = material.structure
        print(f"Formula: {material.formula}")
        print(f"Space Group: {structure.space_group}")
        print(f"Crystal System: {structure.crystal_system}")
        print(f"Volume: {structure.volume:.2f} Ų")
        print(f"Lattice Parameters: {structure.lattice_parameters}")
        
        # Properties
        props = material.properties
        if props.band_gap:
            print(f"Band Gap: {props.band_gap:.2f} eV")
        if props.formation_energy:
            print(f"Formation Energy: {props.formation_energy:.2f} eV/atom")
        if props.bulk_modulus:
            print(f"Bulk Modulus: {props.bulk_modulus:.2f} GPa")
        
        # Metadata
        metadata = material.metadata
        print(f"Source: {metadata.source}")
        print(f"Calculation Method: {metadata.calculation_method}")
        print(f"Is Experimental: {metadata.is_experimental}")
    
    await connector.disconnect()
```

### 6. Bulk Operations

```python
async def bulk_operations():
    connector = NOMADConnector(config)
    await connector.connect()
    
    # Fetch large number of materials efficiently
    materials = await connector.fetch_bulk_materials(
        elements=["Li", "Co", "O"],  # Battery materials
        min_elements=3,
        max_elements=5,
        limit=5000
    )
    
    # Analyze bulk data
    formulas = {}
    for material in materials:
        formula = material.formula
        formulas[formula] = formulas.get(formula, 0) + 1
    
    # Most common formulas
    common_formulas = sorted(formulas.items(), key=lambda x: x[1], reverse=True)
    print("Most common formulas:")
    for formula, count in common_formulas[:10]:
        print(f"  {formula}: {count} entries")
    
    await connector.disconnect()
```

## Integration with Job System

### 1. Creating NOMAD Jobs

```python
from app.services.job_processor import JobProcessor

async def create_nomad_job():
    job_processor = JobProcessor()
    
    job_config = {
        "source_type": "nomad",
        "query": {
            "elements": ["Fe", "O"],
            "band_gap_min": 1.0,
            "band_gap_max": 3.0,
            "limit": 1000
        },
        "destination": {
            "type": "database",
            "table": "iron_oxides"
        }
    }
    
    job_id = await job_processor.create_job(
        job_type="data_extraction",
        config=job_config
    )
    
    print(f"Created NOMAD job: {job_id}")
```

### 2. Advanced Job Configuration

```python
job_config = {
    "source_type": "nomad",
    "connector_config": {
        "base_url": "https://nomad-lab.eu/prod/v1/api/v1",
        "stream_threshold": 500,
        "timeout": 60.0
    },
    "query_builder": {
        "elements": ["Ti", "O"],
        "element_count_min": 2,
        "element_count_max": 3,
        "band_gap_range": [2.0, 4.0],
        "space_groups": ["P4/mmm", "I4/mmm"],
        "sections": ["results", "run", "system"]
    },
    "processing": {
        "batch_size": 100,
        "parallel_workers": 4,
        "filter_experimental": True
    }
}
```

## Query Builder Reference

### NOMADQueryBuilder Methods

#### Element Filters
- `elements(elements: List[str], operator: str = "HAS ANY")` - Filter by elements
- `element_count(count: int, operator: str = "gte")` - Filter by element count

#### Formula Filters
- `formula(formula: str)` - Exact formula match
- `formula_contains(partial: str)` - Partial formula match

#### Structure Filters
- `space_group(group: Union[int, str])` - Space group filter
- `crystal_system(system: str)` - Crystal system filter

#### Property Filters
- `band_gap_range(min_val: float, max_val: float)` - Band gap range
- `formation_energy_range(min_val: float, max_val: float)` - Formation energy range
- `property_range(property_path: str, min_val: float, max_val: float)` - Custom property range

#### Query Building
- `add_section(section: str)` - Add required data section
- `paginate(page_size: int, page_offset: int = 0)` - Set pagination
- `build()` - Build final query dictionary

### Query Syntax Examples

#### NOMAD Query Language
```python
# Element filters
"results.material.elements HAS ANY [\"Fe\", \"O\"]"
"results.material.elements HAS ALL [\"Ba\", \"Ti\", \"O\"]"

# Property ranges
"results.material.n_elements:gte: 3"
"results.properties.electronic.band_gap.value:gte:1.0"
"results.properties.electronic.band_gap.value:lte:3.0"

# Structure filters
"results.material.symmetry.space_group_number:225"
"results.material.symmetry.space_group_symbol:\"Fm-3m\""

# Formula filters
"results.material.chemical_formula_reduced:\"Fe2O3\""
"results.material.chemical_formula_reduced:*Fe*"
```

## Performance Guidelines

### 1. Query Optimization
- Use specific element filters to reduce result sets
- Combine multiple filters to narrow search scope
- Use appropriate page sizes (50-200 for most use cases)
- Request only necessary data sections

### 2. Streaming Configuration
- Set `stream_threshold` based on memory constraints
- Use streaming for datasets > 1000 materials
- Process streamed data in chunks for better performance
- Monitor memory usage during large operations

### 3. Rate Limiting
- NOMAD API has rate limits - use built-in rate limiter
- Avoid parallel requests to same endpoints
- Implement exponential backoff for retries
- Cache results when possible

### 4. Memory Management
```python
# Good: Process in chunks
async def process_large_dataset():
    materials = await connector.search_materials(elements=["O"], limit=10000)
    
    for i in range(0, len(materials), 1000):
        chunk = materials[i:i+1000]
        await process_chunk(chunk)
        # Chunk is garbage collected after processing

# Better: Use streaming
async def stream_large_dataset():
    async for material in connector.stream_materials(elements=["O"]):
        await process_material(material)
        # Each material is processed and discarded immediately
```

## Error Handling

### 1. Connection Errors
```python
try:
    await connector.connect()
except httpx.RequestError as e:
    print(f"Network error: {e}")
except httpx.HTTPStatusError as e:
    print(f"HTTP error: {e.response.status_code}")
```

### 2. Query Errors
```python
try:
    materials = await connector.search_materials(invalid_param="value")
except ValueError as e:
    print(f"Invalid query parameter: {e}")
except Exception as e:
    print(f"Query failed: {e}")
```

### 3. Data Validation
```python
# Validate response data
response = await connector._make_request("/entries", params)
if not connector.validate_response(response):
    raise ValueError("Invalid response format from NOMAD API")
```

## API Reference

### NOMADConnector Class

#### Methods
- `__init__(config: Dict, rate_limiter=None)` - Initialize connector
- `connect() -> bool` - Connect to NOMAD API
- `disconnect() -> bool` - Disconnect from API
- `search_materials(**kwargs) -> List[StandardizedMaterial]` - Search materials
- `get_material_by_id(material_id: str) -> StandardizedMaterial` - Get specific material
- `fetch_bulk_materials(**kwargs) -> List[StandardizedMaterial]` - Bulk fetch
- `validate_response(response: Dict) -> bool` - Validate API response

#### Configuration Options
```python
{
    "base_url": str,              # NOMAD API base URL
    "timeout": float,             # Request timeout in seconds
    "stream_threshold": int,      # Streaming threshold
    "max_retries": int,          # Maximum retry attempts
    "retry_delay": float,        # Delay between retries
    "verify_ssl": bool           # SSL verification
}
```

### NOMADQueryBuilder Class

#### Core Methods
- `elements(elements, operator)` - Element filtering
- `element_count(count, operator)` - Element count filtering
- `formula(formula)` - Formula filtering
- `space_group(group)` - Space group filtering
- `property_range(path, min_val, max_val)` - Property range filtering
- `build()` - Build query dictionary

#### Convenience Methods
- `band_gap_range(min_val, max_val)` - Band gap filtering
- `formation_energy_range(min_val, max_val)` - Formation energy filtering
- `formula_contains(partial)` - Partial formula matching

## Testing

### Running Tests
```bash
# Run all NOMAD connector tests
pytest tests/test_nomad_connector.py -v

# Run specific test class
pytest tests/test_nomad_connector.py::TestNOMADQueryBuilder -v

# Run with coverage
pytest tests/test_nomad_connector.py --cov=app.services.connectors.nomad_connector
```

### Running Demo
```bash
# Run interactive demo
python nomad_demo.py

# Run specific demo sections
python -c "
from nomad_demo import NOMADDemo
import asyncio

demo = NOMADDemo()
asyncio.run(demo.demo_query_builder())
"
```

## Troubleshooting

### Common Issues

1. **Connection Timeout**
   ```python
   # Increase timeout
   config = {"timeout": 60.0}
   ```

2. **Rate Limiting**
   ```python
   # Add rate limiter
   from app.services.rate_limiter import RateLimiter
   rate_limiter = RateLimiter(requests_per_second=2)
   connector = NOMADConnector(config, rate_limiter=rate_limiter)
   ```

3. **Memory Issues with Large Datasets**
   ```python
   # Use streaming
   config = {"stream_threshold": 100}
   ```

4. **Invalid Queries**
   ```python
   # Validate query before execution
   query = builder.build()
   if not query.get("query"):
       raise ValueError("Empty query")
   ```

### Debug Mode
```python
import logging
logging.basicConfig(level=logging.DEBUG)

# This will show detailed HTTP requests and responses
```

## Contributing

### Adding New Query Types
1. Add method to `NOMADQueryBuilder`
2. Update `_build_simple_query` if needed
3. Add tests for new functionality
4. Update documentation

### Extending Properties
1. Update `_extract_properties` method
2. Modify `MaterialProperties` if needed
3. Add property-specific query methods
4. Test with real NOMAD data

For more information, see the [NOMAD Laboratory API documentation](https://nomad-lab.eu/prod/v1/docs/).
