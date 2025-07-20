#!/usr/bin/env python3
"""
PRISM Platform Setup Script

This script configures the PRISM platform as an installable package with
executable command-line tools.
"""

from setuptools import setup, find_packages
from pathlib import Path

# Read README for long description
readme_path = Path(__file__).parent / "README.md"
long_description = readme_path.read_text(encoding="utf-8") if readme_path.exists() else ""

# Read requirements
requirements_path = Path(__file__).parent / "requirements.txt"
if requirements_path.exists():
    requirements = [
        line.strip() 
        for line in requirements_path.read_text(encoding="utf-8").splitlines()
        if line.strip() and not line.startswith("#")
    ]
else:
    requirements = [
        "click>=8.0.0",
        "rich>=13.0.0",
        "fastapi>=0.100.0",
        "uvicorn>=0.20.0",
        "sqlalchemy>=2.0.0",
        "alembic>=1.10.0",
        "pydantic>=2.0.0",
        "pydantic-settings>=2.0.0",
        "httpx>=0.24.0",
        "redis>=4.5.0",
        "asyncio-throttle>=1.0.0",
        "python-multipart>=0.0.6",
        "python-dotenv>=1.0.0",
        "pytest>=7.0.0",
        "pytest-asyncio>=0.21.0",
        "pytest-mock>=3.10.0",
    ]

setup(
    name="prism-platform",
    version="1.0.0",
    description="PRISM - A comprehensive materials science data ingestion and processing platform",
    long_description=long_description,
    long_description_content_type="text/markdown",
    author="PRISM Development Team",
    author_email="dev@prism-platform.org",
    url="https://github.com/Darth-Hidious/PRISM",
    packages=find_packages(),
    include_package_data=True,
    install_requires=requirements,
    python_requires=">=3.8",
    entry_points={
        "console_scripts": [
            "prism=app.cli:cli",
            "prism-cli=app.cli:cli",
            "prism-platform=app.cli:cli",
        ],
    },
    classifiers=[
        "Development Status :: 4 - Beta",
        "Environment :: Console",
        "Intended Audience :: Science/Research",
        "License :: OSI Approved :: MIT License",
        "Operating System :: OS Independent",
        "Programming Language :: Python :: 3",
        "Programming Language :: Python :: 3.8",
        "Programming Language :: Python :: 3.9",
        "Programming Language :: Python :: 3.10",
        "Programming Language :: Python :: 3.11",
        "Topic :: Scientific/Engineering :: Chemistry",
        "Topic :: Scientific/Engineering :: Physics",
        "Topic :: Database :: Front-Ends",
        "Topic :: System :: Distributed Computing",
    ],
    keywords="materials science, data ingestion, research tools, scientific computing",
    project_urls={
        "Bug Reports": "https://github.com/Darth-Hidious/PRISM/issues",
        "Source": "https://github.com/Darth-Hidious/PRISM",
        "Documentation": "https://github.com/Darth-Hidious/PRISM/wiki",
    },
    extras_require={
        "dev": [
            "pytest>=7.0.0",
            "pytest-asyncio>=0.21.0",
            "pytest-mock>=3.10.0",
            "pytest-cov>=4.0.0",
            "black>=22.0.0",
            "flake8>=5.0.0",
            "mypy>=1.0.0",
        ],
        "export": [
            "pandas>=1.5.0",
            "openpyxl>=3.0.0",
            "pyarrow>=10.0.0",
        ],
        "monitoring": [
            "prometheus-client>=0.15.0",
            "grafana-api>=1.0.0",
        ],
    },
)
