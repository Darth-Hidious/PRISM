//! PRISM CLI — the main entry point for the `prism` binary.
//!
//! Handles command routing (setup, login, node, workflow, etc.), auth bootstrap
//! via device-flow OAuth, Python worker supervision, and dynamic workflow
//! discovery from `~/.prism/workflows/`.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use prism_client::api::PlatformClient;
use prism_client::auth::{DeviceCodeResponse, TokenResponse};
use prism_client::DeviceFlowAuth;
use prism_proto::NodeCapabilities;
use prism_python_bridge::PythonWorkerConfig;
use prism_runtime::{PlatformEndpoints, PrismPaths, StoredCredentials};
use prism_workflows::{
    discover_workflows, execute_workflow, find_workflow, parse_workflow_command_args,
    WorkflowRunResult, WorkflowSpec,
};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "prism")]
#[command(about = "PRISM — AI-native materials discovery platform")]
#[command(version = env!("CARGO_PKG_VERSION"))]
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
    /// Ingest a data file into the knowledge graph.
    Ingest {
        /// Path to a CSV/Parquet file or directory to watch.
        path: PathBuf,
        /// LLM provider: ollama (default), openai (OpenAI-compatible — works with MARC27, vLLM, etc.)
        #[arg(long, default_value = "ollama")]
        llm_provider: String,
        /// LLM model name (e.g. "qwen2.5:7b", "gpt-4o", "claude-sonnet-4-6").
        #[arg(long, default_value = "qwen2.5:7b")]
        model: String,
        /// LLM base URL (e.g. "http://localhost:11434", "https://api.openai.com", "https://platform.marc27.com/api/v1/llm").
        #[arg(long, default_value = "http://localhost:11434")]
        llm_url: String,
        /// API key for authenticated LLM providers. Also reads from LLM_API_KEY env var.
        #[arg(long, env = "LLM_API_KEY")]
        api_key: Option<String>,
        /// Neo4j HTTP endpoint.
        #[arg(long, default_value = "http://localhost:7474")]
        neo4j_url: String,
        /// Neo4j username.
        #[arg(long, default_value = "neo4j")]
        neo4j_user: String,
        /// Neo4j password.
        #[arg(long, default_value = "prism-local")]
        neo4j_pass: String,
        /// Qdrant HTTP endpoint.
        #[arg(long, default_value = "http://localhost:6333")]
        qdrant_url: String,
        /// Skip LLM extraction (schema detection only).
        #[arg(long)]
        schema_only: bool,
        /// Watch a directory for new/modified files and ingest continuously.
        #[arg(long)]
        watch: bool,
        /// Path to a YAML ontology mapping file (custom entity/relationship rules).
        #[arg(long)]
        mapping: Option<PathBuf>,
    },
    /// Query the knowledge graph.
    Query {
        /// Natural language or Cypher query.
        text: String,
        /// Direct Cypher query (skip LLM translation).
        #[arg(long)]
        cypher: bool,
        /// Semantic vector search.
        #[arg(long)]
        semantic: bool,
        /// Use the MARC27 platform API instead of local graph.
        #[arg(long)]
        platform: bool,
        /// Output as JSON (for piping to other tools / agents).
        #[arg(long)]
        json: bool,
        /// Query all known mesh peers and merge results.
        #[arg(long)]
        federated: bool,
        /// Neo4j HTTP endpoint.
        #[arg(long, default_value = "http://localhost:7474")]
        neo4j_url: String,
        /// Neo4j username.
        #[arg(long, default_value = "neo4j")]
        neo4j_user: String,
        /// Neo4j password.
        #[arg(long, default_value = "prism-local")]
        neo4j_pass: String,
        /// Qdrant HTTP endpoint.
        #[arg(long, default_value = "http://localhost:6333")]
        qdrant_url: String,
        /// LLM provider: ollama (default), openai (OpenAI-compatible — works with MARC27, vLLM, etc.)
        #[arg(long, default_value = "ollama")]
        llm_provider: String,
        /// LLM base URL.
        #[arg(long, default_value = "http://localhost:11434")]
        llm_url: String,
        /// LLM model name.
        #[arg(long, default_value = "qwen2.5:7b")]
        model: String,
        /// API key for authenticated LLM providers. Also reads from LLM_API_KEY env var.
        #[arg(long, env = "LLM_API_KEY")]
        api_key: Option<String>,
        /// Max results to return.
        #[arg(long, default_value = "10")]
        limit: usize,
        /// Dashboard URL for federated query peer discovery.
        #[arg(long, default_value = "http://127.0.0.1:7327")]
        dashboard_url: String,
    },
    /// Print available commands for AI agents. Pipe-friendly, grep-friendly.
    Agent,
    /// Submit a compute job (run a container on local Docker, MARC27 cloud, or BYOC).
    Run {
        /// Container image to run.
        image: String,
        /// Job name.
        #[arg(long, default_value = "experiment")]
        name: String,
        /// JSON inputs (key=value pairs merged into inputs object).
        #[arg(long, value_delimiter = ',')]
        input: Vec<String>,
        /// Backend: local, marc27, or byoc.
        #[arg(long, default_value = "local")]
        backend: String,
        /// MARC27 platform API URL (for marc27 backend).
        #[arg(long, default_value = "https://platform.marc27.com/api/v1")]
        platform_url: String,
    },
    /// Check status of a compute job.
    JobStatus {
        /// Job UUID.
        job_id: String,
    },
    /// Mesh networking — discover peers, publish datasets, manage subscriptions.
    Mesh {
        #[command(subcommand)]
        command: MeshCommands,
    },
    /// Report a bug or issue — captures system context and files it automatically.
    Report {
        /// Description of what went wrong.
        description: String,
        /// Attach a log file or error output.
        #[arg(long)]
        log_file: Option<PathBuf>,
        /// Don't open a GitHub issue (only send to MARC27 platform).
        #[arg(long)]
        no_github: bool,
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
#[allow(clippy::large_enum_variant)]
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
        /// Run in offline mode (no platform registration, local Kafka for mesh).
        #[arg(long)]
        offline: bool,
        /// Dashboard HTTP port (default: 7327).
        #[arg(long, default_value_t = 7327)]
        dashboard_port: u16,
        /// Skip starting managed services (Neo4j, Qdrant, Kafka).
        #[arg(long)]
        no_services: bool,
        /// Connect to existing Neo4j instead of starting a container.
        #[arg(long)]
        external_neo4j: Option<String>,
        /// Connect to existing Qdrant instead of starting a container.
        #[arg(long)]
        external_qdrant: Option<String>,
        /// Also start Kafka (for mesh/pub-sub, off by default in dev).
        #[arg(long)]
        with_kafka: bool,
        /// Broadcast this node on the local network (mDNS) and register for platform discovery.
        /// Without this flag, the node runs privately — it can discover peers but won't be found.
        #[arg(long)]
        broadcast: bool,
    },
    /// Stop a running node daemon.
    Down,
    /// Show current node capabilities and status.
    Status,
    /// Probe local capabilities without connecting.
    Probe,
    /// Stream logs from a managed service (neo4j, qdrant, kafka).
    Logs {
        /// Service name: neo4j, qdrant, or kafka.
        service: String,
        /// Number of tail lines to show (default: 100).
        #[arg(long, default_value_t = 100)]
        tail: usize,
    },
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

