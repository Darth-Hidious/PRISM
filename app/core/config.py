import logging
import sys
import os
from functools import lru_cache
from typing import Optional, List, Any

from pydantic import Field, field_validator, ValidationError, computed_field
from pydantic_settings import BaseSettings, SettingsConfigDict


class Settings(BaseSettings):
    model_config = SettingsConfigDict(
        env_file=".env",
        env_file_encoding="utf-8",
        case_sensitive=False,
        extra="ignore",
        # Add validation alias to handle malformed env vars gracefully
        validate_assignment=True
    )

    # App settings
    app_name: str = Field(default="Data Ingestion Microservice", description="Application name")
    app_version: str = Field(default="1.0.0", description="Application version")
    debug: bool = Field(default=False, description="Debug mode")
    environment: str = Field(default="development", description="Environment")
    
    # Server settings
    host: str = Field(default="0.0.0.0", description="Server host")
    port: int = Field(default=8000, description="Server port")
    reload: bool = Field(default=True, description="Auto-reload on code changes")
    
    # CORS settings with better defaults and validation
    cors_origins: List[str] = Field(
        default=["http://localhost:3000", "http://localhost:8080"],
        description="Allowed CORS origins"
    )
    cors_allow_credentials: bool = Field(default=True, description="Allow credentials in CORS")
    cors_allow_methods: str = Field(default="*", description="Allowed CORS methods")
    cors_allow_headers: str = Field(default="*", description="Allowed CORS headers")
    
    # Database settings
    postgres_server: str = Field(default="localhost", description="PostgreSQL server host")
    postgres_user: str = Field(default="postgres", description="PostgreSQL username")
    postgres_password: str = Field(default="password", description="PostgreSQL password")
    postgres_db: str = Field(default="data_ingestion", description="PostgreSQL database name")
    postgres_port: int = Field(default=5432, description="PostgreSQL port")
    postgres_echo: bool = Field(default=False, description="Echo SQL queries")
    
    # Redis settings
    redis_host: str = Field(default="localhost", description="Redis host")
    redis_port: int = Field(default=6379, description="Redis port")
    redis_password: Optional[str] = Field(default=None, description="Redis password")
    redis_db: int = Field(default=0, description="Redis database number")
    redis_decode_responses: bool = Field(default=True, description="Decode Redis responses")
    
    # Logging settings
    log_level: str = Field(default="INFO", description="Log level")
    log_format: str = Field(
        default="%(asctime)s - %(name)s - %(levelname)s - %(message)s",
        description="Log format"
    )
    
    # Security settings
    secret_key: str = Field(
        default="your-secret-key-change-this-in-production",
        description="Secret key for JWT tokens"
    )
    access_token_expire_minutes: int = Field(
        default=30,
        description="Access token expiration time in minutes"
    )
    
    # Job queue settings
    max_retries: int = Field(default=3, description="Maximum job retries")
    retry_delay: int = Field(default=60, description="Retry delay in seconds")
    batch_size: int = Field(default=50, description="Default batch size for job processing")
    job_timeout: int = Field(default=3600, description="Job timeout in seconds")
    max_concurrent_jobs: int = Field(default=5, description="Maximum concurrent jobs")
    job_cleanup_interval: int = Field(default=300, description="Job cleanup interval in seconds")
    
    # Database connector settings
    # JARVIS settings
    jarvis_base_url: str = Field(default="https://jarvis.nist.gov/", description="JARVIS API base URL")
    jarvis_rate_limit: int = Field(default=100, description="JARVIS API rate limit (requests per minute)")
    jarvis_burst_size: int = Field(default=20, description="JARVIS API burst size")
    jarvis_timeout: int = Field(default=30, description="JARVIS API timeout in seconds")
    jarvis_retry_count: int = Field(default=3, description="JARVIS API retry count")
    
    # NOMAD settings
    nomad_base_url: str = Field(default="https://nomad-lab.eu/prod/v1/api/v1", description="NOMAD API base URL")
    nomad_rate_limit: int = Field(default=60, description="NOMAD API rate limit (requests per minute)")
    nomad_burst_size: int = Field(default=10, description="NOMAD API burst size")
    nomad_api_key: Optional[str] = Field(default=None, description="NOMAD API key (optional)")
    nomad_timeout: int = Field(default=45, description="NOMAD API timeout in seconds")
    nomad_retry_count: int = Field(default=3, description="NOMAD API retry count")
    
    # OQMD settings
    oqmd_base_url: str = Field(default="http://oqmd.org/api", description="OQMD API base URL")
    oqmd_rate_limit: int = Field(default=30, description="OQMD API rate limit (requests per minute)")
    oqmd_burst_size: int = Field(default=5, description="OQMD API burst size")
    oqmd_timeout: int = Field(default=30, description="OQMD API timeout in seconds")
    oqmd_retry_count: int = Field(default=3, description="OQMD API retry count")
    
    # Rate limiting configuration
    rate_limiter_enabled: bool = Field(default=True, description="Enable rate limiting")
    rate_limiter_backend: str = Field(default="redis", description="Rate limiter backend")
    rate_limiter_default_limit: int = Field(default=100, description="Default rate limit")
    rate_limiter_default_period: int = Field(default=60, description="Default rate limit period")
    rate_limiter_adaptive: bool = Field(default=True, description="Enable adaptive rate limiting")
    
    # API configuration
    api_host: str = Field(default="0.0.0.0", description="API host")
    api_port: int = Field(default=8000, description="API port")
    api_workers: int = Field(default=4, description="Number of API workers")
    api_reload: bool = Field(default=False, description="API auto-reload")
    api_access_log: bool = Field(default=True, description="Enable API access logging")
    
    # Data processing settings
    data_cache_ttl: int = Field(default=3600, description="Data cache TTL in seconds")
    data_cache_size: int = Field(default=1000, description="Data cache size")
    enable_data_validation: bool = Field(default=True, description="Enable data validation")
    enable_data_transformation: bool = Field(default=True, description="Enable data transformation")
    
    # Performance tuning
    http_timeout: int = Field(default=30, description="HTTP timeout in seconds")
    http_max_connections: int = Field(default=100, description="Maximum HTTP connections")
    http_max_keepalive_connections: int = Field(default=20, description="Maximum HTTP keepalive connections")
    http_keepalive_expiry: int = Field(default=30, description="HTTP keepalive expiry in seconds")
    
    # Monitoring settings
    enable_metrics: bool = Field(default=True, description="Enable metrics collection")
    metrics_port: int = Field(default=9090, description="Metrics endpoint port")
    log_file_path: str = Field(default="logs/prism.log", description="Log file path")
    log_file_max_size: str = Field(default="10MB", description="Log file maximum size")
    log_file_backup_count: int = Field(default=5, description="Log file backup count")
    
    # Development settings
    development_mode: bool = Field(default=False, description="Development mode")
    mock_external_apis: bool = Field(default=False, description="Mock external APIs")
    enable_profiling: bool = Field(default=False, description="Enable profiling")
    
    # CLI configuration
    cli_default_output_format: str = Field(default="json", description="CLI default output format")
    cli_default_batch_size: int = Field(default=10, description="CLI default batch size")
    cli_progress_bar: bool = Field(default=True, description="CLI progress bar enabled")
    cli_color_output: bool = Field(default=True, description="CLI color output enabled")


