use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use prism_proto::NodeCapabilities;
use prism_python_bridge::PythonWorkerConfig;
use prism_runtime::{PlatformEndpoints, PrismPaths, StoredCredentials};
use prism_workflows::{
    discover_workflows, execute_workflow, find_workflow, parse_workflow_command_args,
    WorkflowRunResult, WorkflowSpec,
};
use serde::{Deserialize, Serialize};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "prism")]
#[command(about = "Rust control-plane backbone for PRISM")]
struct Cli {
    #[arg(long, global = true, default_value = "python3")]
    python: PathBuf,
    #[arg(long, global = true, default_value = ".")]
    project_root: PathBuf,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Run first-time native setup and platform login.
    Setup,
    /// Authenticate against the MARC27 platform using device flow.
    Login,
    /// Show runtime paths, endpoints, and auth status.
    Status,
    /// List, inspect, and run YAML-defined workflows.
    Workflow {
        #[command(subcommand)]
        command: WorkflowCommands,
    },
    /// Start the Python backend worker under Rust supervision.
    Backend {
        #[arg(long, default_value = ".")]
        project_root: PathBuf,
        #[arg(long, default_value = "python3")]
        python: PathBuf,
    },
    /// PRISM node lifecycle commands.
    Node {
        #[command(subcommand)]
        command: NodeCommands,
    },
    #[command(external_subcommand)]
    External(Vec<String>),
}

#[derive(Debug, Subcommand)]
enum WorkflowCommands {
    List,
    Show {
        name: String,
    },
    Run {
        name: String,
        #[arg(long = "set")]
        pairs: Vec<String>,
        #[arg(long)]
        execute: bool,
    },
}

#[derive(Debug, Subcommand)]
enum NodeCommands {
    /// Start the node daemon — register with the platform and wait for jobs.
    Up {
        /// Node name (default: hostname).
        #[arg(long)]
        name: Option<String>,
        /// Visibility: public, org, or private.
        #[arg(long, default_value = "private")]
        visibility: String,
        /// Price per hour in USD if public (default: free).
        #[arg(long)]
        price: Option<f64>,
        /// Additional paths to scan for datasets (comma-separated).
        #[arg(long, value_delimiter = ',')]
        data_paths: Vec<String>,
        /// Additional paths to scan for models (comma-separated).
        #[arg(long, value_delimiter = ',')]
        model_paths: Vec<String>,
        /// Don't offer compute services.
        #[arg(long)]
        no_compute: bool,
        /// Don't offer storage services.
        #[arg(long)]
        no_storage: bool,
        #[arg(
            long,
            help = "Advertise an SSH endpoint for this node, bound to the logged-in user"
        )]
        ssh_host: Option<String>,
        #[arg(
            long,
            default_value_t = 22,
            help = "SSH port for the advertised endpoint"
        )]
        ssh_port: u16,
        #[arg(long, help = "SSH user for the advertised endpoint")]
        ssh_user: Option<String>,
        /// Run as a background daemon (detach from terminal).
        #[arg(long)]
        background: bool,
        /// Serve a specific model for inference via Ollama.
        #[arg(long)]
        serve: Option<String>,
    },
    /// Stop a running node daemon.
    Down,
    /// Show current node capabilities and status.
    Status,
    /// Probe local capabilities without connecting.
    Probe,
    /// Manage E2EE node keypair.
    Key {
        #[command(subcommand)]
        command: KeyCommands,
    },
}

#[derive(Debug, Subcommand)]
enum KeyCommands {
    /// Show the node's public key (base64-encoded).
    Show,
    /// Rotate the keypair — generates a new key, old data unrecoverable.
    Rotate,
}

#[derive(Debug, Deserialize)]
struct DeviceStartResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: i64,
    interval: i64,
}

