//! Managed Python venv — ensures a working Python 3.11+ environment exists
//! under `~/.prism/venv/` before any Python tools are invoked.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::PythonBridgeError;

/// Minimum acceptable Python version.
const MIN_MAJOR: u8 = 3;
const MIN_MINOR: u8 = 11;

/// The workspace `pyproject.toml`, baked into the binary at compile time.
///
/// The venv is provisioned from a *published* wheel (or git main), so there is
/// no pyproject on the target box to read — `--project-root` defaults to `.`,
/// whatever directory the user happened to run `prism` from. Embedding it is
/// what lets a shipped binary know which distributions its own Python code
/// expects, and makes cargo rebuild this crate whenever the declaration
/// changes. Hardcoding the list here instead is the exact failure this file is
/// being repaired for: a set that silently drifts from the declaration.
const PYPROJECT: &str = include_str!("../../../pyproject.toml");

/// Marker written inside the venv **after** its contents were verified.
const MARKER_FILE: &str = ".prism-venv.json";

/// Python that answers "which declared distributions is this venv missing?".
///
/// `importlib.metadata` reads dist-info directories; it does not import the
/// packages, so checking 21 distributions costs ~40 ms instead of the many
/// seconds importing pandas/matplotlib/torch would take. Distribution names
/// are passed as argv so this string never has to be formatted.
const COMPLETENESS_PROBE: &str = r#"
import importlib.metadata as md, sys
missing = []
try:
    import app  # the PRISM tool platform itself
except Exception:
    missing.append("prism-platform")
for name in sys.argv[1:]:
    try:
        md.version(name)
    except Exception:
        missing.append(name)
sys.stdout.write("\n".join(missing))
"#;

/// What a completed provision recorded about itself.
///
/// Deliberately stores the requirement *names* rather than a hash of them: it
/// is the same size in practice, it survives any change of hash function, and
/// `cat ~/.prism/venv/.prism-venv.json` tells a user exactly what their venv
/// was provisioned for. A mismatch can therefore name the new dependency
/// instead of just saying "fingerprint differs".
#[derive(Debug, Serialize, Deserialize)]
struct VenvMarker {
    prism_version: String,
    requirements: Vec<String>,
}

/// Distribution names from `[project] dependencies` in the embedded
/// pyproject, sorted and deduplicated. Parsed once per process.
pub fn declared_requirements() -> &'static [String] {
    static CACHE: OnceLock<Vec<String>> = OnceLock::new();
    CACHE.get_or_init(|| parse_core_requirements(PYPROJECT))
}

/// Pull the `[project] dependencies` array out of a pyproject.
///
/// A hand-rolled scan rather than a TOML dependency: the shape being read is
/// one array of strings in one table, and adding a parser to this crate to
/// read a file it already has in memory is not worth it.
fn parse_core_requirements(pyproject: &str) -> Vec<String> {
    let mut in_project = false;
    let mut in_deps = false;
    let mut names = Vec::new();
    for raw in pyproject.lines() {
        let line = raw.trim();
        if in_deps {
            if line.starts_with(']') {
                break;
            }
            // Quoted entries only — comment lines inside the array are skipped.
            if let Some(spec) = line.strip_prefix('"').and_then(|r| r.split('"').next())
                && let Some(name) = distribution_name(spec)
            {
                names.push(name);
            }
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            in_project = line == "[project]";
            continue;
        }
        if in_project && line.starts_with("dependencies") && line.ends_with('[') {
            in_deps = true;
        }
    }
    names.sort();
    names.dedup();
    names
}

/// `optimade[http-client]>=1.2.0` → `optimade`. Extras, version specifiers and
/// environment markers are all cut at the first character that can start one.
fn distribution_name(spec: &str) -> Option<String> {
    let name = spec
        .split(|c: char| "[<>=!~;, @".contains(c))
        .next()?
        .trim();
    (!name.is_empty()).then(|| name.to_ascii_lowercase())
}

