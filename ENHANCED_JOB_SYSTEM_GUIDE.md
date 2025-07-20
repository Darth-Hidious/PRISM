# Enhanced Job System - Quick Start Guide

## Overview

The enhanced job system provides a comprehensive framework for processing materials data with advanced features like batch processing, scheduling, dependencies, and real-time progress tracking.

## Job Types

### 1. FETCH_SINGLE_MATERIAL
Fetch a specific material by ID.

```python
{
    "job_type": "fetch_single_material",
    "source_type": "jarvis",
    "source_config": {
        "material_id": "JVASP-1002",
        "dataset": "dft_3d"
    },
    "destination_type": "database",
    "priority": 8
}
```

### 2. BULK_FETCH_BY_FORMULA
Fetch multiple materials by chemical formulas.

```python
{
    "job_type": "bulk_fetch_by_formula",
    "source_type": "jarvis",
    "source_config": {
        "formulas": ["Si", "GaAs", "AlN"],
        "dataset": "dft_3d"
    },
    "destination_type": "database",
    "batch_size": 10,
    "priority": 6
}
```

### 3. BULK_FETCH_BY_PROPERTIES
Fetch materials based on property filters.

```python
{
    "job_type": "bulk_fetch_by_properties",
    "source_type": "jarvis",
    "source_config": {
        "property_filters": {
            "formation_energy_per_atom": {"min": -2.0, "max": 0.0},
            "band_gap": {"min": 0.5, "max": 3.0}
        },
        "dataset": "dft_3d"
    },
    "destination_type": "database",
    "batch_size": 20,
    "priority": 5
}
```

### 4. SYNC_DATABASE
Synchronize entire datasets.

```python
{
    "job_type": "sync_database",
    "source_type": "jarvis",
    "source_config": {
        "dataset": "dft_2d",
        "incremental": True
    },
    "destination_type": "database",
    "batch_size": 50,
    "priority": 10
}
```

## Scheduling Jobs

### Cron-based Scheduling
```python
{
    "job_type": "sync_database",
    "source_type": "jarvis",
    "source_config": {"dataset": "dft_3d"},
    "schedule_config": {
        "enabled": true,
        "cron_expression": "0 2 * * *",  # Daily at 2 AM
        "max_runs": 30
    }
}
```

### Interval-based Scheduling
```python
{
    "job_type": "bulk_fetch_by_formula",
    "source_type": "jarvis",
    "source_config": {"formulas": ["Si", "GaAs"]},
    "schedule_config": {
        "enabled": true,
        "interval_seconds": 3600,  # Every hour
        "max_runs": 10
    }
}
```

## Job Dependencies

Create dependent jobs that wait for parent jobs to complete:

```python
# First, create parent job
parent_response = await client.post("/api/v1/jobs/", json={
    "job_type": "fetch_single_material",
    "source_type": "jarvis",
    "source_config": {"material_id": "JVASP-1001"}
})
parent_job_id = parent_response.json()["id"]

# Then create dependent job
await client.post("/api/v1/jobs/", json={
    "job_type": "bulk_fetch_by_formula",
    "source_type": "jarvis",
    "source_config": {"formulas": ["Si", "Ge"]},
    "dependencies": [parent_job_id]
})
```

## API Endpoints

### Create Job
```bash
POST /api/v1/jobs/
Content-Type: application/json

{
    "job_type": "fetch_single_material",
    "source_type": "jarvis",
    "source_config": {"material_id": "JVASP-1002"},
    "destination_type": "database",
    "priority": 5,
    "batch_size": 1,
    "retry_count": 3
}
```

### List Jobs with Filtering
```bash
GET /api/v1/jobs/?status_filter=completed&job_type_filter=fetch_single_material&limit=50
```

### Get Job Progress
```bash
GET /api/v1/jobs/{job_id}
```

### Get Job Statistics
```bash
GET /api/v1/jobs/stats?hours=24
```

### Get Job Materials
```bash
GET /api/v1/jobs/{job_id}/materials?limit=100
```

### Bulk Operations
```bash
# Create multiple jobs
POST /api/v1/jobs/bulk/create
Content-Type: application/json
[job1_data, job2_data, job3_data]

# Cancel multiple jobs
POST /api/v1/jobs/bulk/cancel
Content-Type: application/json
[job_id1, job_id2, job_id3]
```

## Job Priorities

- **CRITICAL (10)**: Emergency sync jobs
- **HIGH (8)**: Important single material fetches
- **NORMAL (5)**: Regular bulk operations (default)
- **LOW (1)**: Background maintenance jobs

## Progress Tracking

Jobs provide real-time progress information:

```json
{
    "id": "job-uuid",
    "status": "processing",
    "progress": 65,
    "processed_records": 650,
    "total_records": 1000,
    "processing_rate": 12.5,
    "estimated_completion": "2024-01-15T14:30:00Z",
    "current_batch": 13,
    "error_count": 2
}
```

## Error Handling

Jobs automatically retry on failure with exponential backoff:

- **retry_count**: Number of retry attempts (default: 3)
- **current_retry**: Current retry attempt
- **Backoff schedule**: 2^retry minutes (2, 4, 8, 16 minutes)

## Rate Limiting Integration

The job system integrates with the distributed rate limiter:

- Per-source rate limiting (e.g., different limits for JARVIS vs Materials Project)
- Adaptive rate limiting based on API responses
- Automatic backoff on 429 (rate limit) errors
- Coordinated across multiple service instances via Redis

## Starting the Services

### 1. Start Job Processor
```python
from app.services.job_processor import start_job_processor

# In your application startup
processor_task = await start_job_processor()
```

### 2. Start Job Scheduler
```python
from app.services.job_scheduler import start_job_scheduler

# In your application startup
scheduler_task = await start_job_scheduler()
```

### 3. Example FastAPI Integration
```python
from contextlib import asynccontextmanager
import asyncio

@asynccontextmanager
async def lifespan(app: FastAPI):
    # Start background services
    processor_task = await start_job_processor()
    scheduler_task = await start_job_scheduler()
    
    yield
    
    # Stop background services
    await stop_job_processor()
    await stop_job_scheduler()

app = FastAPI(lifespan=lifespan)
```

## Monitoring

### Health Checks
```bash
GET /health  # General API health
GET /api/v1/jobs/stats  # Job system statistics
```

### Logs
Jobs create detailed logs accessible via:
```bash
GET /api/v1/jobs/{job_id}/logs?level=ERROR
```

### Redis Monitoring
Check queue status:
```python
from app.services.connectors.redis_connector import get_job_queue

job_queue = await get_job_queue()
stats = await job_queue.get_queue_stats()
print(f"Pending jobs: {stats['pending_jobs']}")
```

## Testing

Run the comprehensive demo:
```bash
python enhanced_job_system_demo.py
```

Run the test suite:
```bash
pytest tests/test_enhanced_job_system.py -v
```

## Production Considerations

1. **Database Setup**: Ensure PostgreSQL is configured with proper indexes
2. **Redis Setup**: Configure Redis for persistence and high availability
3. **Scaling**: Multiple worker instances can process jobs concurrently
4. **Monitoring**: Set up alerts for job failure rates and queue depths
5. **Backup**: Regular backups of job data and configuration
6. **Rate Limits**: Configure appropriate rate limits for external APIs

## Next Steps

1. Set up PostgreSQL and Redis
2. Run database migrations to create new tables
3. Start the FastAPI application with background services
4. Test with the demo script
5. Configure monitoring and alerting
6. Deploy to production with proper scaling configuration