#[derive(Debug, Deserialize)]
struct DevicePollResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    #[serde(rename = "token_type")]
    _token_type: Option<String>,
    expires_in: Option<u64>,
    error: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct OrgSummary {
    id: String,
    name: String,
    slug: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct ProjectSummary {
    id: String,
    name: String,
    slug: String,
    org_id: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct UserProfile {
    id: String,
    display_name: Option<String>,
}

#[derive(Debug, Clone)]
struct SelectedContext {
    org_id: Option<String>,
    org_name: Option<String>,
    project_id: Option<String>,
    project_name: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    let python = cli.python.clone();
    let project_root = cli.project_root.clone();
    let endpoints = PlatformEndpoints::from_env();
    let paths = PrismPaths::discover()?;

    match cli.command.unwrap_or(Commands::Setup) {
        Commands::Setup => {
            let mut state = paths.load_cli_state()?;
            state.preferred_python = Some(python.display().to_string());
            if state.credentials.is_none() {
                let credentials = run_device_login(&endpoints).await?;
                let profile = fetch_current_user(&endpoints, &credentials.access_token)
                    .await
                    .ok();
                let selected = select_project(
                    &endpoints,
                    &credentials.access_token,
                    profile
                        .as_ref()
                        .and_then(|user| user.display_name.as_deref()),
                )
                .await?;
                state.credentials = Some(StoredCredentials {
                    access_token: credentials.access_token,
                    refresh_token: credentials.refresh_token,
                    platform_url: credentials.platform_url,
                    user_id: profile.as_ref().map(|p| p.id.clone()),
                    display_name: profile.and_then(|p| p.display_name),
                    org_id: selected.org_id,
                    org_name: selected.org_name,
                    project_id: selected.project_id,
                    project_name: selected.project_name,
                    expires_at: credentials.expires_at,
                });
                paths.save_cli_state(&state)?;
            } else if let Some(creds) = state.credentials.as_mut() {
                if creds.user_id.is_none() || creds.display_name.is_none() {
                    if let Ok(profile) = fetch_current_user(&endpoints, &creds.access_token).await {
                        creds.user_id = Some(profile.id);
                        creds.display_name = profile.display_name;
                    }
                }
                let env_project_id = env_project_override();
                if creds.project_id.is_none()
                    || env_project_id
                        .as_ref()
                        .is_some_and(|project_id| Some(project_id) != creds.project_id.as_ref())
                {
                    let selected = select_project(
                        &endpoints,
                        &creds.access_token,
                        creds.display_name.as_deref(),
                    )
                    .await?;
                    creds.org_id = selected.org_id;
                    creds.org_name = selected.org_name;
                    creds.project_id = selected.project_id;
                    creds.project_name = selected.project_name;
                }
                paths.save_cli_state(&state)?;
            }
            // Auto-refresh expired token before launching TUI
            if let Some(creds) = state.credentials.as_ref() {
                if let Some(expires_at) = creds.expires_at {
                    if chrono::Utc::now() >= expires_at && !creds.refresh_token.is_empty() {
                        match refresh_access_token(&endpoints, creds).await {
                            Ok(new_creds) => {
                                state.credentials = Some(new_creds);
                                paths.save_cli_state(&state)?;
                                tracing::info!("access token refreshed");
                            }
                            Err(e) => {
                                eprintln!("warning: token refresh failed ({e}), you may need to run `prism login`");
                            }
                        }
                    }
                }
            }
            launch_tui(&paths, &python, &project_root, state.credentials.as_ref())?;
        }
        Commands::Login => {
            let mut state = paths.load_cli_state()?;
            let credentials = run_device_login(&endpoints).await?;
            let profile = fetch_current_user(&endpoints, &credentials.access_token)
                .await
                .ok();
            let selected = select_project(
                &endpoints,
                &credentials.access_token,
                profile
                    .as_ref()
                    .and_then(|user| user.display_name.as_deref()),
            )
            .await?;
            state.preferred_python = Some(python.display().to_string());
            state.credentials = Some(StoredCredentials {
                access_token: credentials.access_token,
                refresh_token: credentials.refresh_token,
                platform_url: credentials.platform_url,
                user_id: profile.as_ref().map(|p| p.id.clone()),
                display_name: profile.and_then(|p| p.display_name),
                org_id: selected.org_id,
                org_name: selected.org_name,
                project_id: selected.project_id,
                project_name: selected.project_name,
                expires_at: credentials.expires_at,
            });
            paths.save_cli_state(&state)?;
            println!("Login complete.");
        }
        Commands::Status => {
            let state = paths.load_cli_state()?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "paths": paths,
                    "platform": endpoints,
                    "credentials_present": state.credentials.is_some(),
                    "user_id": state.credentials.as_ref().and_then(|c| c.user_id.clone()),
                    "display_name": state.credentials.as_ref().and_then(|c| c.display_name.clone()),
                    "project_id": state.credentials.as_ref().and_then(|c| c.project_id.clone()),
                    "project_name": state.credentials.as_ref().and_then(|c| c.project_name.clone()),
                    "org_id": state.credentials.as_ref().and_then(|c| c.org_id.clone()),
                    "org_name": state.credentials.as_ref().and_then(|c| c.org_name.clone()),
                    "preferred_python": state.preferred_python,
                    "backbone": {
                        "python_worker": "app.backend",
                        "node_binary": "prism-node",
                        "tui": "compiled ts/ink binary",
                        "workflow_runtime": "rust",
                    }
                }))?
            );
        }
        Commands::Workflow { command } => {
            handle_workflow_command(command, &project_root).await?;
        }
        Commands::Backend {
            project_root,
            python,
        } => {
            let mut config = PythonWorkerConfig::backend(project_root);
            let state = paths.load_cli_state()?;
            config.python_bin = python;
            config
                .env
                .insert("PYTHONUNBUFFERED".to_string(), "1".to_string());
            apply_cli_state_env(&mut config.env, state.credentials.as_ref());
            let mut child = config.stdio_command().spawn()?;
            let status = child.wait().await?;
            std::process::exit(status.code().unwrap_or(1));
        }
        Commands::Node { command } => match command {
            NodeCommands::Up {
                name,
                visibility,
                price,
                data_paths,
                model_paths,
                no_compute,
                no_storage,
                ssh_host,
                ssh_port,
                ssh_user,
                background,
                serve,
            } => {
                // --background: re-exec self as a detached process
                if background {
                    let exe = std::env::current_exe()
                        .context("failed to determine current executable")?;
                    let log_path = paths.state_dir.join("node.log");
                    std::fs::create_dir_all(&paths.state_dir)?;
                    let log_file =
                        std::fs::File::create(&log_path).context("failed to create log file")?;

                    let mut cmd = std::process::Command::new(exe);
                    cmd.arg("node").arg("up");
                    if let Some(ref n) = name {
                        cmd.args(["--name", n]);
                    }
                    cmd.args(["--visibility", &visibility]);
                    if let Some(p) = price {
                        cmd.args(["--price", &p.to_string()]);
                    }
                    if !data_paths.is_empty() {
                        cmd.args(["--data-paths", &data_paths.join(",")]);
                    }
                    if !model_paths.is_empty() {
                        cmd.args(["--model-paths", &model_paths.join(",")]);
                    }
                    if no_compute {
                        cmd.arg("--no-compute");
                    }
                    if no_storage {
                        cmd.arg("--no-storage");
                    }
                    if let Some(ref host) = ssh_host {
                        cmd.args(["--ssh-host", host]);
                        cmd.args(["--ssh-port", &ssh_port.to_string()]);
                        if let Some(ref user) = ssh_user {
                            cmd.args(["--ssh-user", user]);
                        }
                    }
                    if let Some(ref m) = serve {
                        cmd.args(["--serve", m]);
                    }

                    cmd.stdout(log_file.try_clone()?)
                        .stderr(log_file)
                        .stdin(std::process::Stdio::null());

                    let child = cmd.spawn().context("failed to start background daemon")?;
                    println!("Node daemon started in background (PID {}).", child.id());
                    println!("Log: {}", log_path.display());
                    return Ok(());
                }

                // Inject extra scan paths into env
                if !data_paths.is_empty() {
                    let existing = std::env::var("PRISM_DATA_PATHS").unwrap_or_default();
                    let combined = if existing.is_empty() {
                        data_paths.join(",")
                    } else {
                        format!("{},{}", existing, data_paths.join(","))
                    };
                    std::env::set_var("PRISM_DATA_PATHS", combined);
                }
                if !model_paths.is_empty() {
                    let existing = std::env::var("PRISM_MODEL_PATHS").unwrap_or_default();
                    let combined = if existing.is_empty() {
                        model_paths.join(",")
                    } else {
                        format!("{},{}", existing, model_paths.join(","))
                    };
                    std::env::set_var("PRISM_MODEL_PATHS", combined);
                }

                // --serve: check Ollama has the model
                if let Some(ref model) = serve {
                    println!("Checking Ollama for model '{model}'...");
                    match check_ollama_model(model).await {
                        Ok(true) => println!("Model '{model}' available."),
                        Ok(false) => {
                            println!("Model '{model}' not found, pulling...");
                            let status = tokio::process::Command::new("ollama")
                                .args(["pull", model])
                                .status()
                                .await
                                .context("failed to run ollama pull")?;
                            if !status.success() {
                                bail!("ollama pull {model} failed");
                            }
                        }
                        Err(e) => {
                            bail!("Ollama not reachable: {e}. Is Ollama running?");
                        }
                    }
                    // Set env so the probe detects the served model
                    std::env::set_var("PRISM_NODE_SERVE_MODEL", model);
                }

                let options = prism_node::daemon::DaemonOptions {
                    name: name.unwrap_or_else(|| {
                        sysinfo::System::host_name().unwrap_or_else(|| "prism-node".to_string())
                    }),
                    visibility,
                    price_per_hour_usd: price,
                    no_compute,
                    no_storage,
                    ssh: ssh_host.map(|host| prism_node::daemon::SshCapability {
                        host,
                        port: ssh_port,
                        user: ssh_user.or_else(default_ssh_user),
                    }),
                };

                prism_node::daemon::run_daemon(&endpoints, &paths, options).await?;
            }
            NodeCommands::Down => {
                prism_node::daemon::stop_daemon(&paths)?;
            }
            NodeCommands::Status => {
                let caps = prism_node::detect::probe_local_capabilities_async().await;
                print_node_status(&caps, &endpoints);
            }
            NodeCommands::Probe => {
                let caps = prism_node::detect::probe_local_capabilities_async().await;
                println!("{}", serde_json::to_string_pretty(&caps)?);
            }
            NodeCommands::Key { command } => match command {
                KeyCommands::Show => {
                    let (_secret, public) =
                        prism_node::crypto::load_or_generate_key(&paths.state_dir)?;
                    println!("{}", prism_node::crypto::encode_public_key(&public));
                }
                KeyCommands::Rotate => {
                    let public = prism_node::crypto::rotate_key(&paths.state_dir)?;
                    println!("Key rotated.");
                    println!(
                        "New public key: {}",
                        prism_node::crypto::encode_public_key(&public)
                    );
                }
            },
        },
        Commands::External(args) => {
            if try_run_workflow_alias(&project_root, &args).await? {
                return Ok(());
            }
            proxy_python_cli(&python, &project_root, &args).await?;
        }
    }

    Ok(())
}

