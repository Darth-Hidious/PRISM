# 🚀 PRISM Platform - Materials Science Data Management

PRISM (Platform for Research in Integrated Scientific Materials) is a comprehensive, production-ready materials science data ingestion and processing platform that provides unified access to multiple materials databases including NOMAD Laboratory and JARVIS through a powerful CLI interface with PostgreSQL database storage.

## ⚡ Quick Start

### Prerequisites
- Python 3.9+
- PostgreSQL 12+ (for production database)
- Redis (optional, for caching and job queues)

### 🚀 One-Command Setup
```bash
# Clone and initialize everything
git clone https://github.com/Darth-Hidious/PRISM.git
cd PRISM
./install.sh
```

### 🐳 Docker Setup (Recommended for Production)
```bash
# Start PostgreSQL and Redis with Docker
docker-compose up -d postgres redis

# Initialize database
python init_database.py

# Start using PRISM
./prism fetch-and-store --stats
```

## 🎯 Core Features

### Materials Database Integration
- **NOMAD Laboratory**: Access to 19M+ materials entries
- **JARVIS Database**: NIST materials properties database
- **Unified Data Model**: Standardized material representations
- **PostgreSQL Storage**: Production-ready database with proper indexing
- **Batch Processing**: Intelligent batching with progress tracking

### Advanced CLI Interface
- **Interactive Progress**: Real-time fetching progress with ETA
- **Database Management**: Local storage with search capabilities
- **Error Handling**: Robust error recovery and logging
- **Configuration Management**: Environment-based configuration
- **Production Ready**: Health checks and monitoring

### Data Management
- **Automated Storage**: Materials automatically stored with deduplication
- **Search & Filter**: Query local database by elements, formula, properties
- **Export Capabilities**: JSON, CSV, YAML export formats
- **Statistics**: Database analytics and reporting

## 📋 Essential Commands

### Database Operations
```bash
# Initialize PostgreSQL database
python init_database.py

# Show database statistics
./prism fetch-and-store --stats

# Search local database only
./prism fetch-and-store --database-only --elements Si
```

### Material Fetching
```bash
# Fetch Silicon materials (controlled batch processing)
./prism fetch-and-store --elements Si --max-results 100

# Fetch specific compound
./prism fetch-and-store --formula SiO2 --max-results 50

# Large dataset with progress tracking
./prism fetch-and-store --elements Si,O --max-results 1000 --batch-size 25
```

### System Management
```bash
# Test all connections
./prism test-connection --source all

# Check configuration
./prism config --list

# Monitor system status
./prism queue-status
```

## 🏗️ Architecture

### Database Schema
The platform stores materials with comprehensive metadata:

**Materials Table Fields:**
- **Identification**: material_id, origin, source_id
- **Composition**: composition, reduced_formula, elements, nsites
- **Physical Properties**: volume, density, bandgap
- **Symmetry**: point_group, space_group, crystal_system
- **Energetics**: formation_energy, decomposition_energy
- **Metadata**: structure_data, properties_data, processing_status

### Data Flow
1. **Connect** to external databases (NOMAD/JARVIS)
2. **Query** with intelligent parameter handling
3. **Fetch** in optimized batches with rate limiting
4. **Standardize** data format across sources
5. **Store** in PostgreSQL with proper indexing
6. **Index** for fast search and retrieval

## 🔧 Configuration

### Environment Setup
The platform uses `.env` file for configuration:

```bash
# Database (PostgreSQL)
POSTGRES_SERVER=localhost
POSTGRES_USER=prism_user
POSTGRES_PASSWORD=prism_password
POSTGRES_DB=prism_materials
POSTGRES_PORT=5432

# API Settings
NOMAD_BASE_URL=https://nomad-lab.eu/prod/rae/api/v1
NOMAD_TIMEOUT=30.0
NOMAD_RATE_LIMIT=120

# Application
ENVIRONMENT=production
DEBUG=false
```

### Database Configuration
Supports multiple database connection methods:
- **Local PostgreSQL**: Direct connection
- **Docker PostgreSQL**: Containerized database
- **External Database**: Production database servers
- **Connection URL**: Full database URLs

## 🚀 Production Deployment

