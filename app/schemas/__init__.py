from datetime import datetime
from typing import Optional, Dict, Any, List
from uuid import UUID
from enum import Enum

from pydantic import BaseModel, Field, field_validator


class JobType(str, Enum):
    """Job types for the enhanced job system."""
    FETCH_SINGLE_MATERIAL = "fetch_single_material"
    BULK_FETCH_BY_FORMULA = "bulk_fetch_by_formula"
    BULK_FETCH_BY_PROPERTIES = "bulk_fetch_by_properties"
    SYNC_DATABASE = "sync_database"
    
    # Legacy job type
    DATA_INGESTION = "data_ingestion"


class JobPriority(int, Enum):
    """Job priority levels."""
    LOW = 1
    NORMAL = 5
    HIGH = 8
    CRITICAL = 10


class BaseSchema(BaseModel):
    """Base schema with common configuration."""
    
    class Config:
        from_attributes = True
        use_enum_values = True
        json_encoders = {
            datetime: lambda v: v.isoformat(),
            UUID: lambda v: str(v)
        }


class HealthCheckResponse(BaseSchema):
    """Health check response schema."""
    status: str
    timestamp: datetime = Field(default_factory=datetime.utcnow)
    services: Dict[str, str] = Field(default_factory=dict)
    version: str = "1.0.0"


class JobCreate(BaseSchema):
    """Schema for creating a new ingestion job."""
    job_type: JobType = Field(..., description="Type of job to create")
    source_type: str = Field(..., description="Type of data source (e.g., 'jarvis', 'materials_project')")
    source_config: Dict[str, Any] = Field(..., description="Source configuration")
    destination_type: str = Field(default="database", description="Type of destination")
    destination_config: Optional[Dict[str, Any]] = Field(default=None, description="Destination configuration")
    priority: JobPriority = Field(default=JobPriority.NORMAL, description="Job priority")
    batch_size: int = Field(default=100, ge=1, le=10000, description="Batch size for bulk operations")
    retry_count: int = Field(default=3, ge=0, le=10, description="Number of retries on failure")
    dependencies: Optional[List[UUID]] = Field(default=None, description="Job dependencies (job IDs)")
    schedule_config: Optional[Dict[str, Any]] = Field(default=None, description="Recurring job configuration")
    metadata: Optional[Dict[str, Any]] = Field(default=None, description="Additional metadata")
    
    @field_validator("source_type")
    def validate_source_type(cls, v):
        allowed_types = ["jarvis", "nomad", "materials_project", "aflow", "file", "database", "api", "stream"]
        if v not in allowed_types:
            raise ValueError(f"Source type must be one of: {allowed_types}")
        return v
    
    @field_validator("destination_type")
    def validate_destination_type(cls, v):
        allowed_types = ["database", "file", "api", "warehouse"]
        if v not in allowed_types:
            raise ValueError(f"Destination type must be one of: {allowed_types}")
        return v
    
    @field_validator("source_config")
    def validate_source_config(cls, v, info):
        job_type = info.data.get("job_type")
        
        if job_type == JobType.FETCH_SINGLE_MATERIAL:
            if "material_id" not in v:
                raise ValueError("material_id is required for single material fetch")
        
        elif job_type == JobType.BULK_FETCH_BY_FORMULA:
            if "formulas" not in v and "formula_pattern" not in v:
                raise ValueError("formulas or formula_pattern is required for bulk fetch by formula")
        
        elif job_type == JobType.BULK_FETCH_BY_PROPERTIES:
            if "property_filters" not in v:
                raise ValueError("property_filters is required for bulk fetch by properties")
        
        elif job_type == JobType.SYNC_DATABASE:
            if "dataset" not in v:
                raise ValueError("dataset is required for database sync")
        
        return v


class JobResponse(BaseSchema):
    """Schema for job response."""
    id: UUID
    job_type: str
    source_type: str
    source_config: Dict[str, Any]
    destination_type: str
    destination_config: Optional[Dict[str, Any]]
    status: str
    progress: int
    total_records: int
    processed_records: int
    error_count: int
    error_message: Optional[str]
    batch_size: int
    retry_count: int
    current_retry: int
    processing_rate: Optional[float]  # items per second
    estimated_completion: Optional[datetime]
    dependencies: Optional[List[UUID]]
    schedule_config: Optional[Dict[str, Any]]
    created_at: datetime
    updated_at: datetime
    started_at: Optional[datetime]
    completed_at: Optional[datetime]
    created_by: Optional[str]
    metadata: Optional[Dict[str, Any]]


class JobStatus(BaseSchema):
    """Schema for job status."""
    job_id: UUID
    status: str
    progress: int = Field(ge=0, le=100)
    message: Optional[str] = None
    updated_at: datetime = Field(default_factory=datetime.utcnow)


