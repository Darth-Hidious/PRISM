//! Science-sidecar manager — install, update, and check the py3.12 venv
//! that carries deps the main venv's Python can't (pyiron, pycalphad).
//!
//! The main PRISM venv rides the system Python (3.14+); pyiron_atomistics
//! caps at Python 3.12. PRISM provisions `~/.prism/venv-sci` automatically
//! and proxies the affected tools there (app/tools/_sidecar.py — keep the
//! package spec in sync with SIDECAR_PACKAGES).

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::Command;

/// Keep in sync with app/tools/_sidecar.py SIDECAR_PACKAGES.
const SIDECAR_SPEC: &[&str] = &["pyiron_atomistics>=0.5,<0.6", "pycalphad"];
const PYTHON_CANDIDATES: &[&str] = &["python3.12", "python3.11"];

fn sidecar_venv() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".prism").join("venv-sci")
}

fn sidecar_python() -> PathBuf {
    sidecar_venv().join("bin").join("python3")
}

fn find_base_python() -> Option<String> {
    PYTHON_CANDIDATES.iter().find_map(|cand| {
        Command::new(cand)
            .arg("--version")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|_| (*cand).to_string())
    })
}

fn ensure_venv() -> Result<()> {
    if sidecar_python().exists() {
        return Ok(());
    }
    let base = find_base_python().context(
        "no Python 3.12/3.11 found for the science sidecar — install one \
         (e.g. `brew install python@3.12`) and retry",
    )?;
    let out = Command::new(&base)
        .args(["-m", "venv"])
        .arg(sidecar_venv())
        .output()
        .context("failed to create sidecar venv")?;
    if !out.status.success() {
        anyhow::bail!(
            "venv creation failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

/// Check if pyiron is installed in the sidecar and return its version.
pub fn status() -> Result<Option<String>> {
    if !sidecar_python().exists() {
        return Ok(None);
    }
    let out = Command::new(sidecar_python())
        .args([
            "-c",
            "import pyiron_atomistics; print(pyiron_atomistics.__version__)",
        ])
        .output()?;
    if out.status.success() {
        Ok(Some(
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        ))
    } else {
        Ok(None)
    }
}

/// Install the science stack into the sidecar venv.
pub fn install() -> Result<String> {
    ensure_venv()?;
    let out = Command::new(sidecar_python())
        .args(["-m", "pip", "install"])
        .args(SIDECAR_SPEC)
        .output()
        .context("failed to run pip install")?;
    if out.status.success() {
        // Marker consumed by app/tools/_sidecar.py's fast path.
        let _ = std::fs::write(sidecar_venv().join(".provisioned"), SIDECAR_SPEC.join("\n"));
        let v = status()?.unwrap_or_else(|| "unknown".into());
        Ok(format!(
            "science sidecar ready (pyiron_atomistics {v}, pycalphad) at {}",
            sidecar_venv().display()
        ))
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        anyhow::bail!("sidecar pip install failed: {err}")
    }
}

/// Update the science stack inside the pinned windows.
pub fn update() -> Result<String> {
    ensure_venv()?;
    let out = Command::new(sidecar_python())
        .args(["-m", "pip", "install", "--upgrade"])
        .args(SIDECAR_SPEC)
        .output()
        .context("failed to run pip upgrade")?;
    if out.status.success() {
        let v = status()?.unwrap_or_else(|| "unknown".into());
        Ok(format!("science sidecar updated (pyiron_atomistics {v})."))
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        anyhow::bail!("pip upgrade failed: {err}")
    }
}
