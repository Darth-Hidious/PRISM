"""
Simplified FastAPI Data Ingestion Microservice Demo
This demonstrates the core functionality without complex dependencies.
"""

import logging
from datetime import datetime
from typing import Dict, List
import uuid

from fastapi import FastAPI, HTTPException, status
from fastapi.middleware.cors import CORSMiddleware
from pydantic import BaseModel, Field


# Simple configuration
class SimpleConfig:
    app_name = "Data Ingestion Microservice"
    app_version = "1.0.0"
    host = "0.0.0.0"
    port = 8000
    cors_origins = ["http://localhost:3000", "http://localhost:8080"]


config = SimpleConfig()

# In-memory storage
jobs_storage: Dict[str, dict] = {}
sources_storage: Dict[str, dict] = {}
destinations_storage: Dict[str, dict] = {}


# Schemas
class JobCreate(BaseModel):
    source_type: str = Field(..., description="Type of data source")
    source_config: Dict = Field(..., description="Source configuration")
    destination_type: str = Field(..., description="Type of destination")
    destination_config: Dict = Field(..., description="Destination configuration")
    priority: int = Field(default=0, ge=0, le=10, description="Job priority")


class JobResponse(BaseModel):
    id: str
    source_type: str
    destination_type: str
    status: str
    progress: int = 0
    created_at: datetime


class HealthCheckResponse(BaseModel):
    status: str
    timestamp: datetime
    services: Dict[str, str] = Field(default_factory=dict)
    version: str = "1.0.0"


# Create FastAPI app
app = FastAPI(
    title=config.app_name,
    version=config.app_version,
    description="A demonstration data ingestion microservice built with FastAPI"
)

# Add CORS middleware
app.add_middleware(
    CORSMiddleware,
    allow_origins=config.cors_origins,
    allow_credentials=True,
    allow_methods=["*"],
    allow_headers=["*"],
)


@app.get("/", tags=["root"])
async def root():
    """Root endpoint."""
    return {
        "message": f"Welcome to {config.app_name}",
        "version": config.app_version,
        "status": "operational",
        "docs_url": "/docs",
        "features": [
            "FastAPI with async/await",
            "Pydantic data validation",
            "CORS middleware",
            "Structured error handling",
            "Interactive API documentation",
            "Health check endpoints",
            "Job management system",
            "Data source management",
            "RESTful API design"
        ]
    }


@app.get("/api/v1/health/", response_model=HealthCheckResponse, tags=["health"])
async def health_check():
    """Health check endpoint."""
    return HealthCheckResponse(
        status="healthy",
        timestamp=datetime.utcnow(),
        services={"application": "healthy", "storage": "in-memory"}
    )


@app.get("/api/v1/health/liveness", tags=["health"])
async def liveness_check():
    """Kubernetes liveness probe endpoint."""
    return {"status": "alive", "timestamp": datetime.utcnow()}


@app.get("/api/v1/health/readiness", tags=["health"])
async def readiness_check():
    """Kubernetes readiness probe endpoint."""
    return {"status": "ready", "timestamp": datetime.utcnow()}


@app.post("/api/v1/jobs/", response_model=JobResponse, status_code=status.HTTP_201_CREATED, tags=["jobs"])
async def create_job(job_data: JobCreate):
    """Create a new data ingestion job."""
    job_id = str(uuid.uuid4())
    job = {
        "id": job_id,
        "source_type": job_data.source_type,
        "source_config": job_data.source_config,
        "destination_type": job_data.destination_type,
        "destination_config": job_data.destination_config,
        "status": "created",
        "progress": 0,
        "created_at": datetime.utcnow(),
        "priority": job_data.priority
    }
    
    jobs_storage[job_id] = job
    print(f"Created job {job_id}")
    
    return JobResponse(**job)


@app.get("/api/v1/jobs/", response_model=List[JobResponse], tags=["jobs"])
async def list_jobs(skip: int = 0, limit: int = 100):
    """List data ingestion jobs."""
    jobs = list(jobs_storage.values())
    jobs.sort(key=lambda x: x["created_at"], reverse=True)
    return [JobResponse(**job) for job in jobs[skip:skip + limit]]


@app.get("/api/v1/jobs/{job_id}", response_model=JobResponse, tags=["jobs"])
async def get_job(job_id: str):
    """Get a specific job by ID."""
    if job_id not in jobs_storage:
        raise HTTPException(
            status_code=status.HTTP_404_NOT_FOUND,
            detail=f"Job {job_id} not found"
        )
    
    job = jobs_storage[job_id]
    return JobResponse(**job)