async fn handle_workflow_command(command: WorkflowCommands, project_root: &Path) -> Result<()> {
    let specs = discover_workflows(Some(project_root))?;
    match command {
        WorkflowCommands::List => {
            if specs.is_empty() {
                println!("No workflows found.");
                return Ok(());
            }
            for spec in specs.values() {
                println!("{}\t{}\t{}", spec.name, spec.command_name, spec.description);
            }
        }
        WorkflowCommands::Show { name } => {
            let spec = find_workflow(&specs, &name)
                .ok_or_else(|| anyhow!("Workflow not found: {name}"))?;
            render_workflow_spec(spec);
        }
        WorkflowCommands::Run {
            name,
            pairs,
            execute,
        } => {
            let spec = find_workflow(&specs, &name)
                .ok_or_else(|| anyhow!("Workflow not found: {name}"))?;
            let values = parse_set_pairs(&pairs)?;
            let result = execute_workflow(spec, &values, execute).await?;
            render_workflow_result(spec, &result);
        }
    }
    Ok(())
}

async fn try_run_workflow_alias(project_root: &Path, args: &[String]) -> Result<bool> {
    if args.is_empty() {
        return Ok(false);
    }
    let specs = discover_workflows(Some(project_root))?;
    let request = parse_workflow_command_args(args)?;
    let Some(spec) = find_workflow(&specs, &request.name) else {
        return Ok(false);
    };
    let result = execute_workflow(spec, &request.values, request.execute).await?;
    render_workflow_result(spec, &result);
    Ok(true)
}

