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

    // Fast path — venv exists AND actually has the PRISM tools. A venv
    // directory alone proves nothing (fresh boxes used to end up with an
    // empty, pipless venv that this fast path then trusted forever).
    if venv_python.exists() && python_has_app(&venv_python).await {
        return Ok(venv_python);
    }

    // 1. Create the venv if the interpreter is missing entirely.
    if !venv_python.exists() {
        eprintln!("[prism] Python venv not found — setting up (~30 s first time)…");
        let system_python = find_system_python().await?;
        eprintln!("[prism] Using {} to create venv", system_python.display());

        let status = Command::new(&system_python)
            .args(["-m", "venv", &venv_dir.to_string_lossy()])
            .status()
            .await
            .map_err(PythonBridgeError::Spawn)?;
        // Debian/Ubuntu without python3-venv half-creates: interpreter
        // lands, ensurepip fails. Retry without pip; we bootstrap it below.
        if !status.success() && !venv_python.exists() {
            let _ = Command::new(&system_python)
                .args(["-m", "venv", "--without-pip", &venv_dir.to_string_lossy()])
                .status()
                .await;
        }
        if !venv_python.exists() {
            return Err(PythonBridgeError::Spawn(std::io::Error::other(
                "python -m venv failed — on Debian/Ubuntu run: sudo apt-get install -y python3-venv",
            )));
        }
    }

    // 2. Self-heal a pipless venv: ensurepip, then pypa's get-pip bootstrap
    // (works without python3-venv and without sudo).
    let pip = venv_dir.join("bin/pip");
    if !pip.exists() {
        let _ = Command::new(&venv_python)
            .args(["-m", "ensurepip", "--upgrade"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await;
    }
    if !pip.exists() {
        eprintln!("[prism] Bootstrapping pip (get-pip.py)…");
        let _ = Command::new("sh")
            .args([
                "-c",
                &format!(
                    "curl -fsSL https://bootstrap.pypa.io/get-pip.py | {} - --quiet",
                    venv_python.to_string_lossy()
                ),
            ])
            .status()
            .await;
    }
    // Verify via `-m pip` (covers the pip-module-but-no-shim case too).
    let pip_works = Command::new(&venv_python)
        .args(["-m", "pip", "--version"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false);
    if !pip_works {
        return Err(PythonBridgeError::Spawn(std::io::Error::other(
            "venv has no working pip — on Debian/Ubuntu run: sudo apt-get install -y python3-venv, \
             then delete ~/.prism/venv and relaunch prism",
        )));
    }

    // 3. Install PRISM tools into the venv — CORE ONLY. `[all]` pulls
    // torch + JAX + MACE + pycalphad (multi-GB, tens of minutes; one user
    // clocked first-run at "23 working days"). Heavy science extras are
    // provisioned on demand by the sidecar (`prism pyiron install`) and
    // per-tool installers instead of taxing every first launch.
    //
    // Version-matched wheel from the release assets first (no git needed
    // on the target box); git main as fallback for dev builds without a
    // published wheel.
    eprintln!("[prism] Installing PRISM tools into venv…");
    let version = env!("CARGO_PKG_VERSION");
    let wheel_spec = format!(
        "prism-platform @ https://github.com/Darth-Hidious/PRISM/releases/download/v{version}/prism_platform-{version}-py3-none-any.whl"
    );
    let git_spec = "prism-platform @ git+https://github.com/Darth-Hidious/PRISM.git";
    let mut installed = false;
    for spec in [wheel_spec.as_str(), git_spec] {
        let pip_status = Command::new(&venv_python)
            .args(["-m", "pip", "install", spec])
            .current_dir(project_root)
            .stderr(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::null())
            .status()
            .await
            .map_err(PythonBridgeError::Spawn)?;
        if pip_status.success() {
            installed = true;
            break;
        }
        eprintln!("[prism] install from {spec} failed — trying fallback…");
    }
    if !installed || !python_has_app(&venv_python).await {
        return Err(PythonBridgeError::Spawn(std::io::Error::other(format!(
            "could not install PRISM tools — retry manually: \
             ~/.prism/venv/bin/pip install \"{wheel_spec}\""
        ))));
    }

    eprintln!("[prism] Venv ready at {}", venv_dir.display());
    Ok(venv_python)
}

/// Does this interpreter have the PRISM tool platform importable?
async fn python_has_app(python: &Path) -> bool {
    Command::new(python)
        .args(["-c", "import app"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
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
        && output.status.success()
    {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path.is_empty() {
            return Ok(PathBuf::from(path));
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
