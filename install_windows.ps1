# MARC27's PRISM Platform - PowerShell Installation Script
# Advanced Windows installation with modern PowerShell features

# ASCII Art Logo
$logo = @"
    ███╗   ███╗ █████╗ ██████╗  ██████╗██████╗ ███████╗
    ████╗ ████║██╔══██╗██╔══██╗██╔════╝╚════██╗╚════██║
    ██╔████╔██║███████║██████╔╝██║      █████╔╝    ██╔╝
    ██║╚██╔╝██║██╔══██║██╔══██╗██║     ██╔═══╝    ██╔╝ 
    ██║ ╚═╝ ██║██║  ██║██║  ██║╚██████╗███████╗  ██╗
    ╚═╝     ╚═╝╚═╝  ╚═╝╚═╝  ╚═╝ ╚═════╝╚══════╝ ╚══╝
                                                        
         ██████╗ ██████╗ ██╗███████╗███╗   ███╗
         ██╔══██╗██╔══██╗██║██╔════╝████╗ ████║
         ██████╔╝██████╔╝██║███████╗██╔████╔██║
         ██╔═══╝ ██╔══██╗██║╚════██║██║╚██╔╝██║
         ██║     ██║  ██║██║███████║██║ ╚═╝ ██║
         ╚═╝     ╚═╝  ╚═╝╚═╝╚══════╝╚═╝     ╚═╝

MARC27's Platform for Research in Intelligent Synthesis of Materials
PowerShell Installation Script
"@

# Display logo
Write-Host $logo -ForegroundColor Cyan

# Helper functions
function Test-Command {
    param($Command)
    try {
        Get-Command $Command -ErrorAction Stop | Out-Null
        return $true
    }
    catch {
        return $false
    }
}

function Write-StatusMessage {
    param(
        [string]$Message,
        [string]$Type = "Info"
    )
    
    switch ($Type) {
        "Success" { Write-Host "✅ $Message" -ForegroundColor Green }
        "Error"   { Write-Host "❌ $Message" -ForegroundColor Red }
        "Warning" { Write-Host "⚠️  $Message" -ForegroundColor Yellow }
        "Info"    { Write-Host "ℹ️  $Message" -ForegroundColor Blue }
        default   { Write-Host $Message }
    }
}

function Install-WithPip {
    Write-StatusMessage "Installing MARC27's PRISM with pip..." "Info"
    
    try {
        pip install -e . 2>$null
        if ($LASTEXITCODE -eq 0) {
            Write-StatusMessage "Installation complete!" "Success"
            return $true
        } else {
            Write-StatusMessage "Installation failed" "Error"
            return $false
        }
    }
    catch {
        Write-StatusMessage "Error during pip installation: $($_.Exception.Message)" "Error"
        return $false
    }
}

function Install-WithUv {
    Write-StatusMessage "Checking for uv..." "Info"
    
    if (-not (Test-Command "uv")) {
        Write-StatusMessage "Installing uv..." "Info"
        try {
            Invoke-RestMethod https://astral.sh/uv/install.ps1 | Invoke-Expression
            # Refresh PATH
            $env:PATH = [System.Environment]::GetEnvironmentVariable("PATH", "Machine") + ";" + [System.Environment]::GetEnvironmentVariable("PATH", "User")
        }
        catch {
            Write-StatusMessage "Failed to install uv. Falling back to pip..." "Warning"
            return Install-WithPip
        }
    }
    
    Write-StatusMessage "Installing MARC27's PRISM with uv..." "Info"
    
    try {
        uv pip install -e . 2>$null
        if ($LASTEXITCODE -eq 0) {
            Write-StatusMessage "Installation complete!" "Success"
            return $true
        } else {
            Write-StatusMessage "uv installation failed, trying pip..." "Warning"
            return Install-WithPip
        }
    }
    catch {
        Write-StatusMessage "Error during uv installation: $($_.Exception.Message)" "Warning"
        return Install-WithPip
    }
}

function Install-Development {
    Write-StatusMessage "Installing development version..." "Info"
    
    try {
        pip install -e ".[dev,export,monitoring]" 2>$null
        if ($LASTEXITCODE -eq 0) {
            Write-StatusMessage "Development installation complete!" "Success"
            return $true
        } else {
            Write-StatusMessage "Development installation failed" "Error"
            return $false
        }
    }
    catch {
        Write-StatusMessage "Error during development installation: $($_.Exception.Message)" "Error"
        return $false
    }
}

function Test-Installation {
    Write-StatusMessage "Verifying installation..." "Info"
    
    try {
        prism --version 2>$null | Out-Null
        if ($LASTEXITCODE -eq 0) {
            Write-StatusMessage "MARC27's PRISM is ready to use!" "Success"
            Write-Host ""
            Write-Host "🚀 Quick start commands:" -ForegroundColor Magenta
            Write-Host "  prism --help          # Show all commands" -ForegroundColor White
            Write-Host "  prism list-databases  # List available databases" -ForegroundColor White
            Write-Host "  prism getting-started # Interactive tutorial" -ForegroundColor White
            Write-Host ""
            return $true
        } else {
            Write-StatusMessage "Command 'prism' not found in PATH" "Warning"
            Write-Host "Try restarting PowerShell or running:" -ForegroundColor Yellow
            Write-Host "  python -m app.cli --help" -ForegroundColor White
            return $false
        }
    }
    catch {
        Write-StatusMessage "Installation verification failed" "Error"
        return $false
    }
}

# Main script
try {
    # Check Python installation
    if (-not (Test-Command "python")) {
        Write-StatusMessage "Python not found. Please install Python 3.8+ from https://python.org" "Error"
        Write-Host "Make sure to check 'Add Python to PATH' during installation" -ForegroundColor Yellow
        Read-Host "Press Enter to exit"
        exit 1
    }
    
    Write-StatusMessage "Python found, proceeding with installation..." "Success"
    
    # Check if we're in the right directory
    if (-not (Test-Path "pyproject.toml") -and -not (Test-Path "setup.py")) {
        Write-StatusMessage "Please run this script from the PRISM project directory" "Error"
        Read-Host "Press Enter to exit"
        exit 1
    }
    
    # Installation menu
    Write-Host ""
    Write-Host "🎯 Choose installation method:" -ForegroundColor Magenta
    Write-Host "1. Install with pip (recommended)" -ForegroundColor White
    Write-Host "2. Install with uv (fastest)" -ForegroundColor White
    Write-Host "3. Development installation" -ForegroundColor White
    Write-Host "4. Exit" -ForegroundColor White
    Write-Host ""
    
    do {
        $choice = Read-Host "Select option (1-4)"
        
        switch ($choice) {
            "1" { 
                $success = Install-WithPip
                break
            }
            "2" { 
                $success = Install-WithUv
                break
            }
            "3" { 
                $success = Install-Development
                break
            }
            "4" { 
                Write-Host "Installation cancelled." -ForegroundColor Yellow
                exit 0
            }
            default { 
                Write-StatusMessage "Invalid choice. Please select 1-4." "Error"
                continue
            }
        }
        break
    } while ($true)
    
    if ($success) {
        Test-Installation
    }
    
} catch {
    Write-StatusMessage "Unexpected error: $($_.Exception.Message)" "Error"
    Read-Host "Press Enter to exit"
    exit 1
}

Write-Host ""
Write-Host "Thank you for using MARC27's PRISM Platform! 🌟" -ForegroundColor Cyan
Read-Host "Press Enter to exit"