/// Declared distributions this interpreter cannot account for. Empty means the
/// venv is complete for the requirement set this binary was built against.
///
/// An interpreter that is absent or refuses to run reports *everything*
/// missing — which is what is true of it, and is what the callers need in
/// order to say something accurate rather than shrug.
///
/// Deliberately synchronous, and the only implementation: `doctor` calls it
/// from a sync predicate closure (`settle`'s re-check) and `ensure_venv` calls
/// it through `spawn_blocking`. A second, async copy of this probe is how the
/// old `import app` check ended up written twice and documented with "if
/// either probe ever changes, change both".
pub fn missing_requirements(venv_python: &Path) -> Vec<String> {
    if !venv_python.exists() {
        return everything_missing();
    }
    let declared = declared_requirements();
    let output = std::process::Command::new(venv_python)
        .args(["-I", "-c", COMPLETENESS_PROBE])
        .args(declared.iter().map(String::as_str))
        .stderr(std::process::Stdio::null())
        .output();
    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect(),
        _ => everything_missing(),
    }
}

fn everything_missing() -> Vec<String> {
    std::iter::once("prism-platform".to_string())
        .chain(declared_requirements().iter().cloned())
        .collect()
}

/// [`missing_requirements`] off the async workers — the probe starts a Python
/// interpreter, which is ~40 ms of blocking work.
async fn missing_requirements_off_thread(venv_python: &Path) -> Vec<String> {
    let python = venv_python.to_path_buf();
    tokio::task::spawn_blocking(move || missing_requirements(&python))
        .await
        .unwrap_or_else(|_| everything_missing())
}

fn marker_path(venv_dir: &Path) -> PathBuf {
    venv_dir.join(MARKER_FILE)
}

/// Does the venv carry a marker from a verified provision of *this* PRISM
/// version against *this* requirement set?
///
/// A missing, unreadable, torn or stale marker is simply "no" — this can only
/// ever cause the slow, thorough check to run, never a false OK.
fn marker_is_current(venv_dir: &Path, declared: &[String]) -> bool {
    std::fs::read_to_string(marker_path(venv_dir))
        .ok()
        .and_then(|text| serde_json::from_str::<VenvMarker>(&text).ok())
        .is_some_and(|m| m.prism_version == env!("CARGO_PKG_VERSION") && m.requirements == declared)
}

/// Record that the venv was *observed* complete. Only ever called right after
/// [`missing_requirements`] came back empty — never on the strength of what an
/// install claimed about itself.
fn write_marker(venv_dir: &Path, declared: &[String]) {
    let marker = VenvMarker {
        prism_version: env!("CARGO_PKG_VERSION").to_string(),
        requirements: declared.to_vec(),
    };
    if let Ok(text) = serde_json::to_string_pretty(&marker) {
        let _ = std::fs::write(marker_path(venv_dir), text);
    }
}

/// Candidates to try, newest first.
#[cfg(not(windows))]
const PYTHON_CANDIDATES: &[&str] = &[
    "python3.14",
    "python3.13",
    "python3.12",
    "python3.11",
    "python3",
];
const OFFLINE_ENV: &str = "PRISM_OFFLINE";
const WHEELHOUSE_ENV: &str = "PRISM_WHEELHOUSE";
const SCIENCE_EXTRAS: &[&str] = &[
    "qe",
    "calphad",
    "mace",
    "precipitation",
    "lpbf",
    "simulation",
    "ml",
];

/// Windows installers put `python.exe` on PATH, not `python3.X`. `python3`
/// usually resolves to the Microsoft Store stub, which exits non-zero on
/// `--version` — `check_python` rejects it on that basis, so it stays last
/// rather than being special-cased.
#[cfg(windows)]
const PYTHON_CANDIDATES: &[&str] = &["python", "python3"];

/// A venv's interpreter and pip live in different places per platform:
/// `bin/python3` + `bin/pip` on Unix, `Scripts\python.exe` + `Scripts\pip.exe`
/// on Windows. There is no `bin/` and no `python3.exe` in a Windows venv, so
/// hardcoding the Unix layout made `ensure_venv` unable to ever succeed
/// there — it would create the venv, fail to find `bin/python3`, and return
/// the Debian "install python3-venv" error on a machine where venv creation
/// had actually worked.
pub fn venv_layout(venv_dir: &Path) -> (PathBuf, PathBuf) {
    if cfg!(windows) {
        (
            venv_dir.join("Scripts").join("python.exe"),
            venv_dir.join("Scripts").join("pip.exe"),
        )
    } else {
        (venv_dir.join("bin/python3"), venv_dir.join("bin/pip"))
    }
}

