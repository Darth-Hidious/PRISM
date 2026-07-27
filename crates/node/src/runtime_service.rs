// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! Lifecycle for the local runtime sidecar — the service that answers `/run`
//! (document text extraction, embeddings) and `/deploy` (local model serving)
//! on port 8090.
//!
//! Every caller used to assume something else had already started it. Nothing
//! ever did, so a first-time `prism ingest paper.pdf` died on a bare
//! `Connection refused` against a port no PRISM code path had ever bound.
//!
//! The runtime is shipped as a container image (`docker/Dockerfile.runtime`,
//! `EXPOSE 8090`, published to GHCR by `build-platform-images.yml`, run as
//! `-p 8090:8090` in `docker/docker-compose.enterprise.yml`), so "start it"
//! here means exactly what it means in those files: run that image on the
//! container runtime PRISM already drives for node jobs ([`crate::executor`]).
//!
//! Contract, in order:
//! 1. already answering `/health` → do nothing;
//! 2. loopback URL + a container runtime → start the image, wait for `/health`;
//! 3. anything else → one error that says what was needed, why it could not
//!    start, and the exact command to fix it. Never a bare connect error.

use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use tokio::process::Command;

use crate::executor::{self, ContainerRuntime};

/// Where the runtime lives when nobody says otherwise. Single source of truth
/// for the CLI flag default and the daemon's env fallback.
pub const DEFAULT_RUNTIME_URL: &str = "http://127.0.0.1:8090";

/// Published runtime image. Override with `PRISM_RUNTIME_IMAGE` (air-gapped
/// sites mirror it into their own registry).
const DEFAULT_IMAGE: &str = "ghcr.io/darth-hidious/marc27-runtime:latest";

/// Container name for the PRISM-managed runtime. Stable so a second `prism
/// ingest` reuses the container the first one started instead of stacking up
/// copies fighting over the port.
const CONTAINER_NAME: &str = "prism-runtime";

/// Port the runtime listens on *inside* the container (`EXPOSE 8090`).
const CONTAINER_PORT: u16 = 8090;

/// The architecture the runtime image is published for. Hosts of any other
/// architecture run it emulated rather than not at all.
const EMULATED_PLATFORM: &str = "linux/amd64";

const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);
/// Ceiling for container commands that are quick on a healthy engine.
const CONTAINER_CMD_TIMEOUT: Duration = Duration::from_secs(60);
const READY_TIMEOUT: Duration = Duration::from_secs(120);
const READY_POLL: Duration = Duration::from_millis(500);

/// Runtime URL from the environment, falling back to [`DEFAULT_RUNTIME_URL`].
pub fn default_runtime_url() -> String {
    std::env::var("PRISM_RUNTIME_URL").unwrap_or_else(|_| DEFAULT_RUNTIME_URL.to_string())
}

fn runtime_image() -> String {
    std::env::var("PRISM_RUNTIME_IMAGE").unwrap_or_else(|_| DEFAULT_IMAGE.to_string())
}

fn autostart_enabled() -> bool {
    !matches!(
        std::env::var("PRISM_RUNTIME_AUTOSTART").as_deref(),
        Ok("0") | Ok("false") | Ok("no")
    )
}

/// What [`ensure_running`] had to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeStatus {
    /// Already answering `/health` — nothing was started.
    AlreadyRunning,
    /// PRISM started the container and it became healthy.
    Started,
}

/// Is a runtime answering `/health` at `base_url` right now?
pub async fn is_up(base_url: &str) -> bool {
    let Ok(client) = reqwest::Client::builder().timeout(HEALTH_TIMEOUT).build() else {
        return false;
    };
    matches!(
        client
            .get(format!("{}/health", base_url.trim_end_matches('/')))
            .send()
            .await,
        Ok(response) if response.status().is_success()
    )
}

