# Data Ingestion Microservice

A high-performance FastAPI microservice for data ingestion with Redis job queue and PostgreSQL storage.

## Features

- **FastAPI Framework**: High-performance async web framework
- **Async/Await Patterns**: Full async support throughout the application
- **PostgreSQL Integration**: AsyncPG for high-performance database operations
- **Redis Job Queue**: Background job processing with priority support
- **Pydantic Validation**: Comprehensive data validation and serialization
- **CORS Support**: Configurable cross-origin resource sharing
- **Health Checks**: Kubernetes-ready health and readiness probes
- **Structured Logging**: JSON-structured logging with correlation IDs
- **Environment Configuration**: Pydantic-settings based configuration
- **Error Handling**: Comprehensive error handling and logging

## Architecture

```
/app
├── api/
│   └── v1/
│       ├── endpoints/
│       │   ├── health.py      # Health check endpoints
│       │   ├── jobs.py        # Job management endpoints
│       │   ├── sources.py     # Data source management
│       │   └── destinations.py # Data destination management
│       └── __init__.py
├── core/
│   ├── config.py              # Application configuration
│   └── dependencies.py       # FastAPI dependencies
├── db/
│   ├── database.py           # Database connection management
│   └── models.py             # SQLAlchemy models
├── services/
│   └── connectors/
│       └── redis_connector.py # Redis connection and job queue
├── schemas/
│   └── __init__.py           # Pydantic schemas
└── main.py                   # FastAPI application
```

## Quick Start

### Prerequisites

- Python 3.11+
- PostgreSQL 13+
- Redis 6+

### Installation

1. Install dependencies:
```bash
pip install -r requirements.txt
```

2. Set up environment variables:
```bash
cp .env.example .env
# Edit .env with your configuration
```

3. Set up PostgreSQL database:
```sql
CREATE DATABASE data_ingestion;
CREATE USER postgres WITH PASSWORD 'password';
GRANT ALL PRIVILEGES ON DATABASE data_ingestion TO postgres;
```

4. Start Redis:
```bash
redis-server
```

5. Run the application:
```bash
python app/main.py
```

The API will be available at `http://localhost:8000` with automatic documentation at `http://localhost:8000/docs`.

## API Endpoints

### Health Check
- `GET /api/v1/health/` - General health check
- `GET /api/v1/health/liveness` - Kubernetes liveness probe
- `GET /api/v1/health/readiness` - Kubernetes readiness probe
- `GET /api/v1/health/queue` - Job queue statistics

### Jobs
- `POST /api/v1/jobs/` - Create a new ingestion job
- `GET /api/v1/jobs/` - List jobs with filtering
- `GET /api/v1/jobs/{job_id}` - Get specific job
- `GET /api/v1/jobs/{job_id}/status` - Get real-time job status
- `PUT /api/v1/jobs/{job_id}/progress` - Update job progress
- `GET /api/v1/jobs/{job_id}/logs` - Get job logs
- `DELETE /api/v1/jobs/{job_id}` - Cancel job

### Data Sources
- `POST /api/v1/sources/` - Create data source
- `GET /api/v1/sources/` - List data sources
- `GET /api/v1/sources/{source_id}` - Get specific source
- `PUT /api/v1/sources/{source_id}` - Update source
- `DELETE /api/v1/sources/{source_id}` - Delete source
- `POST /api/v1/sources/{source_id}/activate` - Activate source
- `POST /api/v1/sources/{source_id}/deactivate` - Deactivate source

### Data Destinations
- `POST /api/v1/destinations/` - Create data destination
- `GET /api/v1/destinations/` - List destinations
- `GET /api/v1/destinations/{destination_id}` - Get specific destination
- `PUT /api/v1/destinations/{destination_id}` - Update destination
- `DELETE /api/v1/destinations/{destination_id}` - Delete destination
- `POST /api/v1/destinations/{destination_id}/activate` - Activate destination
- `POST /api/v1/destinations/{destination_id}/deactivate` - Deactivate destination

## Configuration

The application uses environment variables for configuration. See `.env.example` for all available options.

Key configuration sections:
- **Application**: Basic app settings
- **Server**: Host, port, and server settings
- **CORS**: Cross-origin resource sharing
- **Database**: PostgreSQL connection settings
- **Redis**: Redis connection and job queue settings
- **Logging**: Log level and formatting
- **Security**: JWT tokens and secrets
- **Job Queue**: Retry and delay settings

## Development

### Running Tests
```bash
pytest
```

### Code Formatting
```bash
black app/
isort app/
```

### Linting
```bash
flake8 app/
```

## Deployment

### Docker
```bash
docker build -t data-ingestion-service .
docker run -p 8000:8000 data-ingestion-service
```

### Kubernetes
The service includes health check endpoints for Kubernetes:
- Liveness probe: `/api/v1/health/liveness`
- Readiness probe: `/api/v1/health/readiness`

## Monitoring

The service provides structured JSON logging and comprehensive health checks for monitoring:

- Application logs with correlation IDs
- Database connection health
- Redis connection health
- Job queue statistics
- Performance metrics

## Security

- Environment-based configuration
- CORS protection
- Trusted host middleware
- Input validation with Pydantic
- SQL injection prevention with SQLAlchemy
- Comprehensive error handling

## License

MIT License