fn parse_set_pairs(pairs: &[String]) -> Result<BTreeMap<String, String>> {
    let mut values = BTreeMap::new();
    for pair in pairs {
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| anyhow!("invalid --set value: {pair}. Expected key=value."))?;
        values.insert(key.to_string(), value.to_string());
    }
    Ok(values)
}

fn render_workflow_spec(spec: &WorkflowSpec) {
    println!("{}\t{}", spec.name, spec.command_name);
    println!("{}", spec.description);
    println!("source: {}", spec.source_path);
    for argument in &spec.arguments {
        let required = if argument.required {
            "required"
        } else {
            "optional"
        };
        println!(
            "--{}\t{}\t{}\t{}",
            argument.name, argument.r#type, required, argument.help
        );
    }
}

fn render_workflow_result(spec: &WorkflowSpec, result: &WorkflowRunResult) {
    println!("{}\t{}", spec.command_name, result.mode);
    println!("{}", spec.description);
    for step in &result.steps {
        println!(
            "{}\t{}\t{}\t{}",
            step.id, step.action, step.status, step.summary
        );
    }
}

async fn proxy_python_cli(python: &Path, project_root: &Path, args: &[String]) -> Result<()> {
    let mut cmd = tokio::process::Command::new(python);
    cmd.arg("-m")
        .arg("app.cli.main")
        .args(args)
        .current_dir(project_root)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .env("PRISM_DISABLE_RUST_BOOTSTRAP", "1");
    let status = cmd.spawn()?.wait().await?;
    std::process::exit(status.code().unwrap_or(1));
}

