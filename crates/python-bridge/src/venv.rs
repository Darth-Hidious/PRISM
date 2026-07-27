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
    // thorough check once (~40 ms) and let its answer decide.
    let missing = missing_requirements_off_thread(&venv_python).await;
    if venv_python.exists() && missing.is_empty() {
        // Complete already — a venv provisioned before markers existed, or a
        // pyproject edit that turned out not to change the requirement set.
        // Record it and return; reinstalling a working venv on every startup
        // would be its own bug.
        write_marker(&venv_dir, declared);
        return Ok(venv_python);
    }
    let platform_absent = missing.iter().any(|m| m == "prism-platform");
    if venv_python.exists() {
        eprintln!(
            "[prism] venv is missing {} — re-syncing: {}",
            plural_deps(missing.len()),
            missing.join(", ")
        );
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
    // `sh` and `curl` are Unix assumptions; on Windows ensurepip above is the
    // only bootstrap (python.org installers ship it), so skip rather than
    // spawn a shell that does not exist.
    if !pip.exists() && !cfg!(windows) {
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
    // Version-matched wheel from the release assets first (no git needed
    // on the target box); git main as fallback for dev builds without a
    // published wheel.
    //
    // Skipped entirely when the platform is already importable and only a
    // leaf dependency has gone missing: re-downloading the whole platform to
    // deliver one 15 KB pure-Python wheel is minutes of network for nothing,
    // and step 4 below installs the shortfall by name either way.
    let version = env!("CARGO_PKG_VERSION");
    let wheel_spec = format!(
        "prism-platform @ https://github.com/Darth-Hidious/PRISM/releases/download/v{version}/prism_platform-{version}-py3-none-any.whl"
    );
    if platform_absent {
        eprintln!("[prism] Installing PRISM tools into venv…");
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
        if !installed {
            return Err(PythonBridgeError::Spawn(std::io::Error::other(format!(
                "could not install PRISM tools — retry manually: \
                 {} install \"{wheel_spec}\"",
                pip.display()
            ))));
        }
    }

    // 4. The published wheel is only ever as current as the last release, so
    // a binary built after a dependency was declared can land a platform that
    // does not pull it. Install whatever is still outstanding by name rather
    // than failing at the user with a diff they did not cause.
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
        let _ = Command::new(&venv_python)
            .args(["-m", "pip", "install"])
            .args(&outstanding)
            .stderr(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::null())
            .status()
            .await;
        missing = missing_requirements_off_thread(&venv_python).await;
    }

    // The verdict is the re-check, never what pip reported about itself.
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

#[cfg(test)]
mod tests {
    use super::*;

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