/// Ensure a managed venv exists at `{prism_dir}/venv/` and return the path to
/// its `python3` binary.  Creates the venv (and pip-installs PRISM) on first
/// run, printing progress to stderr so it never interferes with JSON stdio.
pub async fn ensure_venv(
    prism_dir: &Path,
    project_root: &Path,
) -> Result<PathBuf, PythonBridgeError> {
    let venv_dir = prism_dir.join("venv");
    let (venv_python, pip) = venv_layout(&venv_dir);
    let declared = declared_requirements();
    let offline = std::env::var(OFFLINE_ENV).is_ok_and(|value| value == "1");
    let wheelhouse = offline_wheelhouse(prism_dir);

    // Fast path — the venv carries a marker from a provision that was
    // *verified* complete for this PRISM version and this declared
    // requirement set. Costs one small file read; no Python starts here.
    //
    // What it replaced was `python -c "import app"`, which said nothing about
    // the rest of the declaration: `app` imported fine on a venv with no
    // `python-ulid` in it, so seven test modules and every torch-free MACE
    // code path were unimportable while this function reported healthy.
    if venv_python.exists() && marker_is_current(&venv_dir, declared) {
        return Ok(venv_python);
    }

    // No marker, or the declaration moved since it was written. Pay for the
    // thorough check once and let its answer decide.
    let missing = missing_requirements_off_thread(&venv_python).await;
    if venv_python.exists() && missing.is_empty() {
        write_marker(&venv_dir, declared);
        return Ok(venv_python);
    }
    let platform_absent = missing.iter().any(|name| name == "prism-platform");
    if venv_python.exists() {
        eprintln!(
            "[prism] venv is missing {} — re-syncing: {}",
            plural_deps(missing.len()),
            missing.join(", ")
        );
    }

    if offline && !wheelhouse.is_dir() {
        return Err(PythonBridgeError::Spawn(std::io::Error::other(format!(
            "offline mode: PRISM Python is incomplete and no wheelhouse exists at {}. \
             Pre-stage it with `prism provision wheels --output {}` before moving this node.",
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
                if cfg!(windows) {
                    "python -m venv failed — reinstall Python from python.org with the \
                     \"pip\" and \"py launcher\" options enabled"
                } else {
                    "python -m venv failed — on Debian/Ubuntu run: sudo apt-get install -y python3-venv"
                },
            )));
        }
    }

    // 2. Self-heal a pipless venv: ensurepip, then pypa's get-pip bootstrap
    // (works without python3-venv and without sudo).
    if !pip.exists() {
        let _ = Command::new(&venv_python)
            .args(["-m", "ensurepip", "--upgrade"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await;
    }
    // `sh` and `curl` are Unix assumptions and are forbidden in hard offline
    // mode. On Windows, python.org installers provide ensurepip.
    if !pip.exists() && !offline && !cfg!(windows) {
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
            if cfg!(windows) {
                "venv has no working pip — reinstall Python from python.org with the \"pip\" \
                 option enabled, then delete %USERPROFILE%\\.prism\\venv and relaunch prism"
            } else {
                "venv has no working pip — on Debian/Ubuntu run: sudo apt-get install -y \
                 python3-venv, then delete ~/.prism/venv and relaunch prism"
            },
        )));
    }

    // 3. Install PRISM tools into the venv — CORE ONLY. `[all]` pulls
    // torch + JAX + MACE + pycalphad (multi-GB, tens of minutes; one user
    // clocked first-run at "23 working days"). Heavy science extras are
    // provisioned on demand by the sidecar (`prism pyiron install`) and
    // per-tool installers instead of taxing every first launch.
    //
    // Install the platform only when it is absent. Source builds install their
    // own checkout first; released binaries use the matching wheel and then git
    // as an online fallback. Hard offline mode can use only source or wheelhouse.
    let version = env!("CARGO_PKG_VERSION");
    let plan = if offline {
        offline_install_plan(version, build_source_root())
    } else {
        install_plan(version, build_source_root())
    };
    if platform_absent {
        eprintln!("[prism] Installing PRISM tools into venv…");
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
            let status = Command::new(&venv_python)
                .args(&pip_args)
                .current_dir(project_root)
                .stderr(std::process::Stdio::inherit())
                .stdout(std::process::Stdio::null())
                .status()
                .await
                .map_err(PythonBridgeError::Spawn)?;
            if status.success() {
                installed = true;
                break;
            }
            eprintln!("[prism] that install failed — trying the next source…");
        }
        if !installed {
            let attempted = plan
                .first()
                .map(|source| source.pip_args().join(" "))
                .unwrap_or_default();
            let message = if offline {
                format!(
                    "offline mode: could not install PRISM tools from {} — \
                     pre-stage compatible wheels with `prism provision wheels --output {}`; \
                     attempted: {attempted}",
                    wheelhouse.display(),
                    wheelhouse.display()
                )
            } else {
                format!(
                    "could not install PRISM tools — retry manually: {} install {attempted}",
                    pip.display()
                )
            };
            return Err(PythonBridgeError::Spawn(std::io::Error::other(message)));
        }
    }

    // A published wheel may predate a newly declared leaf dependency. Install
    // only the outstanding declarations, then verify the complete set again.
    let mut missing = missing_requirements_off_thread(&venv_python).await;
    let outstanding: Vec<&str> = missing
        .iter()
        .map(String::as_str)
        .filter(|name| *name != "prism-platform")
        .collect();
    if !outstanding.is_empty() {
        eprintln!(
            "[prism] Installing {} the platform did not pull: {}",
            plural_deps(outstanding.len()),
            outstanding.join(", ")
        );
        let mut args = vec!["-m", "pip", "install"];
        if offline {
            args.extend(["--no-index", "--find-links"]);
        }
        let mut command = Command::new(&venv_python);
        command.args(&args);
        if offline {
            command.arg(&wheelhouse);
        }
        let _ = command
            .args(&outstanding)
            .stderr(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::null())
            .status()
            .await;
        missing = missing_requirements_off_thread(&venv_python).await;
    }

    if !missing.is_empty() {
        return Err(PythonBridgeError::Spawn(std::io::Error::other(format!(
            "venv at {} is missing {}: {} — install them with: {} install {}",
            venv_dir.display(),
            plural_deps(missing.len()),
            missing.join(", "),
            pip.display(),
            missing.join(" "),
        ))));
    }

    write_marker(&venv_dir, declared);
    eprintln!("[prism] Venv ready at {}", venv_dir.display());
    Ok(venv_python)
}

