#!/bin/bash
# PRISM Platform Installation Script
# This script sets up the PRISM platform for easy execution

set -e  # Exit on any error

echo "🚀 PRISM Platform Installation Script"
echo "====================================="

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Function to print colored output
print_status() {
    echo -e "${GREEN}✅ $1${NC}"
}

print_warning() {
    echo -e "${YELLOW}⚠️  $1${NC}"
}

print_error() {
    echo -e "${RED}❌ $1${NC}"
}

print_info() {
    echo -e "${BLUE}ℹ️  $1${NC}"
}

# Check if Python 3.8+ is available
check_python() {
    print_info "Checking Python installation..."
    
    if command -v python3 &> /dev/null; then
        PYTHON_CMD="python3"
    elif command -v python &> /dev/null; then
        PYTHON_CMD="python"
    else
        print_error "Python is not installed or not in PATH"
        echo "Please install Python 3.8+ from https://python.org"
        exit 1
    fi
    
    # Check Python version
    PYTHON_VERSION=$($PYTHON_CMD -c "import sys; print(f'{sys.version_info.major}.{sys.version_info.minor}')")
    PYTHON_MAJOR=$(echo $PYTHON_VERSION | cut -d. -f1)
    PYTHON_MINOR=$(echo $PYTHON_VERSION | cut -d. -f2)
    
    if [[ $PYTHON_MAJOR -lt 3 ]] || [[ $PYTHON_MAJOR -eq 3 && $PYTHON_MINOR -lt 8 ]]; then
        print_error "Python 3.8+ is required. Found Python $PYTHON_VERSION"
        exit 1
    fi
    
    print_status "Python $PYTHON_VERSION found"
}

# Create virtual environment
setup_venv() {
    print_info "Setting up virtual environment..."
    
    if [[ ! -d "venv" ]]; then
        $PYTHON_CMD -m venv venv
        print_status "Virtual environment created"
    else
        print_warning "Virtual environment already exists"
    fi
    
    # Activate virtual environment
    if [[ "$OSTYPE" == "msys" || "$OSTYPE" == "win32" ]]; then
        source venv/Scripts/activate
    else
        source venv/bin/activate
    fi
    
    print_status "Virtual environment activated"
}

# Install dependencies
install_dependencies() {
    print_info "Installing dependencies..."
    
    # Upgrade pip
    pip install --upgrade pip
    
    # Install requirements
    if [[ -f "requirements.txt" ]]; then
        pip install -r requirements.txt
        print_status "Dependencies installed from requirements.txt"
    else
        # Fallback to manual installation
        pip install click rich fastapi uvicorn sqlalchemy alembic pydantic pydantic-settings httpx redis asyncio-throttle python-multipart python-dotenv pytest pytest-asyncio pytest-mock
        print_status "Core dependencies installed"
    fi
}

# Install PRISM in development mode
install_prism() {
    print_info "Installing PRISM platform..."
    
    if [[ -f "setup.py" ]]; then
        pip install -e .
        print_status "PRISM installed in development mode"
    else
        print_warning "setup.py not found, skipping pip installation"
    fi
}

# Make scripts executable
setup_executables() {
    print_info "Setting up executable scripts..."
    
    chmod +x prism
    chmod +x setup.sh
    
    print_status "Scripts made executable"
}

# Create .env file if it doesn't exist
setup_config() {
    print_info "Setting up configuration..."
    
    if [[ ! -f ".env" ]]; then
        print_warning ".env file not found, creating default configuration"
        cat > .env << 'EOF'
# PRISM Platform Configuration
# Database Configuration
DATABASE_URL=sqlite:///./prism.db

# JARVIS Configuration
JARVIS_BASE_URL=https://jarvis.nist.gov/
JARVIS_RATE_LIMIT=100
JARVIS_BURST_SIZE=20
JARVIS_TIMEOUT=30

# NOMAD Configuration  
NOMAD_BASE_URL=https://nomad-lab.eu/prod/v1/api/v1
NOMAD_RATE_LIMIT=60
NOMAD_BURST_SIZE=10
NOMAD_TIMEOUT=45

# OQMD Configuration
OQMD_BASE_URL=http://oqmd.org/api
OQMD_RATE_LIMIT=30
OQMD_BURST_SIZE=5
OQMD_TIMEOUT=30

# Job Processing
BATCH_SIZE=50
MAX_RETRIES=3
RETRY_DELAY=60
JOB_TIMEOUT=3600
MAX_CONCURRENT_JOBS=5

# Development Settings
DEVELOPMENT_MODE=true
DEBUG=false
EOF
        print_status "Default .env file created"
    else
        print_status "Configuration file .env already exists"
    fi
}

# Test installation
test_installation() {
    print_info "Testing PRISM installation..."
    
    # Test the CLI
    if ./prism --help &> /dev/null; then
        print_status "PRISM CLI is working correctly"
    else
        print_error "PRISM CLI test failed"
        exit 1
    fi
    
    # Test configuration
    if $PYTHON_CMD -c "from app.core.config import get_settings; get_settings()" &> /dev/null; then
        print_status "Configuration system is working"
    else
        print_error "Configuration test failed"
        exit 1
    fi
}

# Main installation process
main() {
    echo
    print_info "Starting PRISM installation process..."
    echo
    
    check_python
    setup_venv
    install_dependencies
    install_prism
    setup_executables
    setup_config
    test_installation
    
    echo
    print_status "🎉 PRISM Platform installation completed successfully!"
    echo
    print_info "Usage examples:"
    echo "  ./prism --help                    # Show help"
    echo "  ./prism test-connection          # Test database connections"
    echo "  ./prism list-sources             # List available data sources"
    echo "  ./prism fetch-material -s jarvis -f 'Si'  # Fetch silicon materials"
    echo
    print_info "Configuration file: .env"
    print_info "Documentation: README.md"
    print_info "Report issues: https://github.com/Darth-Hidious/PRISM/issues"
    echo
}

# Run installation if script is executed directly
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    main "$@"
fi
