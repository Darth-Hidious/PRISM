# PRISM Platform Makefile
# Provides convenient commands for development and execution

.PHONY: help install dev-install format lint test clean run

# Default target
help:
	@echo "🚀 PRISM Platform - Available Commands"
	@echo "====================================="
	@echo ""
	@echo "Setup & Installation:"
	@echo "  make install      Install PRISM and its dependencies"
	@echo "  make dev-install  Install in development mode with dev dependencies"
	@echo ""
	@echo "Development:"
	@echo "  make format       Format code with black and isort"
	@echo "  make lint         Run linting checks with flake8 and mypy"
	@echo "  make test         Run the test suite with pytest"
	@echo ""
	@echo "Execution:"
	@echo "  make run          Run the PRISM CLI"
	@echo ""
	@echo "Utilities:"
	@echo "  make clean        Clean up temporary build and cache files"
	@echo ""

# Installation and setup
install:
	@echo "📦 Installing PRISM Platform..."
	python -m pip install -e .

dev-install:
	@echo "🔧 Installing PRISM in development mode..."
	python -m pip install -e ".[dev]"
	pre-commit install

# Development commands
format:
	@echo "🎨 Formatting code..."
	black app/ tests/ *.py
	isort app/ tests/ *.py

lint:
	@echo "🔍 Running linting checks..."
	flake8 app/ tests/
	mypy app/

test:
	@echo "🧪 Running test suite..."
	python -m pytest

# Execution commands
run:
	@echo "🚀 Starting PRISM CLI..."
	prism --help

# Utility commands
clean:
	@echo "🧹 Cleaning up temporary files..."
	find . -type f -name "*.pyc" -delete
	find . -type d -name "__pycache__" -delete
	find . -type d -name "*.egg-info" -exec rm -rf {} +
	rm -rf build/ dist/ .coverage htmlcov/ .pytest_cache/ .mypy_cache/
	@echo "✅ Cleanup complete"