/// Install one of PRISM's science extras into an existing Python environment.
///
/// This is the agent-actionable provisioning path: callers do not need to
/// construct a pip command from an install hint. Online installs may use the
/// configured index; offline installs require a local wheelhouse and use
/// `--no-index` so pip cannot escape to the network.
pub async fn install_extra(
    python: &Path,
    project_root: &Path,
    extra: &str,
    wheelhouse: Option<&Path>,
) -> Result<(), PythonBridgeError> {
    let extra = validate_extra(extra)?;
    let offline = std::env::var(OFFLINE_ENV).is_ok_and(|value| value == "1");
    let wheelhouse = wheelhouse
        .map(Path::to_path_buf)
        .unwrap_or_else(offline_wheelhouse_from_home);
    if offline && !wheelhouse.is_dir() {
        return Err(PythonBridgeError::Spawn(std::io::Error::other(format!(
            "offline mode: extra '{extra}' needs a wheelhouse at {}",
            wheelhouse.display()
        ))));
    }

    let source = prism_source_root(project_root).or_else(build_source_root);
    let requirement = source
        .map(|root| format!("{}[{extra}]", root.display()))
        .unwrap_or_else(|| format!("prism-platform[{extra}]=={}", env!("CARGO_PKG_VERSION")));
    let mut args = vec!["-m", "pip", "install"];
    if offline {
        args.extend(["--no-index", "--find-links"]);
    } else if wheelhouse.is_dir() {
        args.push("--find-links");
    }
    let mut owned_args = args.into_iter().map(str::to_string).collect::<Vec<_>>();
    if offline || wheelhouse.is_dir() {
        owned_args.push(wheelhouse.to_string_lossy().into_owned());
    }
    owned_args.push(requirement);

    eprintln!("[prism] provisioning [{extra}] into {}", python.display());
    let status = Command::new(python)
        .args(&owned_args)
        .current_dir(project_root)
        .status()
        .await
        .map_err(PythonBridgeError::Spawn)?;
    if status.success() {
        Ok(())
    } else {
        Err(PythonBridgeError::Spawn(std::io::Error::other(format!(
            "pip could not install PRISM extra '{extra}' (exit status {status})"
        ))))
    }
}