class SettingsNoEnv(BaseSettings):
    """Settings class that doesn't load from environment - for fallback use."""
    model_config = SettingsConfigDict(
        # No env_file, no environment loading
        case_sensitive=False,
        extra="ignore"
    )

    # Copy all the same fields but without environment loading
    app_name: str = "Data Ingestion Microservice"
    app_version: str = "1.0.0"
    debug: bool = True
    environment: str = "development"
    host: str = "0.0.0.0"
    port: int = 8000
    reload: bool = True
    cors_origins: List[str] = ["http://localhost:3000", "http://localhost:8080"]
    cors_allow_credentials: bool = True
    cors_allow_methods: List[str] = ["*"]
    cors_allow_headers: List[str] = ["*"]
    
    # Database settings
    postgres_server: str = Field(default="localhost", description="PostgreSQL server host")
    postgres_user: str = Field(default="postgres", description="PostgreSQL username")
    postgres_password: str = Field(default="password", description="PostgreSQL password")
    postgres_db: str = Field(default="data_ingestion", description="PostgreSQL database name")
    postgres_port: int = Field(default=5432, description="PostgreSQL port")
    postgres_echo: bool = Field(default=False, description="Echo SQL queries")
    
    # Redis settings
    redis_host: str = Field(default="localhost", description="Redis host")
    redis_port: int = Field(default=6379, description="Redis port")
    redis_password: Optional[str] = Field(default=None, description="Redis password")
    redis_db: int = Field(default=0, description="Redis database number")
    redis_decode_responses: bool = Field(default=True, description="Decode Redis responses")
    
    # Logging settings
    log_level: str = Field(default="INFO", description="Logging level")
    log_format: str = Field(
        default="%(asctime)s - %(name)s - %(levelname)s - %(message)s",
        description="Log format"
    )
    
    # Security settings
    secret_key: str = Field(
        default="your-secret-key-change-this-in-production",
        description="Secret key for JWT tokens"
    )
    access_token_expire_minutes: int = Field(
        default=30,
        description="Access token expiration time in minutes"
    )
    
    # Job queue settings
    max_retries: int = Field(default=3, description="Maximum job retries")
    retry_delay: int = Field(default=60, description="Retry delay in seconds")
    batch_size: int = Field(default=50, description="Default batch size for job processing")
    job_timeout: int = Field(default=3600, description="Job timeout in seconds")
    max_concurrent_jobs: int = Field(default=5, description="Maximum concurrent jobs")
    job_cleanup_interval: int = Field(default=300, description="Job cleanup interval in seconds")
    
    # Database connector settings
    jarvis_base_url: str = Field(default="https://jarvis.nist.gov/", description="JARVIS API base URL")
    jarvis_rate_limit: int = Field(default=100, description="JARVIS API rate limit")
    jarvis_burst_size: int = Field(default=20, description="JARVIS API burst size")
    jarvis_timeout: int = Field(default=30, description="JARVIS API timeout")
    jarvis_retry_count: int = Field(default=3, description="JARVIS API retry count")
    
    nomad_base_url: str = Field(default="https://nomad-lab.eu/prod/v1/api/v1", description="NOMAD API base URL")
    nomad_rate_limit: int = Field(default=60, description="NOMAD API rate limit")
    nomad_burst_size: int = Field(default=10, description="NOMAD API burst size")
    nomad_api_key: Optional[str] = Field(default=None, description="NOMAD API key")
    nomad_timeout: int = Field(default=45, description="NOMAD API timeout")
    nomad_retry_count: int = Field(default=3, description="NOMAD API retry count")
    
    oqmd_base_url: str = Field(default="http://oqmd.org/api", description="OQMD API base URL")
    oqmd_rate_limit: int = Field(default=30, description="OQMD API rate limit")
    oqmd_burst_size: int = Field(default=5, description="OQMD API burst size")
    oqmd_timeout: int = Field(default=30, description="OQMD API timeout")
    oqmd_retry_count: int = Field(default=3, description="OQMD API retry count")
    
    # Additional settings
    rate_limiter_enabled: bool = Field(default=True, description="Enable rate limiting")
    rate_limiter_backend: str = Field(default="redis", description="Rate limiter backend")
    rate_limiter_default_limit: int = Field(default=100, description="Default rate limit")
    rate_limiter_default_period: int = Field(default=60, description="Default rate limit period")
    rate_limiter_adaptive: bool = Field(default=True, description="Enable adaptive rate limiting")
    
    api_host: str = Field(default="0.0.0.0", description="API host")
    api_port: int = Field(default=8000, description="API port")
    api_workers: int = Field(default=4, description="API workers")
    api_reload: bool = Field(default=False, description="API auto-reload")
    api_access_log: bool = Field(default=True, description="API access logging")
    
    data_cache_ttl: int = Field(default=3600, description="Data cache TTL")
    data_cache_size: int = Field(default=1000, description="Data cache size")
    enable_data_validation: bool = Field(default=True, description="Enable data validation")
    enable_data_transformation: bool = Field(default=True, description="Enable data transformation")
    
    http_timeout: int = Field(default=30, description="HTTP timeout")
    http_max_connections: int = Field(default=100, description="Max HTTP connections")
    http_max_keepalive_connections: int = Field(default=20, description="Max HTTP keepalive connections")
    http_keepalive_expiry: int = Field(default=30, description="HTTP keepalive expiry")
    
    enable_metrics: bool = Field(default=True, description="Enable metrics")
    metrics_port: int = Field(default=9090, description="Metrics port")
    log_file_path: str = Field(default="logs/prism.log", description="Log file path")
    log_file_max_size: str = Field(default="10MB", description="Log file max size")
    log_file_backup_count: int = Field(default=5, description="Log file backup count")
    
    development_mode: bool = Field(default=False, description="Development mode")
    mock_external_apis: bool = Field(default=False, description="Mock external APIs")
    enable_profiling: bool = Field(default=False, description="Enable profiling")
    
    cli_default_output_format: str = Field(default="json", description="CLI default output format")
    cli_default_batch_size: int = Field(default=10, description="CLI default batch size")
    cli_progress_bar: bool = Field(default=True, description="CLI progress bar")
    cli_color_output: bool = Field(default=True, description="CLI color output")
    
    @field_validator("cors_origins", mode="before")
    @classmethod
    def parse_cors_origins(cls, v):
        """Parse CORS origins from various formats."""
        if isinstance(v, str):
            # Handle both JSON array strings and comma-separated strings
            if v.startswith('[') and v.endswith(']'):
                try:
                    import json
                    return json.loads(v)
                except (json.JSONDecodeError, ValueError):
                    # Fall back to comma-separated parsing
                    v = v.strip('[]').replace('"', '').replace("'", "")
                    return [origin.strip() for origin in v.split(",") if origin.strip()]
            return [origin.strip() for origin in v.split(",") if origin.strip()]
        elif isinstance(v, list):
            return v
        return ["http://localhost:3000"]  # Safe fallback
    
    @field_validator("cors_allow_methods", mode="before")
    @classmethod
    def parse_cors_methods(cls, v):
        """Parse CORS methods from various formats."""
        if isinstance(v, str):
            # Handle wildcard
            if v.strip() == "*":
                return "*"
            # Handle comma-separated values
            return v.strip()
        elif isinstance(v, list):
            # Convert list back to string for storage
            if "*" in v:
                return "*"
            return ",".join(v)
        return "*"  # Safe fallback
    
    @field_validator("cors_allow_headers", mode="before")
    @classmethod
    def parse_cors_headers(cls, v):
        """Parse CORS headers from various formats."""
        if isinstance(v, str):
            # Handle wildcard
            if v.strip() == "*":
                return "*"
            # Handle comma-separated values
            return v.strip()
        elif isinstance(v, list):
            # Convert list back to string for storage
            if "*" in v:
                return "*"
            return ",".join(v)
        return "*"  # Safe fallback
    
    @field_validator("log_level")
    @classmethod
    def validate_log_level(cls, v):
        valid_levels = ["DEBUG", "INFO", "WARNING", "ERROR", "CRITICAL"]
        if v.upper() not in valid_levels:
            raise ValueError(f"Log level must be one of: {valid_levels}")
        return v.upper()
    
    def get_cors_methods_list(self) -> List[str]:
        """Get CORS methods as list for FastAPI."""
        if self.cors_allow_methods == "*":
            return ["*"]
        return [method.strip() for method in self.cors_allow_methods.split(",") if method.strip()]
    
    def get_cors_headers_list(self) -> List[str]:
        """Get CORS headers as list for FastAPI."""
        if self.cors_allow_headers == "*":
            return ["*"]
        return [header.strip() for header in self.cors_allow_headers.split(",") if header.strip()]

    @property
    def database_url(self) -> str:
        """Construct PostgreSQL database URL."""
        return (
            f"postgresql+asyncpg://{self.postgres_user}:{self.postgres_password}"
            f"@{self.postgres_server}:{self.postgres_port}/{self.postgres_db}"
        )
    
    @property
    def redis_url(self) -> str:
        """Construct Redis URL."""
        auth_part = f":{self.redis_password}@" if self.redis_password else ""
        return f"redis://{auth_part}{self.redis_host}:{self.redis_port}/{self.redis_db}"
    
    def setup_logging(self) -> None:
        """Setup application logging."""
        logging.basicConfig(
            level=getattr(logging, self.log_level),
            format=self.log_format,
            handlers=[
                logging.StreamHandler(sys.stdout),
                logging.FileHandler(f"{self.app_name.lower().replace(' ', '_')}.log")
            ]
        )


