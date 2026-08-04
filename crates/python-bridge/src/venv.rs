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
const OFFLINE_ENV: &str = "PRISM_OFFLINE";
const WHEELHOUSE_ENV: &str = "PRISM_WHEELHOUSE";

/// Ensure a managed venv exists at `{prism_dir}/venv/` and return the path to
/// its `python3` binary.  Creates the venv (and pip-installs PRISM) on first
/// run, printing progress to stderr so it never interferes with JSON stdio.
pub async fn ensure_venv(
    prism_dir: &Path,
    project_root: &Path,
) -> Result<PathBuf, PythonBridgeError> {
    let venv_dir = prism_dir.join("venv");
    let venv_python = venv_dir.join("bin/python3");
    let offline = std::env::var(OFFLINE_ENV).is_ok_and(|value| value == "1");
    let wheelhouse = offline_wheelhouse(prism_dir);

    // Fast path — venv exists AND actually has the PRISM tools. A venv
    // directory alone proves nothing (fresh boxes used to end up with an
    // empty, pipless venv that this fast path then trusted forever).
    if venv_python.exists() && python_has_app(&venv_python).await {
        return Ok(venv_python);
    }

    if offline && !wheelhouse.is_dir() {
        return Err(PythonBridgeError::Spawn(std::io::Error::other(format!(
            "offline mode: PRISM Python is not installed and no wheelhouse exists at {}. \
             Pre-stage it with `prism provision --wheels {}` before moving this node.",
            wheelhouse.display(),
            wheelhouse.display()
        ))));
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
    if !pip.exists() && !offline {
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
    // WHERE the tools come from is decided by `install_plan` — see there
    // for why a source build must never pull a published wheel.
    eprintln!("[prism] Installing PRISM tools into venv…");
    let version = env!("CARGO_PKG_VERSION");
    let plan = if offline {
        offline_install_plan(version, build_source_root())
    } else {
        install_plan(version, build_source_root())
    };
    let mut installed = false;
    for source in &plan {
        eprintln!("[prism] {}", source.describe());
        let mut pip_args = vec!["-m".to_string(), "pip".to_string(), "install".to_string()];
        if offline {
            pip_args.extend([
                "--no-index".to_string(),
                "--find-links".to_string(),
                wheelhouse.to_string_lossy().into_owned(),
            ]);
        }
        pip_args.extend(source.pip_args());
        let pip_status = Command::new(&venv_python)
            .args(&pip_args)
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
        eprintln!("[prism] that install failed — trying the next source…");
    }
    if !installed || !python_has_app(&venv_python).await {
        let retry = plan
            .first()
            .map(|s| s.pip_args().join(" "))
            .unwrap_or_default();
        let message = if offline {
            format!(
                "offline mode: could not install PRISM tools from {} — \
                 pre-stage compatible wheels with `prism provision --wheels {}`; \
                 attempted: {retry}",
                wheelhouse.display(),
                wheelhouse.display()
            )
        } else {
            format!(
                "could not install PRISM tools — retry manually: \
                 ~/.prism/venv/bin/pip install {retry}"
            )
        };
        return Err(PythonBridgeError::Spawn(std::io::Error::other(message)));
    }

    eprintln!("[prism] Venv ready at {}", venv_dir.display());
    Ok(venv_python)
}

/// The directory holding *this crate's* `Cargo.toml`, baked in at compile
/// time. On the machine that COMPILED the binary it points at the checkout
/// that was built; on any other machine it does not exist. That difference
/// is the whole signal: it distinguishes "developer running what they just
/// built" from "user running a downloaded release", with no flag to set and
/// nothing to detect at runtime.
const BUILD_MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

/// Where the Python half of PRISM is installed from, in preference order.
#[derive(Debug, Clone, PartialEq, Eq)]
enum InstallSource {
    /// A wheelhouse install of the published Python package.
    Wheelhouse { version: String },
    /// An editable install of the checkout this binary was built from.
    /// Editable rather than a plain `pip install <dir>` so that later
    /// edits to `app/` are live without re-provisioning the venv.
    Source(PathBuf),
    /// The wheel published alongside *this binary's own* release tag.
    Wheel { version: String },
    /// Git main — last resort for a build whose version has no published
    /// wheel (a tag cut but not yet released, a fork, a nightly).
    Git,
}

impl InstallSource {
    fn pip_args(&self) -> Vec<String> {
        match self {
            Self::Wheelhouse { version } => vec![format!("prism-platform=={version}")],
            Self::Source(root) => vec!["-e".to_string(), root.to_string_lossy().into_owned()],
            Self::Wheel { version } => vec![format!(
                "prism-platform @ https://github.com/Darth-Hidious/PRISM/releases/download/v{version}/prism_platform-{version}-py3-none-any.whl"
            )],
            Self::Git => {
                vec!["prism-platform @ git+https://github.com/Darth-Hidious/PRISM.git".to_string()]
            }
        }
    }

    fn describe(&self) -> String {
        match self {
            Self::Wheelhouse { version } => format!("from the offline wheelhouse (v{version})"),
            Self::Source(root) => format!("from this source tree ({})", root.display()),
            Self::Wheel { version } => format!("from the v{version} release wheel"),
            Self::Git => "from git main".to_string(),
        }
    }
}

/// Ordered list of places to try installing `prism-platform` from.
///
/// The bug this encodes against: the plan used to be the release wheel
/// first, unconditionally. The crate version has sat at `1.0.0` across
/// months of work, so *every* build from source — including one made from
/// HEAD five minutes ago — pip-installed the wheel attached to the v1.0.0
/// release and ran months-old Python. None of the day's work reached the
/// user's tools, and nothing said so.
///
/// So: if the checkout this binary was compiled from is still on disk, that
/// is what gets installed. Otherwise the wheel, keyed on the binary's own
/// version (never a literal), then git main.
fn install_plan(version: &str, source_root: Option<PathBuf>) -> Vec<InstallSource> {
    let mut plan = Vec::new();
    if let Some(root) = source_root {
        plan.push(InstallSource::Source(root));
    }
    plan.push(InstallSource::Wheel {
        version: version.to_string(),
    });
    plan.push(InstallSource::Git);
    plan
}

fn offline_wheelhouse(prism_dir: &Path) -> PathBuf {
    std::env::var_os(WHEELHOUSE_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| prism_dir.join("wheelhouse"))
}

fn offline_install_plan(version: &str, source_root: Option<PathBuf>) -> Vec<InstallSource> {
    source_root
        .map(|root| vec![InstallSource::Source(root)])
        .unwrap_or_else(|| {
            vec![InstallSource::Wheelhouse {
                version: version.to_string(),
            }]
        })
}

/// The PRISM checkout this binary was built from, if it is still present.
fn build_source_root() -> Option<PathBuf> {
    // `crates/python-bridge` → repo root.
    let root = Path::new(BUILD_MANIFEST_DIR).parent()?.parent()?;
    prism_source_root(root)
}

/// `Some(root)` iff `root` is a PRISM source checkout we can install from.
///
/// Checked rather than assumed: the compile-time path can be occupied by
/// something else entirely by the time the binary runs (a CI workspace
/// reused for another repo, a directory the developer moved). Installing
/// whatever happens to live there would be worse than falling back to the
/// wheel.
fn prism_source_root(root: &Path) -> Option<PathBuf> {
    if !root.join("app").join("__init__.py").is_file() {
        return None;
    }
    let pyproject = std::fs::read_to_string(root.join("pyproject.toml")).ok()?;
    pyproject
        .lines()
        .any(|l| l.trim_start().starts_with("name") && l.contains("prism-platform"))
        .then(|| root.to_path_buf())
}

/// Does this interpreter have the PRISM tool platform importable?
async fn python_has_app(python: &Path) -> bool {
    Command::new(python)
        .args(["-I", "-c", "import app"])
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

    // `uv python find` is useful online, but keep the offline path strictly
    // local: a future uv configuration must not turn this into a download.
    if std::env::var(OFFLINE_ENV).is_err()
        && let Ok(output) = Command::new("uv")
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

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    /// THE defect. A binary built from this checkout must install the
    /// Python half from this checkout — not from the wheel attached to
    /// whatever release happens to share the crate's version number.
    ///
    /// Before the fix a cold install of a build from HEAD pip-installed
    /// `prism_platform-1.0.0-py3-none-any.whl` from the v1.0.0 release
    /// (3 July), so the day's Python work never reached the user.
    #[test]
    fn a_build_from_source_installs_that_source_not_a_published_wheel() {
        let root = build_source_root().expect(
            "this test runs from the checkout it was compiled in, so the \
             source tree must be detected",
        );
        assert!(
            root.join("app").join("__init__.py").is_file(),
            "detected root {} is not the PRISM source tree",
            root.display()
        );

        let plan = install_plan(env!("CARGO_PKG_VERSION"), Some(root.clone()));
        assert_eq!(
            plan.first(),
            Some(&InstallSource::Source(root.clone())),
            "a source build must install its own app/ first"
        );
        assert_eq!(
            plan[0].pip_args(),
            vec!["-e".to_string(), root.to_string_lossy().into_owned()]
        );
        assert!(
            !plan[0]
                .pip_args()
                .iter()
                .any(|a| a.contains("releases/download")),
            "a source build must not reach for a release asset"
        );
    }

    /// The released-binary path, unchanged and still keyed on the binary's
    /// OWN version — never a literal. A release that forgets to publish a
    /// wheel still falls through to git main rather than dying.
    #[test]
    fn without_a_source_tree_the_wheel_matches_the_binarys_own_version() {
        let plan = install_plan("7.3.1", None);
        assert_eq!(
            plan,
            vec![
                InstallSource::Wheel {
                    version: "7.3.1".to_string()
                },
                InstallSource::Git
            ]
        );
        let url = plan[0].pip_args().remove(0);
        assert!(
            url.contains("/download/v7.3.1/prism_platform-7.3.1-py3-none-any.whl"),
            "wheel URL must carry the binary's own version, got {url}"
        );
        assert!(
            !url.contains("1.0.0"),
            "the version must not be hardcoded, got {url}"
        );
    }

    #[test]
    fn offline_install_never_falls_back_to_remote_sources() {
        let source_root = PathBuf::from("/opt/prism");
        let source_plan = offline_install_plan("1.0.0", Some(source_root.clone()));
        assert_eq!(
            source_plan,
            vec![InstallSource::Source(source_root.clone())]
        );
        assert!(
            source_plan[0]
                .pip_args()
                .iter()
                .all(|arg| !arg.contains("https://") && !arg.contains("git+")),
            "offline source install must not contain a remote URL"
        );

        let wheel_plan = offline_install_plan("1.0.0", None);
        assert_eq!(
            wheel_plan,
            vec![InstallSource::Wheelhouse {
                version: "1.0.0".to_string()
            }]
        );
        assert_eq!(wheel_plan[0].pip_args(), vec!["prism-platform==1.0.0"]);
    }

    /// The compile-time path can be occupied by something else by the time
    /// the binary runs. Installing whatever lives there would be worse than
    /// falling back to the wheel.
    #[test]
    fn only_a_real_prism_checkout_counts_as_a_source_tree() {
        let tmp = tempfile::tempdir().unwrap();

        // Empty directory.
        assert_eq!(prism_source_root(tmp.path()), None);

        // Somebody else's Python project sitting at the same path.
        let other = tmp.path().join("other");
        write(&other.join("app").join("__init__.py"), "");
        write(
            &other.join("pyproject.toml"),
            "[project]\nname = \"totally-different\"\n",
        );
        assert_eq!(
            prism_source_root(&other),
            None,
            "a non-PRISM project must not be installed into the user's venv"
        );

        // PRISM's pyproject but no app/ — a partial or pruned tree.
        let pruned = tmp.path().join("pruned");
        write(
            &pruned.join("pyproject.toml"),
            "[project]\nname = \"prism-platform\"\n",
        );
        assert_eq!(prism_source_root(&pruned), None);

        // The real shape.
        let good = tmp.path().join("good");
        write(&good.join("app").join("__init__.py"), "");
        write(
            &good.join("pyproject.toml"),
            "[project]\nname = \"prism-platform\"\nversion = \"1.0.0\"\n",
        );
        assert_eq!(prism_source_root(&good), Some(good));
    }
}
