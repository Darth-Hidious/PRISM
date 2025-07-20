#!/usr/bin/env python3
"""
Development runner for the Data Ingestion Microservice.
This script starts the FastAPI application with development settings.
"""

import os
import sys

# Add the project root to the Python path
project_root = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, project_root)

if __name__ == "__main__":
    try:
        import uvicorn
        from app.core.config import get_settings
        
        # Get settings
        settings = get_settings()
        
        print(f"Starting {settings.app_name} v{settings.app_version}")
        print(f"Environment: {settings.environment}")
        print(f"Debug mode: {settings.debug}")
        print(f"Server: http://{settings.host}:{settings.port}")
        print(f"API Documentation: http://{settings.host}:{settings.port}/docs")
        print("-" * 50)
        
        # Run the application
        uvicorn.run(
            "app.main:app",
            host=settings.host,
            port=settings.port,
            reload=settings.reload and settings.debug,
            log_level=settings.log_level.lower(),
            access_log=True,
            reload_dirs=[project_root] if settings.reload else None
        )
    except ImportError as e:
        print(f"ImportError: {e}")
        print("Please install the required dependencies:")
        print("pip install -r requirements.txt")
        sys.exit(1)
    except Exception as e:
        print(f"Error starting application: {e}")
        sys.exit(1)
