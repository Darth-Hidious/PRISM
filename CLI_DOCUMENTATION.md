# PRISM CLI Tool Documentation

## Overview

The PRISM CLI tool provides a comprehensive command-line interface for managing the PRISM data ingestion platform. It offers commands for material fetching, bulk operations, queue management, system monitoring, and configuration management.

## Installation & Setup

### Prerequisites
- Python 3.8+
- Required packages: `click`, `rich`, `asyncio`
- Optional packages for full functionality: `pandas`, `openpyxl`, `pyarrow`, `pyyaml`

### Installation
```bash
# Install required dependencies
pip install click rich

# Optional dependencies for enhanced functionality
pip install pandas openpyxl pyarrow pyyaml
```

## Usage

### Demo Version (Standalone)
For testing without database setup:
```bash
python cli_demo.py [COMMAND] [OPTIONS]
```

### Full Version (Production)
With database and connector setup:
```bash
python cli_runner.py [COMMAND] [OPTIONS]
# OR
python -m app.cli [COMMAND] [OPTIONS]
```

## Available Commands

### 1. fetch-material
Fetch material data from a specific source.

**Usage:**
```bash
python cli_demo.py fetch-material -s jarvis -e Si,Ga
python cli_demo.py fetch-material -s nomad --formula TiO2
python cli_demo.py fetch-material -s jarvis --material-id JVASP-1000
```

**Options:**
- `-s, --source`: Data source (jarvis, nomad) [REQUIRED]
- `-m, --material-id`: Specific material ID to fetch
- `-f, --formula`: Chemical formula to search for
- `-e, --elements`: Comma-separated list of elements
- `-o, --output`: Output file path
- `--format`: Output format (json, csv, yaml) [default: json]

**Examples:**
```bash
# Fetch silicon-containing materials from JARVIS
python cli_demo.py fetch-material -s jarvis -e Si

# Fetch specific material by ID
python cli_demo.py fetch-material -s jarvis --material-id JVASP-1000

# Search by formula and save to file
python cli_demo.py fetch-material -s nomad -f TiO2 -o results.json

# Export as CSV
python cli_demo.py fetch-material -s jarvis -e Ga,As --format csv
```

### 2. bulk-fetch
Perform bulk material fetching with progress tracking.

**Usage:**
```bash
python cli_demo.py bulk-fetch -s all -l 100 -b 10
```

**Options:**
- `-s, --source`: Data source(s) (jarvis, nomad, all) [REQUIRED]
- `-e, --elements`: Comma-separated list of elements to filter
- `-l, --limit`: Maximum number of materials to fetch [default: 100]
- `-b, --batch-size`: Batch size for processing [default: 10]
- `--dry-run`: Show what would be done without executing

**Examples:**
```bash
# Bulk fetch from all sources
python cli_demo.py bulk-fetch -s all -l 50

# Fetch specific elements only
python cli_demo.py bulk-fetch -s jarvis -e Ti,O -l 20

# Dry run to preview operation
python cli_demo.py bulk-fetch -s nomad -l 100 --dry-run
```

### 3. list-sources
List all available data sources and their status.

**Usage:**
```bash
python cli_demo.py list-sources
```

**Options:**
- `--format`: Output format (table, json, list) [default: table]

**Examples:**
```bash
# Display as table (default)
python cli_demo.py list-sources

# Output as JSON
python cli_demo.py list-sources --format json

# Simple list format
python cli_demo.py list-sources --format list
```

### 4. test-connection
Test connection to data sources.

**Usage:**
```bash
python cli_demo.py test-connection
```

**Options:**
- `-s, --source`: Source to test (jarvis, nomad, all) [default: all]
- `-t, --timeout`: Connection timeout in seconds [default: 30]

**Examples:**
```bash
# Test all connections
python cli_demo.py test-connection

# Test specific source
python cli_demo.py test-connection -s jarvis

# Test with custom timeout
python cli_demo.py test-connection -t 10
```

### 5. queue-status
Show job queue status and statistics.

**Usage:**
```bash
python cli_demo.py queue-status
```

**Features:**
- Total job count and breakdown by status
- Recent job failures
- Queue health indicators
- Color-coded status display

### 6. monitor
Monitor system performance and metrics.

**Usage:**
```bash
python cli_demo.py monitor
```

**Features:**
- Real-time system metrics
- Job processing statistics
- Connection status
- Performance indicators

## Full Version Commands (Production)

The production version includes additional commands when running with database connectivity:

### 7. retry-failed-jobs
Retry failed jobs with filtering options.

**Options:**
- `--max-age`: Maximum age of failed jobs to retry (hours) [default: 24]
- `--source`: Source type to filter (jarvis, nomad, all)
- `--dry-run`: Show what would be retried without executing
- `--batch-size`: Number of jobs to retry in each batch [default: 10]