/// Vendor the core package plus requested extras into a portable wheelhouse.
/// Run this on a connected staging machine, then copy the output directory to
/// the offline node and set `PRISM_WHEELHOUSE` if it is not `~/.prism/wheelhouse`.
pub async fn pre_stage_wheels(
    python: &Path,
    project_root: &Path,
    output: &Path,
    extras: &[String],
) -> Result<(), PythonBridgeError> {
    if std::env::var(OFFLINE_ENV).is_ok_and(|value| value == "1") {
        return Err(PythonBridgeError::Spawn(std::io::Error::other(
            "cannot pre-stage wheels in hard offline mode; run this command on a connected staging machine",
        )));
    }
    if extras.is_empty() {
        return Err(PythonBridgeError::Spawn(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "at least one --extra is required",
        )));
    }
    let extras = extras
        .iter()
        .map(|extra| validate_extra(extra).map(str::to_string))
        .collect::<Result<Vec<_>, _>>()?;
    std::fs::create_dir_all(output).map_err(PythonBridgeError::Spawn)?;

    let source = prism_source_root(project_root).or_else(build_source_root);
    let requirement = source
        .map(|_| format!(".[{}]", extras.join(",")))
        .unwrap_or_else(|| {
            format!(
                "prism-platform[{}]=={}",
                extras.join(","),
                env!("CARGO_PKG_VERSION")
            )
        });
    let status = Command::new(python)
        .args([
            "-m",
            "pip",
            "wheel",
            "--wheel-dir",
            &output.to_string_lossy(),
            "--prefer-binary",
            &requirement,
        ])
        .current_dir(project_root)
        .status()
        .await
        .map_err(PythonBridgeError::Spawn)?;
    if status.success() {
        Ok(())
    } else {
        Err(PythonBridgeError::Spawn(std::io::Error::other(format!(
            "pip could not pre-stage PRISM extras {} (exit status {status})",
            extras.join(", ")
        ))))
    }
}

fn validate_extra(extra: &str) -> Result<&str, PythonBridgeError> {
    let extra = extra.trim();
    if SCIENCE_EXTRAS.contains(&extra) {
        Ok(extra)
    } else {
        Err(PythonBridgeError::Spawn(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "unsupported PRISM extra '{extra}'; choose one of: {}",
                SCIENCE_EXTRAS.join(", ")
            ),
        )))
    }
}

fn offline_wheelhouse_from_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".prism/wheelhouse")
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