async fn run_device_login(endpoints: &PlatformEndpoints) -> Result<StoredCredentials> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    let response = client
        .post(format!("{}/auth/device/start", endpoints.api_base))
        .json(&serde_json::json!({}))
        .send()
        .await
        .context("failed to start device flow")?
        .error_for_status()
        .context("device flow start returned error status")?;

    let start: DeviceStartResponse = response.json().await?;

    println!();
    println!("PRISM setup needs MARC27 platform login.");
    println!("Open: {}", start.verification_uri);
    println!("Code: {}", start.user_code);
    println!();
    if let Err(err) = open_browser(&start.verification_uri) {
        eprintln!("warning: failed to open browser automatically: {err}");
    }
    println!("Approve the device in your browser, then return here.");

    let poll_url = format!("{}/auth/device/poll", endpoints.api_base);
    let deadline = std::time::Instant::now() + Duration::from_secs(start.expires_in as u64);
    let mut interval = Duration::from_secs(start.interval.max(1) as u64);

    while std::time::Instant::now() < deadline {
        tokio::time::sleep(interval).await;

        let poll = client
            .post(&poll_url)
            .json(&serde_json::json!({ "device_code": start.device_code }))
            .send()
            .await
            .context("failed to poll device flow")?;

        let status = poll.status();
        let payload: DevicePollResponse = poll.json().await?;

        if payload.error.is_none()
            && payload.access_token.is_some()
            && payload.refresh_token.is_some()
        {
            let expires_at = payload.expires_in.and_then(|secs| {
                chrono::Utc::now().checked_add_signed(chrono::Duration::seconds(secs as i64))
            });
            return Ok(StoredCredentials {
                access_token: payload.access_token.unwrap_or_default(),
                refresh_token: payload.refresh_token.unwrap_or_default(),
                platform_url: endpoints.api_base.trim_end_matches("/api/v1").to_string(),
                user_id: None,
                display_name: None,
                org_id: None,
                org_name: None,
                project_id: None,
                project_name: None,
                expires_at,
            });
        }

        match payload.error.as_deref() {
            Some("authorization_pending") => continue,
            Some("slow_down") => {
                interval += Duration::from_secs(5);
                continue;
            }
            Some("access_denied") => bail!("device login denied by user"),
            Some("expired_token") => bail!("device login expired before approval"),
            Some(other) => bail!("device login failed: {other} (http {status})"),
            None => bail!("device login returned unexpected payload"),
        }
    }

    bail!("device login timed out")
}

async fn select_project(
    endpoints: &PlatformEndpoints,
    access_token: &str,
    display_name: Option<&str>,
) -> Result<SelectedContext> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    if let Some(project_id) = env_project_override() {
        match fetch_project_by_id(&client, endpoints, access_token, &project_id).await {
            Ok(project) => {
                let org_name =
                    fetch_org_name_for_project(&client, endpoints, access_token, &project)
                        .await
                        .ok()
                        .flatten();
                println!(
                    "Using project from MARC27_PROJECT_ID: {} ({})",
                    project.name, project.id
                );
                return Ok(SelectedContext {
                    org_id: Some(project.org_id.clone()),
                    org_name,
                    project_id: Some(project.id),
                    project_name: Some(project.name),
                });
            }
            Err(err) => {
                eprintln!(
                    "warning: MARC27_PROJECT_ID={} could not be resolved: {err}",
                    project_id
                );
            }
        }
    }

    let orgs: Vec<OrgSummary> = client
        .get(format!("{}/orgs", endpoints.api_base))
        .bearer_auth(access_token)
        .send()
        .await
        .context("failed to list orgs")?
        .error_for_status()
        .context("listing orgs returned error status")?
        .json()
        .await
        .context("failed to parse org list")?;

    if orgs.is_empty() {
        println!("No organizations available for this account yet.");
        return Ok(SelectedContext {
            org_id: None,
            org_name: None,
            project_id: None,
            project_name: None,
        });
    }

    let selected_org = prompt_select("Select organization", &orgs, |org| {
        format!("{} ({})", org.name, org.slug)
    })?;

    let projects: Vec<ProjectSummary> = client
        .get(format!("{}/projects", endpoints.api_base))
        .query(&[("org_id", &selected_org.id)])
        .bearer_auth(access_token)
        .send()
        .await
        .context("failed to list projects")?
        .error_for_status()
        .context("listing projects returned error status")?
        .json()
        .await
        .context("failed to parse project list")?;

    if projects.is_empty() {
        println!("No projects found in organization {}.", selected_org.name);
        let created = create_default_project(
            &client,
            endpoints,
            access_token,
            &selected_org,
            display_name,
        )
        .await
        .with_context(|| {
            format!(
                "failed to auto-create a PRISM project in organization {}",
                selected_org.name
            )
        })?;
        println!("Created PRISM project: {} ({})", created.name, created.slug);
        return Ok(SelectedContext {
            org_id: Some(selected_org.id.clone()),
            org_name: Some(selected_org.name.clone()),
            project_id: Some(created.id),
            project_name: Some(created.name),
        });
    }

    let selected_project = prompt_select("Select project", &projects, |project| {
        format!("{} ({})", project.name, project.slug)
    })?;

    Ok(SelectedContext {
        org_id: Some(selected_org.id.clone()),
        org_name: Some(selected_org.name.clone()),
        project_id: Some(selected_project.id.clone()),
        project_name: Some(selected_project.name.clone()),
    })
}

