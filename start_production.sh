#!/bin/bash
set -e

# PRISM Platform Production Startup Script
# This script ensures the database is properly initialized before starting the application

echo "🚀 Starting PRISM Platform (Production Mode)"
echo "=============================================="

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Function to print colored output
print_status() {
    echo -e "${BLUE}[$(date +'%Y-%m-%d %H:%M:%S')]${NC} $1"
}

print_success() {
    echo -e "${GREEN}[$(date +'%Y-%m-%d %H:%M:%S')]${NC} ✅ $1"
}

print_warning() {
    echo -e "${YELLOW}[$(date +'%Y-%m-%d %H:%M:%S')]${NC} ⚠️  $1"
}

print_error() {
    echo -e "${RED}[$(date +'%Y-%m-%d %H:%M:%S')]${NC} ❌ $1"
}

# Check if .env file exists
if [ ! -f ".env" ]; then
    print_warning ".env file not found, creating default configuration..."
    python init_database.py
fi

# Load environment variables
if [ -f ".env" ]; then
    export $(cat .env | grep -v '^#' | xargs)
fi

# Set default environment
export ENVIRONMENT=${ENVIRONMENT:-production}
export POSTGRES_SERVER=${POSTGRES_SERVER:-localhost}
export POSTGRES_PORT=${POSTGRES_PORT:-5432}
export POSTGRES_USER=${POSTGRES_USER:-prism_user}
export POSTGRES_DB=${POSTGRES_DB:-prism_materials}

print_status "Environment: $ENVIRONMENT"
print_status "Database: $POSTGRES_USER@$POSTGRES_SERVER:$POSTGRES_PORT/$POSTGRES_DB"

# Function to wait for PostgreSQL
wait_for_postgres() {
    print_status "Waiting for PostgreSQL to be ready..."
    
    for i in {1..30}; do
        if PGPASSWORD=$POSTGRES_PASSWORD psql -h "$POSTGRES_SERVER" -U "$POSTGRES_USER" -d "$POSTGRES_DB" -c "SELECT 1;" > /dev/null 2>&1; then
            print_success "PostgreSQL is ready!"
            return 0
        fi
        
        if [ $i -eq 30 ]; then
            print_error "PostgreSQL is not responding after 30 attempts"
            return 1
        fi
        
        print_status "Attempt $i/30: PostgreSQL not ready, waiting 2 seconds..."
        sleep 2
    done
}

# Function to initialize database
init_database() {
    print_status "Initializing database..."
    
    if python init_database.py; then
        print_success "Database initialization completed"
        return 0
    else
        print_error "Database initialization failed"
        return 1
    fi
}

# Function to check database health
check_database_health() {
    print_status "Checking database health..."
    
    if python -c "
import asyncio
from app.db.database import db_manager
from app.core.config import get_settings

async def test():
    settings = get_settings()
    try:
        async with db_manager.async_engine.connect() as conn:
            result = await conn.execute('SELECT 1')
            print('Database connection successful')
            return True
    except Exception as e:
        print(f'Database connection failed: {e}')
        return False

result = asyncio.run(test())
exit(0 if result else 1)
"; then
        print_success "Database health check passed"
        return 0
    else
        print_error "Database health check failed"
        return 1
    fi
}

# Main execution
main() {
    # Wait for PostgreSQL
    if ! wait_for_postgres; then
        print_error "Cannot connect to PostgreSQL. Please check:"
        echo "  1. PostgreSQL server is running"
        echo "  2. Connection parameters in .env file are correct"
        echo "  3. Database user has proper permissions"
        exit 1
    fi
    
    # Initialize database
    if ! init_database; then
        print_error "Database initialization failed. Please check the logs above."
        exit 1
    fi
    
    # Health check
    if ! check_database_health; then
        print_error "Database health check failed"
        exit 1
    fi
    
    print_success "All systems ready!"
    print_status "You can now use the PRISM CLI:"
    echo ""
    echo "  # Show database statistics"
    echo "  ./prism fetch-and-store --stats"
    echo ""
    echo "  # Fetch Silicon materials"
    echo "  ./prism fetch-and-store --elements Si --max-results 100"
    echo ""
    echo "  # Search local database"
    echo "  ./prism fetch-and-store --database-only --elements Si"
    echo ""
    echo "  # Start the web API server"
    echo "  python run.py"
    echo ""
}

# Handle script arguments
case "${1:-start}" in
    "start")
        main
        ;;
    "db-only")
        wait_for_postgres && init_database
        ;;
    "health-check")
        wait_for_postgres && check_database_health
        ;;
    "reset-db")
        print_warning "This will reset the database. All data will be lost!"
        read -p "Are you sure? (yes/no): " -r
        if [[ $REPLY == "yes" ]]; then
            print_status "Resetting database..."
            python init_database.py
        else
            print_status "Database reset cancelled"
        fi
        ;;
    *)
        echo "Usage: $0 {start|db-only|health-check|reset-db}"
        echo ""
        echo "  start       - Full startup sequence (default)"
        echo "  db-only     - Initialize database only"
        echo "  health-check - Check database connectivity"
        echo "  reset-db    - Reset and reinitialize database"
        exit 1
        ;;
esac