@app.put("/api/v1/jobs/{job_id}/progress", tags=["jobs"])
async def update_job_progress(job_id: str, progress: int):
    """Update job progress."""
    if job_id not in jobs_storage:
        raise HTTPException(
            status_code=status.HTTP_404_NOT_FOUND,
            detail=f"Job {job_id} not found"
        )
    
    jobs_storage[job_id]["progress"] = max(0, min(100, progress))
    if progress >= 100:
        jobs_storage[job_id]["status"] = "completed"
    elif progress > 0:
        jobs_storage[job_id]["status"] = "processing"
    
    print(f"Updated job {job_id} progress to {progress}%")
    return {"status": "updated", "job_id": job_id, "progress": progress}


@app.delete("/api/v1/jobs/{job_id}", status_code=status.HTTP_204_NO_CONTENT, tags=["jobs"])
async def cancel_job(job_id: str):
    """Cancel a job."""
    if job_id not in jobs_storage:
        raise HTTPException(
            status_code=status.HTTP_404_NOT_FOUND,
            detail=f"Job {job_id} not found"
        )
    
    jobs_storage[job_id]["status"] = "cancelled"
    print(f"Cancelled job {job_id}")


@app.post("/api/v1/sources/", tags=["sources"])
async def create_data_source(name: str, source_type: str, config: Dict):
    """Create a data source configuration."""
    source_id = str(uuid.uuid4())
    source = {
        "id": source_id,
        "name": name,
        "source_type": source_type,
        "config": config,
        "created_at": datetime.utcnow(),
        "is_active": True
    }
    
    sources_storage[source_id] = source
    print(f"Created data source {source_id}: {name}")
    return source


@app.get("/api/v1/sources/", tags=["sources"])
async def list_data_sources():
    """List data source configurations."""
    return list(sources_storage.values())


@app.post("/api/v1/destinations/", tags=["destinations"])
async def create_data_destination(name: str, destination_type: str, config: Dict):
    """Create a data destination configuration."""
    dest_id = str(uuid.uuid4())
    destination = {
        "id": dest_id,
        "name": name,
        "destination_type": destination_type,
        "config": config,
        "created_at": datetime.utcnow(),
        "is_active": True
    }
    
    destinations_storage[dest_id] = destination
    print(f"Created data destination {dest_id}: {name}")
    return destination


@app.get("/api/v1/destinations/", tags=["destinations"])
async def list_data_destinations():
    """List data destination configurations."""
    return list(destinations_storage.values())


# Initialize sample data
def populate_sample_data():
    """Populate sample data for demonstration."""
    # Sample source
    source_id = str(uuid.uuid4())
    sources_storage[source_id] = {
        "id": source_id,
        "name": "Sample CSV Source",
        "source_type": "file",
        "config": {"path": "/data/sample.csv", "format": "csv"},
        "created_at": datetime.utcnow(),
        "is_active": True
    }
    
    # Sample destination
    dest_id = str(uuid.uuid4())
    destinations_storage[dest_id] = {
        "id": dest_id,
        "name": "Sample Database Destination",
        "destination_type": "database",
        "config": {"host": "localhost", "database": "analytics"},
        "created_at": datetime.utcnow(),
        "is_active": True
    }
    
    # Sample job
    job_id = str(uuid.uuid4())
    jobs_storage[job_id] = {
        "id": job_id,
        "source_type": "file",
        "source_config": {"path": "/data/sample.csv"},
        "destination_type": "database",
        "destination_config": {"table": "analytics.raw_data"},
        "status": "completed",
        "progress": 100,
        "created_at": datetime.utcnow(),
        "priority": 5
    }
    
    print("Sample data initialized")


# Initialize sample data on startup
@app.on_event("startup")
async def startup_event():
    """Initialize sample data."""
    populate_sample_data()


if __name__ == "__main__":
    import uvicorn
    
    print(f"Starting {config.app_name} v{config.app_version}")
    print(f"Server: http://{config.host}:{config.port}")
    print(f"API Documentation: http://{config.host}:{config.port}/docs")
    print(f"ReDoc Documentation: http://{config.host}:{config.port}/redoc")
    print("-" * 50)
    
    uvicorn.run(
        "simple_demo:app",
        host=config.host,
        port=config.port,
        reload=True,
        log_level="info",
    )
