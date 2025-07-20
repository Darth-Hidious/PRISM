#!/bin/bash

# Development setup script for Data Ingestion Microservice

set -e

echo "Setting up Data Ingestion Microservice development environment..."

# Check Python version
python_version=$(python3 --version 2>&1 | awk '{print $2}' | cut -d. -f1,2)
required_version="3.11"

if [ "$(printf '%s\n' "$required_version" "$python_version" | sort -V | head -n1)" != "$required_version" ]; then
    echo "Error: Python 3.11+ is required. Found Python $python_version"
    exit 1
fi

echo "✓ Python version check passed ($python_version)"

# Create virtual environment if it doesn't exist
if [ ! -d "venv" ]; then
    echo "Creating virtual environment..."
    python3 -m venv venv
fi

# Activate virtual environment
echo "Activating virtual environment..."
source venv/bin/activate

# Upgrade pip
echo "Upgrading pip..."
pip install --upgrade pip

# Install dependencies
echo "Installing dependencies..."
pip install -r requirements.txt

# Create .env file if it doesn't exist
if [ ! -f ".env" ]; then
    echo "Creating .env file from template..."
    cp .env.example .env
    echo "⚠️  Please edit .env file with your configuration"
fi

echo "✓ Environment setup completed!"
echo ""
echo "Next steps:"
echo "1. Edit .env file with your database and Redis configuration"
echo "2. Start PostgreSQL and Redis services"
echo "3. Run the application with: python run.py"
echo "4. Or use Docker: docker-compose up"
echo ""
echo "API will be available at: http://localhost:8000"
echo "Documentation at: http://localhost:8000/docs"
