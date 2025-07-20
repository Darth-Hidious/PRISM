"""
Base database connector interface for external materials databases.

This module provides a comprehensive abstract base class for all materials database
connectors with built-in rate limiting, caching, error handling, and data standardization.
"""

import asyncio
import json
import logging
import time
from abc import ABC, abstractmethod
from datetime import datetime, timedelta
from typing import Any, Dict, List, Optional, Union, Callable
from dataclasses import dataclass, asdict
from enum import Enum

import httpx
from tenacity import (
    retry,
    stop_after_attempt, 
    wait_exponential,
    retry_if_exception_type,
    RetryError
)

# Import rate limiter components
from .rate_limiter import RateLimiter, TokenBucket


logger = logging.getLogger(__name__)


class ConnectorStatus(Enum):
    """Connector status enumeration."""
    DISCONNECTED = "disconnected"
    CONNECTING = "connecting"
    CONNECTED = "connected"
    ERROR = "error"


@dataclass
class ConnectorMetrics:
    """Metrics collection for connector performance."""
    total_requests: int = 0
    successful_requests: int = 0
    failed_requests: int = 0
    cache_hits: int = 0
    cache_misses: int = 0
    total_latency: float = 0.0
    last_request_time: Optional[datetime] = None
    error_count_by_type: Dict[str, int] = None
    
    def __post_init__(self):
        if self.error_count_by_type is None:
            self.error_count_by_type = {}
    
    @property
    def success_rate(self) -> float:
        """Calculate success rate percentage."""
        if self.total_requests == 0:
            return 0.0
        return (self.successful_requests / self.total_requests) * 100
    
    @property
    def average_latency(self) -> float:
        """Calculate average latency in seconds."""
        if self.successful_requests == 0:
            return 0.0
        return self.total_latency / self.successful_requests
    
    @property
    def cache_hit_rate(self) -> float:
        """Calculate cache hit rate percentage."""
        total_cache_ops = self.cache_hits + self.cache_misses
        if total_cache_ops == 0:
            return 0.0
        return (self.cache_hits / total_cache_ops) * 100


@dataclass
class MaterialStructure:
    """Standardized material structure representation."""
    lattice_parameters: List[List[float]]  # 3x3 lattice matrix
    atomic_positions: List[List[float]]    # Fractional coordinates
    atomic_species: List[str]              # Element symbols
    space_group: Optional[str] = None
    crystal_system: Optional[str] = None
    volume: Optional[float] = None
    
    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)


@dataclass
class MaterialProperties:
    """Standardized material properties representation."""
    formation_energy: Optional[float] = None
    energy_above_hull: Optional[float] = None
    band_gap: Optional[float] = None
    bulk_modulus: Optional[float] = None
    shear_modulus: Optional[float] = None
    elastic_tensor: Optional[List[List[float]]] = None
    magnetic_moment: Optional[float] = None
    thermal_properties: Optional[Dict[str, float]] = None
    electronic_properties: Optional[Dict[str, float]] = None
    
    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return {k: v for k, v in asdict(self).items() if v is not None}


@dataclass
class MaterialMetadata:
    """Standardized material metadata."""
    fetched_at: datetime
    version: str
    source_url: Optional[str] = None
    last_updated: Optional[datetime] = None
    confidence_score: Optional[float] = None
    experimental: bool = False
    
    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        data = asdict(self)
        # Convert datetime objects to ISO strings
        data['fetched_at'] = self.fetched_at.isoformat()
        if self.last_updated:
            data['last_updated'] = self.last_updated.isoformat()
        return data


