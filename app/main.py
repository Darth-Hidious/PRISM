import logging
from contextlib import asynccontextmanager

from fastapi import FastAPI, Request, HTTPException
from fastapi.middleware.cors import CORSMiddleware
from fastapi.middleware.trustedhost import TrustedHostMiddleware
from fastapi.responses import JSONResponse
import structlog

from .api.v1 import api_router
from .core.config import get_settings
from .db.database import init_db, close_db
from .services.connectors.redis_connector import close_redis


# Configure structured logging
structlog.configure(
    processors=[
        structlog.stdlib.filter_by_level,
        structlog.stdlib.add_logger_name,
        structlog.stdlib.add_log_level,
        structlog.stdlib.PositionalArgumentsFormatter(),
        structlog.processors.TimeStamper(fmt="iso"),
        structlog.processors.StackInfoRenderer(),
        structlog.processors.format_exc_info,
        structlog.processors.UnicodeDecoder(),
        structlog.processors.JSONRenderer()
    ],
    context_class=dict,
    logger_factory=structlog.stdlib.LoggerFactory(),
    wrapper_class=structlog.stdlib.BoundLogger,
    cache_logger_on_first_use=True,
)

logger = structlog.get_logger()


@asynccontextmanager
async def lifespan(app: FastAPI):
    """
    Application lifespan manager.
    """
    settings = get_settings()
    
    # Setup logging
    settings.setup_logging()
    logger.info("Starting Data Ingestion Microservice", version=settings.app_version)
    
    try:
        # Initialize database
        await init_db()
        logger.info("Database initialized successfully")
        
        # Application is ready
        logger.info("Application startup completed")
        yield
        
    except Exception as e:
        logger.error("Failed to start application", error=str(e))
        raise
    
    finally:
        # Cleanup
        logger.info("Shutting down application")
        try:
            await close_db()
            await close_redis()
            logger.info("Application shutdown completed")
        except Exception as e:
            logger.error("Error during shutdown", error=str(e))


def create_application() -> FastAPI:
    """
    Create and configure FastAPI application.
    """
    settings = get_settings()
    
    app = FastAPI(
        title=settings.app_name,
        version=settings.app_version,
        description="A high-performance data ingestion microservice built with FastAPI",
        docs_url="/docs" if settings.debug else None,
        redoc_url="/redoc" if settings.debug else None,
        openapi_url="/openapi.json" if settings.debug else None,
        lifespan=lifespan
    )
    
    # Add CORS middleware
    app.add_middleware(
        CORSMiddleware,
        allow_origins=settings.cors_origins,
        allow_credentials=settings.cors_allow_credentials,
        allow_methods=settings.cors_allow_methods,
        allow_headers=settings.cors_allow_headers,
    )
    
    # Add trusted host middleware for security
    if not settings.debug:
        app.add_middleware(
            TrustedHostMiddleware,
            allowed_hosts=["*"]  # Configure this properly for production
        )
    
    # Include API router
    app.include_router(api_router, prefix="/api/v1")
    
    return app


# Create application instance
app = create_application()


@app.middleware("http")
async def log_requests(request: Request, call_next):
    """
    Log HTTP requests with structured logging.
    """
    start_time = structlog.get_logger().info
    
    # Log request
    logger.info(
        "HTTP request started",
        method=request.method,
        url=str(request.url),
        client_ip=request.client.host if request.client else None,
        user_agent=request.headers.get("user-agent"),
    )
    
    response = await call_next(request)
    
    # Log response
    logger.info(
        "HTTP request completed",
        method=request.method,
        url=str(request.url),
        status_code=response.status_code,
        client_ip=request.client.host if request.client else None,
    )
    
    return response


@app.exception_handler(HTTPException)
async def http_exception_handler(request: Request, exc: HTTPException):
    """
    Handle HTTP exceptions with structured logging.
    """
    logger.error(
        "HTTP exception occurred",
        status_code=exc.status_code,
        detail=exc.detail,
        method=request.method,
        url=str(request.url),
        client_ip=request.client.host if request.client else None,
    )
    
    return JSONResponse(
        status_code=exc.status_code,
        content={
            "error": "HTTP Exception",
            "message": exc.detail,
            "status_code": exc.status_code
        }
    )


@app.exception_handler(Exception)
async def general_exception_handler(request: Request, exc: Exception):
    """
    Handle general exceptions with structured logging.
    """
    logger.error(
        "Unhandled exception occurred",
        exception=str(exc),
        exception_type=type(exc).__name__,
        method=request.method,
        url=str(request.url),
        client_ip=request.client.host if request.client else None,
    )
    
    return JSONResponse(
        status_code=500,
        content={
            "error": "Internal Server Error",
            "message": "An unexpected error occurred",
            "status_code": 500
        }
    )


@app.get("/")
async def root():
    """
    Root endpoint.
    """
    settings = get_settings()
    return {
        "message": f"Welcome to {settings.app_name}",
        "version": settings.app_version,
        "status": "operational",
        "docs_url": "/docs" if settings.debug else "Documentation disabled in production"
    }


if __name__ == "__main__":
    import uvicorn
    
    settings = get_settings()
    
    uvicorn.run(
        "main:app",
        host=settings.host,
        port=settings.port,
        reload=settings.reload,
        log_level=settings.log_level.lower(),
        access_log=True
    )