fn env_project_override() -> Option<String> {
    std::env::var("MARC27_PROJECT_ID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

async fn fetch_project_by_id(
    client: &reqwest::Client,
    endpoints: &PlatformEndpoints,
    access_token: &str,
    project_id: &str,
) -> Result<ProjectSummary> {
    client
        .get(format!("{}/projects/{project_id}", endpoints.api_base))
        .bearer_auth(access_token)
        .send()
        .await
        .context("failed to fetch project by id")?
        .error_for_status()
        .context("project lookup returned error status")?
        .json()
        .await
        .context("failed to parse project payload")
}

async fn fetch_org_name_for_project(
    client: &reqwest::Client,
    endpoints: &PlatformEndpoints,
    access_token: &str,
    project: &ProjectSummary,
) -> Result<Option<String>> {
    let orgs: Vec<OrgSummary> = client
        .get(format!("{}/orgs", endpoints.api_base))
        .bearer_auth(access_token)
        .send()
        .await
        .context("failed to list orgs")?
        .error_for_status()
        .context("listing orgs returned error status")?
        .json()
        .await
        .context("failed to parse org list")?;
    Ok(orgs
        .into_iter()
        .find(|org| org.id == project.org_id)
        .map(|org| org.name))
}

async fn create_default_project(
    client: &reqwest::Client,
    endpoints: &PlatformEndpoints,
    access_token: &str,
    org: &OrgSummary,
    display_name: Option<&str>,
) -> Result<ProjectSummary> {
    let name = default_project_name(display_name);
    let slug = default_project_slug();
    let response = client
        .post(format!("{}/projects", endpoints.api_base))
        .bearer_auth(access_token)
        .json(&serde_json::json!({
            "name": name,
            "slug": slug,
            "org_id": org.id,
        }))
        .send()
        .await
        .context("failed to create default project")?;

    if response.status() == reqwest::StatusCode::FORBIDDEN {
        bail!(
            "project creation forbidden for organization {}. Set MARC27_PROJECT_ID to an existing project you can access, or create a project from the platform dashboard first",
            org.name
        );
    }

    response
        .error_for_status()
        .context("project creation returned error status")?
        .json()
        .await
        .context("failed to parse created project payload")
}

fn default_project_name(display_name: Option<&str>) -> String {
    match display_name
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(name) => format!("{name} PRISM Workspace"),
        None => "PRISM Workspace".to_string(),
    }
}

fn default_project_slug() -> String {
    let timestamp = chrono::Utc::now().format("%Y%m%d%H%M%S");
    format!("prism-{timestamp}")
}

fn default_ssh_user() -> Option<String> {
    std::env::var("USER")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn prompt_select<'a, T, F>(label: &'a str, items: &'a [T], formatter: F) -> Result<&'a T>
where
    F: Fn(&T) -> String,
{
    println!();
    println!("{label}:");
    for (idx, item) in items.iter().enumerate() {
        println!("  {}. {}", idx + 1, formatter(item));
    }
    print!("Enter choice [1-{}]: ", items.len());
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let trimmed = input.trim();
    let index = if trimmed.is_empty() {
        0
    } else {
        trimmed
            .parse::<usize>()
            .map_err(|_| anyhow!("invalid selection"))?
            .saturating_sub(1)
    };
    items
        .get(index)
        .ok_or_else(|| anyhow!("selection out of range"))
}

async fn refresh_access_token(
    endpoints: &PlatformEndpoints,
    creds: &StoredCredentials,
) -> Result<StoredCredentials> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    #[derive(Deserialize)]
    struct RefreshResponse {
        access_token: String,
        refresh_token: Option<String>,
        expires_in: Option<u64>,
    }

    let resp = client
        .post(format!("{}/auth/refresh", endpoints.api_base))
        .json(&serde_json::json!({ "refresh_token": creds.refresh_token }))
        .send()
        .await
        .context("failed to refresh token")?
        .error_for_status()
        .context("token refresh returned error")?;

    let refreshed: RefreshResponse = resp.json().await?;

    let mut new_creds = creds.clone();
    new_creds.access_token = refreshed.access_token;
    if let Some(rt) = refreshed.refresh_token {
        new_creds.refresh_token = rt;
    }
    new_creds.expires_at = refreshed.expires_in.and_then(|secs| {
        chrono::Utc::now().checked_add_signed(chrono::Duration::seconds(secs as i64))
    });

    Ok(new_creds)
}

async fn fetch_current_user(
    endpoints: &PlatformEndpoints,
    access_token: &str,
) -> Result<UserProfile> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    client
        .get(format!("{}/users/me", endpoints.api_base))
        .bearer_auth(access_token)
        .send()
        .await
        .context("failed to fetch current user")?
        .error_for_status()
        .context("current user request returned error status")?
        .json()
        .await
        .context("failed to parse current user payload")
}