class JobProgress(BaseSchema):
    """Schema for job progress update."""
    processed_records: int = Field(ge=0)
    total_records: int = Field(ge=0)
    error_count: int = Field(ge=0, default=0)
    current_batch: int = Field(ge=0, default=0)
    processing_rate: Optional[float] = Field(default=None, description="Items per second")
    estimated_completion: Optional[datetime] = Field(default=None)
    status: Optional[str] = None
    message: Optional[str] = None
    
    @field_validator("processed_records")
    def validate_progress(cls, v, info):
        total = info.data.get("total_records", 0)
        if total > 0 and v > total:
            raise ValueError("Processed records cannot exceed total records")
        return v


class ScheduleConfig(BaseSchema):
    """Schema for job scheduling configuration."""
    enabled: bool = True
    cron_expression: Optional[str] = Field(None, description="Cron expression for scheduling")
    interval_seconds: Optional[int] = Field(None, ge=60, description="Interval in seconds (minimum 60)")
    max_runs: Optional[int] = Field(None, ge=1, description="Maximum number of runs")
    next_run: Optional[datetime] = Field(None, description="Next scheduled run")
    
    @field_validator("cron_expression")
    def validate_schedule_cron(cls, v, info):
        interval = info.data.get("interval_seconds")
        if v is not None and interval is not None:
            raise ValueError("Cannot specify both cron_expression and interval_seconds")
        return v

    @field_validator("interval_seconds")
    def validate_schedule_interval(cls, v, info):
        cron = info.data.get("cron_expression")
        if v is not None and cron is not None:
            raise ValueError("Cannot specify both cron_expression and interval_seconds")
        return v


class JobDependency(BaseSchema):
    """Schema for job dependencies."""
    job_id: UUID
    required_status: str = "completed"
    timeout_minutes: Optional[int] = Field(default=60, ge=1)


class JobStats(BaseSchema):
    """Schema for job execution statistics."""
    total_jobs: int = Field(ge=0)
    queued_jobs: int = Field(ge=0)
    processing_jobs: int = Field(ge=0)
    completed_jobs: int = Field(ge=0)
    failed_jobs: int = Field(ge=0)
    cancelled_jobs: int = Field(ge=0)
    avg_processing_time: Optional[float] = Field(default=None, description="Average processing time in seconds")
    success_rate: Optional[float] = Field(default=None, ge=0, le=100, description="Success rate percentage")
    timestamp: datetime = Field(default_factory=datetime.utcnow)


class DataSourceCreate(BaseSchema):
    """Schema for creating a data source."""
    name: str = Field(..., min_length=1, max_length=100)
    description: Optional[str] = Field(None, max_length=500)
    source_type: str
    connection_config: Dict[str, Any]
    tags: Optional[List[str]] = Field(default=None)
    
    @field_validator("name")
    def validate_name(cls, v):
        if not v.strip():
            raise ValueError("Name cannot be empty")
        return v.strip()


class DataSourceResponse(BaseSchema):
    """Schema for data source response."""
    id: UUID
    name: str
    description: Optional[str]
    source_type: str
    connection_config: Dict[str, Any]
    is_active: bool
    created_at: datetime
    updated_at: datetime
    created_by: Optional[str]
    tags: Optional[List[str]]


class DataDestinationCreate(BaseSchema):
    """Schema for creating a data destination."""
    name: str = Field(..., min_length=1, max_length=100)
    description: Optional[str] = Field(None, max_length=500)
    destination_type: str
    connection_config: Dict[str, Any]
    tags: Optional[List[str]] = Field(default=None)


class DataDestinationResponse(BaseSchema):
    """Schema for data destination response."""
    id: UUID
    name: str
    description: Optional[str]
    destination_type: str
    connection_config: Dict[str, Any]
    is_active: bool
    created_at: datetime
    updated_at: datetime
    created_by: Optional[str]
    tags: Optional[List[str]]


class QueueStats(BaseSchema):
    """Schema for queue statistics."""
    pending_jobs: int = Field(ge=0)
    delayed_jobs: int = Field(ge=0)
    total_jobs: int = Field(ge=0)
    timestamp: datetime = Field(default_factory=datetime.utcnow)


class JobLogCreate(BaseSchema):
    """Schema for creating a job log entry."""
    job_id: UUID
    level: str = Field(..., pattern="^(DEBUG|INFO|WARNING|ERROR|CRITICAL)$")
    message: str = Field(..., min_length=1)
    metadata: Optional[Dict[str, Any]] = None


class JobLogResponse(BaseSchema):
    """Schema for job log response."""
    id: UUID
    job_id: UUID
    level: str
    message: str
    timestamp: datetime
    metadata: Optional[Dict[str, Any]]


class PaginatedResponse(BaseSchema):
    """Schema for paginated responses."""
    items: List[Any]
    total: int = Field(ge=0)
    page: int = Field(ge=1)
    per_page: int = Field(ge=1, le=100)
    pages: int = Field(ge=1)
    has_next: bool
    has_prev: bool


class ErrorResponse(BaseSchema):
    """Schema for error responses."""
    error: str
    message: str
    timestamp: datetime = Field(default_factory=datetime.utcnow)
    details: Optional[Dict[str, Any]] = None