fn plural_deps(n: usize) -> String {
    if n == 1 {
        "1 declared dependency".to_string()
    } else {
        format!("{n} declared dependencies")
    }
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

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "prism-venv-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The declaration is read from the real pyproject, not restated here.
    /// If this list is ever wrong, everything downstream is wrong.
    #[test]
    fn requirements_come_from_the_real_pyproject() {
        let reqs = declared_requirements();
        // A parse that silently yields nothing would quietly restore the old
        // behaviour: a check that only ever runs `import app`. Name it.
        assert!(
            reqs.len() > 10,
            "the pyproject parse produced {} requirements — the completeness \
             check would be inert: {reqs:?}",
            reqs.len()
        );
        assert!(reqs.contains(&"rich".to_string()), "{reqs:?}");
        assert!(reqs.contains(&"beautifulsoup4".to_string()), "{reqs:?}");
        // Extras are stripped, not carried into the distribution name.
        assert!(reqs.contains(&"optimade".to_string()), "{reqs:?}");
        assert!(
            !reqs.iter().any(|r| r.contains('[') || r.contains('>')),
            "specifiers/extras must be stripped: {reqs:?}"
        );
        // Optional-dependency groups are a different table and must not leak in.
        assert!(!reqs.contains(&"torch".to_string()), "{reqs:?}");
        assert!(!reqs.contains(&"pycalphad".to_string()), "{reqs:?}");
        assert!(!reqs.contains(&"pytest".to_string()), "{reqs:?}");
    }

    /// The regression that started this: `python-ulid` is imported at module
    /// scope by `app/tools/simulation/mace/ids.py`, so it has to be part of
    /// the set a default provision is checked against. While it lived in the
    /// `[mace]` extra it was not, and the venv reported healthy without it.
    #[test]
    fn python_ulid_is_part_of_the_default_declaration() {
        assert!(
            declared_requirements().contains(&"python-ulid".to_string()),
            "declared: {:?}",
            declared_requirements()
        );
    }

    #[test]
    fn parser_takes_only_the_project_table_and_ignores_comments() {
        let toml = "\
[build-system]
requires = [\"setuptools>=61.0\"]

[project]
name = \"x\"
classifiers = [
    \"Topic :: Scientific/Engineering\",
]
dependencies = [
    \"rich>=12.0.0\",
    # a comment inside the array
    \"optimade[http-client]>=1.2.0\",
    \"pyiron-base>=0.9; python_version<'3.15'\",
]

[project.optional-dependencies]
mace = [
    \"torch>=2.5.0\",
]
";
        assert_eq!(
            parse_core_requirements(toml),
            vec![
                "optimade".to_string(),
                "pyiron-base".to_string(),
                "rich".to_string(),
            ]
        );
    }

    /// An interpreter that cannot be run accounts for nothing. Reporting an
    /// empty missing-list there would be the same lie as `import app` passing
    /// on an incomplete venv.
    #[test]
    fn an_absent_interpreter_accounts_for_nothing() {
        let missing = missing_requirements(Path::new("/nonexistent/prism/bin/python3"));
        assert!(missing.contains(&"prism-platform".to_string()));
        assert_eq!(missing.len(), declared_requirements().len() + 1);
    }

    /// RED before the fix: a venv missing a declared dependency has to be
    /// *detected*. A real interpreter with none of the declared distributions
    /// installed stands in for one.
    #[test]
    fn a_venv_missing_declared_dependencies_is_detected() {
        let Some(python) = ["/usr/bin/python3", "/opt/homebrew/bin/python3"]
            .into_iter()
            .map(Path::new)
            .find(|p| p.exists())
        else {
            return;
        };
        let missing = missing_requirements(python);
        assert!(
            !missing.is_empty(),
            "a stock interpreter is not a provisioned PRISM venv"
        );
        assert!(
            missing.contains(&"prism-platform".to_string()),
            "{missing:?}"
        );
    }

    /// The fast path must not go green off a marker that was written for a
    /// different declaration — that is exactly how a venv goes stale in place.
    #[test]
    fn a_marker_from_a_different_declaration_is_stale() {
        let venv = tmpdir("marker-stale");
        write_marker(&venv, &["rich".to_string()]);
        assert!(
            !marker_is_current(&venv, declared_requirements()),
            "a marker for a smaller requirement set must not satisfy the real one"
        );
        let _ = std::fs::remove_dir_all(&venv);
    }

    /// …and a marker for a different PRISM version is stale too: the platform
    /// wheel it was provisioned from is version-matched.
    #[test]
    fn a_marker_from_a_different_prism_version_is_stale() {
        let venv = tmpdir("marker-version");
        let marker = VenvMarker {
            prism_version: "0.0.0-not-this-build".to_string(),
            requirements: declared_requirements().to_vec(),
        };
        std::fs::write(marker_path(&venv), serde_json::to_string(&marker).unwrap()).unwrap();
        assert!(!marker_is_current(&venv, declared_requirements()));
        let _ = std::fs::remove_dir_all(&venv);
    }

    /// GREEN: a venv provisioned against this exact declaration takes the
    /// fast path — no Python start, no reinstall. This is what keeps the
    /// check affordable before every subcommand.
    #[test]
    fn a_marker_matching_this_build_is_current() {
        let venv = tmpdir("marker-current");
        write_marker(&venv, declared_requirements());
        assert!(marker_is_current(&venv, declared_requirements()));
        let _ = std::fs::remove_dir_all(&venv);
    }

    /// Garbage on disk must degrade to the slow, thorough check — never to a
    /// false OK.
    #[test]
    fn an_unreadable_marker_is_never_treated_as_healthy() {
        let venv = tmpdir("marker-torn");
        std::fs::write(marker_path(&venv), b"{\"prism_version\": ").unwrap();
        assert!(!marker_is_current(&venv, declared_requirements()));
        let _ = std::fs::remove_dir_all(&venv);
    }

    #[test]
    fn missing_dependencies_are_counted_in_english() {
        assert_eq!(plural_deps(1), "1 declared dependency");
        assert_eq!(plural_deps(3), "3 declared dependencies");
    }

    /// A path that cannot host a venv must produce an error naming what is
    /// missing and an exact command — never a silent success.
    #[tokio::test]
    async fn ensure_venv_fails_loudly_when_it_cannot_build_one() {
        let root = tmpdir("ensure-fail");
        std::fs::write(root.join("venv"), b"not a directory").unwrap();
        let err = ensure_venv(&root, &root)
            .await
            .expect_err("must not claim success");
        let text = err.to_string();
        assert!(!text.is_empty(), "an empty error is not an actionable one");
        let _ = std::fs::remove_dir_all(&root);
    }
}