@dataclass
class StandardizedMaterial:
    """Standardized material data schema that all connectors must conform to."""
    source_db: str
    source_id: str
    formula: str
    structure: MaterialStructure
    properties: MaterialProperties
    metadata: MaterialMetadata
    
    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return {
            'source_db': self.source_db,
            'source_id': self.source_id,
            'formula': self.formula,
            'structure': self.structure.to_dict(),
            'properties': self.properties.to_dict(),
            'metadata': self.metadata.to_dict()
        }
    
    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> 'StandardizedMaterial':
        """Create from dictionary data."""
        structure = MaterialStructure(**data['structure'])
        properties = MaterialProperties(**data['properties'])
        
        # Handle datetime conversion for metadata
        metadata_data = data['metadata'].copy()
        metadata_data['fetched_at'] = datetime.fromisoformat(metadata_data['fetched_at'])
        if metadata_data.get('last_updated'):
            metadata_data['last_updated'] = datetime.fromisoformat(metadata_data['last_updated'])
        
        metadata = MaterialMetadata(**metadata_data)
        
        return cls(
            source_db=data['source_db'],
            source_id=data['source_id'],
            formula=data['formula'],
            structure=structure,
            properties=properties,
            metadata=metadata
        )


class CacheEntry:
    """Cache entry with TTL support."""
    
    def __init__(self, data: Any, ttl_seconds: int = 3600):
        self.data = data
        self.created_at = datetime.now()
        self.ttl = timedelta(seconds=ttl_seconds)
    
    @property
    def is_expired(self) -> bool:
        """Check if cache entry has expired."""
        return datetime.now() > self.created_at + self.ttl


