@echo off
REM PRISM Platform CLI for Windows
REM This batch file provides a convenient way to run PRISM on Windows systems

setlocal enabledelayedexpansion

REM Check if Python is available
python --version >nul 2>&1
if %errorlevel% neq 0 (
    echo ❌ Python is not installed or not in PATH
    echo 📖 Please install Python from https://python.org
    exit /b 1
)

REM Change to the script directory
cd /d "%~dp0"

REM Run the PRISM CLI
python prism %*

REM Preserve error level
exit /b %errorlevel%