### Method 1: Docker Compose (Recommended)
```bash
# Production deployment with PostgreSQL and Redis
docker-compose up -d

# Check all services are healthy
docker-compose ps
```

### Method 2: Local Production Setup
```bash
# Initialize production environment
./start_production.sh

# Check database health
./start_production.sh health-check
```

### Method 3: Manual Setup
```bash
# Install PostgreSQL
sudo apt-get install postgresql postgresql-contrib  # Ubuntu
brew install postgresql  # macOS

# Create database and user
createdb prism_materials
createuser prism_user

# Install Python dependencies
pip install -r requirements.txt

# Initialize and start
python init_database.py
./prism fetch-and-store --stats
```

## 📊 Usage Examples

### Basic Material Search
```bash
# Get database overview
./prism fetch-and-store --stats

# Search for Silicon compounds (local database)
./prism fetch-and-store --database-only --elements Si

# Fetch new Silicon materials from NOMAD
./prism fetch-and-store --elements Si --max-results 50
```

### Advanced Material Research
```bash
# Multi-element search with progress tracking
./prism fetch-and-store --elements "Si,O" --max-results 200 --show-progress

# Specific compound research
./prism fetch-and-store --formula "SiO2" --max-results 25

# Large-scale data collection
./prism fetch-and-store --elements Si --max-results 5000 --batch-size 50
```

### Database Management
```bash
# View stored materials
./prism fetch-and-store --database-only --formula SiO2

# Export data
./prism export-data --format json --output silicon_materials.json

# System health check
./prism test-connection --source nomad
```

## 🔍 API Documentation

When running the web server (`python run.py`), access:
- **Swagger UI**: `http://localhost:8000/docs`
- **ReDoc**: `http://localhost:8000/redoc`
- **Health Check**: `http://localhost:8000/api/v1/health/`

### Key API Endpoints
- `GET /api/v1/health/` - System health status
- `POST /api/v1/jobs/` - Create material fetching jobs
- `GET /api/v1/jobs/` - List and monitor jobs
- `GET /api/v1/sources/` - Available data sources

## 🛠️ Development

### Setup Development Environment
```bash
# Clone and setup
git clone https://github.com/Darth-Hidious/PRISM.git
cd PRISM

# Install in development mode
pip install -e .

# Run tests
pytest

# Code formatting
black app/
isort app/
```

### Testing
```bash
# Run all tests
pytest

# Test specific connector
python test_nomad_fix.py

# Integration tests
python -m pytest tests/integration/
```

## 🔒 Security & Production Features

### Security
- Environment-based configuration (no hardcoded secrets)
- SQL injection prevention with SQLAlchemy ORM
- Input validation with Pydantic schemas
- Rate limiting for external API calls
- Connection pooling and timeout handling

### Production Features
- **Health Checks**: Kubernetes-ready liveness/readiness probes
- **Logging**: Structured JSON logging with correlation IDs
- **Monitoring**: Database and connection health monitoring
- **Error Recovery**: Automatic retry logic with exponential backoff
- **Batch Processing**: Optimized for large dataset processing
- **Database Indexing**: Optimized queries for materials search

### Performance
- **Async Processing**: Non-blocking I/O for API calls
- **Connection Pooling**: Efficient database connections
- **Batch Optimization**: Intelligent batch sizing
- **Progress Tracking**: Real-time progress with ETA calculations
- **Memory Management**: Streaming for large datasets

## 🤝 Contributing

1. Fork the repository
2. Create feature branch (`git checkout -b feature/amazing-feature`)
3. Commit changes (`git commit -m 'Add amazing feature'`)
4. Push to branch (`git push origin feature/amazing-feature`)
5. Open a Pull Request

## 📞 Support

### Common Issues
1. **Database Connection Failed**: Check PostgreSQL is running and credentials are correct
2. **NOMAD API Timeout**: Reduce batch size or check network connection
3. **Memory Issues**: Use streaming mode for large datasets

### Getting Help
- Check `./prism --help` for command documentation
- Review logs in the application output
- Check database connectivity with `./start_production.sh health-check`

## 📄 License

MIT License - see LICENSE file for details

---

**PRISM Platform** - Making materials science data accessible and manageable.