class DatabaseConnector(ABC):
    """
    Abstract base class for external materials database connectors.
    
    Provides common functionality including:
    - Rate limiting with token bucket algorithm
    - Exponential backoff retry logic  
    - Response caching with TTL
    - Error handling and logging
    - Metrics collection
    - Data standardization
    """
    
    def __init__(
        self,
        base_url: str,
        timeout: int = 30,
        requests_per_second: float = 2.0,
        burst_capacity: int = 10,
        cache_ttl: int = 3600,
        max_retries: int = 3,
        redis_client: Optional[Any] = None  # Redis client for distributed rate limiting
    ):
        """
        Initialize database connector.
        
        Args:
            base_url: Base URL for the database API
            timeout: Request timeout in seconds
            requests_per_second: Rate limit for API requests
            burst_capacity: Maximum burst requests allowed
            cache_ttl: Cache time-to-live in seconds
            max_retries: Maximum retry attempts for failed requests
            redis_client: Optional Redis client for distributed rate limiting
        """
        self.base_url = base_url.rstrip('/')
        self.timeout = timeout
        self.cache_ttl = cache_ttl
        self.max_retries = max_retries
        
        # Status tracking
        self.status = ConnectorStatus.DISCONNECTED
        self.last_error: Optional[Exception] = None
        
        # HTTP client
        self._client: Optional[httpx.AsyncClient] = None
        
        # Rate limiting
        self.rate_limiter = RateLimiter()
        self.rate_limiter.add_bucket(
            "default",
            capacity=burst_capacity,
            refill_rate=requests_per_second
        )
        
        # Caching
        self._cache: Dict[str, CacheEntry] = {}
        self._cache_lock = asyncio.Lock()
        
        # Metrics
        self.metrics = ConnectorMetrics()
        
        # Redis for distributed features (optional)
        self.redis_client = redis_client
        
        logger.info(f"Initialized {self.__class__.__name__} connector")
    
    # Abstract methods that must be implemented by subclasses
    
    @abstractmethod
    async def connect(self) -> bool:
        """
        Establish connection to the external database.
        
        Returns:
            bool: True if connection successful, False otherwise
        """
        pass
    
    @abstractmethod
    async def disconnect(self) -> None:
        """Close connection to the external database."""
        pass
    
    @abstractmethod
    async def search_materials(
        self,
        query: Dict[str, Any],
        limit: int = 100,
        offset: int = 0
    ) -> List[StandardizedMaterial]:
        """
        Search for materials based on query criteria.
        
        Args:
            query: Search criteria (formula, properties, etc.)
            limit: Maximum number of results
            offset: Number of results to skip
            
        Returns:
            List of standardized materials
        """
        pass
    
    @abstractmethod
    async def get_material_by_id(self, material_id: str) -> StandardizedMaterial:
        """
        Get a specific material by its database ID.
        
        Args:
            material_id: Database-specific material identifier
            
        Returns:
            Standardized material data
        """
        pass
    
    @abstractmethod
    async def fetch_bulk_materials(
        self,
        limit: int = 100,
        offset: int = 0,
        filters: Optional[Dict[str, Any]] = None
    ) -> List[StandardizedMaterial]:
        """
        Fetch materials in bulk with optional filtering.
        
        Args:
            limit: Maximum number of materials to fetch
            offset: Number of materials to skip
            filters: Optional filtering criteria
            
        Returns:
            List of standardized materials
        """
        pass
    
    @abstractmethod
    async def validate_response(self, response: Dict[str, Any]) -> bool:
        """
        Validate response data from the external database.
        
        Args:
            response: Raw response data
            
        Returns:
            True if response is valid, False otherwise
        """
        pass
    
    @abstractmethod
    async def standardize_data(self, raw_data: Dict[str, Any]) -> StandardizedMaterial:
        """
        Convert raw database response to standardized format.
        
        Args:
            raw_data: Raw material data from database
            
        Returns:
            Standardized material object
        """
        pass
    
    # Common functionality provided by base class
    
    async def health_check(self) -> bool:
        """
        Check if the external database is accessible.
        
        Returns:
            True if database is healthy, False otherwise
        """
        try:
            if not self._client:
                await self.connect()
            
            start_time = time.time()
            response = await self._make_request("GET", "/health", timeout=5.0)
            latency = time.time() - start_time
            
            self._update_metrics(True, latency)
            return response.status_code == 200
            
        except Exception as e:
            logger.warning(f"Health check failed: {e}")
            self._update_metrics(False, 0.0, e)
            return False
    
    async def _make_request(
        self,
        method: str,
        endpoint: str,
        params: Optional[Dict[str, Any]] = None,
        data: Optional[Dict[str, Any]] = None,
        timeout: Optional[float] = None
    ) -> httpx.Response:
        """
        Make HTTP request with rate limiting and retry logic.
        
        Args:
            method: HTTP method (GET, POST, etc.)
            endpoint: API endpoint
            params: Query parameters
            data: Request body data
            timeout: Request timeout override
            
        Returns:
            HTTP response object
        """
        # Apply rate limiting
        await self.rate_limiter.wait_for_permit("default")
        
        if not self._client:
            await self.connect()
        
        url = f"{self.base_url}{endpoint}"
        request_timeout = timeout or self.timeout
        
        # Use tenacity for retry logic
        @retry(
            stop=stop_after_attempt(self.max_retries),
            wait=wait_exponential(multiplier=1, min=2, max=10),
            retry=retry_if_exception_type((
                httpx.TimeoutException,
                httpx.ConnectError,
                httpx.RemoteProtocolError
            ))
        )
        async def _make_request_with_retry():
            return await self._client.request(
                method=method,
                url=url,
                params=params,
                json=data,
                timeout=request_timeout
            )
        
        try:
            return await _make_request_with_retry()
        except RetryError as e:
            logger.error(f"Request failed after {self.max_retries} retries: {e}")
            raise ConnectorTimeoutException(f"Request timeout after retries: {e}")
    
    async def _get_cached(self, cache_key: str) -> Optional[Any]:
        """
        Get data from cache if available and not expired.
        
        Args:
            cache_key: Cache key
            
        Returns:
            Cached data or None if not found/expired
        """
        async with self._cache_lock:
            entry = self._cache.get(cache_key)
            if entry and not entry.is_expired:
                self.metrics.cache_hits += 1
                logger.debug(f"Cache hit for key: {cache_key}")
                return entry.data
            elif entry:
                # Remove expired entry
                del self._cache[cache_key]
            
            self.metrics.cache_misses += 1
            return None
    
    async def _set_cached(self, cache_key: str, data: Any) -> None:
        """
        Store data in cache with TTL.
        
        Args:
            cache_key: Cache key
            data: Data to cache
        """
        async with self._cache_lock:
            self._cache[cache_key] = CacheEntry(data, self.cache_ttl)
            logger.debug(f"Cached data for key: {cache_key}")
    
    def _update_metrics(
        self,
        success: bool,
        latency: float,
        error: Optional[Exception] = None
    ) -> None:
        """
        Update connector metrics.
        
        Args:
            success: Whether request was successful
            latency: Request latency in seconds
            error: Exception if request failed
        """
        self.metrics.total_requests += 1
        self.metrics.last_request_time = datetime.now()
        
        if success:
            self.metrics.successful_requests += 1
            self.metrics.total_latency += latency
        else:
            self.metrics.failed_requests += 1
            if error:
                error_type = error.__class__.__name__
                self.metrics.error_count_by_type[error_type] = (
                    self.metrics.error_count_by_type.get(error_type, 0) + 1
                )
    
    async def get_metrics(self) -> Dict[str, Any]:
        """
        Get connector performance metrics.
        
        Returns:
            Dictionary of performance metrics
        """
        return {
            'connector': self.__class__.__name__,
            'status': self.status.value,
            'total_requests': self.metrics.total_requests,
            'success_rate': self.metrics.success_rate,
            'average_latency': self.metrics.average_latency,
            'cache_hit_rate': self.metrics.cache_hit_rate,
            'last_request': self.metrics.last_request_time.isoformat() if self.metrics.last_request_time else None,
            'error_breakdown': self.metrics.error_count_by_type.copy(),
            'cache_size': len(self._cache)
        }
    
    async def clear_cache(self) -> None:
        """Clear all cached data."""
        async with self._cache_lock:
            self._cache.clear()
            logger.info("Cache cleared")
    
    async def cleanup_expired_cache(self) -> int:
        """
        Remove expired cache entries.
        
        Returns:
            Number of entries removed
        """
        removed_count = 0
        async with self._cache_lock:
            expired_keys = [
                key for key, entry in self._cache.items()
                if entry.is_expired
            ]
            for key in expired_keys:
                del self._cache[key]
                removed_count += 1
        
        if removed_count > 0:
            logger.debug(f"Removed {removed_count} expired cache entries")
        
        return removed_count
    
    # Convenience methods that delegate to abstract methods
    
    async def search(self, **kwargs) -> List[StandardizedMaterial]:
        """Generic search interface (delegates to search_materials)."""
        return await self.search_materials(kwargs)
    
    async def get_by_id(self, record_id: str) -> StandardizedMaterial:
        """Generic get by ID interface (delegates to get_material_by_id)."""
        return await self.get_material_by_id(record_id)
    
    async def fetch_bulk(self, limit: int = 100, offset: int = 0) -> List[StandardizedMaterial]:
        """Generic bulk fetch interface (delegates to fetch_bulk_materials)."""
        return await self.fetch_bulk_materials(limit, offset)


# Exception hierarchy

class ConnectorException(Exception):
    """Base exception for connector errors."""
    pass


class ConnectorTimeoutException(ConnectorException):
    """Exception raised when connector requests timeout."""
    pass


class ConnectorRateLimitException(ConnectorException):
    """Exception raised when rate limit is exceeded."""
    pass


class ConnectorNotFoundException(ConnectorException):
    """Exception raised when requested resource is not found."""
    pass


class ConnectorAuthenticationException(ConnectorException):
    """Exception raised when authentication fails."""
    pass


class ConnectorValidationException(ConnectorException):
    """Exception raised when response validation fails."""
    pass


class ConnectorDataException(ConnectorException):
    """Exception raised when data standardization fails."""
    pass