fn launch_tui(
    paths: &PrismPaths,
    python: &Path,
    project_root: &Path,
    credentials: Option<&StoredCredentials>,
) -> Result<()> {
    let backend_bin = std::env::current_exe().context("failed to determine current executable")?;
    let tui_binary = discover_tui_binary(paths).ok_or_else(|| {
        anyhow!(
            "no compiled TS TUI binary found. Install or bundle prism-tui before using native shell"
        )
    })?;

    let mut cmd = std::process::Command::new(tui_binary);
    cmd.arg("--python")
        .arg(python)
        .arg("--backend-bin")
        .arg(backend_bin)
        .current_dir(project_root)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    apply_process_env(&mut cmd, credentials);

    let status = cmd.status().context("failed to launch TS TUI")?;
    std::process::exit(status.code().unwrap_or(1));
}

fn discover_tui_binary(paths: &PrismPaths) -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok();
    let dist_dir = cwd.as_ref().map(|dir| dir.join("frontend").join("dist"));

    if let Some(dist_dir) = dist_dir {
        let mut candidates = vec![
            dist_dir.join(platform_tui_name()),
            dist_dir.join("prism-tui"),
        ];

        if let Ok(entries) = std::fs::read_dir(&dist_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                if name.starts_with("prism-tui") {
                    candidates.push(path);
                }
            }
        }

        candidates.push(paths.data_dir.join("bin").join(platform_tui_name()));
        candidates.push(paths.data_dir.join("bin").join("prism-tui"));

        for candidate in candidates {
            if !candidate.as_os_str().is_empty() && candidate.exists() {
                return Some(candidate);
            }
        }
    } else {
        for candidate in [
            paths.data_dir.join("bin").join(platform_tui_name()),
            paths.data_dir.join("bin").join("prism-tui"),
        ] {
            if !candidate.as_os_str().is_empty() && candidate.exists() {
                return Some(candidate);
            }
        }
    }

    None
}

fn apply_cli_state_env(
    env_map: &mut std::collections::BTreeMap<String, String>,
    credentials: Option<&StoredCredentials>,
) {
    if let Some(creds) = credentials {
        env_map.insert("MARC27_TOKEN".to_string(), creds.access_token.clone());
        env_map.insert(
            "MARC27_PLATFORM_URL".to_string(),
            creds.platform_url.clone(),
        );
        if let Some(project_id) = &creds.project_id {
            env_map.insert("MARC27_PROJECT_ID".to_string(), project_id.clone());
        }
        if let Some(user_id) = &creds.user_id {
            env_map.insert("PRISM_ACCOUNT_USER_ID".to_string(), user_id.clone());
        }
        if let Some(display_name) = &creds.display_name {
            env_map.insert(
                "PRISM_ACCOUNT_DISPLAY_NAME".to_string(),
                display_name.clone(),
            );
        }
        if let Some(org_id) = &creds.org_id {
            env_map.insert("PRISM_ACCOUNT_ORG_ID".to_string(), org_id.clone());
        }
        if let Some(org_name) = &creds.org_name {
            env_map.insert("PRISM_ACCOUNT_ORG_NAME".to_string(), org_name.clone());
        }
        if let Some(project_name) = &creds.project_name {
            env_map.insert(
                "PRISM_ACCOUNT_PROJECT_NAME".to_string(),
                project_name.clone(),
            );
        }
    }
}

fn apply_process_env(cmd: &mut std::process::Command, credentials: Option<&StoredCredentials>) {
    if let Some(creds) = credentials {
        cmd.env("MARC27_TOKEN", &creds.access_token)
            .env("MARC27_PLATFORM_URL", &creds.platform_url);
        if let Some(project_id) = &creds.project_id {
            cmd.env("MARC27_PROJECT_ID", project_id);
        }
        if let Some(user_id) = &creds.user_id {
            cmd.env("PRISM_ACCOUNT_USER_ID", user_id);
        }
        if let Some(display_name) = &creds.display_name {
            cmd.env("PRISM_ACCOUNT_DISPLAY_NAME", display_name);
        }
        if let Some(org_id) = &creds.org_id {
            cmd.env("PRISM_ACCOUNT_ORG_ID", org_id);
        }
        if let Some(org_name) = &creds.org_name {
            cmd.env("PRISM_ACCOUNT_ORG_NAME", org_name);
        }
        if let Some(project_name) = &creds.project_name {
            cmd.env("PRISM_ACCOUNT_PROJECT_NAME", project_name);
        }
    }
}

fn platform_tui_name() -> &'static str {
    if cfg!(windows) {
        "prism-tui.exe"
    } else if cfg!(target_os = "macos") && cfg!(target_arch = "aarch64") {
        "prism-tui-darwin-arm64"
    } else if cfg!(target_os = "macos") {
        "prism-tui-darwin-x64"
    } else if cfg!(target_arch = "aarch64") {
        "prism-tui-linux-arm64"
    } else {
        "prism-tui-linux-x64"
    }
}

fn open_browser(url: &str) -> Result<()> {
    let status = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).status()
    } else if cfg!(target_os = "windows") {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .status()
    } else {
        std::process::Command::new("xdg-open").arg(url).status()
    }
    .context("failed to spawn browser opener")?;

    if status.success() {
        Ok(())
    } else {
        bail!("browser opener exited with status {status}")
    }
}

