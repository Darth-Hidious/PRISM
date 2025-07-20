"""
Database initialization and migration script for PRISM platform.

This script handles:
- PostgreSQL database creation
- Table initialization
- Migration running
- Data seeding for production
"""

import asyncio
import logging
import sys
import os
from pathlib import Path

import asyncpg
from sqlalchemy import text
from sqlalchemy.exc import OperationalError

# Add the project root to the path
project_root = Path(__file__).parent.parent
sys.path.insert(0, str(project_root))

from app.core.config import get_settings
from app.db.database import db_manager, init_db_sync
from app.db.models import Base

logger = logging.getLogger(__name__)


async def create_database_if_not_exists():
    """Create the PostgreSQL database if it doesn't exist."""
    settings = get_settings()
    
    # Connect to the default 'postgres' database to create our target database
    try:
        conn = await asyncpg.connect(
            host=settings.postgres_server,
            port=settings.postgres_port,
            user=settings.postgres_user,
            password=settings.postgres_password,
            database='postgres'  # Connect to default postgres database
        )
        
        # Check if our database exists
        result = await conn.fetchval(
            "SELECT 1 FROM pg_database WHERE datname = $1",
            settings.postgres_db
        )
        
        if not result:
            # Create the database
            await conn.execute(f'CREATE DATABASE "{settings.postgres_db}"')
            logger.info(f"Created database '{settings.postgres_db}'")
        else:
            logger.info(f"Database '{settings.postgres_db}' already exists")
            
        await conn.close()
        return True
        
    except Exception as e:
        logger.error(f"Failed to create database: {e}")
        return False


async def test_database_connection():
    """Test the database connection."""
    settings = get_settings()
    try:
        conn = await asyncpg.connect(
            host=settings.postgres_server,
            port=settings.postgres_port,
            user=settings.postgres_user,
            password=settings.postgres_password,
            database=settings.postgres_db
        )
        
        # Test a simple query
        result = await conn.fetchval("SELECT version()")
        logger.info(f"Connected to PostgreSQL: {result}")
        
        await conn.close()
        return True
        
    except Exception as e:
        logger.error(f"Database connection failed: {e}")
        return False


async def create_tables():
    """Create all database tables."""
    try:
        # Use the async engine to create tables
        async with db_manager.async_engine.begin() as conn:
            await conn.run_sync(Base.metadata.create_all)
        logger.info("Database tables created successfully")
        return True
        
    except Exception as e:
        logger.error(f"Failed to create tables: {e}")
        return False


def create_tables_sync():
    """Create tables synchronously (fallback method)."""
    try:
        init_db_sync()
        logger.info("Database tables created successfully (sync)")
        return True
    except Exception as e:
        logger.error(f"Failed to create tables (sync): {e}")
        return False


async def check_database_health():
    """Comprehensive database health check."""
    settings = get_settings()
    health_checks = {
        "connection": False,
        "database_exists": False,
        "tables_exist": False,
        "can_query": False
    }
    
    try:
        # Test connection
        conn = await asyncpg.connect(
            host=settings.postgres_server,
            port=settings.postgres_port,
            user=settings.postgres_user,
            password=settings.postgres_password,
            database=settings.postgres_db
        )
        health_checks["connection"] = True
        health_checks["database_exists"] = True
        
        # Check if key tables exist
        tables = await conn.fetch("""
            SELECT table_name 
            FROM information_schema.tables 
            WHERE table_schema = 'public' AND table_type = 'BASE TABLE'
        """)
        
        table_names = [row['table_name'] for row in tables]
        required_tables = ['materials', 'data_ingestion_jobs']
        
        if all(table in table_names for table in required_tables):
            health_checks["tables_exist"] = True
            
            # Test a simple query on materials table
            count = await conn.fetchval("SELECT COUNT(*) FROM materials")
            health_checks["can_query"] = True
            logger.info(f"Database health check passed. Materials count: {count}")
        
        await conn.close()
        
    except Exception as e:
        logger.error(f"Database health check failed: {e}")
    
    return health_checks


async def initialize_production_database():
    """Initialize database for production use."""
    logger.info("Initializing PostgreSQL database for production...")
    
    # Step 1: Create database if needed
    if not await create_database_if_not_exists():
        return False
    
    # Step 2: Test connection
    if not await test_database_connection():
        return False
    
    # Step 3: Create tables
    if not await create_tables():
        # Fallback to sync method
        if not create_tables_sync():
            return False
    
    # Step 4: Health check
    health = await check_database_health()
    if not all(health.values()):
        logger.error(f"Database health check failed: {health}")
        return False
    
    logger.info("✅ Database initialization complete!")
    return True


def setup_environment_file():
    """Create .env file with PostgreSQL settings if it doesn't exist."""
    env_file = Path(".env")
    
    if env_file.exists():
        logger.info(".env file already exists")
        return
    
    env_content = """# PRISM Platform Environment Configuration

# Database Configuration (PostgreSQL)
POSTGRES_SERVER=localhost
POSTGRES_USER=prism_user
POSTGRES_PASSWORD=prism_password
POSTGRES_DB=prism_materials
POSTGRES_PORT=5432
POSTGRES_ECHO=false

# Alternative: Use DATABASE_URL for full connection string
# DATABASE_URL=postgresql+asyncpg://prism_user:prism_password@localhost:5432/prism_materials

# Application Settings
APP_NAME=PRISM Materials Platform
APP_VERSION=1.0.0
ENVIRONMENT=production
DEBUG=false

# Server Settings
HOST=0.0.0.0
PORT=8000

# CORS Settings
CORS_ORIGINS=["http://localhost:3000","http://localhost:8080"]
CORS_ALLOW_CREDENTIALS=true
CORS_ALLOW_METHODS=*
CORS_ALLOW_HEADERS=*

# Redis Settings (optional, for caching)
REDIS_HOST=localhost
REDIS_PORT=6379
REDIS_DB=0

# NOMAD API Settings
NOMAD_BASE_URL=https://nomad-lab.eu/prod/rae/api/v1
NOMAD_TIMEOUT=30.0
NOMAD_RATE_LIMIT=120
NOMAD_BURST_SIZE=10

# JARVIS API Settings
JARVIS_BASE_URL=https://www.ctcms.nist.gov/~knc6/jdft_docs/
JARVIS_TIMEOUT=30.0
JARVIS_RATE_LIMIT=60

# Logging
LOG_LEVEL=INFO
"""
    
    env_file.write_text(env_content)
    logger.info("Created .env file with production defaults")


def main():
    """Main initialization function."""
    logging.basicConfig(
        level=logging.INFO,
        format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
    )
    
    print("🚀 PRISM Platform Database Initialization")
    print("=" * 50)
    
    # Create environment file if needed
    setup_environment_file()
    
    # Run async initialization
    success = asyncio.run(initialize_production_database())
    
    if success:
        print("\n✅ Database initialization completed successfully!")
        print("\nNext steps:")
        print("1. Review the .env file and update PostgreSQL credentials")
        print("2. Ensure PostgreSQL server is running")
        print("3. Run: ./prism fetch-and-store --stats")
        print("4. Start fetching materials: ./prism fetch-and-store --elements Si --max-results 100")
    else:
        print("\n❌ Database initialization failed!")
        print("\nTroubleshooting:")
        print("1. Check PostgreSQL server is running: pg_ctl status")
        print("2. Verify credentials in .env file")
        print("3. Ensure database user has CREATE DATABASE privileges")
        print("4. Check connection: psql -h localhost -U prism_user -d postgres")
        sys.exit(1)


if __name__ == "__main__":
    main()