#[derive(Debug, Subcommand)]
enum MeshCommands {
    /// Discover peers on the local network via mDNS.
    Discover {
        /// Timeout in seconds for discovery.
        #[arg(long, default_value_t = 5)]
        timeout: u64,
    },
    /// List known mesh peers (from a running node).
    Peers {
        /// Dashboard URL of the running node.
        #[arg(long, default_value = "http://127.0.0.1:7327")]
        dashboard_url: String,
    },
    /// Publish a dataset to the mesh.
    Publish {
        /// Name of the dataset to publish.
        name: String,
        /// Schema version.
        #[arg(long, default_value = "1.0")]
        schema_version: String,
        /// Dashboard URL of the running node.
        #[arg(long, default_value = "http://127.0.0.1:7327")]
        dashboard_url: String,
    },
    /// Subscribe to a dataset on a remote node.
    Subscribe {
        /// Dataset name to subscribe to.
        dataset_name: String,
        /// Publisher node UUID.
        #[arg(long)]
        publisher: String,
        /// Dashboard URL of the running node.
        #[arg(long, default_value = "http://127.0.0.1:7327")]
        dashboard_url: String,
    },
    /// Unsubscribe from a remote dataset.
    Unsubscribe {
        /// Dataset name to unsubscribe from.
        dataset_name: String,
        /// Publisher node UUID.
        #[arg(long)]
        publisher: String,
        /// Dashboard URL of the running node.
        #[arg(long, default_value = "http://127.0.0.1:7327")]
        dashboard_url: String,
    },
    /// Show current subscriptions.
    Subscriptions {
        /// Dashboard URL of the running node.
        #[arg(long, default_value = "http://127.0.0.1:7327")]
        dashboard_url: String,
    },
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
                let platform =
                    PlatformClient::new(&endpoints.api_base).with_token(&credentials.access_token);
                let profile = platform.fetch_current_user().await.ok();
                let selected = select_project(
                    &platform,
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
                let platform =
                    PlatformClient::new(&endpoints.api_base).with_token(&creds.access_token);
                if creds.user_id.is_none() || creds.display_name.is_none() {
                    if let Ok(profile) = platform.fetch_current_user().await {
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
                    let selected = select_project(&platform, creds.display_name.as_deref()).await?;
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
            let platform =
                PlatformClient::new(&endpoints.api_base).with_token(&credentials.access_token);
            let profile = platform.fetch_current_user().await.ok();
            let selected = select_project(
                &platform,
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

            // Sync credentials to ~/.prism/credentials.json for Python SDK
            if let Some(ref creds) = state.credentials {
                let sdk_creds = serde_json::json!({
                    "access_token": creds.access_token,
                    "refresh_token": creds.refresh_token,
                    "platform_url": creds.platform_url,
                    "user_id": creds.user_id,
                    "org_id": creds.org_id,
                    "project_id": creds.project_id,
                });
                if let Some(home) = std::env::var_os("HOME") {
                    let sdk_path = std::path::PathBuf::from(home)
                        .join(".prism")
                        .join("credentials.json");
                    if let Ok(json) = serde_json::to_string_pretty(&sdk_creds) {
                        let _ = std::fs::create_dir_all(sdk_path.parent().unwrap());
                        let _ = std::fs::write(&sdk_path, json);
                    }
                }
            }

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
                offline,
                dashboard_port,
                no_services,
                external_neo4j,
                external_qdrant,
                with_kafka,
                broadcast,
            } => {
                // Load prism.toml config (global + project), CLI flags override
                let node_config = prism_core::config::NodeConfig::load(Some(&project_root));
                tracing::debug!(?node_config, "loaded prism.toml config");

                let node_name = name.unwrap_or_else(|| {
                    if node_config.node.name != "prism-node" {
                        node_config.node.name.clone()
                    } else {
                        sysinfo::System::host_name().unwrap_or_else(|| "prism-node".to_string())
                    }
                });

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
                    cmd.args(["--name", &node_name]);
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
                    if offline {
                        cmd.arg("--offline");
                    }
                    if no_services {
                        cmd.arg("--no-services");
                    }
                    if with_kafka {
                        cmd.arg("--with-kafka");
                    }
                    if broadcast {
                        cmd.arg("--broadcast");
                    }
                    cmd.args(["--dashboard-port", &dashboard_port.to_string()]);
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
                    if let Some(ref uri) = external_neo4j {
                        cmd.args(["--external-neo4j", uri]);
                    }
                    if let Some(ref uri) = external_qdrant {
                        cmd.args(["--external-qdrant", uri]);
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
                    std::env::set_var("PRISM_NODE_SERVE_MODEL", model);
                }

                // ── V2: Start managed services (Docker containers) ──
                let mut service_handles = None;
                if !no_services && external_neo4j.is_none() {
                    println!("\n  PRISM v{}", env!("CARGO_PKG_VERSION"));
                    if offline {
                        println!("  (OFFLINE MODE)");
                    }
                    println!("  Node: {node_name}\n");
                    println!("  Starting services...");

                    match prism_orch::DockerOrchestrator::new() {
                        Ok(orch) => {
                            use prism_orch::ServiceOrchestrator;
                            let mut svc_config = prism_orch::ServiceConfig::default();
                            if with_kafka {
                                svc_config.kafka =
                                    Some(prism_orch::services::KafkaConfig::default());
                            }

                            match orch.start_all(&svc_config).await {
                                Ok(handles) => {
                                    for h in &handles.services {
                                        let mark = if h.healthy { "\u{2713}" } else { "~" };
                                        println!("  {mark} {:<12} localhost:{}", h.name, h.port);
                                    }
                                    service_handles = Some(handles);
                                }
                                Err(e) => {
                                    eprintln!("  Warning: Failed to start managed services: {e}");
                                    eprintln!(
                                        "  (Is Docker running? Continuing without containers.)"
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("  Warning: Docker not available: {e}");
                            eprintln!("  (Continuing without managed services.)");
                        }
                    }
                }

                // ── V2: Start the embedded dashboard server ──
                let mut server_node_state = prism_server::NodeState::new(node_name.clone());

                // Wire core databases (RBAC + audit)
                let state_dir = &paths.state_dir;
                std::fs::create_dir_all(state_dir)?;
                server_node_state.audit_db_path = Some(state_dir.join("audit.db"));
                server_node_state.rbac_db_path = Some(state_dir.join("rbac.db"));
                server_node_state.session_db_path = Some(state_dir.join("sessions.db"));

                // Scan for tools
                let tools_dir = paths.config_dir.join("tools");
                if tools_dir.is_dir() {
                    if let Ok(mut reg) = server_node_state.tool_registry.write() {
                        let _ = reg.scan_directory(&tools_dir);
                    }
                }

                // Wire backend configs — CLI flags > prism.toml > defaults
                if external_neo4j.is_some()
                    || node_config.services.neo4j_uri.is_some()
                    || service_handles.is_some()
                {
                    let neo4j_url = external_neo4j
                        .clone()
                        .or_else(|| node_config.services.neo4j_uri.clone())
                        .unwrap_or_else(|| "http://localhost:7474".into());
                    server_node_state.neo4j = Some(prism_ingest::Neo4jConfig {
                        base_url: neo4j_url,
                        database: "neo4j".into(),
                        username: "neo4j".into(),
                        password: "prism-local".into(),
                    });
                }
                if external_qdrant.is_some()
                    || node_config.services.qdrant_uri.is_some()
                    || service_handles.is_some()
                {
                    let qdrant_url = external_qdrant
                        .clone()
                        .or_else(|| node_config.services.qdrant_uri.clone())
                        .unwrap_or_else(|| "http://localhost:6333".into());
                    server_node_state.qdrant = Some(prism_ingest::QdrantConfig {
                        base_url: qdrant_url,
                        collection: "prism_embeddings".into(),
                        api_key: None,
                    });
                }

                // Wire LLM config from prism.toml [indexer] section or defaults
                {
                    let api_key =
                        prism_core::config::NodeConfig::resolve_api_key(&node_config.indexer);
                    let provider = match node_config.indexer.mode.as_str() {
                        "platform" | "marc27" => prism_ingest::llm::LlmProvider::OpenAi,
                        "external" => prism_ingest::llm::LlmProvider::OpenAi,
                        _ => prism_ingest::llm::LlmProvider::Ollama,
                    };
                    let base_url =
                        node_config
                            .indexer
                            .uri
                            .clone()
                            .unwrap_or_else(|| match provider {
                                prism_ingest::llm::LlmProvider::OpenAi => {
                                    node_config.platform.url.clone() + "/llm"
                                }
                                prism_ingest::llm::LlmProvider::Ollama => {
                                    "http://localhost:11434".into()
                                }
                            });
                    server_node_state.llm = Some(prism_ingest::LlmConfig {
                        provider,
                        base_url,
                        model: node_config
                            .indexer
                            .model
                            .clone()
                            .unwrap_or_else(|| "qwen2.5:7b".into()),
                        api_key,
                        embedding_model: node_config.indexer.embedding_model.clone(),
                        max_sample_rows: 10,
                        timeout_secs: 120,
                    });
                }

                // ── Platform registration (unless --offline) ──
                let mut daemon_platform_client: Option<PlatformClient> = None;
                let mut daemon_platform_node_id: Option<String> = None;
                let mut daemon_org_id: Option<String> = None;

                if !offline {
                    let cli_state = paths.load_cli_state()?;
                    if let Some(ref creds) = cli_state.credentials {
                        daemon_org_id = creds.org_id.clone();
                        let platform = PlatformClient::new(&endpoints.api_base)
                            .with_token(&creds.access_token);
                        let registry =
                            prism_client::node_registry::NodeRegistryClient::new(&platform);
                        let caps = serde_json::json!({
                            "compute": !no_compute,
                            "storage": !no_storage,
                            "dashboard_port": dashboard_port,
                        });
                        match registry.register_node(&node_name, &caps).await {
                            Ok(reg) => {
                                println!(
                                    "  \u{2713} Registered with platform (node_id: {})",
                                    reg.node_id
                                );
                                daemon_platform_node_id = Some(reg.node_id);
                                server_node_state.platform_client = Some(platform.clone());
                                daemon_platform_client = Some(platform);
                            }
                            Err(e) => {
                                eprintln!("  Warning: Platform registration failed: {e}");
                                eprintln!("  (Continuing in offline mode.)");
                            }
                        }
                    } else {
                        eprintln!("  Warning: No credentials — run `prism setup` first to register with platform.");
                    }
                }

                let daemon_rbac_db_path = server_node_state.rbac_db_path.clone();
                let server_state = std::sync::Arc::new(server_node_state);
                if let Some(ref handles) = service_handles {
                    server_state.update_services(
                        handles
                            .services
                            .iter()
                            .map(|h| prism_server::ServiceEntry {
                                name: h.name.clone(),
                                port: h.port,
                                healthy: h.healthy,
                            })
                            .collect(),
                    );
                }
                let (_addr, _server_handle) =
                    prism_server::start_server(server_state.clone(), dashboard_port)
                        .await
                        .context("Failed to start dashboard server")?;
                println!(
                    "  \u{2713} {:<12} http://localhost:{}",
                    "Dashboard", dashboard_port
                );
                println!();

                // ── V1: Run the platform daemon (heartbeat, job dispatch) ──
                let daemon_options = prism_node::daemon::DaemonOptions {
                    name: node_name,
                    visibility,
                    price_per_hour_usd: price,
                    no_compute,
                    no_storage,
                    ssh: ssh_host.map(|host| prism_node::daemon::SshCapability {
                        host,
                        port: ssh_port,
                        user: ssh_user.or_else(default_ssh_user),
                    }),
                    broadcast,
                    platform_client: daemon_platform_client,
                    platform_node_id: daemon_platform_node_id,
                    rbac_db_path: daemon_rbac_db_path,
                    org_id: daemon_org_id,
                };

                // ── Start mesh networking (mDNS discovery + optional broadcast) ──
                let mesh_cancel = tokio_util::sync::CancellationToken::new();
                let mesh_config = prism_mesh::MeshConfig {
                    node_name: daemon_options.name.clone(),
                    publish_port: dashboard_port,
                    discovery: vec![prism_mesh::DiscoveryMethod::Mdns],
                };
                let mesh_handle = prism_mesh::init_mesh(mesh_config)?;
                let mesh_task = prism_mesh::start_mesh(
                    mesh_handle,
                    prism_mesh::MeshStartOptions {
                        node_name: daemon_options.name.clone(),
                        publish_port: dashboard_port,
                        broadcast,
                        capabilities: Vec::new(),
                        discovery_interval_secs: 30,
                        event_tx: Some(server_state.ws_broadcast.clone()),
                    },
                    mesh_cancel.clone(),
                );
                if broadcast {
                    println!("  \u{2713} Mesh: broadcasting (mDNS + platform discovery)");
                } else {
                    println!("  \u{2713} Mesh: passive discovery (use --broadcast to advertise)");
                }

                // Run daemon until Ctrl+C — on shutdown, stop Docker containers
                let result =
                    prism_node::daemon::run_daemon(&endpoints, &paths, daemon_options).await;

                // Stop mesh
                mesh_cancel.cancel();
                mesh_task.await.ok();

                // Graceful shutdown: stop managed services
                if let Some(handles) = service_handles {
                    println!("\nStopping managed services...");
                    if let Ok(orch) = prism_orch::DockerOrchestrator::new() {
                        use prism_orch::ServiceOrchestrator;
                        if let Err(e) = orch.stop_all(&handles).await {
                            eprintln!("Warning: Failed to stop some containers: {e}");
                        } else {
                            println!("All services stopped.");
                        }
                    }
                }

                result?;
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
            NodeCommands::Logs { service, tail } => {
                let orch = prism_orch::DockerOrchestrator::new()?;
                match orch.container_logs(&service, tail).await {
                    Ok(logs) => print!("{logs}"),
                    Err(e) => {
                        eprintln!("Failed to get logs for '{service}': {e}");
                        std::process::exit(1);
                    }
                }
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
        Commands::Ingest {
            path,
            llm_provider,
            model,
            llm_url,
            api_key,
            neo4j_url,
            neo4j_user,
            neo4j_pass,
            qdrant_url,
            schema_only,
            watch,
            mapping,
        } => {
            let llm_cfg = build_llm_config(&llm_provider, &llm_url, &model, api_key.as_deref());
            if watch {
                handle_ingest_watch(
                    &path,
                    &llm_cfg,
                    &neo4j_url,
                    &neo4j_user,
                    &neo4j_pass,
                    &qdrant_url,
                    schema_only,
                    mapping.as_deref(),
                )
                .await?;
            } else {
                handle_ingest(
                    &path,
                    &llm_cfg,
                    &neo4j_url,
                    &neo4j_user,
                    &neo4j_pass,
                    &qdrant_url,
                    schema_only,
                    mapping.as_deref(),
                )
                .await?;
            }
        }
        Commands::Query {
            text,
            cypher,
            semantic,
            platform,
            json: json_output,
            federated,
            neo4j_url,
            neo4j_user,
            neo4j_pass,
            qdrant_url,
            llm_provider,
            llm_url,
            model,
            api_key,
            limit,
            dashboard_url,
        } => {
            if platform {
                // Route through MARC27 platform API
                handle_platform_query(&text, semantic, json_output, limit).await?;
            } else if federated {
                handle_federated_query(&text, &dashboard_url).await?;
            } else {
                let llm_cfg = build_llm_config(&llm_provider, &llm_url, &model, api_key.as_deref());
                handle_query(
                    &text,
                    cypher,
                    semantic,
                    &neo4j_url,
                    &neo4j_user,
                    &neo4j_pass,
                    &qdrant_url,
                    &llm_cfg,
                    limit,
                )
                .await?;
            }
        }
        Commands::Agent => {
            print_agent_guide();
        }
        Commands::Run {
            image,
            name,
            input,
            backend,
            platform_url,
        } => {
            handle_run(&name, &image, &input, &backend, &platform_url).await?;
        }
        Commands::JobStatus { job_id } => {
            handle_job_status(&job_id).await?;
        }
        Commands::Mesh { command } => {
            handle_mesh_command(command).await?;
        }
        Commands::Report {
            description,
            log_file,
            no_github,
        } => {
            handle_report(&paths, &endpoints, &description, log_file.as_deref(), no_github).await?;
        }
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

// ── prism mesh ─────────────────────────────────────────────────────────

async fn handle_mesh_command(command: MeshCommands) -> Result<()> {
    match command {
        MeshCommands::Discover { timeout } => {
            println!(
                "Discovering PRISM nodes on local network ({}s timeout)...",
                timeout
            );
            let config = prism_mesh::MeshConfig {
                node_name: "discovery-probe".into(),
                publish_port: 0,
                discovery: vec![prism_mesh::DiscoveryMethod::Mdns],
            };
            let handle = prism_mesh::init_mesh(config)?;
            // mDNS discovery is async — wait for the timeout period.
            tokio::time::sleep(Duration::from_secs(timeout)).await;
            let peers = handle.peers();
            if peers.is_empty() {
                println!("No peers found.");
            } else {
                println!(
                    "{:<36}  {:<20}  {:<22}  Capabilities",
                    "ID", "Name", "Address"
                );
                println!("{}", "-".repeat(90));
                for p in &peers {
                    println!(
                        "{:<36}  {:<20}  {}:{:<5}  {}",
                        p.node_id,
                        p.name,
                        p.address,
                        p.port,
                        p.capabilities.join(", ")
                    );
                }
                println!("\n{} peer(s) found.", peers.len());
            }
        }
        MeshCommands::Peers { dashboard_url } => {
            let url = format!("{dashboard_url}/api/mesh/nodes");
            let resp = reqwest::get(&url)
                .await
                .with_context(|| format!("Failed to reach node at {url}"))?;
            let body = resp.text().await?;
            let status: serde_json::Value = serde_json::from_str(&body)?;

            let online = status["online"].as_bool().unwrap_or(false);
            if !online {
                println!("Mesh: offline");
                return Ok(());
            }

            println!(
                "Mesh: online (node {})",
                status["node_id"].as_str().unwrap_or("?")
            );
            let peers = status["peers"].as_array();
            match peers {
                Some(list) if !list.is_empty() => {
                    println!("{} peer(s):", list.len());
                    for p in list {
                        println!(
                            "  {} — {}:{}  (last seen: {})",
                            p["name"].as_str().unwrap_or("?"),
                            p["address"].as_str().unwrap_or("?"),
                            p["port"].as_u64().unwrap_or(0),
                            p["last_seen"].as_str().unwrap_or("?"),
                        );
                    }
                }
                _ => println!("No peers connected."),
            }
        }
        MeshCommands::Publish {
            name,
            schema_version,
            dashboard_url,
        } => {
            println!("Publishing dataset '{name}' (v{schema_version}) to mesh...");
            let url = format!("{dashboard_url}/api/mesh/publish");
            let client = reqwest::Client::new();
            let resp = client
                .post(&url)
                .json(&serde_json::json!({
                    "name": name,
                    "schema_version": schema_version,
                }))
                .send()
                .await
                .with_context(|| format!("Failed to reach node at {dashboard_url}"))?;
            if resp.status().is_success() {
                println!(
                    "Dataset '{name}' published. Other nodes can subscribe via mesh discovery."
                );
            } else {
                bail!(
                    "Node returned error: {} — {}",
                    resp.status(),
                    resp.text().await.unwrap_or_default()
                );
            }
        }
        MeshCommands::Subscribe {
            dataset_name,
            publisher,
            dashboard_url,
        } => {
            println!("Subscribing to '{dataset_name}' from node {publisher}...");
            let url = format!("{dashboard_url}/api/mesh/subscribe");
            let client = reqwest::Client::new();
            let resp = client
                .post(&url)
                .json(&serde_json::json!({
                    "dataset_name": dataset_name,
                    "publisher_node": publisher,
                }))
                .send()
                .await
                .with_context(|| format!("Failed to reach node at {dashboard_url}"))?;
            if resp.status().is_success() {
                println!("Subscribed to '{dataset_name}'. Updates will sync automatically.");
            } else {
                bail!(
                    "Node returned error: {} — {}",
                    resp.status(),
                    resp.text().await.unwrap_or_default()
                );
            }
        }
        MeshCommands::Unsubscribe {
            dataset_name,
            publisher,
            dashboard_url,
        } => {
            println!("Unsubscribing from '{dataset_name}'...");
            let url = format!("{dashboard_url}/api/mesh/subscribe");
            let client = reqwest::Client::new();
            let resp = client
                .delete(&url)
                .json(&serde_json::json!({
                    "dataset_name": dataset_name,
                    "publisher_node": publisher,
                }))
                .send()
                .await
                .with_context(|| format!("Failed to reach node at {dashboard_url}"))?;
            if resp.status().is_success() {
                println!("Unsubscribed from '{dataset_name}'.");
            } else {
                bail!(
                    "Node returned error: {} — {}",
                    resp.status(),
                    resp.text().await.unwrap_or_default()
                );
            }
        }
        MeshCommands::Subscriptions { dashboard_url } => {
            let url = format!("{dashboard_url}/api/mesh/subscriptions");
            let resp = reqwest::get(&url)
                .await
                .with_context(|| format!("Failed to reach node at {url}"))?;
            let body = resp.text().await?;
            let data: serde_json::Value = serde_json::from_str(&body)?;

            let published = data["published"].as_array();
            let subscribed = data["subscribed"].as_array();

            println!("Published datasets:");
            match published {
                Some(list) if !list.is_empty() => {
                    for d in list {
                        println!(
                            "  {} (v{}) — {} subscriber(s)",
                            d["name"].as_str().unwrap_or("?"),
                            d["schema_version"].as_str().unwrap_or("?"),
                            d["subscriber_count"].as_u64().unwrap_or(0),
                        );
                    }
                }
                _ => println!("  (none)"),
            }

            println!("\nActive subscriptions:");
            match subscribed {
                Some(list) if !list.is_empty() => {
                    for s in list {
                        println!(
                            "  {} from node {} (since {})",
                            s["dataset_name"].as_str().unwrap_or("?"),
                            s["publisher_node"].as_str().unwrap_or("?"),
                            s["subscribed_at"].as_str().unwrap_or("?"),
                        );
                    }
                }
                _ => println!("  (none)"),
            }
        }
    }
    Ok(())
}

// ── LLM config builder ─────────────────────────────────────────────────

fn build_llm_config(
    provider: &str,
    base_url: &str,
    model: &str,
    api_key: Option<&str>,
) -> prism_ingest::LlmConfig {
    use prism_ingest::llm::LlmProvider;
    let provider_enum = match provider.to_lowercase().as_str() {
        "openai" | "openai-compatible" | "marc27" | "vllm" | "litellm" => LlmProvider::OpenAi,
        _ => LlmProvider::Ollama,
    };
    // Default embedding model based on provider
    let embedding_model = match provider_enum {
        LlmProvider::Ollama => Some("nomic-embed-text".to_string()),
        LlmProvider::OpenAi => None, // OpenAI-compatible APIs use the same model or a default
    };
    prism_ingest::LlmConfig {
        provider: provider_enum,
        base_url: base_url.into(),
        model: model.into(),
        api_key: api_key.map(str::to_string),
        embedding_model,
        max_sample_rows: 10,
        timeout_secs: 120,
    }
}

// ── prism ingest ────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn handle_ingest(
    path: &Path,
    llm_cfg: &prism_ingest::LlmConfig,
    neo4j_url: &str,
    neo4j_user: &str,
    neo4j_pass: &str,
    qdrant_url: &str,
    schema_only: bool,
    mapping_path: Option<&Path>,
) -> Result<()> {
    use prism_ingest::pipeline::{IngestPipeline, PipelineConfig};
    use prism_ingest::{Neo4jConfig, QdrantConfig};

    if !path.exists() {
        bail!("File not found: {}", path.display());
    }

    println!("Ingesting: {}", path.display());

    let mapping = mapping_path
        .map(prism_ingest::mapping::OntologyMapping::from_file)
        .transpose()?;

    let config = if schema_only {
        PipelineConfig {
            llm: None,
            neo4j: None,
            qdrant: None,
            max_sample_rows: 10,
            mapping: None,
        }
    } else {
        PipelineConfig {
            llm: Some(llm_cfg.clone()),
            neo4j: Some(Neo4jConfig {
                base_url: neo4j_url.into(),
                database: "neo4j".into(),
                username: neo4j_user.into(),
                password: neo4j_pass.into(),
            }),
            qdrant: Some(QdrantConfig {
                base_url: qdrant_url.into(),
                collection: "prism_embeddings".into(),
                api_key: None,
            }),
            max_sample_rows: 10,
            mapping,
        }
    };

    let pipeline = IngestPipeline::with_config(config);
    let result = pipeline.ingest_file(path).await?;

    println!();
    println!(
        "  Schema: {} columns, {} rows",
        result.column_count, result.row_count
    );
    println!("  Columns: {}", result.schema.columns.join(", "));

    if !result.validation.passed {
        println!("  Warnings: {} issues", result.validation.issues.len());
    }

    if let Some(ref entities) = result.entities {
        println!(
            "  Entities: {} extracted, {} relationships",
            entities.entities.len(),
            entities.relationships.len()
        );
    }

    if let Some(ref graph) = result.graph {
        println!(
            "  Graph: {} nodes, {} edges written to Neo4j",
            graph.nodes_created, graph.edges_created
        );
    }

    if let Some(ref embeddings) = result.embeddings {
        println!(
            "  Embeddings: {} vectors (dim={})",
            embeddings.vectors.len(),
            embeddings.dimension.unwrap_or(0)
        );
    }

    if schema_only {
        println!("  (schema-only mode — LLM/graph/vector steps skipped)");
    }

    println!("\n  Done.");
    Ok(())
}

/// Watch a directory for new/modified CSV/Parquet files and ingest them.
#[allow(clippy::too_many_arguments)]
async fn handle_ingest_watch(
    dir: &Path,
    llm_cfg: &prism_ingest::LlmConfig,
    neo4j_url: &str,
    neo4j_user: &str,
    neo4j_pass: &str,
    qdrant_url: &str,
    schema_only: bool,
    _mapping: Option<&Path>,
) -> Result<()> {
    use std::collections::HashMap;
    use std::time::{Duration, SystemTime};

    if !dir.is_dir() {
        bail!("Watch mode requires a directory, got: {}", dir.display());
    }

    println!(
        "Watching {} for CSV/Parquet files (Ctrl+C to stop)...\n",
        dir.display()
    );

    // Track file modification times to detect changes
    let mut seen: HashMap<PathBuf, SystemTime> = HashMap::new();

    // Initial scan — ingest all existing files
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if !is_ingestable(&path) {
            continue;
        }
        if let Ok(meta) = path.metadata() {
            if let Ok(modified) = meta.modified() {
                seen.insert(path.clone(), modified);
            }
        }
        println!("[initial] Ingesting {}", path.display());
        if let Err(e) = handle_ingest(
            &path,
            llm_cfg,
            neo4j_url,
            neo4j_user,
            neo4j_pass,
            qdrant_url,
            schema_only,
            None,
        )
        .await
        {
            eprintln!("  Error: {e}");
        }
    }

    // Poll loop — check for new/modified files every 5 seconds
    let poll_interval = Duration::from_secs(5);
    loop {
        tokio::time::sleep(poll_interval).await;

        let entries: Vec<_> = match std::fs::read_dir(dir) {
            Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
            Err(e) => {
                eprintln!("Failed to read directory: {e}");
                continue;
            }
        };

        for entry in entries {
            let path = entry.path();
            if !is_ingestable(&path) {
                continue;
            }

            let modified = match path.metadata().and_then(|m| m.modified()) {
                Ok(t) => t,
                Err(_) => continue,
            };

            let is_new = match seen.get(&path) {
                Some(prev) => modified > *prev,
                None => true,
            };

            if is_new {
                seen.insert(path.clone(), modified);
                println!("\n[watch] Detected: {}", path.display());
                if let Err(e) = handle_ingest(
                    &path,
                    llm_cfg,
                    neo4j_url,
                    neo4j_user,
                    neo4j_pass,
                    qdrant_url,
                    schema_only,
                    None,
                )
                .await
                {
                    eprintln!("  Error: {e}");
                }
            }
        }
    }
}

/// Check if a file has an ingestable extension.
fn is_ingestable(path: &Path) -> bool {
    path.is_file()
        && matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("csv" | "tsv" | "parquet" | "pq")
        )
}

// ── prism query ─────────────────────────────────────────────────────────

/// Print a guide for AI agents describing available PRISM commands.
/// This is the "agent interface" — grep-friendly, no protocol overhead.
fn print_agent_guide() {
    println!(
        r#"PRISM Agent Interface — grep-friendly commands
==============================================

KNOWLEDGE GRAPH (use --platform to query MARC27 cloud, 211K+ entities):
  prism query --platform --semantic "creep resistant superalloy"     # semantic search
  prism query --platform "Inconel 718"                               # graph search
  prism query --platform --semantic "yield strength titanium" --json # JSON output for piping
  prism query --platform --json "fatigue life" | grep MAT            # grep for materials only
  prism status | grep nodes                                          # quick stats

COMPUTE:
  prism run <image> --backend local                    # run container locally
  prism run <image> --backend marc27                   # run on MARC27 cloud
  prism job-status <job-id>                            # check job status

INGEST:
  prism ingest data.csv                                # ingest CSV into local graph
  prism ingest paper.pdf                               # extract + ingest PDF

NODE:
  prism node status                                    # show node capabilities
  prism node up                                        # register node with platform
  prism node down                                      # deregister

WORKFLOWS:
  prism workflow list                                  # list available workflows
  prism workflow run <name> --set key=value            # run a workflow

AUTH (two paths — decoupled):
  # For humans:
  prism login                                          # device flow → JWT (stored in ~/.prism/)
  prism status                                         # show auth + config

  # For agents (no login needed):
  export MARC27_API_KEY=m27_your_key_here              # set once, works forever
  prism query --platform "titanium"                    # just works

OUTPUT:
  Default: human-readable, one result per line (grep-friendly)
  --json:  JSON array (pipe to jq/python)
  --platform: route through MARC27 API (211K nodes, 6.5M edges, 208K embeddings)

AGENT SETUP (one line):
  export MARC27_API_KEY=m27_...                        # that's it. no login, no refresh, no expiry.

EXAMPLES:
  prism query --platform --semantic "creep resistant superalloy" --json | jq '.[].content'
  prism query --platform "Ti-6Al-4V" | grep MAT
  prism query --platform --json "fatigue" | python3 -c "import sys,json;[print(e['name']) for e in json.load(sys.stdin)]"
  prism status | grep nodes
"#
    );
}

/// Resolve platform auth for the human user (prism login → JWT).
fn resolve_user_auth() -> Result<(String, String)> {
    let home = std::env::var("HOME").context("HOME not set")?;
    let cred_path = format!("{home}/.prism/credentials.json");
    let cred_data =
        std::fs::read_to_string(&cred_path).context("Not logged in. Run `prism login`.")?;
    let creds: serde_json::Value = serde_json::from_str(&cred_data)?;
    let raw_url = creds
        .get("platform_url")
        .and_then(|v| v.as_str())
        .unwrap_or("https://api.marc27.com");
    let api_base = if raw_url.ends_with("/api/v1") {
        raw_url.to_string()
    } else {
        format!("{}/api/v1", raw_url.trim_end_matches('/'))
    };
    let token = creds
        .get("access_token")
        .and_then(|v| v.as_str())
        .context("No access_token. Run `prism login`.")?;
    Ok((api_base, format!("Bearer {token}")))
}

/// Auth credentials — either API key (agent) or Bearer token (user).
enum PlatformAuth {
    ApiKey(String), // X-API-Key header
    Bearer(String), // Authorization: Bearer header
}

impl PlatformAuth {
    fn apply(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self {
            PlatformAuth::ApiKey(key) => req.header("X-API-Key", key),
            PlatformAuth::Bearer(token) => req.header("Authorization", format!("Bearer {token}")),
        }
    }
}

/// Resolve platform auth for agents (MARC27_API_KEY env var → X-API-Key header).
/// Decoupled from user auth — agents use API keys, users use JWT.
/// Falls back to user auth if no API key is set (backward compat).
fn resolve_agent_auth() -> Result<(String, PlatformAuth)> {
    if let Ok(api_key) = std::env::var("MARC27_API_KEY") {
        let api_base = std::env::var("MARC27_API_URL")
            .unwrap_or_else(|_| "https://api.marc27.com/api/v1".to_string());
        return Ok((api_base, PlatformAuth::ApiKey(api_key)));
    }
    let (base, token_header) = resolve_user_auth()?;
    // token_header is "Bearer <token>"
    let token = token_header
        .strip_prefix("Bearer ")
        .unwrap_or(&token_header)
        .to_string();
    Ok((base, PlatformAuth::Bearer(token)))
}

/// Query the MARC27 platform API (graph search or semantic search).
async fn handle_platform_query(
    text: &str,
    semantic: bool,
    json_output: bool,
    limit: usize,
) -> Result<()> {
    let (api_base, auth) = resolve_agent_auth()?;

    let client = reqwest::Client::new();

    if semantic {
        // POST /knowledge/search
        let resp = auth
            .apply(client.post(format!("{api_base}/knowledge/search")))
            .json(&serde_json::json!({"query": text, "limit": limit}))
            .send()
            .await?;

        if !resp.status().is_success() {
            bail!("Platform API error: {}", resp.status());
        }

        let results: Vec<serde_json::Value> = resp.json().await?;
        if json_output {
            println!("{}", serde_json::to_string_pretty(&results)?);
        } else {
            println!("Semantic search results ({} matches):\n", results.len());
            for (i, r) in results.iter().enumerate() {
                let sim = r.get("similarity").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let content = r.get("content").and_then(|v| v.as_str()).unwrap_or("?");
                let source = r
                    .get("metadata")
                    .and_then(|m| m.get("source"))
                    .and_then(|s| s.as_str())
                    .unwrap_or("?");
                println!(
                    "  {}. [sim={:.3}] [{}] {}",
                    i + 1,
                    sim,
                    source,
                    &content[..content.len().min(120)]
                );
            }
        }
    } else {
        // GET /knowledge/graph/search
        let resp = auth
            .apply(client.get(format!("{api_base}/knowledge/graph/search")))
            .query(&[("q", text), ("limit", &limit.to_string())])
            .send()
            .await?;

        if !resp.status().is_success() {
            bail!("Platform API error: {}", resp.status());
        }

        let results: Vec<serde_json::Value> = resp.json().await?;
        if json_output {
            println!("{}", serde_json::to_string_pretty(&results)?);
        } else if results.is_empty() {
            println!("No direct matches. Try --semantic for vector search.");
        } else {
            println!("Graph search results ({} matches):\n", results.len());
            for r in &results {
                let name = r.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let etype = r.get("entity_type").and_then(|v| v.as_str()).unwrap_or("?");
                let label = r.get("label").and_then(|v| v.as_str()).unwrap_or("");
                println!(
                    "  [{:5}] {} — {}",
                    etype,
                    name,
                    &label[..label.len().min(80)]
                );
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn handle_query(
    text: &str,
    cypher: bool,
    semantic: bool,
    neo4j_url: &str,
    neo4j_user: &str,
    neo4j_pass: &str,
    qdrant_url: &str,
    llm_cfg: &prism_ingest::LlmConfig,
    limit: usize,
) -> Result<()> {
    use prism_ingest::embeddings::{QdrantVectorStore, VectorStore};
    use prism_ingest::graph::{GraphStore, Neo4jGraphStore};
    use prism_ingest::{Neo4jConfig, QdrantConfig};

    let neo4j_config = Neo4jConfig {
        base_url: neo4j_url.into(),
        database: "neo4j".into(),
        username: neo4j_user.into(),
        password: neo4j_pass.into(),
    };

    if cypher {
        // Direct Cypher execution
        let store = Neo4jGraphStore::new(neo4j_config);
        println!("Executing Cypher: {text}\n");
        let results = store.query_cypher(text, None).await?;
        if results.is_empty() {
            println!("  (no results)");
        } else {
            for (i, row) in results.iter().enumerate() {
                println!("  {}. {}", i + 1, row);
            }
            println!("\n  {} row(s)", results.len());
        }
    } else if semantic {
        // Semantic vector search
        let qdrant_config = QdrantConfig {
            base_url: qdrant_url.into(),
            collection: "prism_embeddings".into(),
            api_key: None,
        };

        // Generate embedding for the query text via provider-agnostic LlmClient
        println!("Generating query embedding...");
        let llm_client = prism_ingest::llm::LlmClient::new(llm_cfg.clone());
        let query_vec = llm_client
            .embed_text(text)
            .await
            .context("failed to generate query embedding")?;

        let store = QdrantVectorStore::new(qdrant_config);
        let results = store.query(&query_vec, limit).await?;

        println!("\nSemantic search results ({} matches):\n", results.len());
        for (i, (id, score)) in results.iter().enumerate() {
            println!("  {}. {id}  (score: {score:.4})", i + 1);
        }
        if results.is_empty() {
            println!("  (no results — collection may be empty)");
        }
    } else {
        // Natural language → graph traversal
        // Find entities whose names match the query text, then traverse neighbors.
        let store = Neo4jGraphStore::new(neo4j_config);
        println!("Querying knowledge graph: \"{text}\"\n");

        let result = store.neighbors(text, 3).await?;
        if result.entities.is_empty() {
            println!("  No direct matches. Try --semantic for vector search.");
        } else {
            println!("  Found {} connected entities:\n", result.entities.len());
            for entity in &result.entities {
                let props_str = if entity.properties.is_object()
                    && entity.properties.as_object().is_none_or(|o| o.is_empty())
                {
                    String::new()
                } else {
                    format!("  {}", entity.properties)
                };
                println!(
                    "  [{type}] {name}{props}",
                    r#type = entity.entity_type,
                    name = entity.name,
                    props = props_str,
                );
            }
        }
    }

    Ok(())
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
    let platform = PlatformClient::new(&endpoints.api_base);
    let http = platform.inner().clone();

    let start: DeviceCodeResponse =
        DeviceFlowAuth::start_device_flow(&http, &endpoints.api_base).await?;

    println!();
    println!("PRISM setup needs MARC27 platform login.");
    println!("Open: {}", start.verification_uri);
    println!("Code: {}", start.user_code);
    println!();
    if let Err(err) = open_browser(&start.verification_uri) {
        eprintln!("warning: failed to open browser automatically: {err}");
    }
    println!("Approve the device in your browser, then return here.");

    let token: TokenResponse = DeviceFlowAuth::poll_for_token(
        &http,
        &endpoints.api_base,
        &start.device_code,
        start.interval.max(1) as u64,
    )
    .await?;

    let expires_at = token.expires_in.and_then(|secs| {
        chrono::Utc::now().checked_add_signed(chrono::Duration::seconds(secs as i64))
    });

    Ok(StoredCredentials {
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        platform_url: endpoints.api_base.trim_end_matches("/api/v1").to_string(),
        user_id: None,
        display_name: None,
        org_id: None,
        org_name: None,
        project_id: None,
        project_name: None,
        expires_at,
    })
}

async fn select_project(
    platform: &PlatformClient,
    display_name: Option<&str>,
) -> Result<SelectedContext> {
    if let Some(project_id) = env_project_override() {
        match platform.get_project(&project_id).await {
            Ok(project) => {
                let org_name = platform.list_orgs().await.ok().and_then(|orgs| {
                    orgs.into_iter()
                        .find(|org| org.id == project.org_id)
                        .map(|org| org.name)
                });
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

    let orgs = platform.list_orgs().await?;

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

    let projects = platform.list_projects_for_org(&selected_org.id).await?;

    if projects.is_empty() {
        println!("No projects found in organization {}.", selected_org.name);
        let name = default_project_name(display_name);
        let slug = default_project_slug();
        let created = platform
            .create_project(&selected_org.id, &name, &slug)
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
    let platform = PlatformClient::new(&endpoints.api_base);
    let refreshed =
        DeviceFlowAuth::refresh_token(platform.inner(), &endpoints.api_base, &creds.refresh_token)
            .await?;

    let mut new_creds = creds.clone();
    new_creds.access_token = refreshed.access_token;
    new_creds.refresh_token = refreshed.refresh_token;
    new_creds.expires_at = refreshed.expires_in.and_then(|secs| {
        chrono::Utc::now().checked_add_signed(chrono::Duration::seconds(secs as i64))
    });

    Ok(new_creds)
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
    println!("  CPU: {} cores, {} GB RAM", caps.cpu_cores, caps.ram_gb);
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

// ── prism query --federated ────────────────────────────────────────────

async fn handle_federated_query(query: &str, dashboard_url: &str) -> Result<()> {
    // Step 1: Get peer list from the running node
    let peers_url = format!("{dashboard_url}/api/mesh/nodes");
    let resp: serde_json::Value = reqwest::get(&peers_url)
        .await
        .with_context(|| format!("Failed to reach node at {dashboard_url}"))?
        .json()
        .await?;

    let peer_list = resp["peers"].as_array();
    let peer_count = peer_list.map(|a| a.len()).unwrap_or(0);

    if peer_count == 0 {
        println!("No mesh peers found. Run with mDNS discovery or register via platform.");
        return Ok(());
    }

    println!("Querying {} peer(s) + local node...\n", peer_count);

    // Step 2: Query local node
    let local_url = format!("{dashboard_url}/api/query");
    let local_body = serde_json::json!({"query": query, "mode": "nl"});
    let local_result = reqwest::Client::new()
        .post(&local_url)
        .json(&local_body)
        .send()
        .await;

    println!("[local] ");
    match local_result {
        Ok(r) => {
            let data: serde_json::Value = r.json().await.unwrap_or_default();
            let count = data["count"].as_u64().unwrap_or(0);
            println!("  {} result(s)", count);
            if let Some(results) = data["results"].as_array() {
                for r in results.iter().take(5) {
                    println!("  {}", serde_json::to_string(r).unwrap_or_default());
                }
            }
        }
        Err(e) => println!("  error: {e}"),
    }

    // Step 3: Query each peer
    let peers = peer_list.unwrap();
    for peer in peers {
        let addr = peer["address"].as_str().unwrap_or("127.0.0.1");
        let port = peer["port"].as_u64().unwrap_or(7327);
        let name = peer["name"].as_str().unwrap_or("unknown");
        let peer_url = format!("http://{}:{}/api/query", addr, port);
        let body = serde_json::json!({"query": query, "mode": "nl"});

        print!("[{name}] ");
        match reqwest::Client::new()
            .post(&peer_url)
            .json(&body)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
        {
            Ok(r) => {
                let data: serde_json::Value = r.json().await.unwrap_or_default();
                let count = data["count"].as_u64().unwrap_or(0);
                println!("{} result(s)", count);
                if let Some(results) = data["results"].as_array() {
                    for r in results.iter().take(5) {
                        println!("  {}", serde_json::to_string(r).unwrap_or_default());
                    }
                }
            }
            Err(e) => println!("unreachable ({e})"),
        }
    }

    Ok(())
}

// ── prism run ─────────────────────────────────────────────────────────

async fn handle_run(
    name: &str,
    image: &str,
    inputs: &[String],
    backend: &str,
    platform_url: &str,
) -> Result<()> {
    use prism_compute::backend::ComputeRouter;
    use prism_compute::ExperimentPlan;

    // Parse key=value inputs into JSON
    let mut input_map = serde_json::Map::new();
    for kv in inputs {
        if let Some((k, v)) = kv.split_once('=') {
            input_map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
        }
    }

    let plan = ExperimentPlan {
        name: name.to_string(),
        image: image.to_string(),
        inputs: serde_json::Value::Object(input_map),
    };

    let router = match backend {
        "marc27" | "platform" => {
            // Read token from credentials
            let token = std::env::var("MARC27_API_TOKEN").unwrap_or_else(|_| "".to_string());
            ComputeRouter::with_marc27(platform_url, &token)
        }
        _ => ComputeRouter::local_only(),
    };

    println!("Submitting job '{name}' (image: {image}, backend: {backend})...");

    // Timeout for submit (Docker may need to pull the image)
    let job_id = tokio::time::timeout(std::time::Duration::from_secs(120), router.submit(&plan))
        .await
        .map_err(|_| {
            anyhow::anyhow!("Job submission timed out after 120s (image pull may be slow)")
        })??;

    println!("Job submitted: {job_id}");
    println!("Check status:  prism job-status {job_id}");

    // Brief poll for initial status
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    match router.status(job_id).await {
        Ok(status) => println!("Status: {:?}", status),
        Err(e) => println!("Status check: {e}"),
    }

    Ok(())
}

async fn handle_job_status(job_id_str: &str) -> Result<()> {
    let job_id: uuid::Uuid = job_id_str
        .parse()
        .with_context(|| format!("invalid job UUID: {job_id_str}"))?;

    // Try local backend first
    let router = prism_compute::backend::ComputeRouter::local_only();
    match router.status(job_id).await {
        Ok(status) => {
            println!("Job: {job_id}");
            println!("Status: {:?}", status);
        }
        Err(e) => {
            println!("Job {job_id}: {e}");
        }
    }

    Ok(())
}

// ── prism report ───────────────────────────────────────────────────────

async fn handle_report(
    paths: &prism_runtime::PrismPaths,
    endpoints: &prism_runtime::PlatformEndpoints,
    description: &str,
    log_file: Option<&Path>,
    no_github: bool,
) -> Result<()> {
    println!("Collecting system context...\n");

    // 1. Gather system context
    let version = env!("CARGO_PKG_VERSION");
    let caps = prism_node::detect::probe_local_capabilities_async().await;
    let os_info = format!(
        "{} ({})",
        caps.software.join(", "),
        std::env::consts::ARCH,
    );
    let python_version = std::process::Command::new("python3")
        .arg("--version")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());

    // Read log file if provided
    let log_content = if let Some(path) = log_file {
        std::fs::read_to_string(path)
            .ok()
            .map(|s| {
                if s.len() > 5000 {
                    format!("...(truncated)...\n{}", &s[s.len() - 5000..])
                } else {
                    s
                }
            })
    } else {
        None
    };

    // Read credentials for platform submission
    let state = paths.load_cli_state()?;
    let creds = state.credentials.as_ref();
    let user_name = creds
        .and_then(|c| c.display_name.as_deref())
        .unwrap_or("anonymous");
    let user_id = creds.and_then(|c| c.user_id.as_deref()).unwrap_or("");
    let project_id = creds.and_then(|c| c.project_id.as_deref()).unwrap_or("");

    // 2. Build the report body
    let mut body = format!(
        "## Bug Report\n\n\
         **Description:** {description}\n\n\
         **Reporter:** {user_name}\n\n\
         ## System Info\n\n\
         | | |\n|---|---|\n\
         | PRISM | v{version} |\n\
         | Python | {python_version} |\n\
         | OS | {os_info} |\n\
         | CPU | {} cores |\n\
         | RAM | {} GB |\n\
         | Docker | {} |\n",
        caps.cpu_cores,
        caps.ram_gb / 1024, // MB to GB
        if caps.docker { "yes" } else { "no" },
    );

    if let Some(ref log) = log_content {
        body.push_str(&format!(
            "\n## Error Output\n\n```\n{}\n```\n",
            log
        ));
    }

    // 3. File GitHub issue (unless --no-github)
    if !no_github {
        print!("Filing GitHub issue... ");
        let gh_result = tokio::process::Command::new("gh")
            .args([
                "issue", "create",
                "--repo", "Darth-Hidious/PRISM",
                "--title", &format!("bug report: {}", &description[..description.len().min(60)]),
                "--body", &body,
                "--label", "bug",
            ])
            .output()
            .await;

        match gh_result {
            Ok(output) if output.status.success() => {
                let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
                println!("done → {url}");
            }
            Ok(output) => {
                let err = String::from_utf8_lossy(&output.stderr);
                println!("failed ({err})");
                println!("  (Is `gh` CLI installed and authenticated?)");
            }
            Err(e) => {
                println!("failed ({e})");
                println!("  Install GitHub CLI: https://cli.github.com");
            }
        }
    }

    // 4. Send to MARC27 platform
    if let Some(ref c) = creds {
        if !c.access_token.is_empty() {
            print!("Sending to MARC27 platform... ");
            let platform_body = serde_json::json!({
                "type": "bug_report",
                "description": description,
                "prism_version": version,
                "python_version": python_version,
                "os": os_info,
                "cpu_cores": caps.cpu_cores,
                "ram_gb": caps.ram_gb / 1024,
                "docker": caps.docker,
                "log": log_content,
                "user_id": user_id,
                "project_id": project_id,
            });

            let url = format!("{}/api/v1/support/tickets", endpoints.api_base);
            let resp = reqwest::Client::new()
                .post(&url)
                .header("Authorization", format!("Bearer {}", c.access_token))
                .json(&platform_body)
                .send()
                .await;

            match resp {
                Ok(r) if r.status().is_success() => {
                    let data: serde_json::Value = r.json().await.unwrap_or_default();
                    let ticket_id = data["ticket_id"].as_str().unwrap_or("unknown");
                    println!("done → ticket {ticket_id}");
                    println!("\n  View on dashboard: {}/dashboard/support", endpoints.api_base.replace("/api/v1", ""));
                }
                Ok(r) => {
                    println!("failed (HTTP {})", r.status());
                }
                Err(e) => {
                    println!("failed ({e})");
                }
            }
        }
    }

    println!("\nReport submitted. We'll follow up on GitHub and your MARC27 dashboard.");
    Ok(())
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

    #[test]
    fn cli_parses_ingest_command() {
        let cli = Cli::try_parse_from(["prism", "ingest", "/tmp/data.csv"]).unwrap();
        match cli.command.unwrap() {
            Commands::Ingest {
                path,
                schema_only,
                model,
                ..
            } => {
                assert_eq!(path, PathBuf::from("/tmp/data.csv"));
                assert!(!schema_only);
                assert_eq!(model, "qwen2.5:7b");
            }
            _ => panic!("expected Ingest command"),
        }
    }

    #[test]
    fn cli_parses_ingest_schema_only() {
        let cli =
            Cli::try_parse_from(["prism", "ingest", "--schema-only", "/tmp/data.parquet"]).unwrap();
        match cli.command.unwrap() {
            Commands::Ingest {
                path, schema_only, ..
            } => {
                assert_eq!(path, PathBuf::from("/tmp/data.parquet"));
                assert!(schema_only);
            }
            _ => panic!("expected Ingest command"),
        }
    }

    #[test]
    fn cli_parses_query_command() {
        let cli = Cli::try_parse_from(["prism", "query", "NbMoTaW alloys"]).unwrap();
        match cli.command.unwrap() {
            Commands::Query {
                text,
                cypher,
                semantic,
                limit,
                ..
            } => {
                assert_eq!(text, "NbMoTaW alloys");
                assert!(!cypher);
                assert!(!semantic);
                assert_eq!(limit, 10);
            }
            _ => panic!("expected Query command"),
        }
    }

    #[test]
    fn cli_parses_query_semantic() {
        let cli = Cli::try_parse_from([
            "prism",
            "query",
            "--semantic",
            "--limit",
            "5",
            "similar to Ti-6Al-4V",
        ])
        .unwrap();
        match cli.command.unwrap() {
            Commands::Query {
                text,
                semantic,
                limit,
                ..
            } => {
                assert_eq!(text, "similar to Ti-6Al-4V");
                assert!(semantic);
                assert_eq!(limit, 5);
            }
            _ => panic!("expected Query command"),
        }
    }
}
