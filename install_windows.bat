@echo off
REM MARC27's PRISM Platform - Windows Installation Script
REM This script provides multiple installation methods for Windows

echo.
echo    ███╗   ███╗ █████╗ ██████╗  ██████╗██████╗ ███████╗
echo    ████╗ ████║██╔══██╗██╔══██╗██╔════╝╚════██╗╚════██║
echo    ██╔████╔██║███████║██████╔╝██║      █████╔╝    ██╔╝
echo    ██║╚██╔╝██║██╔══██║██╔══██╗██║     ██╔═══╝    ██╔╝ 
echo    ██║ ╚═╝ ██║██║  ██║██║  ██║╚██████╗███████╗  ██╗
echo    ╚═╝     ╚═╝╚═╝  ╚═╝╚═╝  ╚═╝ ╚═════╝╚══════╝ ╚══╝
echo.                                                        
echo         ██████╗ ██████╗ ██╗███████╗███╗   ███╗
echo         ██╔══██╗██╔══██╗██║██╔════╝████╗ ████║
echo         ██████╔╝██████╔╝██║███████╗██╔████╔██║
echo         ██╔═══╝ ██╔══██╗██║╚════██║██║╚██╔╝██║
echo         ██║     ██║  ██║██║███████║██║ ╚═╝ ██║
echo         ╚═╝     ╚═╝  ╚═╝╚═╝╚══════╝╚═╝     ╚═╝
echo.
echo MARC27's Platform for Research in Intelligent Synthesis of Materials
echo Windows Installation Script
echo.

REM Check if Python is installed
python --version >nul 2>&1
if %errorlevel% neq 0 (
    echo [ERROR] Python not found. Please install Python 3.8+ from https://python.org
    echo Make sure to check "Add Python to PATH" during installation
    pause
    exit /b 1
)

echo [INFO] Python found, proceeding with installation...

REM Check if we're in the right directory
if not exist "pyproject.toml" (
    if not exist "setup.py" (
        echo [ERROR] Please run this script from the PRISM project directory
        pause
        exit /b 1
    )
)

echo.
echo Choose installation method:
echo 1. Install with pip (recommended)
echo 2. Install with uv (fastest)
echo 3. Development installation
echo 4. Exit
echo.
set /p choice="Select option (1-4): "

if "%choice%"=="1" goto pip_install
if "%choice%"=="2" goto uv_install
if "%choice%"=="3" goto dev_install
if "%choice%"=="4" goto exit
goto invalid_choice

:pip_install
echo [INFO] Installing MARC27's PRISM with pip...
pip install -e .
if %errorlevel% equ 0 (
    echo [SUCCESS] Installation complete!
    goto verify
) else (
    echo [ERROR] Installation failed
    pause
    exit /b 1
)

:uv_install
echo [INFO] Checking for uv...
uv --version >nul 2>&1
if %errorlevel% neq 0 (
    echo [INFO] Installing uv...
    powershell -c "irm https://astral.sh/uv/install.ps1 | iex"
    if %errorlevel% neq 0 (
        echo [ERROR] Failed to install uv. Falling back to pip...
        goto pip_install
    )
)
echo [INFO] Installing MARC27's PRISM with uv...
uv pip install -e .
if %errorlevel% equ 0 (
    echo [SUCCESS] Installation complete!
    goto verify
) else (
    echo [ERROR] Installation failed
    pause
    exit /b 1
)

:dev_install
echo [INFO] Installing development version...
pip install -e ".[dev,export,monitoring]"
if %errorlevel% equ 0 (
    echo [SUCCESS] Development installation complete!
    goto verify
) else (
    echo [ERROR] Development installation failed
    pause
    exit /b 1
)

:verify
echo.
echo [INFO] Verifying installation...
prism --version >nul 2>&1
if %errorlevel% equ 0 (
    echo [SUCCESS] MARC27's PRISM is ready to use!
    echo.
    echo Quick start commands:
    echo   prism --help          # Show all commands
    echo   prism list-databases  # List available databases
    echo   prism getting-started # Interactive tutorial
    echo.
) else (
    echo [WARNING] Command 'prism' not found in PATH
    echo Try restarting your command prompt or running:
    echo   python -m app.cli --help
)
pause
goto exit

:invalid_choice
echo [ERROR] Invalid choice. Please select 1-4.
pause
goto start

:exit
echo.
echo Thank you for using MARC27's PRISM Platform!
exit /b 0