/// Check if Ollama has a specific model available.
async fn check_ollama_model(model: &str) -> Result<bool> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;
    let resp = client
        .get("http://localhost:11434/api/tags")
        .send()
        .await
        .context("failed to connect to Ollama")?;
    let data: serde_json::Value = resp.json().await?;
    let has_model = data
        .get("models")
        .and_then(|m| m.as_array())
        .map(|models| {
            models.iter().any(|m| {
                m.get("name")
                    .and_then(|n| n.as_str())
                    .is_some_and(|n| n == model || n.starts_with(&format!("{model}:")))
            })
        })
        .unwrap_or(false);
    Ok(has_model)
}

fn print_node_status(caps: &NodeCapabilities, endpoints: &PlatformEndpoints) {
    let hostname = sysinfo::System::host_name().unwrap_or_else(|| "unknown".to_string());
    println!("Node: {hostname}");
    println!("Visibility: {}", caps.visibility);
    println!("Platform: {}", endpoints.node_ws);
    println!();

    println!("Compute:");
    println!("  CPU: {} cores, {} MB RAM", caps.cpu_cores, caps.ram_gb);
    if caps.gpus.is_empty() {
        println!("  GPUs: none");
    } else {
        for gpu in &caps.gpus {
            println!(
                "  GPU: {} x{} ({} GB VRAM)",
                gpu.gpu_type, gpu.count, gpu.vram_gb
            );
        }
    }
    if let Some(rt) = &caps.container_runtime {
        println!("  Container runtime: {rt}");
    }
    if let Some(sched) = &caps.scheduler {
        println!("  Scheduler: {sched}");
    }
    println!();

    println!("Storage:");
    println!(
        "  Total: {} GB, Available: {} GB",
        caps.disk_gb, caps.storage_available_gb
    );
    if caps.datasets.is_empty() {
        println!("  Datasets: none detected");
    } else {
        for ds in &caps.datasets {
            let entries = ds
                .entries
                .map(|n| format!(", {n} entries"))
                .unwrap_or_default();
            let fmt = ds.format.as_deref().unwrap_or("unknown");
            println!(
                "  Dataset: {} ({:.2} GB, {fmt}{entries})",
                ds.name, ds.size_gb
            );
        }
    }
    if caps.models.is_empty() {
        println!("  Models: none detected");
    } else {
        for m in &caps.models {
            let fmt = m.format.as_deref().unwrap_or("unknown");
            let size = m
                .size_gb
                .map(|s| format!(", {s:.2} GB"))
                .unwrap_or_default();
            println!("  Model: {} ({fmt}{size})", m.name);
        }
    }
    println!();

    println!("Services:");
    for svc in &caps.services {
        let icon = if svc.status == "ready" { "●" } else { "○" };
        let model_info = svc
            .model
            .as_ref()
            .map(|m| format!(" ({m})"))
            .unwrap_or_default();
        let endpoint_info = svc
            .endpoint
            .as_ref()
            .map(|ep| format!(" <{ep}>"))
            .unwrap_or_default();
        println!(
            "  {icon} {} [{}]{model_info}{endpoint_info}",
            svc.kind, svc.status
        );
    }
    println!();

    println!("Software: {}", caps.software.join(", "));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_project_name_uses_display_name_when_present() {
        assert_eq!(
            default_project_name(Some("Siddhartha")),
            "Siddhartha PRISM Workspace"
        );
        assert_eq!(default_project_name(Some("   ")), "PRISM Workspace");
        assert_eq!(default_project_name(None), "PRISM Workspace");
    }

    #[test]
    fn env_project_override_ignores_empty_values() {
        std::env::remove_var("MARC27_PROJECT_ID");
        assert_eq!(env_project_override(), None);
        std::env::set_var("MARC27_PROJECT_ID", "   ");
        assert_eq!(env_project_override(), None);
        std::env::set_var("MARC27_PROJECT_ID", "project-123");
        assert_eq!(env_project_override(), Some("project-123".to_string()));
        std::env::remove_var("MARC27_PROJECT_ID");
    }

    #[test]
    fn default_project_slug_has_prism_prefix() {
        let slug = default_project_slug();
        assert!(slug.starts_with("prism-"));
        assert!(slug.len() > "prism-".len());
    }

    #[test]
    fn default_ssh_user_ignores_empty_values() {
        std::env::remove_var("USER");
        assert_eq!(default_ssh_user(), None);
        std::env::set_var("USER", "   ");
        assert_eq!(default_ssh_user(), None);
        std::env::set_var("USER", "sid");
        assert_eq!(default_ssh_user(), Some("sid".to_string()));
        std::env::remove_var("USER");
    }
}