def create_development_settings() -> Settings:
    """Create minimal settings for development/testing without environment loading."""
    try:
        # Try to create settings without loading from environment
        return Settings.model_validate({
            "app_name": "Data Ingestion Microservice",
            "app_version": "1.0.0", 
            "debug": True,
            "environment": "development",
            "host": "0.0.0.0",
            "port": 8000,
            "reload": True,
            "cors_origins": ["http://localhost:3000", "http://localhost:8080"],
            "cors_allow_credentials": True,
            "cors_allow_methods": ["*"],
            "cors_allow_headers": ["*"],
            "postgres_server": "localhost",
            "postgres_user": "postgres", 
            "postgres_password": "password",
            "postgres_db": "data_ingestion",
            "postgres_port": 5432,
            "postgres_echo": False,
            "redis_host": "localhost",
            "redis_port": 6379,
            "redis_password": None,
            "redis_db": 0,
            "redis_decode_responses": True,
            "log_level": "INFO",
            "log_format": "%(asctime)s - %(name)s - %(levelname)s - %(message)s",
            "secret_key": "dev-secret-key-change-in-production",
            "access_token_expire_minutes": 30,
            "max_retries": 3,
            "retry_delay": 60,
            "batch_size": 50,
            "job_timeout": 3600,
            "max_concurrent_jobs": 5,
            "job_cleanup_interval": 300,
            "jarvis_base_url": "https://jarvis.nist.gov/",
            "jarvis_rate_limit": 100,
            "jarvis_burst_size": 20,
            "jarvis_timeout": 30,
            "jarvis_retry_count": 3,
            "nomad_base_url": "https://nomad-lab.eu/prod/v1/api/v1",
            "nomad_rate_limit": 60,
            "nomad_burst_size": 10,
            "nomad_api_key": None,
            "nomad_timeout": 45,
            "nomad_retry_count": 3,
            "oqmd_base_url": "http://oqmd.org/api",
            "oqmd_rate_limit": 30,
            "oqmd_burst_size": 5,
            "oqmd_timeout": 30,
            "oqmd_retry_count": 3,
            "rate_limiter_enabled": True,
            "rate_limiter_backend": "redis",
            "rate_limiter_default_limit": 100,
            "rate_limiter_default_period": 60,
            "rate_limiter_adaptive": True,
            "api_host": "0.0.0.0",
            "api_port": 8000,
            "api_workers": 4,
            "api_reload": False,
            "api_access_log": True,
            "data_cache_ttl": 3600,
            "data_cache_size": 1000,
            "enable_data_validation": True,
            "enable_data_transformation": True,
            "http_timeout": 30,
            "http_max_connections": 100,
            "http_max_keepalive_connections": 20,
            "http_keepalive_expiry": 30,
            "enable_metrics": True,
            "metrics_port": 9090,
            "log_file_path": "logs/prism.log",
            "log_file_max_size": "10MB",
            "log_file_backup_count": 5,
            "development_mode": False,
            "mock_external_apis": False,
            "enable_profiling": False,
            "cli_default_output_format": "json",
            "cli_default_batch_size": 10,
            "cli_progress_bar": True,
            "cli_color_output": True
        })
    except Exception as e:
        # Last resort - create a minimal settings object manually
        logging.warning(f"Failed to create validated settings: {e}")
        settings = Settings()
        # Override problematic fields manually
        settings.cors_allow_methods = "*"
        settings.cors_allow_headers = "*"
        settings.cors_origins = ["http://localhost:3000"]
        return settings


@lru_cache()
def get_settings() -> Settings:
    """Get cached settings instance with robust error handling."""
    
    # First, try to load settings normally from environment
    try:
        # Check if we're in a testing/development environment
        is_testing = (
            "pytest" in sys.modules or 
            "test" in sys.argv[0] or
            os.getenv("TESTING") == "true" or
            os.getenv("ENV") == "test"
        )
        
        if is_testing:
            logging.info("Testing environment detected, using development settings")
            return create_development_settings()
        
        # Try normal settings loading
        return Settings()
        
    except ValidationError as e:
        logging.warning(f"Settings validation error: {e}")
        logging.warning("Falling back to development settings")
        return create_development_settings()
        
    except Exception as e:
        logging.error(f"Unexpected error loading settings: {e}")
        logging.warning("Using minimal fallback settings")
        return create_development_settings()


def get_settings_for_production() -> Settings:
    """Get settings for production - will fail fast if configuration is invalid."""
    return Settings()  # No fallbacks - must work or fail
