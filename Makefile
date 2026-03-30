# PRISM Platform Makefile
# Provides convenient commands for development and execution

.PHONY: help install dev-install format lint test clean run publish check dashboard-build rust-build build

# Default target
help:
	@echo "🚀 PRISM Platform - Available Commands"
	@echo "====================================="
	@echo ""
	@echo "Setup & Installation:"
	@echo "  make install      Install PRISM and its dependencies"
	@echo "  make dev-install  Install in development mode with dev dependencies"
	@echo "  make publish      Build and upload to PyPI"
	@echo "  make check        Build and check the distribution"
	@echo ""
	@echo "Development:"
	@echo "  make format       Format code with black and isort"
	@echo "  make lint         Run linting checks with flake8 and mypy"
	@echo "  make test         Run the test suite with pytest"
	@echo ""
	@echo "Execution:"
	@echo "  make run          Run the PRISM CLI"
	@echo ""
	@echo "V2 Builds:"
	@echo "  make dashboard-build  Build the React SPA dashboard"
	@echo "  make rust-build       Build dashboard + Rust workspace"
	@echo "  make build            Full build (dashboard + Rust)"
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

# Development commands
format:
	@echo "🎨 Formatting code..."
	black app/ *.py
	isort app/ *.py

lint:
	@echo "🔍 Running linting checks..."
	flake8 app/
	mypy app/

test:
	@echo "Running test suite..."
	pytest tests/ -v --tb=short

# Execution commands
run:
	@echo "🚀 Starting PRISM CLI..."
	prism --help

# ── V2 Rust + Dashboard builds ─────────────────────────────────────

dashboard-build:
	@echo "Building dashboard SPA..."
	cd dashboard && npm ci && npm run build

rust-build: dashboard-build
	@echo "Building Rust workspace..."
	cargo build --workspace

build: rust-build
	@echo "Full build complete."

# Utility commands
clean:
	@echo "🧹 Cleaning up temporary files..."
	find . -type f -name "*.pyc" -delete
	find . -type d -name "__pycache__" -delete
	find . -type d -name "*.egg-info" -exec rm -rf {} +
	rm -rf build/ dist/ .coverage htmlcov/ .pytest_cache/ .mypy_cache/
	@echo "✅ Cleanup complete"

# Publishing commands
publish:
	@echo "📦 Building and publishing to PyPI..."
	python -m build
	twine upload dist/*

check:
	@echo "🔍 Building and checking distribution..."
	python -m build
	twine check dist/*