/// Make sure a runtime is answering at `base_url`, starting one if needed.
///
/// `on_progress` receives human-readable status while a start is in flight
/// (pulling the image can take minutes on a cold machine, and silence there
/// looks like a hang). The CLI prints it; the daemon logs it.
pub async fn ensure_running(base_url: &str, on_progress: impl Fn(&str)) -> Result<RuntimeStatus> {
    if is_up(base_url).await {
        return Ok(RuntimeStatus::AlreadyRunning);
    }

    let Some(port) = loopback_port(base_url) else {
        bail!(unavailable(
            base_url,
            "it is not on this machine, so PRISM cannot start it",
            &format!(
                "start the runtime on that host, or point PRISM at one that is running:\n         \
                 prism ingest <file> --runtime-url {DEFAULT_RUNTIME_URL}"
            ),
        ));
    };

    if !autostart_enabled() {
        bail!(unavailable(
            base_url,
            "autostart is disabled (PRISM_RUNTIME_AUTOSTART=0)",
            &format!(
                "start it yourself:\n         \
                 docker run -d --name {CONTAINER_NAME} -p {port}:{CONTAINER_PORT} {}\n       \
                 or unset PRISM_RUNTIME_AUTOSTART and re-run",
                runtime_image()
            ),
        ));
    }

    let Some(container) = executor::resolve_container_runtime(
        std::env::var("PRISM_NODE_CONTAINER_RUNTIME")
            .ok()
            .as_deref(),
    ) else {
        bail!(unavailable(
            base_url,
            "no container runtime is installed (PRISM looked for docker and podman)",
            "install Docker Desktop (https://docs.docker.com/get-docker/) or podman, \
             then re-run — PRISM starts the runtime for you",
        ));
    };

    let image = runtime_image();
    on_progress(&format!(
        "runtime not running at {base_url} — starting {image} with {} \
         (first run downloads the image, several GB, so it can take a while)",
        container.as_str()
    ));

    if let Err(error) = start_container(container, port, &image, &on_progress).await {
        bail!(unavailable(
            base_url,
            &format!("{} could not start it: {error}", container.as_str()),
            &format!(
                "check the engine is healthy, and that you can read the image:\n         \
                 {bin} info    {bin} login ghcr.io    {bin} pull {image}\n       \
                 or run a runtime elsewhere and point PRISM at it:\n         \
                 prism ingest <file> --runtime-url http://<host>:{CONTAINER_PORT}",
                bin = container.binary()
            ),
        ));
    }

    let deadline = Instant::now() + READY_TIMEOUT;
    while Instant::now() < deadline {
        if is_up(base_url).await {
            on_progress(&format!("runtime ready at {base_url}"));
            return Ok(RuntimeStatus::Started);
        }
        tokio::time::sleep(READY_POLL).await;
    }

    bail!(unavailable(
        base_url,
        &format!(
            "container {CONTAINER_NAME} started but never answered /health within {}s",
            READY_TIMEOUT.as_secs()
        ),
        &format!(
            "check why it is unhealthy:\n         \
             {} logs {CONTAINER_NAME}",
            container.binary()
        ),
    ));
}

/// One error shape for every "no runtime" outcome: what was needed, why it
/// could not start, and the exact command that fixes it.
fn unavailable(base_url: &str, why: &str, fix: &str) -> String {
    format!(
        "the PRISM runtime at {base_url} is not available — it extracts text from documents \
         on this machine and serves local model deployments\n  \
         why: {why}\n  \
         fix: {fix}"
    )
}

