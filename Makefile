# PRISM Platform Makefile
# Provides convenient commands for development and execution

.PHONY: help install test run clean dev-install format lint check-deps

# Default target
help:
	@echo "🚀 PRISM Platform - Available Commands"
	@echo "====================================="
	@echo ""
	@echo "Setup & Installation:"
	@echo "  make install      Install PRISM and dependencies"
	@echo "  make dev-install  Install in development mode with dev dependencies"
	@echo "  make check-deps   Check if all dependencies are installed"
	@echo ""
	@echo "Development:"
	@echo "  make format       Format code with black"
	@echo "  make lint         Run linting checks"
	@echo "  make test         Run test suite"
	@echo "  make test-watch   Run tests in watch mode"
	@echo ""
	@echo "Execution:"
	@echo "  make run          Run PRISM CLI interactively"
	@echo "  make test-conn    Test database connections"
	@echo "  make status       Show system status"
	@echo ""
	@echo "Utilities:"
	@echo "  make clean        Clean up temporary files"
	@echo "  make reset-db     Reset database (WARNING: destroys data)"
	@echo "  make backup-db    Backup database"
	@echo ""
	@echo "Docker:"
	@echo "  make docker-build Build Docker image"
	@echo "  make docker-run   Run in Docker container"
	@echo ""

# Installation and setup
install:
	@echo "📦 Installing PRISM Platform..."
	./install.sh

dev-install:
	@echo "🔧 Installing PRISM in development mode..."
	pip install -e .[dev]
	pip install pre-commit
	pre-commit install

check-deps:
	@echo "🔍 Checking dependencies..."
	@python -c "import click, rich, fastapi, sqlalchemy, pydantic; print('✅ Core dependencies available')" || (echo "❌ Missing dependencies. Run 'make install'" && exit 1)

# Development commands
format:
	@echo "🎨 Formatting code..."
	black app/ tests/ *.py
	isort app/ tests/ *.py

lint:
	@echo "🔍 Running linting checks..."
	flake8 app/ tests/
	mypy app/
	black --check app/ tests/ *.py

test:
	@echo "🧪 Running test suite..."
	pytest tests/ -v --cov=app --cov-report=html --cov-report=term

test-watch:
	@echo "👀 Running tests in watch mode..."
	pytest-watch tests/ -- -v

# Execution commands
run:
	@echo "🚀 Starting PRISM CLI..."
	./prism

test-conn:
	@echo "🔗 Testing database connections..."
	./prism test-connection --source all

status:
	@echo "📊 Showing system status..."
	./prism queue-status

list-sources:
	@echo "📋 Listing available data sources..."
	./prism list-sources

config:
	@echo "⚙️ Showing configuration..."
	./prism config --list

# Database operations
reset-db:
	@echo "⚠️  WARNING: This will destroy all data!"
	@read -p "Are you sure? [y/N] " -n 1 -r; \
	if [[ $$REPLY =~ ^[Yy]$$ ]]; then \
		echo "\n🗃️ Resetting database..."; \
		rm -f prism.db; \
		./prism migrate; \
	else \
		echo "\n❌ Database reset cancelled"; \
	fi

backup-db:
	@echo "💾 Backing up database..."
	@timestamp=$$(date +%Y%m%d_%H%M%S); \
	cp prism.db "prism_backup_$$timestamp.db"; \
	echo "✅ Database backed up to prism_backup_$$timestamp.db"

# Utility commands
clean:
	@echo "🧹 Cleaning up temporary files..."
	find . -type f -name "*.pyc" -delete
	find . -type d -name "__pycache__" -delete
	find . -type d -name "*.egg-info" -exec rm -rf {} + 2>/dev/null || true
	rm -rf build/ dist/ .coverage htmlcov/ .pytest_cache/ .mypy_cache/
	@echo "✅ Cleanup complete"

# Docker commands
docker-build:
	@echo "🐳 Building Docker image..."
	docker build -t prism-platform:latest .

docker-run:
	@echo "🐳 Running PRISM in Docker..."
	docker run -it --rm -v $$(pwd):/workspace prism-platform:latest

# Advanced features
demo:
	@echo "🎬 Running PRISM demonstration..."
	./prism test-connection
	./prism list-sources
	./prism fetch-material --source jarvis --formula "Si" --limit 5

benchmark:
	@echo "⚡ Running performance benchmarks..."
	./prism bulk-fetch --source jarvis --limit 100 --dry-run
	./prism bulk-fetch --source nomad --limit 100 --dry-run

monitor:
	@echo "📈 Starting system monitor..."
	./prism monitor --interval 5

# CI/CD helpers
ci-test: check-deps lint test
	@echo "✅ All CI checks passed"

pre-commit: format lint test
	@echo "🚀 Pre-commit checks complete"

# Documentation
docs:
	@echo "📚 Generating documentation..."
	@echo "Available commands documented in README.md"
	@echo "API documentation: ./prism --help"

version:
	@echo "📦 PRISM Platform v1.0.0"
	@python -c "from app.core.config import get_settings; print(f'Database: {get_settings().DATABASE_URL}')"

# Quick start for new users
quickstart: install test-conn
	@echo ""
	@echo "🎉 PRISM Platform is ready!"
	@echo "Try these commands:"
	@echo "  make demo         # Run a quick demonstration"
	@echo "  make run          # Start interactive CLI"
	@echo "  ./prism --help    # Show all available commands"