### 8. export-data
Export data to various formats.

**Options:**
- `--format`: Export format (json, csv, xlsx, parquet) [default: json]
- `-o, --output`: Output file path [REQUIRED]
- `--source`: Filter by data source
- `--date-from`: Start date (YYYY-MM-DD)
- `--date-to`: End date (YYYY-MM-DD)
- `--status`: Filter by job status
- `--limit`: Maximum number of records

### 9. config
Manage configuration settings.

**Options:**
- `--list`: List current configuration
- `--set`: Set configuration value (key=value)
- `--get`: Get configuration value

## Output Formats

### JSON Format
Default format, suitable for programmatic processing:
```json
[
  {
    "jid": "JVASP-1000",
    "formula": "Si2",
    "formation_energy_peratom": -5.4,
    "elements": ["Si"],
    "structure": {"lattice_type": "cubic"},
    "band_gap": 1.1
  }
]
```

### CSV Format
Tabular format for spreadsheet applications:
```csv
jid,formula,formation_energy_peratom,elements,structure,band_gap
JVASP-1000,Si2,-5.4,"['Si']","{'lattice_type': 'cubic'}",1.1
```

### Table Format
Rich formatted tables for terminal display with color coding and borders.

## Error Handling

The CLI includes comprehensive error handling:

- **Connection Errors**: Graceful handling of network timeouts and connection failures
- **Data Validation**: Input validation with helpful error messages
- **Graceful Degradation**: Fallback options when optional dependencies are missing
- **User Interruption**: Clean exit on Ctrl+C

## Progress Tracking

- **Real-time Progress Bars**: Visual progress indication for long-running operations
- **Batch Processing**: Efficient handling of large datasets with configurable batch sizes
- **Status Updates**: Detailed status messages throughout operations
- **ETA Calculation**: Estimated time to completion for bulk operations

## Integration Features

### Pipeline Integration
```bash
# Use in shell scripts
python cli_demo.py fetch-material -s jarvis -e Si --format json > materials.json

# Chain commands
python cli_demo.py bulk-fetch -s all -l 100 && python cli_demo.py queue-status
```

### CI/CD Integration
```bash
# Health checks in deployment pipelines
python cli_demo.py test-connection --timeout 5

# Automated data export
python cli_demo.py export-data --format csv --output daily_export.csv
```

## Performance Considerations

- **Async Operations**: All network operations use async/await for efficiency
- **Rate Limiting**: Built-in rate limiting to respect API constraints
- **Memory Management**: Streaming and batching for large datasets
- **Connection Pooling**: Efficient connection reuse

## Troubleshooting

### Common Issues

1. **Import Errors**: Ensure you're running from the project root directory
2. **Connection Timeouts**: Increase timeout values for slow networks
3. **Missing Dependencies**: Install optional packages for full functionality
4. **Permission Errors**: Check file write permissions for output operations

### Debug Mode
Enable debug mode for detailed error information:
```bash
python cli_demo.py --debug fetch-material -s jarvis -e Si
```

## Development

### Adding New Commands
1. Create command function with `@cli.command()` decorator
2. Add appropriate click options and parameters
3. Implement error handling with `@error_handler` decorator
4. Add rich formatting for output display
5. Update help documentation

### Testing
```bash
# Run CLI tests
python -m pytest tests/test_cli.py

# Manual testing with demo version
python cli_demo.py --help
```

## Examples Gallery

### Basic Material Search
```bash
# Search for titanium-containing materials
python cli_demo.py fetch-material -s nomad -e Ti

# Find materials with specific formula
python cli_demo.py fetch-material -s jarvis -f GaAs
```

### Advanced Bulk Operations
```bash
# Large-scale data collection
python cli_demo.py bulk-fetch -s all -l 1000 -b 50

# Filtered bulk collection
python cli_demo.py bulk-fetch -s jarvis -e Si,Ge,Sn -l 200
```

### System Administration
```bash
# Monitor system health
python cli_demo.py test-connection
python cli_demo.py queue-status
python cli_demo.py monitor

# Export system data
python cli_demo.py export-data --format xlsx --output system_report.xlsx
```

### Data Analysis Pipeline
```bash
#!/bin/bash
# Complete data processing pipeline

# 1. Test connections
python cli_demo.py test-connection

# 2. Fetch materials
python cli_demo.py bulk-fetch -s all -l 500 -b 25

# 3. Check queue status
python cli_demo.py queue-status

# 4. Export results
python cli_demo.py export-data --format csv --output analysis_data.csv
```

This CLI tool provides a comprehensive interface for managing the PRISM platform, suitable for both interactive use and automated workflows.