/// Start (or restart) the PRISM-managed runtime container on `port`.
async fn start_container(
    runtime: ContainerRuntime,
    port: u16,
    image: &str,
    on_progress: &impl Fn(&str),
) -> Result<()> {
    // A container from an earlier run may still exist. Reuse it — never
    // collide on the name, never stack a second copy on the same port.
    let existing = tokio::time::timeout(
        CONTAINER_CMD_TIMEOUT,
        executor::inspect_container_handle(runtime, CONTAINER_NAME),
    )
    .await
    .map_err(|_| engine_unresponsive(runtime, "inspect"))?;
    match existing {
        Ok((state, _)) if state == "running" => return Ok(()),
        Ok(_) => {
            let started = container_cmd(runtime, &["start".into(), CONTAINER_NAME.into()]).await?;
            if started.status.success() {
                return Ok(());
            }
            // Stale/broken container (e.g. built against a different port
            // mapping) — drop it and recreate below. Bounded like the rest: a
            // wedged engine must not turn removal into a freeze.
            let _ = tokio::time::timeout(
                CONTAINER_CMD_TIMEOUT,
                executor::cleanup_container(runtime, CONTAINER_NAME),
            )
            .await;
        }
        Err(_) => {}
    }

    // The published runtime image is built for linux/amd64 only, so on any
    // other host the native pull dies with "no matching manifest". Retry once
    // under emulation — the `--platform linux/amd64` a human would type — and
    // carry that choice into `run`, out loud, never silently.
    let platform = match executor::ensure_image_available(runtime, image).await {
        Ok(()) => None,
        Err(native) if std::env::consts::ARCH == "x86_64" => return Err(native),
        Err(native) => {
            on_progress(&format!(
                "no {} build of {image} — retrying under {EMULATED_PLATFORM} emulation",
                std::env::consts::ARCH
            ));
            pull_for_platform(runtime, image, EMULATED_PLATFORM)
                .await
                .map_err(|emulated| {
                    anyhow::anyhow!("{native}; and under {EMULATED_PLATFORM}: {emulated}")
                })?;
            Some(EMULATED_PLATFORM)
        }
    };

    let mut args = vec![
        "run".to_string(),
        "-d".to_string(),
        "--name".to_string(),
        CONTAINER_NAME.to_string(),
        "--restart".to_string(),
        "unless-stopped".to_string(),
        "-p".to_string(),
        format!("{port}:{CONTAINER_PORT}"),
    ];
    if let Some(platform) = platform {
        args.push("--platform".to_string());
        args.push(platform.to_string());
    }
    args.push(image.to_string());

    let run = container_cmd(runtime, &args).await?;
    if !run.status.success() {
        bail!("{}", String::from_utf8_lossy(&run.stderr).trim());
    }
    Ok(())
}

/// Run a container command that is quick on a healthy engine, bounded by
/// [`CONTAINER_CMD_TIMEOUT`]. A wedged Docker Desktop makes `run`/`start`/
/// `inspect` hang forever, and a hang is what the user experiences as
/// "`prism ingest` froze" — the one outcome worse than an error.
async fn container_cmd(runtime: ContainerRuntime, args: &[String]) -> Result<std::process::Output> {
    let mut command = Command::new(runtime.binary());
    command.args(args);
    match tokio::time::timeout(CONTAINER_CMD_TIMEOUT, command.output()).await {
        Ok(output) => Ok(output?),
        Err(_) => Err(engine_unresponsive(runtime, &args.join(" "))),
    }
}

fn engine_unresponsive(runtime: ContainerRuntime, what: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "`{} {what}` did not return within {}s — the container engine is not responding",
        runtime.binary(),
        CONTAINER_CMD_TIMEOUT.as_secs()
    )
}

async fn pull_for_platform(runtime: ContainerRuntime, image: &str, platform: &str) -> Result<()> {
    // Deliberately not bounded by CONTAINER_CMD_TIMEOUT: this image is several
    // GB and a first pull legitimately runs for many minutes.
    let pull = Command::new(runtime.binary())
        .args(["pull", "--platform", platform, image])
        .output()
        .await?;
    if pull.status.success() {
        return Ok(());
    }
    bail!("{}", String::from_utf8_lossy(&pull.stderr).trim());
}

