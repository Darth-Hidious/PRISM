#!/usr/bin/env python3
"""
Completely Standalone Enhanced Database Connector Framework Demonstration

This script demonstrates the enhanced abstract base class for database connectors
with all components copied directly to avoid configuration dependencies.
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

# Setup logging
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)


# ==================== TOKEN BUCKET RATE LIMITER ====================

class TokenBucket:
    """Token bucket implementation for rate limiting."""
    
    def __init__(self, capacity: float, refill_rate: float):
        self.capacity = capacity
        self.tokens = capacity
        self.refill_rate = refill_rate  # tokens per second
        self.last_refill = time.time()
        self._lock = asyncio.Lock()
    
    async def consume(self, tokens: int = 1) -> bool:
        """
        Try to consume tokens from the bucket.
        
        Args:
            tokens: Number of tokens to consume
            
        Returns:
            True if tokens were consumed, False if not enough tokens
        """
        async with self._lock:
            await self._refill()
            if self.tokens >= tokens:
                self.tokens -= tokens
                return True
            return False
    
    async def _refill(self):
        """Refill tokens based on elapsed time."""
        now = time.time()
        elapsed = now - self.last_refill
        tokens_to_add = elapsed * self.refill_rate
        self.tokens = min(self.capacity, self.tokens + tokens_to_add)
        self.last_refill = now
    
    async def wait_for_tokens(self, tokens: int = 1) -> None:
        """Wait until enough tokens are available."""
        while not await self.consume(tokens):
            # Calculate how long to wait for tokens
            async with self._lock:
                await self._refill()
                if self.tokens < tokens:
                    needed_tokens = tokens - self.tokens
                    wait_time = needed_tokens / self.refill_rate
                    await asyncio.sleep(min(wait_time, 1.0))  # Wait max 1 second at a time


class RateLimiter:
    """Rate limiter using multiple token buckets."""
    
    def __init__(self):
        self.buckets: Dict[str, TokenBucket] = {}
    
    def add_bucket(self, name: str, capacity: float, refill_rate: float):
        """Add a new token bucket."""
        self.buckets[name] = TokenBucket(capacity, refill_rate)
    
    async def wait_for_permit(self, bucket_name: str = "default", tokens: int = 1):
        """Wait for permission to proceed (blocks until tokens available)."""
        if bucket_name not in self.buckets:
            raise ValueError(f"Bucket '{bucket_name}' not found")
        
        await self.buckets[bucket_name].wait_for_tokens(tokens)


# ==================== DATA SCHEMAS ====================

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


# ==================== EXCEPTIONS ====================

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


# ==================== CACHE SYSTEM ====================

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


# ==================== ABSTRACT BASE CLASS ====================

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
            response = await self._make_request("GET", "/", timeout=5.0)
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


# ==================== DEMONSTRATION CONNECTOR ====================

class DemoJarvisConnector(DatabaseConnector):
    """
    Demo JARVIS connector for demonstration purposes.
    """
    
    def __init__(self, **kwargs):
        """Initialize demo JARVIS connector."""
        base_url = "https://jarvis.nist.gov"
        super().__init__(base_url=base_url, **kwargs)
        self._client = None
    
    async def connect(self) -> bool:
        """Establish connection to JARVIS API."""
        try:
            self.status = ConnectorStatus.CONNECTING
            
            self._client = httpx.AsyncClient(
                timeout=httpx.Timeout(self.timeout),
                headers={
                    "User-Agent": "PRISM-DataIngestion/1.0",
                    "Accept": "application/json"
                }
            )
            
            # Test connection
            self.status = ConnectorStatus.CONNECTED
            logger.info("Successfully connected to JARVIS database")
            return True
                
        except Exception as e:
            self.status = ConnectorStatus.ERROR
            self.last_error = e
            logger.error(f"Failed to connect to JARVIS: {e}")
            return False
    
    async def disconnect(self) -> None:
        """Close connection to JARVIS."""
        if self._client:
            await self._client.aclose()
            self._client = None
        self.status = ConnectorStatus.DISCONNECTED
        logger.info("Disconnected from JARVIS database")
    
    async def search_materials(
        self,
        query: Dict[str, Any],
        limit: int = 100,
        offset: int = 0
    ) -> List[StandardizedMaterial]:
        """Search for materials in JARVIS database."""
        
        # Create cache key
        cache_key = f"search_{hash(str(sorted(query.items())))}_l{limit}_o{offset}"
        
        # Check cache first
        cached_result = await self._get_cached(cache_key)
        if cached_result is not None:
            logger.info(f"Returning cached search results for {query}")
            return cached_result
        
        try:
            # Simulate API call with demo data
            materials = []
            
            # Create demo materials based on query
            if query.get("formula") == "Si":
                materials = await self._create_demo_silicon_materials(limit)
            elif query.get("formula") == "C":
                materials = await self._create_demo_carbon_materials(limit)
            else:
                materials = await self._create_demo_generic_materials(limit)
            
            # Cache the results
            await self._set_cached(cache_key, materials)
            
            self._update_metrics(True, 0.5)  # Simulate 0.5s latency
            return materials
            
        except Exception as e:
            self._update_metrics(False, 0.0, e)
            raise ConnectorException(f"Search failed: {e}")
    
    async def get_material_by_id(self, material_id: str) -> StandardizedMaterial:
        """Get specific material by ID."""
        cache_key = f"material_{material_id}"
        
        # Check cache
        cached_result = await self._get_cached(cache_key)
        if cached_result is not None:
            return cached_result
        
        # For demo purposes, raise not found for invalid IDs
        if material_id == "invalid_id_12345":
            raise ConnectorNotFoundException(f"Material with ID {material_id} not found")
        
        # Create a demo material
        material = await self._create_demo_material(material_id, "Si2", "silicon")
        
        # Cache the result
        await self._set_cached(cache_key, material)
        
        self._update_metrics(True, 0.3)
        return material
    
    async def fetch_bulk_materials(
        self,
        limit: int = 100,
        offset: int = 0,
        filters: Optional[Dict[str, Any]] = None
    ) -> List[StandardizedMaterial]:
        """Fetch materials in bulk."""
        query = filters or {}
        return await self.search_materials(query, limit, offset)
    
    async def validate_response(self, response: Dict[str, Any]) -> bool:
        """Validate response data."""
        return isinstance(response, dict) and len(response) > 0
    
    async def standardize_data(self, raw_data: Dict[str, Any]) -> StandardizedMaterial:
        """Convert raw JARVIS data to standardized format."""
        return await self._create_demo_material(
            raw_data.get("jid", "demo_001"),
            raw_data.get("formula", "Unknown"),
            raw_data.get("description", "Demo material")
        )
    
    async def _create_demo_silicon_materials(self, count: int) -> List[StandardizedMaterial]:
        """Create demo silicon materials."""
        materials = []
        for i in range(min(count, 3)):
            material = await self._create_demo_material(
                f"JARVIS-{1000 + i}",
                "Si" if i == 0 else f"Si{i+1}",
                f"Silicon structure {i+1}"
            )
            materials.append(material)
        return materials
    
    async def _create_demo_carbon_materials(self, count: int) -> List[StandardizedMaterial]:
        """Create demo carbon materials."""
        materials = []
        structures = ["diamond", "graphite", "graphene"]
        for i in range(min(count, 3)):
            material = await self._create_demo_material(
                f"JARVIS-{2000 + i}",
                "C",
                f"Carbon {structures[i % len(structures)]}"
            )
            materials.append(material)
        return materials
    
    async def _create_demo_generic_materials(self, count: int) -> List[StandardizedMaterial]:
        """Create demo generic materials."""
        materials = []
        formulas = ["Al2O3", "TiO2", "Fe2O3"]
        for i in range(min(count, 3)):
            material = await self._create_demo_material(
                f"JARVIS-{3000 + i}",
                formulas[i % len(formulas)],
                f"Oxide material {i+1}"
            )
            materials.append(material)
        return materials
    
    async def _create_demo_material(
        self, 
        jid: str, 
        formula: str, 
        description: str
    ) -> StandardizedMaterial:
        """Create a standardized demo material."""
        
        # Create structure
        structure = MaterialStructure(
            lattice_parameters=[
                [5.4, 0.0, 0.0],
                [0.0, 5.4, 0.0],
                [0.0, 0.0, 5.4]
            ],
            atomic_positions=[
                [0.0, 0.0, 0.0],
                [0.25, 0.25, 0.25]
            ],
            atomic_species=["Si", "Si"] if "Si" in formula else ["C", "C"],
            space_group="Fd-3m",
            crystal_system="cubic",
            volume=157.464
        )
        
        # Create properties with some variation based on ID
        variation = (hash(jid) % 100) / 1000.0
        properties = MaterialProperties(
            formation_energy=-5.425 + variation,
            energy_above_hull=0.0,
            band_gap=1.14 if "Si" in formula else 0.0,
            bulk_modulus=97.8 + variation * 10,
            shear_modulus=79.9 + variation * 5
        )
        
        # Create metadata
        metadata = MaterialMetadata(
            fetched_at=datetime.now(),
            version="1.0",
            source_url=f"https://jarvis.nist.gov/material/{jid}",
            experimental=False,
            confidence_score=0.95
        )
        
        return StandardizedMaterial(
            source_db="JARVIS-DFT",
            source_id=jid,
            formula=formula,
            structure=structure,
            properties=properties,
            metadata=metadata
        )


# ==================== DEMONSTRATION FUNCTIONS ====================

async def demonstrate_rate_limiter():
    """Demonstrate the token bucket rate limiter."""
    
    print("\n" + "=" * 80)
    print("Rate Limiter Demonstration")
    print("=" * 80)
    
    print("\n1. Creating Token Bucket Rate Limiter")
    print("-" * 50)
    
    # Create a rate limiter with low limits for demonstration
    rate_limiter = RateLimiter()
    rate_limiter.add_bucket("demo", capacity=2, refill_rate=1.0)  # 2 tokens, refill 1/sec
    
    print("✓ Rate limiter created with:")
    print("  - Capacity: 2 tokens")
    print("  - Refill rate: 1 token/second")
    
    print("\n2. Testing Token Consumption")
    print("-" * 50)
    
    # Consume tokens rapidly
    for i in range(5):
        start_time = datetime.now()
        await rate_limiter.wait_for_permit("demo")
        end_time = datetime.now()
        elapsed = (end_time - start_time).total_seconds()
        
        print(f"Request {i+1}: waited {elapsed:.3f}s")
        
        if i < 2:
            print("  ✓ Should be immediate (using initial tokens)")
        else:
            print("  ✓ Should be delayed (waiting for refill)")
    
    print("\n✓ Rate limiting demonstration completed")


async def demonstrate_enhanced_base_class():
    """Demonstrate the enhanced database connector framework."""
    
    print("\n" + "=" * 80)
    print("Enhanced Database Connector Framework Demonstration")
    print("=" * 80)
    
    # Initialize connector with enhanced features
    print("\n1. Initializing JARVIS Connector with Enhanced Features")
    print("-" * 50)
    
    connector = DemoJarvisConnector(
        requests_per_second=1.0,  # Conservative rate limiting for demo
        burst_capacity=3,
        cache_ttl=1800,  # 30 minutes cache
        max_retries=2
    )
    
    print(f"✓ Connector initialized: {connector.__class__.__name__}")
    print(f"✓ Base URL: {connector.base_url}")
    print(f"✓ Rate limiting: {connector.rate_limiter.buckets['default'].capacity} burst, "
          f"{connector.rate_limiter.buckets['default'].refill_rate}/s")
    print(f"✓ Cache TTL: {connector.cache_ttl}s")
    print(f"✓ Max retries: {connector.max_retries}")
    print(f"✓ Status: {connector.status.value}")
    
    try:
        # Test connection
        print("\n2. Testing Connection and Health Check")
        print("-" * 50)
        
        # Connect to the database
        connected = await connector.connect()
        print(f"✓ Connection established: {connected}")
        print(f"✓ Status: {connector.status.value}")
        
        # Perform health check
        healthy = await connector.health_check()
        print(f"✓ Health check: {'Healthy' if healthy else 'Unhealthy'}")
        
        # Test standardized data schema
        print("\n3. Demonstrating Standardized Data Schema")
        print("-" * 50)
        
        # Search for a simple material to demonstrate data standardization
        print("Searching for 'Si' (Silicon) materials...")
        materials = await connector.search_materials({"formula": "Si"}, limit=2)
        
        if materials:
            material = materials[0]
            print(f"✓ Found {len(materials)} materials")
            print(f"✓ First material: {material.formula} (ID: {material.source_id})")
            
            # Demonstrate standardized material schema
            print("\n4. Standardized Material Data Structure")
            print("-" * 50)
            
            print(f"Source Database: {material.source_db}")
            print(f"Source ID: {material.source_id}")
            print(f"Formula: {material.formula}")
            
            # Structure information
            print(f"Crystal Structure:")
            print(f"  - Lattice vectors: {len(material.structure.lattice_parameters)}x{len(material.structure.lattice_parameters[0])}")
            print(f"  - Atomic positions: {len(material.structure.atomic_positions)} atoms")
            print(f"  - Species: {material.structure.atomic_species}")
            print(f"  - Space group: {material.structure.space_group}")
            print(f"  - Volume: {material.structure.volume:.3f} Ų")
            
            # Properties information
            print(f"Properties:")
            props = material.properties
            print(f"  - Formation energy: {props.formation_energy:.3f} eV/atom")
            print(f"  - Energy above hull: {props.energy_above_hull:.3f} eV/atom")
            print(f"  - Band gap: {props.band_gap:.3f} eV")
            print(f"  - Bulk modulus: {props.bulk_modulus:.3f} GPa")
            
            # Metadata information
            print(f"Metadata:")
            meta = material.metadata
            print(f"  - Fetched at: {meta.fetched_at.strftime('%Y-%m-%d %H:%M:%S')}")
            print(f"  - Version: {meta.version}")
            print(f"  - Source URL: {meta.source_url}")
            print(f"  - Experimental: {meta.experimental}")
            
            # Demonstrate JSON serialization
            print("\n5. JSON Serialization/Deserialization")
            print("-" * 50)
            
            material_dict = material.to_dict()
            json_str = json.dumps(material_dict, indent=2)
            print(f"✓ Material serialized to JSON ({len(json_str)} characters)")
            
            # Show a sample of the JSON
            print("\nJSON Sample (first 200 characters):")
            print(json_str[:200] + "..." if len(json_str) > 200 else json_str)
            
            # Deserialize back
            restored_material = StandardizedMaterial.from_dict(material_dict)
            print(f"\n✓ Material deserialized: {restored_material.formula}")
            print(f"✓ Data integrity check: {restored_material.source_id == material.source_id}")
        
        # Test caching functionality
        print("\n6. Testing Caching Functionality")
        print("-" * 50)
        
        # Make the same search again to test caching
        print("Repeating the same search to test caching...")
        start_time = datetime.now()
        cached_materials = await connector.search_materials({"formula": "Si"}, limit=2)
        end_time = datetime.now()
        
        print(f"✓ Second search completed in {(end_time - start_time).total_seconds():.3f}s")
        print(f"✓ Results match: {len(cached_materials) == len(materials)}")
        
        # Test rate limiting
        print("\n7. Testing Rate Limiting")
        print("-" * 50)
        
        print("Making multiple rapid requests to test rate limiting...")
        request_times = []
        
        for i in range(3):
            start = datetime.now()
            await connector.search_materials({"formula": "C"}, limit=1)
            end = datetime.now()
            elapsed = (end - start).total_seconds()
            request_times.append(elapsed)
            print(f"  Request {i+1}: {elapsed:.3f}s")
        
        # Check if rate limiting is working
        avg_time = sum(request_times) / len(request_times)
        print(f"✓ Average request time: {avg_time:.3f}s")
        
        # Test metrics collection
        print("\n8. Performance Metrics")
        print("-" * 50)
        
        metrics = await connector.get_metrics()
        print(f"Connector: {metrics['connector']}")
        print(f"Status: {metrics['status']}")
        print(f"Total requests: {metrics['total_requests']}")
        print(f"Success rate: {metrics['success_rate']:.1f}%")
        print(f"Average latency: {metrics['average_latency']:.3f}s")
        print(f"Cache hit rate: {metrics['cache_hit_rate']:.1f}%")
        print(f"Cache size: {metrics['cache_size']} entries")
        
        # Test exception handling
        print("\n9. Exception Handling")
        print("-" * 50)
        
        try:
            # Try to get a non-existent material
            await connector.get_material_by_id("invalid_id_12345")
        except ConnectorException as e:
            print(f"✓ Exception handling working: {e.__class__.__name__}")
            print(f"  Message: {str(e)}")
        
    except Exception as e:
        logger.error(f"Demonstration error: {e}")
        print(f"✗ Error occurred: {e}")
    
    finally:
        # Clean up
        print("\n10. Cleanup")
        print("-" * 50)
        
        await connector.disconnect()
        print(f"✓ Connector disconnected")
        print(f"✓ Final status: {connector.status.value}")
        
        final_metrics = await connector.get_metrics()
        print(f"✓ Final metrics: {final_metrics['total_requests']} total requests, "
              f"{final_metrics['success_rate']:.1f}% success rate")


async def demonstrate_data_standardization():
    """Demonstrate creating standardized materials manually."""
    
    print("\n" + "=" * 80)
    print("Data Standardization Schema Demonstration")
    print("=" * 80)
    
    # Create a standardized material manually
    print("\n1. Creating Standardized Material Data Structures")
    print("-" * 50)
    
    # Create structure
    structure = MaterialStructure(
        lattice_parameters=[
            [5.4, 0.0, 0.0],
            [0.0, 5.4, 0.0], 
            [0.0, 0.0, 5.4]
        ],
        atomic_positions=[
            [0.0, 0.0, 0.0],
            [0.25, 0.25, 0.25]
        ],
        atomic_species=["Si", "Si"],
        space_group="Fd-3m",
        crystal_system="cubic",
        volume=157.464
    )
    
    # Create properties
    properties = MaterialProperties(
        formation_energy=-5.425,
        energy_above_hull=0.0,
        band_gap=1.14,
        bulk_modulus=97.8,
        shear_modulus=79.9
    )
    
    # Create metadata
    metadata = MaterialMetadata(
        fetched_at=datetime.now(),
        version="1.0",
        source_url="https://example.com/material/si",
        experimental=False,
        confidence_score=0.95
    )
    
    # Create standardized material
    material = StandardizedMaterial(
        source_db="demo_db",
        source_id="demo_si_001",
        formula="Si2",
        structure=structure,
        properties=properties,
        metadata=metadata
    )
    
    print(f"✓ Created standardized material: {material.formula}")
    print(f"✓ Source: {material.source_db} (ID: {material.source_id})")
    print(f"✓ Structure: {len(material.structure.atomic_positions)} atoms")
    print(f"✓ Properties: {len([p for p in material.properties.to_dict().values() if p is not None])} defined")
    
    # Test serialization
    print("\n2. Testing Serialization/Deserialization")
    print("-" * 50)
    
    # Convert to dict and JSON
    material_dict = material.to_dict()
    json_string = json.dumps(material_dict, indent=2)
    
    print(f"✓ Serialized to JSON ({len(json_string)} characters)")
    
    # Deserialize
    restored_material = StandardizedMaterial.from_dict(material_dict)
    
    print(f"✓ Deserialized material: {restored_material.formula}")
    print(f"✓ Structure preserved: {len(restored_material.structure.atomic_positions)} atoms")
    print(f"✓ Properties preserved: {restored_material.properties.formation_energy} eV/atom")
    print(f"✓ Metadata preserved: {restored_material.metadata.confidence_score}")
    
    # Verify data integrity
    integrity_checks = [
        material.source_id == restored_material.source_id,
        material.formula == restored_material.formula,
        material.structure.space_group == restored_material.structure.space_group,
        material.properties.formation_energy == restored_material.properties.formation_energy,
        material.metadata.version == restored_material.metadata.version
    ]
    
    print(f"✓ Data integrity: {all(integrity_checks)} ({sum(integrity_checks)}/{len(integrity_checks)} checks passed)")


async def main():
    """Main demonstration function."""
    
    print("Enhanced Database Connector Framework")
    print("=====================================")
    print("This demonstration shows the comprehensive features of the enhanced")
    print("abstract base class for materials database connectors.")
    print()
    
    try:
        # Demonstrate the rate limiter first
        await demonstrate_rate_limiter()
        
        # Demonstrate the enhanced connector framework
        await demonstrate_enhanced_base_class()
        
        # Demonstrate data standardization
        await demonstrate_data_standardization()
        
        print("\n" + "=" * 80)
        print("✅ DEMONSTRATION COMPLETED SUCCESSFULLY")
        print("=" * 80)
        print()
        print("Key Features Demonstrated:")
        print("• Abstract base class with comprehensive functionality")
        print("• Rate limiting with token bucket algorithm")
        print("• Response caching with TTL")
        print("• Performance metrics collection")
        print("• Standardized data schema for materials")
        print("• Error handling and recovery")
        print("• JSON serialization/deserialization")
        print("• Health monitoring and connection management")
        print()
        
    except Exception as e:
        logger.error(f"Demonstration failed: {e}")
        print(f"\n❌ Demonstration failed: {e}")


if __name__ == "__main__":
    asyncio.run(main())
