//! Managed Python venv — ensures a working Python 3.11+ environment exists
//! under `~/.prism/venv/` before any Python tools are invoked.

use std::path::{Path, PathBuf};

use tokio::process::Command;

use crate::PythonBridgeError;

/// Minimum acceptable Python version.
const MIN_MAJOR: u8 = 3;
const MIN_MINOR: u8 = 11;

/// Candidates to try, newest first.
const PYTHON_CANDIDATES: &[&str] = &[
    "python3.14",
    "python3.13",
    "python3.12",
    "python3.11",
    "python3",
];

/// Ensure a managed venv exists at `{prism_dir}/venv/` and return the path to
/// its `python3` binary.  Creates the venv (and pip-installs PRISM) on first
/// run, printing progress to stderr so it never interferes with JSON stdio.
pub async fn ensure_venv(
    prism_dir: &Path,
    project_root: &Path,
) -> Result<PathBuf, PythonBridgeError> {
    let venv_dir = prism_dir.join("venv");
    let venv_python = venv_dir.join("bin/python3");

    // Fast path — venv already exists.
    if venv_python.exists() {
        return Ok(venv_python);
    }

    eprintln!("[prism] Python venv not found — setting up (~30 s first time)…");

    // 1. Find a suitable system Python.
    let system_python = find_system_python().await?;
    eprintln!("[prism] Using {} to create venv", system_python.display());

    // 2. Create the venv.
    let status = Command::new(&system_python)
        .args(["-m", "venv", &venv_dir.to_string_lossy()])
        .status()
        .await
        .map_err(PythonBridgeError::Spawn)?;

    if !status.success() {
        return Err(PythonBridgeError::Spawn(std::io::Error::other(
            "python -m venv failed",
        )));
    }

    // 3. Install PRISM tools into the venv.
    eprintln!("[prism] Installing PRISM tools into venv…");
    let pip = venv_dir.join("bin/pip");
    let install_spec =
        "prism-platform[all] @ git+https://github.com/Darth-Hidious/PRISM.git".to_string();
    let pip_status = Command::new(&pip)
        .args(["install", &install_spec])
        .current_dir(project_root)
        .stderr(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::null())
        .status()
        .await
        .map_err(PythonBridgeError::Spawn)?;

    if !pip_status.success() {
        return Err(PythonBridgeError::Spawn(std::io::Error::other(
            "pip install prism-platform failed",
        )));
    }

    eprintln!("[prism] Venv ready at {}", venv_dir.display());
    Ok(venv_python)
}

/// Try each candidate, then fall back to `uv python find`.
async fn find_system_python() -> Result<PathBuf, PythonBridgeError> {
    for candidate in PYTHON_CANDIDATES {
        if let Some(path) = check_python(candidate).await {
            return Ok(path);
        }
    }

    // Fallback: uv python find
    if let Ok(output) = Command::new("uv")
        .args(["python", "find", "--min-version", "3.11"])
        .output()
        .await
    {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Ok(PathBuf::from(path));
            }
        }
    }

    Err(PythonBridgeError::Spawn(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "No Python 3.11+ found. Install Python 3.11 or later and try again.",
    )))
}

/// Run `{candidate} --version`, parse the output, and return the path if it
/// meets the minimum version requirement.
async fn check_python(candidate: &str) -> Option<PathBuf> {
    let output = Command::new(candidate)
        .arg("--version")
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    // Output looks like "Python 3.13.2"
    let text = String::from_utf8_lossy(&output.stdout);
    let version_str = text.trim().strip_prefix("Python ")?;
    let mut parts = version_str.split('.');
    let major: u8 = parts.next()?.parse().ok()?;
    let minor: u8 = parts.next()?.parse().ok()?;

    if major > MIN_MAJOR || (major == MIN_MAJOR && minor >= MIN_MINOR) {
        Some(PathBuf::from(candidate))
    } else {
        None
    }
}