/// Port of `base_url` when it points at this machine, else `None` — PRISM only
/// ever starts a runtime for itself, never for another host.
fn loopback_port(base_url: &str) -> Option<u16> {
    let url = reqwest::Url::parse(base_url).ok()?;
    let host = url.host_str()?;
    // `host_str` keeps IPv6 literals bracketed (`[::1]`).
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    let local = bare.eq_ignore_ascii_case("localhost")
        || bare
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    local.then(|| url.port_or_known_default()).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Mutex;

    /// `env::set_var` is process-global; serialize every env-touching test.
    /// Async-aware so the guard can span the `ensure_running` await.
    static ENV_GUARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// A stand-in runtime that answers `GET /health` once.
    fn spawn_healthy_runtime() -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0u8; 4096];
            let read = stream.read(&mut buffer).unwrap();
            assert!(String::from_utf8_lossy(&buffer[..read]).starts_with("GET /health "));
            let body = r#"{"status":"ok"}"#;
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .unwrap();
        });
        (format!("http://{addr}"), handle)
    }

    /// A loopback port with nothing behind it — the "runtime is down" case.
    fn dead_loopback_url() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        format!("http://{addr}")
    }

    /// Already up → report it and start nothing. A second copy on the same
    /// port would fail to bind and take the working one down with it.
    #[tokio::test]
    async fn already_running_runtime_is_never_double_started() {
        let (url, server) = spawn_healthy_runtime();
        let progress = Mutex::new(Vec::<String>::new());

        let status = ensure_running(&url, |msg| progress.lock().unwrap().push(msg.to_string()))
            .await
            .unwrap();

        assert_eq!(status, RuntimeStatus::AlreadyRunning);
        assert!(
            progress.lock().unwrap().is_empty(),
            "a healthy runtime must not trigger a start: {:?}",
            progress.lock().unwrap()
        );
        server.join().unwrap();
    }

    /// The reproduction: runtime down. Before this module the caller surfaced
    /// a bare `Connection refused`; now a start that cannot happen must name
    /// what was needed, why, and the command that fixes it.
    #[tokio::test]
    async fn start_failure_is_actionable_not_a_bare_connect_error() {
        let _guard = ENV_GUARD.lock().await;
        let url = dead_loopback_url();
        unsafe { std::env::set_var("PRISM_RUNTIME_AUTOSTART", "0") };

        let error = ensure_running(&url, |_| {}).await.unwrap_err().to_string();

        unsafe { std::env::remove_var("PRISM_RUNTIME_AUTOSTART") };

        assert!(error.contains(&url), "must name the runtime: {error}");
        assert!(error.contains("why:") && error.contains("fix:"), "{error}");
        assert!(
            error.contains("docker run"),
            "must give the command: {error}"
        );
        assert!(
            !error.contains("Connection refused"),
            "must not degrade to a raw connect error: {error}"
        );
    }

    /// A runtime configured on another host is not ours to start — say so
    /// instead of silently spawning a local container the user never asked for.
    #[tokio::test]
    async fn remote_runtime_is_reported_not_started_locally() {
        // TEST-NET-1 (RFC 5737): routable nowhere, so this only costs the probe timeout.
        let error = ensure_running("http://192.0.2.1:8090", |_| {})
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("not on this machine"), "{error}");
        assert!(error.contains("--runtime-url"), "{error}");
    }

    #[test]
    fn loopback_port_accepts_only_this_machine() {
        assert_eq!(loopback_port("http://127.0.0.1:8090"), Some(8090));
        assert_eq!(loopback_port("http://localhost:8090"), Some(8090));
        assert_eq!(loopback_port("http://[::1]:8090"), Some(8090));
        assert_eq!(loopback_port("http://localhost"), Some(80));
        assert_eq!(loopback_port("http://192.0.2.1:8090"), None);
        assert_eq!(loopback_port("http://runtime.example.com:8090"), None);
        assert_eq!(loopback_port("not a url"), None);
    }
}
