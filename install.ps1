# PRISM installer for Windows PowerShell.
# Usage: irm https://prism.marc27.com/install.ps1 | iex
#
# Env vars:
#   PRISM_VERSION      - version tag (default: latest)
#   PRISM_INSTALL_DIR  - install directory (default: %USERPROFILE%\.prism\bin)
#
# Targets Windows PowerShell 5.1 (what ships with Windows 10/11) and
# PowerShell 7+. Everything runs inside `& { ... }` so `iex` cannot leak
# preference variables or helpers into the caller's session.

& {
    $ErrorActionPreference = 'Stop'
    # PowerShell 7.4+ turns a non-zero exit from a native command into a
    # terminating error when ErrorActionPreference is Stop. This script
    # probes for optional things (py launcher, pip) and inspects
    # $LASTEXITCODE itself, so opt out and behave like 5.1 everywhere.
    $PSNativeCommandUseErrorActionPreference = $false

    $Repo = 'Darth-Hidious/PRISM'
    # Exact archive entry names we are willing to write to disk.
    $Bins = @('prism.exe', 'prism-node.exe')

    # Windows PowerShell 5.1 still negotiates TLS 1.0 on older builds and
    # GitHub refuses that. Force TLS 1.2 for this scope.
    try {
        [Net.ServicePointManager]::SecurityProtocol =
            [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    } catch { }

    # --- Detect architecture --------------------------------------------
    # PROCESSOR_ARCHITEW6432 is set when a 32-bit shell runs on 64-bit
    # Windows; it, not PROCESSOR_ARCHITECTURE, names the real machine.
    $machine = $env:PROCESSOR_ARCHITEW6432
    if (-not $machine) { $machine = $env:PROCESSOR_ARCHITECTURE }

    if ($machine -eq 'AMD64') {
        $arch = 'x86_64'
    } elseif ($machine -eq 'ARM64') {
        # No native ARM64 build. Windows on ARM runs x64 binaries under
        # emulation, so this works - say so rather than failing.
        $arch = 'x86_64'
        Write-Host 'ARM64 Windows detected - installing the x64 build (runs under emulation).'
    } elseif ($machine -eq 'x86') {
        Write-Host 'Error: 32-bit Windows is not supported. PRISM needs 64-bit Windows 10 or later.' -ForegroundColor Red
        return
    } else {
        Write-Host "Error: unsupported processor architecture: $machine" -ForegroundColor Red
        return
    }

    $archive = "prism-windows-$arch.zip"

    # --- Resolve version --------------------------------------------------
    $version = $env:PRISM_VERSION
    if (-not $version) { $version = 'latest' }
    if ($version -eq 'latest') {
        Write-Host 'Fetching latest release...'
        try {
            $release = Invoke-RestMethod -UseBasicParsing `
                -Uri "https://api.github.com/repos/$Repo/releases/latest" `
                -Headers @{ 'User-Agent' = 'prism-installer' }
            $version = $release.tag_name
        } catch {
            Write-Host "Error: failed to fetch the latest version from GitHub. $($_.Exception.Message)" -ForegroundColor Red
            return
        }
        if (-not $version) {
            Write-Host 'Error: GitHub returned a release with no tag name.' -ForegroundColor Red
            return
        }
    }

    $installDir = $env:PRISM_INSTALL_DIR
    if (-not $installDir) { $installDir = Join-Path $env:USERPROFILE '.prism\bin' }

    Write-Host "Installing PRISM $version for windows-$arch..."

    # --- Download ---------------------------------------------------------
    $url = "https://github.com/$Repo/releases/download/$version/$archive"
    $tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("prism-install-" + [System.Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmp -Force | Out-Null

    try {
        $zipPath = Join-Path $tmp $archive
        Write-Host "Downloading $url..."
        # $ProgressPreference slows Invoke-WebRequest to a crawl on 5.1.
        $oldProgress = $ProgressPreference
        try {
            $ProgressPreference = 'SilentlyContinue'
            Invoke-WebRequest -Uri $url -OutFile $zipPath -UseBasicParsing
        } catch {
            Write-Host "Error: download failed. $($_.Exception.Message)" -ForegroundColor Red
            Write-Host "Check that $version has a release asset named $archive."
            Write-Host "Available at: https://github.com/$Repo/releases"
            return
        } finally {
            $ProgressPreference = $oldProgress
        }

        # Clear any mark-of-the-web on the archive so the binaries we pull
        # out of it are not born blocked.
        try { Unblock-File -Path $zipPath } catch { }

        # --- Extract ------------------------------------------------------
        #
        # Read the zip entry-by-entry rather than using Expand-Archive.
        # Expand-Archive on 5.1 honours `..\` and absolute entry names, so a
        # malformed or malicious archive could write anywhere on disk. Here
        # an entry is extracted only when its FULL recorded name exactly
        # matches one of $Bins, and the destination path is built by us -
        # which makes `..\..\evil.exe` and `C:\Windows\evil.exe` non-events.
        Write-Host "Extracting to $installDir..."
        New-Item -ItemType Directory -Path $installDir -Force | Out-Null

        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $extracted = @()
        $names = @()
        $zip = [System.IO.Compression.ZipFile]::OpenRead($zipPath)
        try {
            foreach ($entry in $zip.Entries) {
                $name = $entry.FullName -replace '/', '\'
                $names += $name
                if ($Bins -notcontains $name) { continue }
                [System.IO.Compression.ZipFileExtensions]::ExtractToFile(
                    $entry, (Join-Path $installDir $name), $true)
                $extracted += $name
            }
        } finally {
            $zip.Dispose()
        }

        if ($extracted -notcontains 'prism.exe') {
            Write-Host 'Error: archive did not contain prism.exe.' -ForegroundColor Red
            Write-Host 'Listing what we got:'
            $names | ForEach-Object { Write-Host "  $_" }
            return
        }

        foreach ($b in $extracted) {
            try { Unblock-File -Path (Join-Path $installDir $b) } catch { }
        }
    } finally {
        Remove-Item -Path $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }

    # --- Check the Python prerequisite -------------------------------------
    #
    # We deliberately do NOT create the venv here. `prism` provisions
    # ~/.prism/venv itself on every launch (ensure_venv in
    # crates/python-bridge/src/venv.rs) and that implementation self-heals a
    # pipless venv, falls back to `uv python find`, installs the
    # version-matched wheel with a git fallback, and re-verifies `import app`.
    # A PowerShell copy of it would just be a second thing to keep in sync.
    #
    # The floor is Python 3.11 (pyproject requires-python = ">=3.11"). Not
    # optional: on anything older `prism` exits immediately with
    # "No Python 3.11+ found", so passing silently here would be a lying check.
    $python = $null
    foreach ($v in @('3.14', '3.13', '3.12', '3.11')) {
        try {
            $probe = & py "-$v" -c 'import sys; print(sys.executable)' 2>$null
            if ($LASTEXITCODE -eq 0 -and $probe) { $python = "$probe".Trim(); break }
        } catch { }
    }
    if (-not $python) {
        # No py launcher, or no versioned install under it. Fall back to
        # whatever `python` is on PATH, but only if it is new enough.
        foreach ($cmd in @('python3', 'python')) {
            try {
                $probe = & $cmd -c 'import sys; print(sys.executable if sys.version_info >= (3,11) else "")' 2>$null
                if ($LASTEXITCODE -eq 0 -and "$probe".Trim()) { $python = "$probe".Trim(); break }
            } catch { }
        }
    }


    # --- PATH --------------------------------------------------------------
    # Current session first, so `prism` works without opening a new window.
    if (($env:Path -split ';') -notcontains $installDir) {
        $env:Path = "$installDir;$env:Path"
    }

    # Then persist. Read the RAW registry value: reading through
    # [Environment]::GetEnvironmentVariable expands %VAR% references, and
    # writing that back would bake them in permanently, silently breaking
    # every PATH entry that relied on them.
    $persisted = $false
    try {
        $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
        try {
            $raw = $key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
            $kind = [Microsoft.Win32.RegistryValueKind]::ExpandString
            try { $kind = $key.GetValueKind('Path') } catch { }
            if (("$raw" -split ';') -notcontains $installDir) {
                if ([string]::IsNullOrWhiteSpace("$raw")) { $new = $installDir }
                else { $new = "$raw".TrimEnd(';') + ';' + $installDir }
                $key.SetValue('Path', $new, $kind)
                Write-Host "Added $installDir to your user PATH."
            }
            $persisted = $true
        } finally { $key.Dispose() }
    } catch {
        Write-Host "Warning: could not update the persistent PATH: $($_.Exception.Message)" -ForegroundColor Yellow
        Write-Host "  Add it by hand: $installDir"
    }

    # A registry write alone is invisible to already-running processes,
    # including Explorer - the parent of every terminal you open next.
    # Broadcasting WM_SETTINGCHANGE is what makes a new terminal see it.
    if ($persisted) {
        try {
            if (-not ('PrismEnvBroadcast' -as [type])) {
                Add-Type -Name 'PrismEnvBroadcast' -Namespace '' -MemberDefinition @'
[System.Runtime.InteropServices.DllImport("user32.dll", SetLastError = true, CharSet = System.Runtime.InteropServices.CharSet.Auto)]
public static extern System.IntPtr SendMessageTimeout(
    System.IntPtr hWnd, uint Msg, System.IntPtr wParam, string lParam,
    uint fuFlags, uint uTimeout, out System.IntPtr lpdwResult);
'@
            }
            $res = [System.IntPtr]::Zero
            # HWND_BROADCAST 0xffff, WM_SETTINGCHANGE 0x1A, SMTO_ABORTIFHUNG 0x2
            [void][PrismEnvBroadcast]::SendMessageTimeout(
                [System.IntPtr]0xffff, 0x1A, [System.IntPtr]::Zero, 'Environment', 2, 5000, [ref]$res)
        } catch {
            Write-Host 'Note: open a new terminal (or sign out and back in) to pick up the PATH change.'
        }
    }

    New-Item -ItemType Directory -Path (Join-Path $env:USERPROFILE '.prism') -Force | Out-Null

    # --- Stage 1 done: the app itself is installed and runnable -------------
    $exe = Join-Path $installDir 'prism.exe'
    Write-Host ''
    $stamp = 'binary in place'
    if (Test-Path $exe) { try { $stamp = (& $exe --version) } catch { } }
    Write-Host "[1/2] PRISM $version installed - $stamp" -ForegroundColor Green

    # --- Stage 2: the Python tool platform ----------------------------------
    #
    # Provisioning is the binary's job (ensure_venv runs on every `prism`
    # invocation), so trigger it once here with `prism doctor`. That builds
    # %USERPROFILE%\.prism\venv while the user is watching and leaves them
    # looking at the component status tree they should come back to.
    # Re-running this installer resumes rather than restarts: ensure_venv's
    # fast path returns immediately once `import app` works.
    if (-not $python) {
        Write-Host ''
        Write-Host '[2/2] SKIPPED - no Python 3.11+ on this machine.' -ForegroundColor Yellow
        Write-Host ''
        Write-Host '  PRISM will not start without it: the agent runs its tools in a'
        Write-Host "  Python worker and exits with 'No Python 3.11+ found' otherwise."
        Write-Host ''
        Write-Host '    winget install Python.Python.3.12'
        Write-Host '  (or download from https://www.python.org/downloads/windows/)'
        Write-Host ''
        Write-Host '  Then re-run:  irm https://prism.marc27.com/install.ps1 | iex'
    } elseif ($env:PRISM_SKIP_TOOLS -eq '1') {
        Write-Host '[2/2] SKIPPED - PRISM_SKIP_TOOLS=1. Tools install on your first `prism` run.'
    } elseif (Test-Path $exe) {
        Write-Host '[2/2] Setting up the Python tool platform (first run takes a few minutes)...'
        Write-Host ''
        # Never fail the install over this - the binary retries every launch.
        try { & $exe doctor } catch { }
        if ($LASTEXITCODE -ne 0) {
            Write-Host ''
            Write-Host '  Note: setup did not finish cleanly. It retries automatically on your'
            Write-Host '  next `prism` run; `prism doctor` shows what is still missing.'
        }
    }

    Write-Host ''
    Write-Host '  prism            Launch the interactive chat'
    Write-Host '  prism login      Authenticate with MARC27'
    Write-Host '  prism doctor     Re-check local + platform health'
    Write-Host '  prism --help     See all commands'
    Write-Host ''
}
