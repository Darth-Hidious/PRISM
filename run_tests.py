#!/usr/bin/env python3
"""
Test runner for PRISM integration tests.

Provides convenient commands to run different test suites with proper configuration.
"""

import argparse
import subprocess
import sys
from pathlib import Path


def run_command(cmd: list, description: str):
    """Run a command and handle errors."""
    print(f"\n🚀 {description}")
    print(f"Command: {' '.join(cmd)}")
    print("-" * 50)
    
    try:
        result = subprocess.run(cmd, check=True, cwd=Path(__file__).parent.parent)
        print(f"✅ {description} completed successfully")
        return True
    except subprocess.CalledProcessError as e:
        print(f"❌ {description} failed with exit code {e.returncode}")
        return False


def main():
    parser = argparse.ArgumentParser(description="Run PRISM integration tests")
    parser.add_argument(
        "test_type",
        choices=["unit", "integration", "all", "fast", "slow", "coverage"],
        help="Type of tests to run"
    )
    parser.add_argument(
        "--verbose", "-v",
        action="store_true",
        help="Verbose output"
    )
    parser.add_argument(
        "--fail-fast", "-x",
        action="store_true",
        help="Stop on first failure"
    )
    parser.add_argument(
        "--parallel", "-n",
        type=int,
        help="Number of parallel workers"
    )
    parser.add_argument(
        "--redis-required",
        action="store_true",
        help="Only run tests that require Redis"
    )
    parser.add_argument(
        "--no-cov",
        action="store_true",
        help="Disable coverage reporting"
    )
    
    args = parser.parse_args()
    
    # Base pytest command
    cmd = ["python", "-m", "pytest"]
    
    # Add verbosity
    if args.verbose:
        cmd.append("-v")
    else:
        cmd.append("-q")
    
    # Add fail-fast
    if args.fail_fast:
        cmd.append("-x")
    
    # Add parallel processing
    if args.parallel:
        cmd.extend(["-n", str(args.parallel)])
    
    # Add coverage unless disabled
    if not args.no_cov and args.test_type != "fast":
        cmd.extend([
            "--cov=app",
            "--cov-report=term-missing",
            "--cov-report=html:htmlcov"
        ])
    
    # Configure test selection based on type
    success = True
    
    if args.test_type == "unit":
        cmd.extend([
            "tests/",
            "-m", "not integration and not slow",
            "--ignore=tests/integration/"
        ])
        success = run_command(cmd, "Running unit tests")
    
    elif args.test_type == "integration":
        cmd.extend([
            "tests/integration/",
            "-m", "integration"
        ])
        
        if args.redis_required:
            cmd.extend(["-m", "integration and redis"])
        
        success = run_command(cmd, "Running integration tests")
    
    elif args.test_type == "fast":
        cmd.extend([
            "tests/",
            "-m", "not slow"
        ])
        # Don't add coverage for fast tests
        success = run_command(cmd, "Running fast tests")
    
    elif args.test_type == "slow":
        cmd.extend([
            "tests/",
            "-m", "slow"
        ])
        success = run_command(cmd, "Running slow tests")
    
    elif args.test_type == "all":
        # Run unit tests first
        unit_cmd = cmd + [
            "tests/",
            "-m", "not integration",
            "--ignore=tests/integration/"
        ]
        success = run_command(unit_cmd, "Running unit tests")
        
        if success:
            # Run integration tests
            integration_cmd = cmd + [
                "tests/integration/",
                "-m", "integration"
            ]
            success = run_command(integration_cmd, "Running integration tests")
    
    elif args.test_type == "coverage":
        cmd.extend([
            "tests/",
            "--cov=app",
            "--cov-report=term-missing",
            "--cov-report=html:htmlcov",
            "--cov-report=xml",
            "--cov-fail-under=80"
        ])
        success = run_command(cmd, "Running tests with coverage analysis")
        
        if success:
            print("\n📊 Coverage report generated in htmlcov/index.html")
    
    # Print summary
    print("\n" + "=" * 50)
    if success:
        print("✅ All tests completed successfully!")
        print("\nNext steps:")
        print("- Check coverage report: htmlcov/index.html")
        print("- Review any warnings in test output")
        print("- Run slow tests if not already done: python run_tests.py slow")
    else:
        print("❌ Some tests failed!")
        print("\nTroubleshooting:")
        print("- Check Redis is running: redis-cli ping")
        print("- Verify dependencies: pip install -r requirements.txt")
        print("- Run specific test: pytest tests/integration/test_connectors.py::TestClass::test_method -v")
    
    return 0 if success else 1


if __name__ == "__main__":
    sys.exit(main())
