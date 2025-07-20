#!/bin/bash
set -e

# Create additional databases and users if needed
psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" <<-EOSQL
    -- Create extensions
    CREATE EXTENSION IF NOT EXISTS "uuid-ossp";
    CREATE EXTENSION IF NOT EXISTS "pgcrypto";
    
    -- Create indexes for performance (will be created by SQLAlchemy but good to have)
    -- These will be ignored if tables don't exist yet
    
    -- Grant permissions
    GRANT ALL PRIVILEGES ON DATABASE ${POSTGRES_DB} TO ${POSTGRES_USER};
EOSQL

echo "PostgreSQL initialization completed successfully!"
