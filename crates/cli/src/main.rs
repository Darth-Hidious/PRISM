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
use base64::Engine as _;
use clap::{Parser, Subcommand};
use prism_client::api::PlatformClient;
use prism_client::auth::{DeviceCodeResponse, TokenResponse};
use prism_client::DeviceFlowAuth;
use prism_proto::NodeCapabilities;
use prism_python_bridge::{ensure_venv, ToolServer};
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
    /// Start the agent backend (JSON-RPC server for TUI frontend).
    Backend {
        #[arg(long, default_value = ".")]
        project_root: PathBuf,
        #[arg(long, default_value = "python3")]
        python: PathBuf,
    },
    /// List available Python tools.
    Tools,
    /// PRISM node lifecycle commands.
    Node {
        #[command(subcommand)]
        command: NodeCommands,
    },
    /// Ingest a data file into the knowledge graph.
    Ingest {
        /// Path to a file or directory to ingest. Omit with `--status`.
        path: Option<PathBuf>,
        /// Corpus slug to associate with the ingested data.
        #[arg(long)]
        corpus: Option<String>,
        /// Override LLM model (otherwise uses prism.toml or `prism configure`).
        #[arg(long)]
        model: Option<String>,
        /// Override LLM base URL (otherwise uses prism.toml, default http://localhost:8080).
        #[arg(long)]
        llm_url: Option<String>,
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
        /// Show current ingest/job status instead of ingesting a path.
        #[arg(long)]
        status: bool,
        /// Watch a directory for new/modified files and ingest continuously.
        #[arg(long)]
        watch: bool,
        /// Runtime URL for local PDF extraction.
        #[arg(long, default_value = "http://127.0.0.1:8090")]
        runtime_url: String,
        /// Output JSON instead of human-readable progress.
        #[arg(long)]
        json: bool,
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
        /// Override LLM base URL (otherwise uses prism.toml).
        #[arg(long)]
        llm_url: Option<String>,
        /// Override LLM model (otherwise uses prism.toml).
        #[arg(long)]
        model: Option<String>,
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
        /// BYOC SSH target: user@host (enables SSH backend).
        #[arg(long)]
        ssh: Option<String>,
        /// SSH key path for BYOC SSH.
        #[arg(long, default_value = "~/.ssh/id_ed25519")]
        ssh_key: String,
        /// SSH port (default 22).
        #[arg(long, default_value_t = 22)]
        ssh_port: u16,
        /// Kubernetes context for BYOC K8s.
        #[arg(long)]
        k8s_context: Option<String>,
        /// Kubernetes namespace (default: "default").
        #[arg(long, default_value = "default")]
        k8s_namespace: String,
        /// SLURM head node (user@host) for BYOC SLURM.
        #[arg(long)]
        slurm: Option<String>,
        /// SLURM partition.
        #[arg(long, default_value = "default")]
        slurm_partition: String,
        /// Emit machine-readable JSON instead of human-readable status lines.
        #[arg(long)]
        json: bool,
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
    /// Browse and install tools and workflows from the MARC27 marketplace.
    Marketplace {
        #[command(subcommand)]
        command: MarketplaceCommands,
    },
    /// Start a PRISM/MARC27 research loop for a materials-science goal.
    Research {
        /// Research goal or question that can trigger iterative search and synthesis.
        query: String,
        /// Research depth. Use `0` for the cheapest smoke-test path.
        #[arg(long, default_value_t = 0)]
        depth: u32,
        /// Output as JSON (for piping to other tools / agents).
        #[arg(long)]
        json: bool,
    },
    /// Deploy a model or service to the MARC27 compute platform.
    Deploy {
        #[command(subcommand)]
        command: DeployCommands,
    },
    /// Discover hosted LLM models available for the active MARC27 project.
    Models {
        #[command(subcommand)]
        command: ModelsCommands,
    },
    /// Run multi-agent discourse workflows backed by the MARC27 platform.
    Discourse {
        #[command(subcommand)]
        command: DiscourseCommands,
    },
    /// Publish a model, dataset, or workflow to a remote registry.
    Publish {
        /// Path to the artifact (model checkpoint, dataset directory, workflow YAML).
        path: String,
        /// Target: "huggingface", "marc27", or a custom registry URL.
        #[arg(long, default_value = "marc27")]
        to: String,
        /// Repository name on the target (e.g., "username/my-model").
        #[arg(long)]
        repo: Option<String>,
        /// Make the published artifact private.
        #[arg(long)]
        private: bool,
        /// Emit machine-readable JSON instead of human-readable status lines.
        #[arg(long)]
        json: bool,
    },
    /// Configure PRISM settings — writes to ~/.prism/prism.toml.
    Configure {
        /// LLM provider hint: "llamacpp", "ollama", "openai", "marc27", "anthropic".
        #[arg(long)]
        llm_provider: Option<String>,
        /// LLM base URL (e.g. "http://localhost:8080" for llama.cpp).
        #[arg(long)]
        url: Option<String>,
        /// Generation model name (e.g. "gemma-4-E4B-it").
        #[arg(long)]
        model: Option<String>,
        /// Embedding model name (e.g. "nomic-embed-text").
        #[arg(long)]
        embedding_model: Option<String>,
        /// Show current config without modifying.
        #[arg(long)]
        show: bool,
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
        /// Kafka broker addresses for mesh pub/sub (e.g., "localhost:9092").
        /// If omitted and --with-kafka is set, defaults to "localhost:9092".
        #[arg(long)]
        kafka_brokers: Option<String>,
        /// Also start Spark master (for large-scale data processing, off by default in dev).
        #[arg(long)]
        with_spark: bool,
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

#[derive(Debug, Subcommand)]
enum MarketplaceCommands {
    /// Search the MARC27 marketplace for tools and workflows.
    Search {
        /// Search query.
        query: Option<String>,
    },
    /// Install a tool or workflow from the marketplace.
    Install {
        /// Name of the tool or workflow to install.
        name: String,
        /// Install as workflow (YAML) instead of tool (Python).
        #[arg(long)]
        workflow: bool,
    },
    /// Show details about a marketplace item.
    Info {
        /// Name of the tool or workflow.
        name: String,
    },
}

#[derive(Debug, Subcommand)]
enum DeployCommands {
    /// Create a persistent model or service deployment.
    Create {
        /// Deployment name shown in the platform UI.
        #[arg(long)]
        name: String,
        /// Container image to deploy directly.
        #[arg(long)]
        image: Option<String>,
        /// Marketplace resource slug to deploy instead of a raw image.
        #[arg(long)]
        resource_slug: Option<String>,
        /// Target deployment backend: `local`, `mesh`, `runpod`, `lambda`, or `prism_node`.
        #[arg(long, default_value = "local")]
        target: String,
        /// GPU type to request.
        #[arg(long, default_value = "A100-80GB")]
        gpu: String,
        /// Optional maximum budget in USD.
        #[arg(long)]
        budget: Option<f64>,
        /// Optional PRISM node pin. Accepts `--node` or `--node-id`.
        #[arg(long = "node", alias = "node-id")]
        node_id: Option<String>,
        /// Environment variables injected into the deployment container.
        #[arg(long = "env", value_name = "KEY=VALUE")]
        env_vars: Vec<String>,
        /// Service port exposed by the deployed container.
        #[arg(long, default_value_t = 8080)]
        port: u16,
        /// Health-check path on the deployed service.
        #[arg(long, default_value = "/health")]
        health_path: String,
        /// Output raw JSON instead of a concise summary.
        #[arg(long)]
        json: bool,
    },
    /// List deployments visible to the current auth context.
    List {
        /// Optional status filter such as `running` or `stopped`.
        #[arg(long)]
        status: Option<String>,
        /// Output raw JSON instead of a concise summary.
        #[arg(long)]
        json: bool,
    },
    /// Show deployment details for one deployment ID.
    Status {
        id: String,
        /// Output raw JSON instead of a concise summary.
        #[arg(long)]
        json: bool,
    },
    /// Stop a deployment by ID.
    Stop {
        id: String,
        /// Output raw JSON instead of a concise summary.
        #[arg(long)]
        json: bool,
    },
    /// Force a deployment health check.
    Health {
        id: String,
        /// Output raw JSON instead of a concise summary.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum ModelsCommands {
    /// List hosted models available to the active MARC27 project.
    List {
        /// Filter by provider such as `anthropic`, `openai`, `google`, or `openrouter`.
        #[arg(long)]
        provider: Option<String>,
        /// Output raw JSON instead of a concise summary.
        #[arg(long)]
        json: bool,
    },
    /// Search hosted models client-side by ID, display name, or provider.
    Search {
        query: String,
        /// Optional provider filter applied before the text search.
        #[arg(long)]
        provider: Option<String>,
        /// Output raw JSON instead of a concise summary.
        #[arg(long)]
        json: bool,
    },
    /// Show one hosted model by exact model ID.
    Info {
        model_id: String,
        /// Output raw JSON instead of a concise summary.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum DiscourseCommands {
    /// Create a discourse spec from a YAML file.
    Create {
        /// YAML spec file to upload.
        yaml_file: PathBuf,
        /// Optional slug override. Defaults to the YAML file stem.
        #[arg(long)]
        slug: Option<String>,
        /// Output raw JSON instead of a concise summary.
        #[arg(long)]
        json: bool,
    },
    /// List discourse specs for the current user.
    List {
        /// Output raw JSON instead of a concise summary.
        #[arg(long)]
        json: bool,
    },
    /// Show one discourse spec by UUID.
    Show {
        spec_id: String,
        /// Output raw JSON instead of a concise summary.
        #[arg(long)]
        json: bool,
    },
    /// Run a discourse spec and stream or collect its events.
    Run {
        spec_id: String,
        /// Parameter bindings forwarded to the discourse workflow.
        #[arg(long = "param", value_name = "KEY=VALUE")]
        params: Vec<String>,
        /// Output collected events as JSON instead of a live text stream.
        #[arg(long)]
        json: bool,
    },
    /// Inspect one discourse instance by UUID.
    Status {
        instance_id: String,
        /// Output raw JSON instead of a concise summary.
        #[arg(long)]
        json: bool,
    },
    /// List all turns for one discourse instance.
    Turns {
        instance_id: String,
        /// Output raw JSON instead of a concise summary.
        #[arg(long)]
        json: bool,
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
    let project_root = cli.project_root.clone();
    let endpoints = PlatformEndpoints::from_env();
    let paths = PrismPaths::discover()?;

    // Resolve Python: if user passed an explicit --python, honour it;
    // otherwise manage a venv under ~/.prism/venv/ automatically.
    let python = if cli.python.as_os_str() != "python3" {
        cli.python.clone()
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let prism_dir = PathBuf::from(&home).join(".prism");
        ensure_venv(&prism_dir, &project_root).await?
    };

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
            project_root: backend_pr,
            python: backend_py,
        } => {
            use prism_ingest::LlmConfig;

            // Load from prism.toml [llm] section, env vars as overrides
            let node_config = prism_core::config::NodeConfig::load(Some(&backend_pr));
            let cfg_llm = &node_config.llm;

            let api_key = std::env::var("LLM_API_KEY")
                .or_else(|_| std::env::var("MARC27_TOKEN"))
                .or_else(|_| std::env::var("ANTHROPIC_API_KEY"))
                .or_else(|_| std::env::var("OPENAI_API_KEY"))
                .ok()
                .or_else(|| cfg_llm.resolve_api_key())
                .or_else(|| {
                    paths
                        .load_cli_state()
                        .ok()
                        .and_then(|s| s.credentials)
                        .map(|c| c.access_token)
                });

            let llm_config = LlmConfig {
                base_url: std::env::var("LLM_BASE_URL").unwrap_or_else(|_| cfg_llm.url.clone()),
                model: std::env::var("LLM_MODEL").unwrap_or_else(|_| {
                    cfg_llm
                        .model
                        .clone()
                        .unwrap_or_else(|| "claude-sonnet-4-6".to_string())
                }),
                api_key,
                embedding_model: cfg_llm.embedding_model.clone(),
                ..Default::default()
            };

            let tool_server = prism_python_bridge::ToolServer {
                python_bin: backend_py,
                project_root: backend_pr,
                env: Default::default(),
            };

            prism_agent::protocol::run_server(llm_config, tool_server).await?;
        }
        Commands::Tools => {
            let server = ToolServer {
                python_bin: python.clone(),
                project_root: project_root.clone(),
                env: Default::default(),
            };
            let mut handle = server.spawn().await?;
            let resp = handle.list_tools().await?;
            let mut tools = prism_agent::tool_catalog::ToolCatalog::from_tool_server_json(&resp);
            tools.extend(prism_agent::command_tools::command_tools());

            let mut rows = tools
                .iter()
                .map(|tool| {
                    (
                        tool.name.clone(),
                        tool.description.clone(),
                        tool.permission_mode.as_str().to_string(),
                        tool.requires_approval,
                    )
                })
                .collect::<Vec<_>>();
            rows.sort_by(|a, b| a.0.cmp(&b.0));

            for (name, desc, permission_mode, requires_approval) in &rows {
                let approval = if *requires_approval {
                    "approval required"
                } else {
                    "no approval"
                };
                println!(
                    "  {:<30} {:<16} {:<18} {}",
                    name, permission_mode, approval, desc
                );
            }
            println!("\n{} tools available", rows.len());
            handle.shutdown().await?;
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
                kafka_brokers,
                with_spark,
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
                    if let Some(ref brokers) = kafka_brokers {
                        cmd.args(["--kafka-brokers", brokers]);
                    }
                    if with_spark {
                        cmd.arg("--with-spark");
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
                if !no_services {
                    let mut svc_config = prism_orch::ServiceConfig::default();
                    if external_neo4j.is_some() || node_config.services.neo4j_uri.is_some() {
                        svc_config.neo4j = None;
                    }
                    if external_qdrant.is_some() || node_config.services.qdrant_uri.is_some() {
                        svc_config.vector_db = None;
                    }
                    if with_kafka {
                        svc_config.kafka = Some(prism_orch::services::KafkaConfig::default());
                    }
                    if with_spark {
                        svc_config.spark = Some(prism_orch::services::SparkConfig::default());
                    }

                    let wants_managed_services = svc_config.neo4j.is_some()
                        || svc_config.vector_db.is_some()
                        || svc_config.kafka.is_some()
                        || svc_config.spark.is_some();

                    if wants_managed_services {
                        println!("\n  PRISM v{}", env!("CARGO_PKG_VERSION"));
                        if offline {
                            println!("  (OFFLINE MODE)");
                        }
                        println!("  Node: {node_name}\n");
                        println!("  Starting services...");

                        match prism_orch::DockerOrchestrator::new() {
                            Ok(orch) => {
                                use prism_orch::ServiceOrchestrator;
                                match orch.start_all(&svc_config).await {
                                    Ok(handles) => {
                                        for h in &handles.services {
                                            let mark = if h.healthy { "\u{2713}" } else { "~" };
                                            println!(
                                                "  {mark} {:<12} localhost:{}",
                                                h.name, h.port
                                            );
                                        }
                                        service_handles = Some(handles);
                                    }
                                    Err(e) => {
                                        eprintln!(
                                            "  Warning: Failed to start managed services: {e}"
                                        );
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
                }

                // ── V2: Start the embedded dashboard server ──
                let mut server_node_state = prism_server::NodeState::new(node_name.clone());

                // Wire core databases (RBAC + audit)
                let state_dir = &paths.state_dir;
                std::fs::create_dir_all(state_dir)?;
                server_node_state.audit_db_path = Some(state_dir.join("audit.db"));
                server_node_state.rbac_db_path = Some(state_dir.join("rbac.db"));
                server_node_state.session_db_path = Some(state_dir.join("sessions.db"));
                server_node_state.subscriptions = std::sync::Arc::new(std::sync::RwLock::new(
                    prism_mesh::subscription::SubscriptionManager::open(
                        &state_dir.join("subscriptions.db"),
                    )
                    .unwrap_or_else(|e| {
                        eprintln!(
                            "  Warning: Failed to open subscription store: {e} (using in-memory state)"
                        );
                        prism_mesh::subscription::SubscriptionManager::new()
                    }),
                ));

                // Scan for tools
                let tools_dir = paths.config_dir.join("tools");
                if tools_dir.is_dir() {
                    if let Ok(mut reg) = server_node_state.tool_registry.write() {
                        let _ = reg.scan_directory(&tools_dir);
                    }
                }

                // Wire backend configs — CLI flags > prism.toml > defaults
                let managed_neo4j_running = service_handles.as_ref().is_some_and(|handles| {
                    handles.services.iter().any(|handle| handle.name == "neo4j")
                });
                let managed_qdrant_running = service_handles.as_ref().is_some_and(|handles| {
                    handles
                        .services
                        .iter()
                        .any(|handle| handle.name == "qdrant")
                });

                if external_neo4j.is_some()
                    || node_config.services.neo4j_uri.is_some()
                    || managed_neo4j_running
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
                    || managed_qdrant_running
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
                    let base_url = node_config.indexer.uri.clone().unwrap_or_else(|| {
                        match node_config.indexer.mode.as_str() {
                            "platform" | "marc27" | "external" => {
                                node_config.platform.url.clone() + "/llm"
                            }
                            _ => "http://localhost:8080".into(), // llama.cpp default
                        }
                    });
                    server_node_state.llm = Some(prism_ingest::LlmConfig {
                        base_url,
                        model: node_config
                            .indexer
                            .model
                            .clone()
                            .unwrap_or_else(|| "gemma-3-27b".into()),
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
                // Resolve Kafka brokers: explicit flag > implicit from --with-kafka
                let resolved_kafka_brokers = kafka_brokers.clone().or_else(|| {
                    if with_kafka {
                        Some("127.0.0.1:9092".to_string())
                    } else {
                        None
                    }
                });

                let mesh_config = prism_mesh::MeshConfig {
                    node_name: daemon_options.name.clone(),
                    publish_port: dashboard_port,
                    discovery: vec![prism_mesh::DiscoveryMethod::Mdns],
                    kafka_brokers: resolved_kafka_brokers.clone(),
                };
                let mesh_handle = prism_mesh::init_mesh(mesh_config)?;
                let mesh_node_id = mesh_handle.node_id();
                let mesh_peers_shared = mesh_handle.peers_shared();
                // Update server state so REST API reports mesh as online
                *server_state.mesh.write().unwrap_or_else(|e| e.into_inner()) = mesh_handle.clone();
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
                // Initialize federated query client for cross-mesh searches
                let _ = server_state
                    .federation
                    .set(prism_mesh::federation::FederatedQuery::default());

                if broadcast {
                    println!("  \u{2713} Mesh: broadcasting (mDNS + platform discovery)");
                } else {
                    println!("  \u{2713} Mesh: passive discovery (use --broadcast to advertise)");
                }

                // ── Kafka pub/sub + sync handler (if brokers configured) ──
                let _kafka_tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
                if let Some(ref brokers) = resolved_kafka_brokers {
                    let kafka_cfg = prism_mesh::kafka::KafkaConfig {
                        brokers: brokers.clone(),
                        topic_prefix: "prism.mesh".into(),
                        group_id: format!(
                            "prism-{}",
                            mesh_node_id
                                .map(|id| id.to_string())
                                .unwrap_or_else(|| "unknown".into())
                        ),
                    };

                    match prism_mesh::kafka::MeshKafkaConsumer::new(&kafka_cfg) {
                        Ok(consumer) => {
                            let (tx, rx) = tokio::sync::mpsc::channel(256);

                            // Use shared state extracted from mesh handle before it was moved
                            let peers_arc = mesh_peers_shared.clone().unwrap_or_else(|| {
                                std::sync::Arc::new(std::sync::RwLock::new(Vec::new()))
                            });
                            let our_node_id = mesh_node_id.unwrap_or_else(uuid::Uuid::nil);
                            let subscriptions = server_state.subscriptions.clone();

                            // Build sync config from Neo4j settings if available
                            let sync_config = server_state.neo4j.as_ref().map(|neo4j| {
                                prism_mesh::sync::SyncConfig {
                                    neo4j_url: neo4j.base_url.clone(),
                                    neo4j_user: neo4j.username.clone(),
                                    neo4j_pass: neo4j.password.clone(),
                                }
                            });

                            // Spawn consumer loop
                            tokio::spawn(async move {
                                if let Err(e) = consumer.run(tx).await {
                                    tracing::error!(error = %e, "Kafka consumer loop exited with error");
                                }
                            });

                            // Spawn sync handler
                            tokio::spawn(async move {
                                prism_mesh::sync::run_sync_handler(
                                    rx,
                                    peers_arc,
                                    subscriptions,
                                    our_node_id,
                                    sync_config,
                                )
                                .await;
                            });

                            println!("  \u{2713} Kafka: pub/sub active ({brokers})");
                        }
                        Err(e) => {
                            eprintln!("  Warning: Kafka consumer failed to start: {e}");
                            eprintln!("  (Mesh will work via mDNS only, without Kafka pub/sub.)");
                        }
                    }

                    match prism_mesh::kafka::MeshKafkaProducer::new(&kafka_cfg) {
                        Ok(producer) => {
                            let producer = std::sync::Arc::new(producer);
                            // Store producer in server state so mesh handlers can publish
                            let _ = server_state.kafka_producer.set(producer.clone());
                            if let Some(nid) = mesh_node_id {
                                let _ = server_state.node_id.set(nid);
                            }
                            tracing::info!("Kafka producer ready and wired to mesh handlers");

                            // Announce this node on the mesh via Kafka
                            if let Some(nid) = mesh_node_id {
                                let announce_producer = producer.clone();
                                let node_name = daemon_options.name.clone();
                                tokio::spawn(async move {
                                    let msg = prism_mesh::protocol::MeshMessage::Announce {
                                        node_id: nid,
                                        name: node_name,
                                        address: "127.0.0.1".to_string(),
                                        port: dashboard_port,
                                        capabilities: vec![],
                                    };
                                    if let Err(e) = announce_producer.publish(&msg).await {
                                        tracing::warn!(error = %e, "failed to announce node via Kafka");
                                    }
                                });
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "Kafka producer failed to initialize");
                        }
                    }
                }

                // Run daemon until Ctrl+C — on shutdown, stop Docker containers
                let result =
                    prism_node::daemon::run_daemon(&endpoints, &paths, daemon_options).await;

                // Send Goodbye via Kafka before shutting down
                if let (Some(producer), Some(&nid)) = (
                    server_state.kafka_producer.get(),
                    server_state.node_id.get(),
                ) {
                    let msg = prism_mesh::protocol::MeshMessage::Goodbye { node_id: nid };
                    if let Err(e) = producer.publish(&msg).await {
                        tracing::warn!(error = %e, "failed to send goodbye via Kafka");
                    }
                }

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
            corpus,
            model,
            llm_url,
            api_key,
            neo4j_url,
            neo4j_user,
            neo4j_pass,
            qdrant_url,
            schema_only,
            status,
            watch,
            runtime_url,
            json,
            mapping,
        } => {
            if status {
                handle_ingest_status(corpus.as_deref(), json).await?;
            } else if watch {
                let path = path.as_deref().ok_or_else(|| {
                    anyhow!("`prism ingest --watch` requires a path or directory.")
                })?;
                handle_ingest_watch(
                    &path,
                    &project_root,
                    model.as_deref(),
                    llm_url.as_deref(),
                    api_key.as_deref(),
                    &neo4j_url,
                    &neo4j_user,
                    &neo4j_pass,
                    &qdrant_url,
                    schema_only,
                    &runtime_url,
                    corpus.as_deref(),
                    json,
                    mapping.as_deref(),
                )
                .await?;
            } else {
                let path = path.as_deref().ok_or_else(|| {
                    anyhow!("`prism ingest` requires a file or directory unless `--status` is set.")
                })?;
                handle_ingest(
                    &path,
                    &project_root,
                    model.as_deref(),
                    llm_url.as_deref(),
                    api_key.as_deref(),
                    &neo4j_url,
                    &neo4j_user,
                    &neo4j_pass,
                    &qdrant_url,
                    schema_only,
                    &runtime_url,
                    corpus.as_deref(),
                    json,
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
                handle_federated_query(&text, &dashboard_url, &paths).await?;
            } else {
                let llm_cfg = build_llm_config(
                    &project_root,
                    llm_url.as_deref(),
                    model.as_deref(),
                    api_key.as_deref(),
                )?;
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
            ssh,
            ssh_key,
            ssh_port,
            k8s_context,
            k8s_namespace,
            slurm,
            slurm_partition,
            json,
        } => {
            handle_run(
                &name,
                &image,
                &input,
                &backend,
                &platform_url,
                ssh.as_deref(),
                &ssh_key,
                ssh_port,
                k8s_context.as_deref(),
                &k8s_namespace,
                slurm.as_deref(),
                &slurm_partition,
                json,
            )
            .await?;
        }
        Commands::JobStatus { job_id } => {
            handle_job_status(&job_id).await?;
        }
        Commands::Mesh { command } => {
            handle_mesh_command(command, &paths).await?;
        }
        Commands::Report {
            description,
            log_file,
            no_github,
        } => {
            handle_report(
                &paths,
                &endpoints,
                &description,
                log_file.as_deref(),
                no_github,
            )
            .await?;
        }
        Commands::Marketplace { command } => {
            use prism_client::marketplace::MarketplaceClient;

            let state = paths.load_cli_state()?;
            let token = state.credentials.as_ref().map(|c| c.access_token.clone());
            let platform = if let Some(t) = &token {
                PlatformClient::new(&endpoints.api_base).with_token(t)
            } else {
                PlatformClient::new(&endpoints.api_base)
            };
            let marketplace = MarketplaceClient::new(&platform);

            match command {
                MarketplaceCommands::Search { query } => {
                    let tools = marketplace.list_tools(query.as_deref()).await?;
                    if tools.is_empty() {
                        println!("No results found.");
                    } else {
                        println!("Marketplace tools:\n");
                        for t in &tools {
                            println!("  {:<30} {} (by {})", t.name, t.description, t.author);
                            if t.install_count > 0 {
                                println!("  {:<30} {} installs", "", t.install_count);
                            }
                        }
                        println!(
                            "\n{} tools found. Install with: prism marketplace install <name>",
                            tools.len()
                        );
                    }
                }
                MarketplaceCommands::Install { name, workflow } => {
                    let url = marketplace.install_url(&name).await?;
                    let client = reqwest::Client::new();
                    let resp = client.get(&url).send().await?;
                    let content = resp.text().await?;

                    let home = std::env::var("HOME").unwrap_or_default();
                    let dest = if workflow {
                        let dir = PathBuf::from(&home).join(".prism/workflows");
                        std::fs::create_dir_all(&dir)?;
                        dir.join(format!("{name}.yaml"))
                    } else {
                        let dir = PathBuf::from(&home).join(".prism/tools");
                        std::fs::create_dir_all(&dir)?;
                        dir.join(format!("{name}.py"))
                    };

                    std::fs::write(&dest, &content)?;
                    let kind = if workflow { "workflow" } else { "tool" };
                    println!("Installed {kind} '{name}' to {}", dest.display());
                    println!("It will be auto-discovered on next prism run.");
                }
                MarketplaceCommands::Info { name } => {
                    let tool = marketplace.get_tool(&name).await?;
                    println!("Name:        {}", tool.name);
                    println!("Version:     {}", tool.version);
                    println!("Author:      {}", tool.author);
                    println!("Description: {}", tool.description);
                    println!("Installs:    {}", tool.install_count);
                    if let Some(url) = &tool.download_url {
                        println!("URL:         {url}");
                    }
                }
            }
        }
        Commands::Research { query, depth, json } => {
            let (api_base, auth) = resolve_agent_auth()?;
            let client = reqwest::Client::builder()
                // Research can take longer than a plain graph query because it may
                // run an iterative loop before producing the final answer.
                .timeout(std::time::Duration::from_secs(120))
                .build()?;
            let resp = auth
                .apply(client.post(format!("{api_base}/knowledge/research/query")))
                // Keep smoke tests cheap by always making depth explicit.
                .json(&serde_json::json!({ "query": query, "depth": depth, "stream": false }))
                .send()
                .await?
                .error_for_status()?
                .text()
                .await?;
            let resp = parse_research_response_body(&resp)?;

            if json {
                println!("{}", serde_json::to_string_pretty(&resp)?);
            } else {
                if let Some(answer) = resp.get("answer").and_then(|a| a.as_str()) {
                    println!("{answer}");
                }
                if let Some(sources) = resp.get("sources").and_then(|s| s.as_array()) {
                    if !sources.is_empty() {
                        println!("\nSources:");
                        for src in sources {
                            if let Some(title) = src.get("title").and_then(|t| t.as_str()) {
                                let url = src.get("url").and_then(|u| u.as_str()).unwrap_or("");
                                println!("  - {title} {url}");
                            }
                        }
                    }
                }
                if resp.get("answer").is_none() {
                    // Raw response if no structured answer
                    println!("{}", serde_json::to_string_pretty(&resp)?);
                }
            }
        }
        Commands::Deploy { command } => {
            handle_deploy_command(command).await?;
        }
        Commands::Models { command } => {
            handle_models_command(&paths, command).await?;
        }
        Commands::Discourse { command } => {
            handle_discourse_command(command).await?;
        }
        Commands::Publish {
            path,
            to,
            repo,
            private,
            json,
        } => {
            let artifact_path = std::path::Path::new(&path);
            if !artifact_path.exists() {
                anyhow::bail!("Path not found: {path}");
            }

            match to.as_str() {
                "huggingface" | "hf" => {
                    let repo_name = repo.unwrap_or_else(|| {
                        artifact_path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("my-model")
                            .to_string()
                    });
                    let mut create_args = vec![
                        "repo".to_string(),
                        "create".to_string(),
                        repo_name.clone(),
                        "--type".to_string(),
                        "model".to_string(),
                    ];
                    if private {
                        create_args.push("--private".to_string());
                    }
                    let create = std::process::Command::new("hf").args(&create_args).output();
                    match create {
                        Ok(output) if output.status.success() => {
                            let upload = std::process::Command::new("hf")
                                .args(["upload", &repo_name, &path])
                                .output();
                            match upload {
                                Ok(upload_output) if upload_output.status.success() => {
                                    let published_url =
                                        format!("https://huggingface.co/{repo_name}");
                                    if json {
                                        println!(
                                            "{}",
                                            serde_json::to_string_pretty(&serde_json::json!({
                                                "target": "huggingface",
                                                "path": path,
                                                "repo": repo_name,
                                                "private": private,
                                                "published_url": published_url,
                                                "created": true,
                                                "uploaded": true,
                                            }))?
                                        );
                                    } else {
                                        println!("Publishing to HuggingFace: {repo_name}");
                                        println!("Repository created. Uploading...");
                                        println!("Published: {published_url}");
                                    }
                                }
                                Ok(upload_output) => {
                                    let stderr = String::from_utf8_lossy(&upload_output.stderr)
                                        .trim()
                                        .to_string();
                                    if json {
                                        anyhow::bail!(
                                            "hf upload failed{}",
                                            if stderr.is_empty() {
                                                String::new()
                                            } else {
                                                format!(": {stderr}")
                                            }
                                        );
                                    }
                                    eprintln!("Upload failed. Try: hf upload {repo_name} {path}");
                                    if !stderr.is_empty() {
                                        eprintln!("{stderr}");
                                    }
                                }
                                Err(error) => {
                                    if json {
                                        return Err(error.into());
                                    }
                                    eprintln!("Upload failed. Try: hf upload {repo_name} {path}");
                                }
                            }
                        }
                        Ok(output) => {
                            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                            if json {
                                anyhow::bail!(
                                    "hf repo create failed{}",
                                    if stderr.is_empty() {
                                        String::new()
                                    } else {
                                        format!(": {stderr}")
                                    }
                                );
                            }
                            eprintln!(
                                "HuggingFace CLI (hf) not found or failed. Install: pip install huggingface_hub"
                            );
                            if !stderr.is_empty() {
                                eprintln!("{stderr}");
                            }
                            eprintln!("Then: hf login && prism publish {path} --to hf");
                        }
                        Err(error) => {
                            if json {
                                return Err(error.into());
                            }
                            eprintln!(
                                "HuggingFace CLI (hf) not found or failed. Install: pip install huggingface_hub"
                            );
                            eprintln!("Then: hf login && prism publish {path} --to hf");
                        }
                    }
                }
                "marc27" | "platform" => {
                    let state = paths.load_cli_state()?;
                    let token = state
                        .credentials
                        .as_ref()
                        .map(|c| c.access_token.clone())
                        .ok_or_else(|| {
                            anyhow::anyhow!("Not logged in. Run `prism login` first.")
                        })?;
                    let platform = PlatformClient::new(&endpoints.api_base).with_token(&token);

                    println!("Publishing to MARC27 marketplace...");
                    let name = repo.unwrap_or_else(|| {
                        artifact_path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("artifact")
                            .to_string()
                    });
                    let resp: serde_json::Value = platform
                        .post(
                            "/marketplace",
                            &serde_json::json!({
                                "name": name,
                                "path": path,
                                "private": private,
                            }),
                        )
                        .await?;
                    if json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "target": "marc27",
                                "path": path,
                                "repo": name,
                                "private": private,
                                "result": resp,
                            }))?
                        );
                    } else {
                        println!("{}", serde_json::to_string_pretty(&resp)?);
                    }
                }
                other => {
                    eprintln!("Unknown target: {other}. Use 'huggingface' or 'marc27'.");
                    std::process::exit(1);
                }
            }
        }
        Commands::Configure {
            llm_provider,
            url,
            model,
            embedding_model,
            show,
        } => {
            handle_configure(llm_provider, url, model, embedding_model, show)?;
        }
        Commands::External(args) => {
            if try_run_workflow_alias(&project_root, &args).await? {
                return Ok(());
            }
            // Python CLI has been removed. Show help for unknown commands.
            let cmd = args.first().map(|s| s.as_str()).unwrap_or("?");
            eprintln!("Unknown command: {cmd}");
            eprintln!("Run 'prism --help' for available commands.");
            std::process::exit(1);
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

async fn handle_mesh_command(
    command: MeshCommands,
    paths: &prism_runtime::PrismPaths,
) -> Result<()> {
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
                kafka_brokers: None,
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
            let session_token = create_dashboard_session(&dashboard_url, paths).await?;
            let client = reqwest::Client::new();
            let resp = client
                .post(&url)
                .header("X-Session-Token", session_token)
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
            let session_token = create_dashboard_session(&dashboard_url, paths).await?;
            let client = reqwest::Client::new();
            let resp = client
                .post(&url)
                .header("X-Session-Token", session_token)
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
            let session_token = create_dashboard_session(&dashboard_url, paths).await?;
            let client = reqwest::Client::new();
            let resp = client
                .delete(&url)
                .header("X-Session-Token", session_token)
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

/// Handle `prism configure` — read/write LLM config in ~/.prism/prism.toml.
fn handle_configure(
    provider: Option<String>,
    url: Option<String>,
    model: Option<String>,
    embedding_model: Option<String>,
    show: bool,
) -> Result<()> {
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME env var not set"))?;
    let config_dir = std::path::PathBuf::from(home).join(".prism");
    let config_path = config_dir.join("prism.toml");

    // Load current config (or defaults)
    let mut node_config = if config_path.exists() {
        prism_core::config::NodeConfig::from_file(&config_path).unwrap_or_default()
    } else {
        prism_core::config::NodeConfig::default()
    };

    if show {
        let llm = &node_config.llm;
        println!("LLM configuration (from {})", config_path.display());
        println!("  provider:        {}", llm.provider);
        println!("  url:             {}", llm.url);
        println!(
            "  model:           {}",
            llm.model.as_deref().unwrap_or("(not set)")
        );
        println!(
            "  embedding_model: {}",
            llm.embedding_model.as_deref().unwrap_or("(uses model)")
        );
        println!("  api_key_env:     {}", llm.api_key_env);
        println!("  timeout_secs:    {}", llm.timeout_secs);
        if let Some(key) = llm.resolve_api_key() {
            let masked = if key.len() > 8 {
                format!("{}…{}", &key[..4], &key[key.len() - 4..])
            } else {
                "***".to_string()
            };
            println!("  api_key:         {masked} (resolved)");
        } else {
            println!("  api_key:         (none)");
        }
        return Ok(());
    }

    // Apply updates
    let mut changed = false;
    if let Some(p) = provider {
        node_config.llm.provider = p;
        changed = true;
    }
    if let Some(u) = url {
        node_config.llm.url = u;
        changed = true;
    }
    if let Some(m) = model {
        node_config.llm.model = Some(m);
        changed = true;
    }
    if let Some(e) = embedding_model {
        node_config.llm.embedding_model = Some(e);
        changed = true;
    }

    if !changed {
        eprintln!(
            "No changes specified. Use --url, --model, --embedding-model, or --llm-provider."
        );
        eprintln!("Run `prism configure --show` to see current config.");
        return Ok(());
    }

    // Write back
    std::fs::create_dir_all(&config_dir)?;
    let toml_str = toml::to_string_pretty(&node_config)?;
    std::fs::write(&config_path, toml_str)?;

    println!("Wrote config to {}", config_path.display());
    println!("LLM URL:   {}", node_config.llm.url);
    if let Some(m) = &node_config.llm.model {
        println!("LLM Model: {m}");
    }
    Ok(())
}

/// Build LlmConfig from prism.toml with optional CLI overrides.
///
/// Precedence: CLI flags > prism.toml ([llm] section) > built-in defaults.
/// Returns a helpful error if no model is configured anywhere.
fn build_llm_config(
    project_root: &Path,
    url_override: Option<&str>,
    model_override: Option<&str>,
    api_key_override: Option<&str>,
) -> Result<prism_ingest::LlmConfig> {
    let node_config = prism_core::config::NodeConfig::load(Some(project_root));
    let llm = &node_config.llm;

    let base_url = url_override
        .map(str::to_string)
        .unwrap_or_else(|| llm.url.clone());

    let model = match model_override {
        Some(m) => m.to_string(),
        None => llm.resolve_model()?,
    };

    let api_key = api_key_override
        .map(str::to_string)
        .or_else(|| llm.resolve_api_key());

    Ok(prism_ingest::LlmConfig {
        base_url,
        model,
        api_key,
        embedding_model: llm.embedding_model.clone(),
        max_sample_rows: 10,
        timeout_secs: llm.timeout_secs,
    })
}

// ── prism ingest ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IngestBackend {
    LocalTabular,
    PlatformText,
}

fn ingest_backend(path: &Path) -> Option<IngestBackend> {
    let ext = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    match ext.as_str() {
        "csv" | "tsv" | "parquet" | "pq" => Some(IngestBackend::LocalTabular),
        "pdf" | "json" | "jsonl" | "owl" | "cif" | "txt" | "md" => {
            Some(IngestBackend::PlatformText)
        }
        _ => None,
    }
}

fn ingest_format(path: &Path) -> String {
    path.extension()
        .and_then(|value| value.to_str())
        .unwrap_or("unknown")
        .to_ascii_lowercase()
}

fn collect_ingest_paths(root: &Path) -> Result<Vec<PathBuf>> {
    if root.is_file() {
        return Ok(vec![root.to_path_buf()]);
    }
    if !root.is_dir() {
        bail!("File or directory not found: {}", root.display());
    }

    // Recurse explicitly so `prism ingest ./data` works without forcing users
    // to shell out through `find`/`rg` just to hand PRISM a batch of files.
    let mut stack = vec![root.to_path_buf()];
    let mut files = Vec::new();

    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)
            .with_context(|| format!("failed to read directory {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if is_ingestable(&path) {
                files.push(path);
            }
        }
    }

    files.sort();
    Ok(files)
}

fn split_text_for_platform_ingest(text: &str) -> Vec<String> {
    const MAX_CHARS: usize = 48_000;
    const SPLIT_WINDOW: usize = 2_000;

    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    // The platform extractor truncates around 60K chars, so PRISM keeps each
    // chunk below that ceiling and prefers paragraph/newline boundaries.
    let chars: Vec<char> = trimmed.chars().collect();
    let mut chunks = Vec::new();
    let mut start = 0usize;

    while start < chars.len() {
        let mut end = (start + MAX_CHARS).min(chars.len());
        if end < chars.len() {
            let window_start = end.saturating_sub(SPLIT_WINDOW);
            let window: String = chars[window_start..end].iter().collect();
            if let Some(split_idx) = window.rfind("\n\n").or_else(|| window.rfind('\n')) {
                let split_chars = window[..split_idx].chars().count();
                if split_chars > 0 {
                    end = window_start + split_chars;
                }
            }
        }

        let chunk: String = chars[start..end].iter().collect();
        let chunk = chunk.trim();
        if !chunk.is_empty() {
            chunks.push(chunk.to_string());
        }

        start = end;
        while start < chars.len() && chars[start].is_whitespace() {
            start += 1;
        }
    }

    chunks
}

async fn extract_pdf_text_with_runtime(
    runtime_url: &str,
    path: &Path,
) -> Result<serde_json::Value> {
    let bytes =
        std::fs::read(path).with_context(|| format!("failed to read PDF {}", path.display()))?;
    let request = serde_json::json!({
        "model": "pymupdf",
        "input": {
            "type": "pdf",
            "data": base64::engine::general_purpose::STANDARD.encode(bytes),
        },
        "options": {
            "output_format": "json",
            "extract_tables": true,
            "extract_figures": false,
        }
    });

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(180))
        .build()?;

    let response = client
        .post(format!("{}/run", runtime_url.trim_end_matches('/')))
        .json(&request)
        .send()
        .await
        .with_context(|| format!("runtime PDF extraction failed for {}", path.display()))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!("runtime /run failed ({status}): {body}");
    }

    let value: serde_json::Value = response.json().await?;
    Ok(value)
}

async fn extract_platform_ingest_text(
    path: &Path,
    runtime_url: &str,
) -> Result<(String, Option<u64>, Option<String>)> {
    match ingest_backend(path) {
        Some(IngestBackend::PlatformText) if ingest_format(path) == "pdf" => {
            let response = extract_pdf_text_with_runtime(runtime_url, path).await?;
            let output = response
                .get("output")
                .ok_or_else(|| anyhow!("runtime response missing `output`"))?;
            let text = output
                .get("text")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow!("runtime PDF extraction returned no text"))?
                .to_string();
            let pages = output.get("pages").and_then(|value| value.as_u64());
            let warning = output
                .get("warning")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            Ok((text, pages, warning))
        }
        Some(IngestBackend::PlatformText) => {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("failed to read text source {}", path.display()))?;
            Ok((text, None, None))
        }
        _ => bail!("Unsupported platform ingest format: {}", path.display()),
    }
}

async fn submit_platform_ingest_chunk(
    chunk: &str,
    corpus: Option<&str>,
    model: Option<&str>,
) -> Result<serde_json::Value> {
    let (api_base, auth) = resolve_agent_auth()?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;

    let mut body = serde_json::json!({
        "source": { "type": "query", "query": chunk },
        "mode": "full",
    });
    if let Some(corpus) = corpus {
        body["corpus_slug"] = serde_json::Value::String(corpus.to_string());
    }
    if let Some(model) = model {
        body["llm_model"] = serde_json::Value::String(model.to_string());
    }

    let response = auth
        .apply(client.post(format!("{api_base}/knowledge/ingest-job")))
        .json(&body)
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!("platform ingest job submission failed ({status}): {body}");
    }

    Ok(response.json().await?)
}

async fn run_local_ingest_file(
    path: &Path,
    project_root: &Path,
    model: Option<&str>,
    llm_url: Option<&str>,
    api_key: Option<&str>,
    neo4j_url: &str,
    neo4j_user: &str,
    neo4j_pass: &str,
    qdrant_url: &str,
    schema_only: bool,
    mapping_path: Option<&Path>,
) -> Result<serde_json::Value> {
    use prism_ingest::pipeline::{IngestPipeline, PipelineConfig};
    use prism_ingest::{Neo4jConfig, QdrantConfig};

    let llm_cfg = build_llm_config(project_root, llm_url, model, api_key)?;
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
            llm: Some(llm_cfg),
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

    Ok(serde_json::json!({
        "backend": "local_tabular",
        "path": path.display().to_string(),
        "format": ingest_format(path),
        "schema_only": schema_only,
        "result": result,
    }))
}

async fn run_platform_ingest_file(
    path: &Path,
    runtime_url: &str,
    corpus: Option<&str>,
    model: Option<&str>,
    schema_only: bool,
    mapping_path: Option<&Path>,
) -> Result<serde_json::Value> {
    let (text, pages, warning) = extract_platform_ingest_text(path, runtime_url).await?;
    let chunks = split_text_for_platform_ingest(&text);

    if chunks.is_empty() {
        bail!("No ingestable text found in {}", path.display());
    }

    if mapping_path.is_some() {
        eprintln!(
            "Warning: --mapping is only applied to the local tabular ingest pipeline and is ignored for {}.",
            path.display()
        );
    }

    let mut jobs = Vec::new();
    if !schema_only {
        for (index, chunk) in chunks.iter().enumerate() {
            let mut job = submit_platform_ingest_chunk(chunk, corpus, model).await?;
            if let Some(obj) = job.as_object_mut() {
                obj.insert("chunk_index".to_string(), serde_json::json!(index));
                obj.insert(
                    "chunk_chars".to_string(),
                    serde_json::json!(chunk.chars().count()),
                );
            }
            jobs.push(job);
        }
    }

    Ok(serde_json::json!({
        "backend": "platform_text",
        "path": path.display().to_string(),
        "format": ingest_format(path),
        "corpus": corpus,
        "schema_only": schema_only,
        "pages": pages,
        "chars": text.chars().count(),
        "chunk_count": chunks.len(),
        "jobs": jobs,
        "warning": warning,
    }))
}

fn print_ingest_summary(summary: &serde_json::Value) {
    let backend = value_string(summary, &["backend"]).unwrap_or("ingest");
    let path = value_string(summary, &["path"]).unwrap_or("?");

    println!("Ingesting: {path}");

    match backend {
        "local_tabular" => {
            let result = summary.get("result").unwrap_or(summary);
            let column_count = result
                .get("column_count")
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            let row_count = result
                .get("row_count")
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            println!("  Schema: {column_count} columns, {row_count} rows");
            if let Some(columns) = result
                .get("schema")
                .and_then(|value| value.get("columns"))
                .and_then(|value| value.as_array())
            {
                let column_names = columns
                    .iter()
                    .filter_map(|value| value.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                println!("  Columns: {column_names}");
            }
            let warning_count = result
                .get("validation")
                .and_then(|value| value.get("issues"))
                .and_then(|value| value.as_array())
                .map(|value| value.len())
                .unwrap_or(0);
            if warning_count > 0 {
                println!("  Warnings: {warning_count} issues");
            }
            if let Some(entities) = result
                .get("entities")
                .and_then(|value| value.get("entities"))
                .and_then(|value| value.as_array())
            {
                let relationships = result
                    .get("entities")
                    .and_then(|value| value.get("relationships"))
                    .and_then(|value| value.as_array())
                    .map(|value| value.len())
                    .unwrap_or(0);
                println!(
                    "  Entities: {} extracted, {} relationships",
                    entities.len(),
                    relationships
                );
            }
            if let Some(graph) = result.get("graph") {
                let nodes = graph
                    .get("nodes_created")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0);
                let edges = graph
                    .get("edges_created")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0);
                println!("  Graph: {nodes} nodes, {edges} edges written to Neo4j");
            }
            if let Some(embeddings) = result.get("embeddings") {
                let count = embeddings
                    .get("vectors")
                    .and_then(|value| value.as_array())
                    .map(|value| value.len())
                    .unwrap_or(0);
                let dimension = embeddings
                    .get("dimension")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0);
                println!("  Embeddings: {count} vectors (dim={dimension})");
            }
            if summary
                .get("schema_only")
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
            {
                println!("  (schema-only mode — LLM/graph/vector steps skipped)");
            }
        }
        "platform_text" => {
            let chars = summary
                .get("chars")
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            let chunk_count = summary
                .get("chunk_count")
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            println!("  Text: {chars} chars prepared across {chunk_count} chunk(s)");
            if let Some(pages) = summary.get("pages").and_then(|value| value.as_u64()) {
                println!("  Pages: {pages}");
            }
            if let Some(corpus) = summary.get("corpus").and_then(|value| value.as_str()) {
                println!("  Corpus: {corpus}");
            }
            if let Some(warning) = summary.get("warning").and_then(|value| value.as_str()) {
                println!("  Warning: {warning}");
            }
            if summary
                .get("schema_only")
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
            {
                println!("  (schema-only mode — extracted text was prepared but not submitted)");
            } else if let Some(jobs) = summary.get("jobs").and_then(|value| value.as_array()) {
                for job in jobs {
                    let chunk_index = job
                        .get("chunk_index")
                        .and_then(|value| value.as_u64())
                        .unwrap_or(0);
                    let job_id = value_string(job, &["job_id"]).unwrap_or("?");
                    let status = value_string(job, &["status"]).unwrap_or("submitted");
                    println!("  Chunk {chunk_index}: job {job_id} [{status}]");
                }
            }
        }
        _ => {
            println!(
                "{}",
                serde_json::to_string_pretty(summary).unwrap_or_default()
            );
        }
    }

    println!("\n  Done.");
}

async fn fetch_ingest_status(corpus: Option<&str>) -> Result<serde_json::Value> {
    let (api_base, auth) = resolve_agent_auth()?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?;

    let graph_stats: serde_json::Value = auth
        .apply(client.get(format!("{api_base}/knowledge/graph/stats")))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let embedding_stats: serde_json::Value = auth
        .apply(client.get(format!("{api_base}/knowledge/embeddings/stats")))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let jobs: serde_json::Value = auth
        .apply(client.get(format!("{api_base}/knowledge/ingest-jobs")))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let mut summary = serde_json::json!({
        "graph": graph_stats,
        "embeddings": embedding_stats,
        "jobs": jobs,
    });

    if let Some(corpus) = corpus {
        let catalog: serde_json::Value = auth
            .apply(client.get(format!("{api_base}/knowledge/catalog")))
            .query(&[("limit", "200")])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let matches = value_array(&catalog, &[])
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|item| {
                let query = corpus.to_ascii_lowercase();
                [
                    value_string(item, &["slug"]),
                    value_string(item, &["name"]),
                    value_string(item, &["description"]),
                ]
                .into_iter()
                .flatten()
                .any(|value| value.to_ascii_lowercase().contains(&query))
            })
            .collect::<Vec<_>>();

        if let Some(obj) = summary.as_object_mut() {
            obj.insert(
                "corpus".to_string(),
                serde_json::Value::String(corpus.to_string()),
            );
            obj.insert(
                "catalog_matches".to_string(),
                serde_json::Value::Array(matches),
            );
        }
    }

    Ok(summary)
}

async fn handle_ingest_status(corpus: Option<&str>, json_output: bool) -> Result<()> {
    let summary = fetch_ingest_status(corpus).await?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&summary)?);
        return Ok(());
    }

    let graph = summary.get("graph").unwrap_or(&serde_json::Value::Null);
    let embeddings = summary
        .get("embeddings")
        .unwrap_or(&serde_json::Value::Null);
    println!("Ingest status:");
    println!(
        "  Graph: {} nodes, {} edges",
        graph
            .get("nodes")
            .and_then(|value| value.as_u64())
            .unwrap_or(0),
        graph
            .get("edges")
            .and_then(|value| value.as_u64())
            .unwrap_or(0)
    );
    println!(
        "  Embeddings: {}",
        embeddings
            .get("embeddings")
            .and_then(|value| value.as_u64())
            .unwrap_or(0)
    );

    if let Some(corpus) = corpus {
        let matches = summary
            .get("catalog_matches")
            .and_then(|value| value.as_array())
            .map(|value| value.len())
            .unwrap_or(0);
        println!("  Corpus filter: {corpus} ({matches} catalog matches)");
    }

    if let Some(jobs) = summary.get("jobs").and_then(|value| value.as_array()) {
        println!("  Active jobs: {}", jobs.len());
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn handle_ingest(
    path: &Path,
    project_root: &Path,
    model: Option<&str>,
    llm_url: Option<&str>,
    api_key: Option<&str>,
    neo4j_url: &str,
    neo4j_user: &str,
    neo4j_pass: &str,
    qdrant_url: &str,
    schema_only: bool,
    runtime_url: &str,
    corpus: Option<&str>,
    json_output: bool,
    mapping_path: Option<&Path>,
) -> Result<()> {
    let ingest_targets = collect_ingest_paths(path)?;
    if ingest_targets.is_empty() {
        bail!("No ingestable files found under {}", path.display());
    }

    let mut summaries = Vec::new();
    for target in ingest_targets {
        let summary = match ingest_backend(&target) {
            Some(IngestBackend::LocalTabular) => {
                run_local_ingest_file(
                    &target,
                    project_root,
                    model,
                    llm_url,
                    api_key,
                    neo4j_url,
                    neo4j_user,
                    neo4j_pass,
                    qdrant_url,
                    schema_only,
                    mapping_path,
                )
                .await?
            }
            Some(IngestBackend::PlatformText) => {
                run_platform_ingest_file(
                    &target,
                    runtime_url,
                    corpus,
                    model,
                    schema_only,
                    mapping_path,
                )
                .await?
            }
            None => bail!(
                "Unsupported ingest format for {}. Supported: csv, tsv, parquet, pq, pdf, json, jsonl, owl, cif, txt, md",
                target.display()
            ),
        };
        summaries.push(summary);
    }

    if json_output {
        let payload = if summaries.len() == 1 {
            summaries.into_iter().next().unwrap_or_default()
        } else {
            serde_json::Value::Array(summaries)
        };
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        for (index, summary) in summaries.iter().enumerate() {
            if index > 0 {
                println!();
            }
            print_ingest_summary(summary);
        }
    }

    Ok(())
}

/// Watch a directory for new/modified ingestable files and ingest them.
#[allow(clippy::too_many_arguments)]
async fn handle_ingest_watch(
    dir: &Path,
    project_root: &Path,
    model: Option<&str>,
    llm_url: Option<&str>,
    api_key: Option<&str>,
    neo4j_url: &str,
    neo4j_user: &str,
    neo4j_pass: &str,
    qdrant_url: &str,
    schema_only: bool,
    runtime_url: &str,
    corpus: Option<&str>,
    json_output: bool,
    mapping: Option<&Path>,
) -> Result<()> {
    use std::collections::HashMap;
    use std::time::{Duration, SystemTime};

    if !dir.is_dir() {
        bail!("Watch mode requires a directory, got: {}", dir.display());
    }

    println!(
        "Watching {} for ingestable files (Ctrl+C to stop)...\n",
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
        match handle_ingest(
            &path,
            project_root,
            model,
            llm_url,
            api_key,
            neo4j_url,
            neo4j_user,
            neo4j_pass,
            qdrant_url,
            schema_only,
            runtime_url,
            corpus,
            json_output,
            mapping,
        )
        .await
        {
            Ok(()) => {}
            Err(e) => eprintln!("  Error: {e}"),
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
                match handle_ingest(
                    &path,
                    project_root,
                    model,
                    llm_url,
                    api_key,
                    neo4j_url,
                    neo4j_user,
                    neo4j_pass,
                    qdrant_url,
                    schema_only,
                    runtime_url,
                    corpus,
                    json_output,
                    mapping,
                )
                .await
                {
                    Ok(()) => {}
                    Err(e) => eprintln!("  Error: {e}"),
                }
            }
        }
    }
}

/// Check if a file has an ingestable extension.
fn is_ingestable(path: &Path) -> bool {
    path.is_file() && ingest_backend(path).is_some()
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
  prism deploy create --name serve --image marc27/mace:latest --target local
  prism deploy list                                    # list persistent deployments
  prism deploy status <deployment-id>                  # inspect one deployment

INGEST:
  prism ingest data.csv                                # ingest CSV into local graph
  prism ingest paper.pdf --corpus nasa-propulsion      # extract local PDF text, then submit one ingest flow
  prism ingest --status --corpus nasa-propulsion       # inspect ingest-related graph/vector/job state

NODE:
  prism node status                                    # show node capabilities
  prism node up                                        # register node with platform
  prism node down                                      # deregister

WORKFLOWS:
  prism workflow list                                  # list available workflows
  prism workflow run <name> --set key=value            # run a workflow

MODELS:
  prism models list                                    # list hosted project models
  prism models search gemini                           # search model catalog

DISCOURSE:
  prism discourse list                                 # list debate specs
  prism discourse create alloy.yaml                    # upload a YAML discourse spec
  prism discourse run <spec-id> --param alloy=IN718    # execute a discourse workflow

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
    if let Ok(token) = std::env::var("MARC27_TOKEN").or_else(|_| std::env::var("MARC27_API_TOKEN"))
    {
        let api_base = std::env::var("MARC27_API_URL")
            .unwrap_or_else(|_| "https://api.marc27.com/api/v1".to_string());
        return Ok((api_base, PlatformAuth::Bearer(token)));
    }
    let (base, token_header) = resolve_user_auth()?;
    // token_header is "Bearer <token>"
    let token = token_header
        .strip_prefix("Bearer ")
        .unwrap_or(&token_header)
        .to_string();
    Ok((base, PlatformAuth::Bearer(token)))
}

fn resolve_active_project_id(paths: &PrismPaths) -> Result<String> {
    if let Some(project_id) = env_project_override() {
        return Ok(project_id);
    }

    let state = paths.load_cli_state()?;
    state
        .credentials
        .as_ref()
        .and_then(|creds| creds.project_id.clone())
        .ok_or_else(|| {
            anyhow!("No active project selected. Run `prism login` or set MARC27_PROJECT_ID.")
        })
}

fn parse_string_map_arg(
    pairs: &[String],
    flag_name: &str,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    let mut values = serde_json::Map::new();
    for pair in pairs {
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| anyhow!("invalid {flag_name} value: {pair}. Expected key=value."))?;
        values.insert(
            key.trim().to_string(),
            serde_json::Value::String(value.trim().to_string()),
        );
    }
    Ok(values)
}

fn value_string<'a>(value: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(|field| field.as_str()))
}

fn value_bool(value: &serde_json::Value, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(|field| field.as_bool()))
}

fn value_array<'a>(
    value: &'a serde_json::Value,
    container_keys: &[&str],
) -> Option<&'a Vec<serde_json::Value>> {
    value.as_array().or_else(|| {
        container_keys
            .iter()
            .find_map(|key| value.get(*key).and_then(|field| field.as_array()))
    })
}

fn format_price(value: Option<f64>) -> String {
    match value {
        Some(price) => format!("${price:.4}"),
        None => "?".to_string(),
    }
}

fn model_matches_query(model: &serde_json::Value, query: &str) -> bool {
    let needle = query.to_ascii_lowercase();
    [
        value_string(model, &["model_id", "id"]),
        value_string(model, &["display_name", "name"]),
        value_string(model, &["provider"]),
    ]
    .into_iter()
    .flatten()
    .any(|field| field.to_ascii_lowercase().contains(&needle))
}

fn normalize_deploy_target(target: &str, node_id: Option<&str>) -> Result<&'static str> {
    match target.trim().to_ascii_lowercase().as_str() {
        "local" | "prism_node" => Ok("prism_node"),
        // The platform API uses `prism_node` plus an optional pinned node ID.
        "mesh" => {
            if node_id.is_none() {
                bail!("`--target mesh` currently requires `--node` / `--node-id`.");
            }
            Ok("prism_node")
        }
        "runpod" => Ok("runpod"),
        "lambda" => Ok("lambda"),
        other => {
            bail!("unsupported deploy target `{other}`. Use one of: local, mesh, runpod, lambda.")
        }
    }
}

fn print_deployments_summary(value: &serde_json::Value) -> Result<()> {
    let Some(items) = value_array(value, &["deployments", "items", "data"]) else {
        println!("{}", serde_json::to_string_pretty(value)?);
        return Ok(());
    };

    if items.is_empty() {
        println!("No deployments found.");
        return Ok(());
    }

    println!("Deployments:\n");
    for item in items {
        let id = value_string(item, &["deployment_id", "id"]).unwrap_or("?");
        let name = value_string(item, &["name"]).unwrap_or("(unnamed)");
        let status = value_string(item, &["status"]).unwrap_or("?");
        let image = value_string(item, &["image", "resource_slug"]).unwrap_or("-");
        println!("  {id}  {name}  [{status}]");
        println!("  {:<36} {}", "", image);
    }
    Ok(())
}

fn print_deployment_status(value: &serde_json::Value) -> Result<()> {
    let id = value_string(value, &["deployment_id", "id"]).unwrap_or("?");
    let name = value_string(value, &["name"]).unwrap_or("(unnamed)");
    let status = value_string(value, &["status"]).unwrap_or("?");
    let target = value_string(value, &["target"]).unwrap_or("-");
    let image = value_string(value, &["image", "resource_slug"]).unwrap_or("-");
    let endpoint = value_string(value, &["endpoint_url", "endpoint"]).unwrap_or("-");
    let healthy = value_bool(value, &["healthy"]).unwrap_or(false);

    println!("Deployment: {name}");
    println!("ID:         {id}");
    println!("Status:     {status}");
    println!("Target:     {target}");
    println!("Image:      {image}");
    println!("Endpoint:   {endpoint}");
    println!("Healthy:    {healthy}");
    if let Some(stopped_at) = value_string(value, &["stopped_at"]) {
        println!("Stopped at: {stopped_at}");
    }
    Ok(())
}

fn print_models_summary(models: &[serde_json::Value]) {
    if models.is_empty() {
        println!("No models found.");
        return;
    }

    println!("Hosted models:\n");
    for model in models {
        let model_id = value_string(model, &["model_id", "id"]).unwrap_or("?");
        let display_name = value_string(model, &["display_name", "name"]).unwrap_or(model_id);
        let provider = value_string(model, &["provider"]).unwrap_or("?");
        let status = value_string(model, &["status"]).unwrap_or("?");
        let context_window = model
            .get("context_window")
            .and_then(|value| value.as_u64())
            .map(|value| value.to_string())
            .unwrap_or_else(|| "?".to_string());
        let input_price = format_price(model.get("input_price").and_then(|value| value.as_f64()));
        let output_price = format_price(model.get("output_price").and_then(|value| value.as_f64()));

        println!("  {model_id}  [{provider}]  {status}");
        println!(
            "  {:<36} {}  ctx={}  in={}  out={}",
            "", display_name, context_window, input_price, output_price
        );
    }
}

fn print_discourse_specs_summary(value: &serde_json::Value) -> Result<()> {
    let Some(items) = value_array(value, &["specs", "items", "data"]) else {
        println!("{}", serde_json::to_string_pretty(value)?);
        return Ok(());
    };

    if items.is_empty() {
        println!("No discourse specs found.");
        return Ok(());
    }

    println!("Discourse specs:\n");
    for item in items {
        let id = value_string(item, &["id"]).unwrap_or("?");
        let slug = value_string(item, &["slug"]).unwrap_or("(no slug)");
        let name = value_string(item, &["name"]).unwrap_or("(unnamed)");
        let version = item
            .get("version")
            .and_then(|value| value.as_i64())
            .map(|value| value.to_string())
            .unwrap_or_else(|| "?".to_string());
        println!("  {id}  {slug}  v{version}");
        println!("  {:<36} {}", "", name);
    }
    Ok(())
}

fn print_discourse_status(value: &serde_json::Value) -> Result<()> {
    let instance_id = value_string(value, &["instance_id"]).unwrap_or("?");
    let spec_id = value_string(value, &["spec_id"]).unwrap_or("?");
    let status = value_string(value, &["status"]).unwrap_or("?");
    let total_turns = value
        .get("total_turns")
        .and_then(|value| value.as_i64())
        .map(|value| value.to_string())
        .unwrap_or_else(|| "?".to_string());
    let total_calls = value
        .get("total_llm_calls")
        .and_then(|value| value.as_i64())
        .map(|value| value.to_string())
        .unwrap_or_else(|| "?".to_string());
    let cost = value
        .get("cost_usd")
        .and_then(|value| value.as_f64())
        .map(|value| format!("${value:.4}"))
        .unwrap_or_else(|| "?".to_string());

    println!("Discourse instance: {instance_id}");
    println!("Spec:               {spec_id}");
    println!("Status:             {status}");
    println!("Turns:              {total_turns}");
    println!("LLM calls:          {total_calls}");
    println!("Cost:               {cost}");
    Ok(())
}

fn print_discourse_turns(value: &serde_json::Value) -> Result<()> {
    let Some(items) = value_array(value, &["turns", "items", "data"]) else {
        println!("{}", serde_json::to_string_pretty(value)?);
        return Ok(());
    };

    if items.is_empty() {
        println!("No discourse turns found.");
        return Ok(());
    }

    println!("Discourse turns:\n");
    for item in items {
        let round = item
            .get("round_num")
            .and_then(|value| value.as_i64())
            .map(|value| value.to_string())
            .unwrap_or_else(|| "?".to_string());
        let turn = item
            .get("turn_num")
            .and_then(|value| value.as_i64())
            .map(|value| value.to_string())
            .unwrap_or_else(|| "?".to_string());
        let agent = value_string(item, &["agent_id"]).unwrap_or("?");
        let content = value_string(item, &["content"]).unwrap_or("");
        let preview = if content.len() > 120 {
            format!("{}...", &content[..120])
        } else {
            content.to_string()
        };
        println!("  round {round} turn {turn}  [{agent}]");
        println!("  {:<36} {}", "", preview.replace('\n', " "));
    }
    Ok(())
}

fn print_discourse_run_events(events: &[serde_json::Value]) {
    if events.is_empty() {
        println!("No discourse events returned.");
        return;
    }

    for event in events {
        let event_name = value_string(event, &["event", "step"]).unwrap_or("event");
        match event_name {
            "started" => {
                let instance_id = value_string(event, &["instance_id"]).unwrap_or("?");
                let spec_name = value_string(event, &["spec_name", "name"]).unwrap_or("?");
                println!("started: {spec_name} ({instance_id})");
            }
            "round_started" => {
                let round = event
                    .get("round")
                    .and_then(|value| value.as_i64())
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "?".to_string());
                let round_type = value_string(event, &["type"]).unwrap_or("?");
                println!("round_started: round {round} [{round_type}]");
            }
            "agent_turn" => {
                let agent = value_string(event, &["agent_id"]).unwrap_or("?");
                let content = value_string(event, &["content"]).unwrap_or("");
                let preview = if content.len() > 140 {
                    format!("{}...", &content[..140])
                } else {
                    content.to_string()
                };
                println!("agent_turn: {agent}: {}", preview.replace('\n', " "));
            }
            "round_complete" => {
                let round = event
                    .get("round")
                    .and_then(|value| value.as_i64())
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "?".to_string());
                println!("round_complete: round {round}");
            }
            "complete" => {
                let turns = event
                    .get("total_turns")
                    .and_then(|value| value.as_i64())
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "?".to_string());
                let cost = event
                    .get("cost_usd")
                    .and_then(|value| value.as_f64())
                    .map(|value| format!("${value:.4}"))
                    .unwrap_or_else(|| "?".to_string());
                println!("complete: turns={turns} cost={cost}");
            }
            other => {
                println!("{other}: {}", event);
            }
        }
    }
}

async fn handle_deploy_command(command: DeployCommands) -> Result<()> {
    let (api_base, auth) = resolve_agent_auth()?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    match command {
        DeployCommands::Create {
            name,
            image,
            resource_slug,
            target,
            gpu,
            budget,
            node_id,
            env_vars,
            port,
            health_path,
            json,
        } => {
            let has_image = image.is_some();
            let has_resource = resource_slug.is_some();
            if has_image == has_resource {
                bail!("Specify exactly one of `--image` or `--resource-slug`.");
            }
            let target = normalize_deploy_target(&target, node_id.as_deref())?;

            let mut body = serde_json::Map::new();
            body.insert("name".to_string(), serde_json::Value::String(name.clone()));
            body.insert(
                "target".to_string(),
                serde_json::Value::String(target.to_string()),
            );
            body.insert("gpu_type".to_string(), serde_json::Value::String(gpu));
            body.insert(
                "deploy_config".to_string(),
                serde_json::json!({
                    "port": port,
                    "health_path": health_path,
                }),
            );

            if let Some(image) = image {
                body.insert("image".to_string(), serde_json::Value::String(image));
            }
            if let Some(resource_slug) = resource_slug {
                body.insert(
                    "resource_slug".to_string(),
                    serde_json::Value::String(resource_slug),
                );
            }
            if let Some(budget) = budget {
                body.insert("budget_max_usd".to_string(), serde_json::json!(budget));
            }
            if let Some(node_id) = node_id {
                body.insert("node_id".to_string(), serde_json::Value::String(node_id));
            }
            if !env_vars.is_empty() {
                body.insert(
                    "env_vars".to_string(),
                    serde_json::Value::Object(parse_string_map_arg(&env_vars, "--env")?),
                );
            }

            let response: serde_json::Value = auth
                .apply(client.post(format!("{api_base}/compute/deployments")))
                .json(&serde_json::Value::Object(body))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;

            if json {
                println!("{}", serde_json::to_string_pretty(&response)?);
            } else {
                println!("Deployment create requested.");
                print_deployment_status(&response)?;
            }
        }
        DeployCommands::List { status, json } => {
            let mut request = auth.apply(client.get(format!("{api_base}/compute/deployments")));
            if let Some(status) = status.as_deref() {
                request = request.query(&[("status", status)]);
            }
            let response: serde_json::Value =
                request.send().await?.error_for_status()?.json().await?;

            if json {
                println!("{}", serde_json::to_string_pretty(&response)?);
            } else {
                print_deployments_summary(&response)?;
            }
        }
        DeployCommands::Status { id, json } => {
            let response: serde_json::Value = auth
                .apply(client.get(format!("{api_base}/compute/deployments/{id}")))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;

            if json {
                println!("{}", serde_json::to_string_pretty(&response)?);
            } else {
                print_deployment_status(&response)?;
            }
        }
        DeployCommands::Stop { id, json } => {
            let response = auth
                .apply(client.delete(format!("{api_base}/compute/deployments/{id}")))
                .send()
                .await?
                .error_for_status()?
                .text()
                .await?;

            if json {
                let value = if response.trim().is_empty() {
                    serde_json::json!({ "deployment_id": id, "status": "stop_requested" })
                } else {
                    serde_json::from_str::<serde_json::Value>(&response).unwrap_or_else(|_| {
                        serde_json::json!({
                            "deployment_id": id,
                            "status": "stop_requested",
                            "message": response,
                        })
                    })
                };
                println!("{}", serde_json::to_string_pretty(&value)?);
            } else {
                println!("Stop requested for deployment {id}.");
            }
        }
        DeployCommands::Health { id, json } => {
            let response: serde_json::Value = auth
                .apply(client.get(format!("{api_base}/compute/deployments/{id}/health")))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;

            if json {
                println!("{}", serde_json::to_string_pretty(&response)?);
            } else {
                print_deployment_status(&response)?;
            }
        }
    }

    Ok(())
}

async fn handle_models_command(paths: &PrismPaths, command: ModelsCommands) -> Result<()> {
    let (api_base, auth) = resolve_agent_auth()?;
    let project_id = resolve_active_project_id(paths)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    let response: serde_json::Value = auth
        .apply(client.get(format!("{api_base}/projects/{project_id}/llm/models")))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let mut models = value_array(&response, &["models", "items", "data"])
        .cloned()
        .unwrap_or_default();

    match command {
        ModelsCommands::List { provider, json } => {
            if let Some(provider) = provider {
                let provider = provider.to_ascii_lowercase();
                models.retain(|model| {
                    value_string(model, &["provider"])
                        .map(|value| value.eq_ignore_ascii_case(&provider))
                        .unwrap_or(false)
                });
            }

            if json {
                println!("{}", serde_json::to_string_pretty(&models)?);
            } else {
                print_models_summary(&models);
            }
        }
        ModelsCommands::Search {
            query,
            provider,
            json,
        } => {
            if let Some(provider) = provider {
                let provider = provider.to_ascii_lowercase();
                models.retain(|model| {
                    value_string(model, &["provider"])
                        .map(|value| value.eq_ignore_ascii_case(&provider))
                        .unwrap_or(false)
                });
            }
            models.retain(|model| model_matches_query(model, &query));

            if json {
                println!("{}", serde_json::to_string_pretty(&models)?);
            } else {
                print_models_summary(&models);
            }
        }
        ModelsCommands::Info { model_id, json } => {
            let model = models
                .into_iter()
                .find(|model| {
                    value_string(model, &["model_id", "id"])
                        .map(|value| value == model_id)
                        .unwrap_or(false)
                })
                .ok_or_else(|| anyhow!("Model not found in project catalog: {model_id}"))?;

            if json {
                println!("{}", serde_json::to_string_pretty(&model)?);
            } else {
                println!("{}", serde_json::to_string_pretty(&model)?);
            }
        }
    }

    Ok(())
}

async fn handle_discourse_command(command: DiscourseCommands) -> Result<()> {
    let (api_base, auth) = resolve_agent_auth()?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;

    match command {
        DiscourseCommands::Create {
            yaml_file,
            slug,
            json,
        } => {
            let yaml = std::fs::read_to_string(&yaml_file).with_context(|| {
                format!("failed to read discourse YAML {}", yaml_file.display())
            })?;
            let slug = slug.unwrap_or_else(|| {
                yaml_file
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .unwrap_or("discourse-spec")
                    .to_string()
            });
            let response: serde_json::Value = auth
                .apply(client.post(format!("{api_base}/discourse/specs")))
                .json(&serde_json::json!({
                    "slug": slug,
                    "yaml": yaml,
                }))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;

            if json {
                println!("{}", serde_json::to_string_pretty(&response)?);
            } else {
                let id = value_string(&response, &["id"]).unwrap_or("?");
                let slug = value_string(&response, &["slug"]).unwrap_or("?");
                let name = value_string(&response, &["name"]).unwrap_or("(unnamed)");
                let version = response
                    .get("version")
                    .and_then(|value| value.as_i64())
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "?".to_string());
                println!("Created discourse spec {name} ({slug}) v{version}");
                println!("ID: {id}");
            }
        }
        DiscourseCommands::List { json } => {
            let response: serde_json::Value = auth
                .apply(client.get(format!("{api_base}/discourse/specs")))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;

            if json {
                println!("{}", serde_json::to_string_pretty(&response)?);
            } else {
                print_discourse_specs_summary(&response)?;
            }
        }
        DiscourseCommands::Show { spec_id, .. } => {
            let response: serde_json::Value = auth
                .apply(client.get(format!("{api_base}/discourse/specs/{spec_id}")))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            // YAML-backed specs are easier to inspect as pretty JSON than a lossy summary.
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        DiscourseCommands::Run {
            spec_id,
            params,
            json,
        } => {
            let body = serde_json::json!({
                "parameters": parse_string_map_arg(&params, "--param")?,
            });
            let response = auth
                .apply(client.post(format!("{api_base}/discourse/run/{spec_id}")))
                .json(&body)
                .send()
                .await?
                .error_for_status()?
                .text()
                .await?;
            let events = normalize_stream_events(parse_sse_json_events(&response)?);

            if json {
                let mut payload = serde_json::Map::new();
                payload.insert(
                    "events".to_string(),
                    serde_json::Value::Array(events.clone()),
                );
                if let Some(instance_id) = events.iter().find_map(|event| {
                    value_string(event, &["instance_id"]).map(|value| value.to_string())
                }) {
                    payload.insert(
                        "instance_id".to_string(),
                        serde_json::Value::String(instance_id),
                    );
                }
                if let Some(complete) = events
                    .iter()
                    .find(|event| value_string(event, &["event", "step"]) == Some("complete"))
                {
                    payload.insert("complete".to_string(), complete.clone());
                }
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::Value::Object(payload))?
                );
            } else {
                print_discourse_run_events(&events);
            }
        }
        DiscourseCommands::Status { instance_id, json } => {
            let response: serde_json::Value = auth
                .apply(client.get(format!("{api_base}/discourse/{instance_id}")))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;

            if json {
                println!("{}", serde_json::to_string_pretty(&response)?);
            } else {
                print_discourse_status(&response)?;
            }
        }
        DiscourseCommands::Turns { instance_id, json } => {
            let response: serde_json::Value = auth
                .apply(client.get(format!("{api_base}/discourse/{instance_id}/turns")))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;

            if json {
                println!("{}", serde_json::to_string_pretty(&response)?);
            } else {
                print_discourse_turns(&response)?;
            }
        }
    }

    Ok(())
}

fn parse_research_response_body(body: &str) -> Result<serde_json::Value> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        bail!("research endpoint returned an empty response body");
    }
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return Ok(value);
    }

    let events = normalize_stream_events(parse_sse_json_events(trimmed)?);
    if events.is_empty() {
        bail!("research endpoint returned a non-JSON response body");
    }

    let mut result = serde_json::Map::new();
    let mut answer = None;
    let mut sources = None;
    let mut complete = None;

    for event in &events {
        if let Some(obj) = event.as_object() {
            let step = obj.get("step").and_then(|value| value.as_str());
            let event_answer = extract_research_answer(obj);
            if step == Some("answer") {
                answer = event_answer.or(answer);
            } else if answer.is_none() {
                answer = event_answer;
            }
            if sources.is_none() {
                if let Some(found) = obj.get("sources").and_then(|value| value.as_array()) {
                    sources = Some(serde_json::Value::Array(found.clone()));
                }
            }
            if complete.is_none()
                && obj.get("step").and_then(|value| value.as_str()) == Some("complete")
            {
                complete = Some(event.clone());
            }
        }
    }

    result.insert("events".to_string(), serde_json::Value::Array(events));
    if let Some(answer) = answer {
        result.insert("answer".to_string(), serde_json::Value::String(answer));
    }
    if let Some(sources) = sources {
        result.insert("sources".to_string(), sources);
    }
    if let Some(complete) = complete {
        result.insert("complete".to_string(), complete);
    }

    Ok(serde_json::Value::Object(result))
}

fn parse_sse_json_events(body: &str) -> Result<Vec<serde_json::Value>> {
    fn flush_sse_event(
        events: &mut Vec<serde_json::Value>,
        event_name: &Option<String>,
        data_lines: &mut Vec<String>,
    ) {
        if data_lines.is_empty() {
            return;
        }

        let payload = data_lines.join("\n");
        data_lines.clear();
        let payload = payload.trim();
        if payload.is_empty() || payload == "[DONE]" {
            return;
        }

        let mut value = serde_json::from_str::<serde_json::Value>(payload)
            .unwrap_or_else(|_| serde_json::json!({ "text": payload }));
        if let Some(name) = event_name {
            if let Some(obj) = value.as_object_mut() {
                obj.entry("event".to_string())
                    .or_insert_with(|| serde_json::Value::String(name.clone()));
            } else {
                value = serde_json::json!({
                    "event": name,
                    "data": value,
                });
            }
        }
        events.push(value);
    }

    let mut events = Vec::new();
    let mut event_name = None;
    let mut data_lines = Vec::new();

    for raw_line in body.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line.is_empty() {
            flush_sse_event(&mut events, &event_name, &mut data_lines);
            event_name = None;
            continue;
        }
        if let Some(name) = line.strip_prefix("event:") {
            event_name = Some(name.trim().to_string());
            continue;
        }
        if let Some(data) = line.strip_prefix("data:") {
            // Some platform streams send one JSON payload per `data:` line without
            // blank-line separators. When that happens, treat each new `data:` line
            // as a fresh event so discourse/research JSON stays structured.
            if event_name.is_none() && !data_lines.is_empty() {
                flush_sse_event(&mut events, &event_name, &mut data_lines);
            }
            data_lines.push(data.trim_start().to_string());
        }
    }
    flush_sse_event(&mut events, &event_name, &mut data_lines);

    Ok(events)
}

fn normalize_stream_events(events: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
    let mut normalized = Vec::new();

    for event in events {
        // Some routes hand back transport-level `data: {...}` strings inside a
        // generic `text` field. Unwrap those so downstream code sees the real
        // event objects, not opaque transport wrappers.
        if let Some(text) = event.get("text").and_then(|value| value.as_str()) {
            if let Some(payload) = text.trim().strip_prefix("data:") {
                let payload = payload.trim();
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) {
                    normalized.push(value);
                    continue;
                }
            }
        }

        normalized.push(event);
    }

    normalized
}

fn extract_research_answer(obj: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    for key in ["answer", "text", "content", "message"] {
        if let Some(value) = obj.get(key).and_then(|value| value.as_str()) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }

    if let Some(data) = obj.get("data").and_then(|value| value.as_object()) {
        return extract_research_answer(data);
    }

    None
}

#[derive(serde::Deserialize)]
struct DashboardSessionResponse {
    session_id: String,
}

async fn create_dashboard_session(
    dashboard_url: &str,
    paths: &prism_runtime::PrismPaths,
) -> Result<String> {
    let state = paths.load_cli_state()?;
    let creds = state
        .credentials
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Not logged in. Run `prism login` first."))?;
    let user_id = creds.user_id.as_deref().ok_or_else(|| {
        anyhow::anyhow!("Stored credentials are missing user_id. Run `prism login` again.")
    })?;

    // The local CLI is the node operator on this machine, so it should be able
    // to manage its own dashboard routes without a separate bootstrap dance.
    let rbac_db_path = paths.state_dir.join("rbac.db");
    let rbac_engine = prism_core::rbac::RbacEngine::new(&rbac_db_path)?;
    rbac_engine.assign_role(user_id, prism_core::rbac::LocalRole::NodeAdmin)?;

    create_dashboard_session_for_user(dashboard_url, user_id, creds.display_name.as_deref()).await
}

async fn create_dashboard_session_for_user(
    dashboard_url: &str,
    user_id: &str,
    display_name: Option<&str>,
) -> Result<String> {
    let url = format!("{dashboard_url}/api/sessions");
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&serde_json::json!({
            "user_id": user_id,
            "display_name": display_name,
        }))
        .send()
        .await
        .with_context(|| format!("Failed to create dashboard session at {url}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("Dashboard session creation failed: {status} — {body}");
    }

    let session: DashboardSessionResponse = resp.json().await?;
    Ok(session.session_id)
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

async fn handle_federated_query(
    query: &str,
    dashboard_url: &str,
    paths: &prism_runtime::PrismPaths,
) -> Result<()> {
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
    // Protected dashboard routes need a local session token, even for the CLI
    // running on the same machine as the node.
    let local_session = create_dashboard_session(dashboard_url, paths).await.ok();
    let mut local_req = reqwest::Client::new().post(&local_url).json(&local_body);
    if let Some(session_token) = local_session {
        local_req = local_req.header("X-Session-Token", session_token);
    }
    let local_result = local_req.send().await;

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
        let peer_base = format!("http://{}:{}", addr, port);
        let peer_url = format!("{peer_base}/api/query");
        let body = serde_json::json!({"query": query, "mode": "nl"});
        let peer_session =
            create_dashboard_session_for_user(&peer_base, "federated-cli", Some("PRISM CLI"))
                .await
                .ok();

        print!("[{name}] ");
        let mut peer_req = reqwest::Client::new().post(&peer_url).json(&body);
        if let Some(session_token) = peer_session {
            peer_req = peer_req.header("X-Session-Token", session_token);
        }
        match peer_req
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

#[allow(clippy::too_many_arguments)]
async fn handle_run(
    name: &str,
    image: &str,
    inputs: &[String],
    backend: &str,
    platform_url: &str,
    ssh: Option<&str>,
    ssh_key: &str,
    ssh_port: u16,
    k8s_context: Option<&str>,
    k8s_namespace: &str,
    slurm: Option<&str>,
    slurm_partition: &str,
    json: bool,
) -> Result<()> {
    use prism_compute::backend::ComputeRouter;
    use prism_compute::byoc::ByocTarget;
    use prism_compute::ExperimentPlan;

    // Parse key=value inputs into JSON
    let mut input_map = serde_json::Map::new();
    for kv in inputs {
        if let Some((k, v)) = kv.split_once('=') {
            input_map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
        }
    }

    let inputs_json = serde_json::Value::Object(input_map);
    let plan = ExperimentPlan {
        name: name.to_string(),
        image: image.to_string(),
        inputs: inputs_json.clone(),
    };

    let (router, resolved_backend, target) = if let Some(ssh_target) = ssh {
        // Parse user@host — default user is "root" if no '@' present
        let (user, host) = if let Some((u, h)) = ssh_target.split_once('@') {
            (u.to_string(), h.to_string())
        } else {
            ("root".to_string(), ssh_target.to_string())
        };
        let target = ByocTarget::Ssh {
            host,
            user,
            key_path: ssh_key.to_string(),
            port: ssh_port,
        };
        (
            ComputeRouter::local_only().with_byoc(target),
            "byoc",
            serde_json::json!({
                "kind": "ssh",
                "endpoint": ssh_target,
                "port": ssh_port,
            }),
        )
    } else if let Some(ctx) = k8s_context {
        let target = ByocTarget::Kubernetes {
            context: ctx.to_string(),
            namespace: k8s_namespace.to_string(),
        };
        (
            ComputeRouter::local_only().with_byoc(target),
            "byoc",
            serde_json::json!({
                "kind": "kubernetes",
                "context": ctx,
                "namespace": k8s_namespace,
            }),
        )
    } else if let Some(slurm_host) = slurm {
        // Parse user@host for SLURM head node
        let (user, head_node) = if let Some((u, h)) = slurm_host.split_once('@') {
            (u.to_string(), h.to_string())
        } else {
            ("root".to_string(), slurm_host.to_string())
        };
        let target = ByocTarget::Slurm {
            head_node,
            user,
            partition: slurm_partition.to_string(),
        };
        (
            ComputeRouter::local_only().with_byoc(target),
            "byoc",
            serde_json::json!({
                "kind": "slurm",
                "endpoint": slurm_host,
                "partition": slurm_partition,
            }),
        )
    } else {
        match backend {
            "marc27" | "platform" => {
                // Read token from credentials
                let token = std::env::var("MARC27_API_TOKEN").unwrap_or_else(|_| "".to_string());
                (
                    ComputeRouter::with_marc27(platform_url, &token),
                    "marc27",
                    serde_json::json!({
                        "kind": "marc27",
                        "platform_url": platform_url,
                    }),
                )
            }
            _ => (
                ComputeRouter::local_only(),
                "local",
                serde_json::json!({
                    "kind": "local",
                }),
            ),
        }
    };

    if !json {
        println!("Submitting job '{name}' (image: {image}, backend: {resolved_backend})...");
    }

    // Timeout for submit (Docker may need to pull the image)
    let job_id = tokio::time::timeout(std::time::Duration::from_secs(120), router.submit(&plan))
        .await
        .map_err(|_| {
            anyhow::anyhow!("Job submission timed out after 120s (image pull may be slow)")
        })??;

    // Brief poll for initial status
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let status_result = router.status(job_id).await;

    if json {
        let mut payload = serde_json::json!({
            "job_id": job_id,
            "name": name,
            "image": image,
            "backend": resolved_backend,
            "target": target,
            "inputs": inputs_json,
        });
        if let Some(object) = payload.as_object_mut() {
            match status_result {
                Ok(status) => {
                    object.insert("initial_status".to_string(), serde_json::to_value(status)?);
                }
                Err(error) => {
                    object.insert(
                        "status_error".to_string(),
                        serde_json::Value::String(error.to_string()),
                    );
                }
            }
        }
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        println!("Job submitted: {job_id}");
        println!("Check status:  prism job-status {job_id}");
        match status_result {
            Ok(status) => println!("Status: {:?}", status),
            Err(e) => println!("Status check: {e}"),
        }
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
    let os_info = format!("{} ({})", caps.software.join(", "), std::env::consts::ARCH,);
    let python_version = std::process::Command::new("python3")
        .arg("--version")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());

    // Read log file if provided
    let log_content = if let Some(path) = log_file {
        std::fs::read_to_string(path).ok().map(|s| {
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
        body.push_str(&format!("\n## Error Output\n\n```\n{}\n```\n", log));
    }

    // 3. File GitHub issue (unless --no-github)
    if !no_github {
        print!("Filing GitHub issue... ");
        let gh_result = tokio::process::Command::new("gh")
            .args([
                "issue",
                "create",
                "--repo",
                "Darth-Hidious/PRISM",
                "--title",
                &format!("bug report: {}", &description[..description.len().min(60)]),
                "--body",
                &body,
                "--label",
                "bug",
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
    if let Some(c) = creds {
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
                    println!(
                        "\n  View on dashboard: {}/dashboard/support",
                        endpoints.api_base.replace("/api/v1", "")
                    );
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
                assert_eq!(path, Some(PathBuf::from("/tmp/data.csv")));
                assert!(!schema_only);
                // Model is now None by default (reads from config)
                assert_eq!(model, None);
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
                assert_eq!(path, Some(PathBuf::from("/tmp/data.parquet")));
                assert!(schema_only);
            }
            _ => panic!("expected Ingest command"),
        }
    }

    #[test]
    fn cli_parses_ingest_status_without_path() {
        let cli = Cli::try_parse_from([
            "prism",
            "ingest",
            "--status",
            "--corpus",
            "nasa-propulsion",
            "--json",
        ])
        .unwrap();
        match cli.command.unwrap() {
            Commands::Ingest {
                path,
                status,
                corpus,
                json,
                ..
            } => {
                assert_eq!(path, None);
                assert!(status);
                assert_eq!(corpus.as_deref(), Some("nasa-propulsion"));
                assert!(json);
            }
            _ => panic!("expected Ingest command"),
        }
    }

    #[test]
    fn ingest_backend_detects_platform_text_files() {
        assert_eq!(
            ingest_backend(Path::new("/tmp/paper.pdf")),
            Some(IngestBackend::PlatformText)
        );
        assert_eq!(
            ingest_backend(Path::new("/tmp/graph.jsonl")),
            Some(IngestBackend::PlatformText)
        );
        assert_eq!(
            ingest_backend(Path::new("/tmp/table.parquet")),
            Some(IngestBackend::LocalTabular)
        );
        assert_eq!(ingest_backend(Path::new("/tmp/image.png")), None);
    }

    #[test]
    fn split_text_for_platform_ingest_creates_bounded_chunks() {
        let text = format!("{}\n\n{}", "A".repeat(30_000), "B".repeat(30_000));
        let chunks = split_text_for_platform_ingest(&text);
        assert_eq!(chunks.len(), 2);
        assert!(chunks.iter().all(|chunk| chunk.chars().count() <= 48_000));
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

    #[test]
    fn cli_parses_research_depth() {
        let cli = Cli::try_parse_from([
            "prism",
            "research",
            "--depth",
            "0",
            "--json",
            "Find materials containing nickel",
        ])
        .unwrap();
        match cli.command.unwrap() {
            Commands::Research { query, depth, json } => {
                assert_eq!(query, "Find materials containing nickel");
                assert_eq!(depth, 0);
                assert!(json);
            }
            _ => panic!("expected Research command"),
        }
    }

    #[test]
    fn cli_parses_run_json_command() {
        let cli = Cli::try_parse_from([
            "prism",
            "run",
            "--name",
            "trial",
            "--backend",
            "marc27",
            "--json",
            "ghcr.io/acme/model:latest",
        ])
        .unwrap();
        match cli.command.unwrap() {
            Commands::Run {
                image,
                name,
                backend,
                json,
                ..
            } => {
                assert_eq!(image, "ghcr.io/acme/model:latest");
                assert_eq!(name, "trial");
                assert_eq!(backend, "marc27");
                assert!(json);
            }
            _ => panic!("expected Run command"),
        }
    }

    #[test]
    fn cli_parses_publish_json_command() {
        let cli = Cli::try_parse_from([
            "prism",
            "publish",
            "models/mace.ckpt",
            "--to",
            "marc27",
            "--private",
            "--json",
        ])
        .unwrap();
        match cli.command.unwrap() {
            Commands::Publish {
                path,
                to,
                private,
                json,
                ..
            } => {
                assert_eq!(path, "models/mace.ckpt");
                assert_eq!(to, "marc27");
                assert!(private);
                assert!(json);
            }
            _ => panic!("expected Publish command"),
        }
    }

    #[test]
    fn parse_research_response_body_accepts_plain_json() {
        let parsed = parse_research_response_body(
            r#"{"answer":"Nickel Oxide","sources":[{"title":"Paper","url":"https://example.com"}]}"#,
        )
        .unwrap();
        assert_eq!(parsed["answer"], "Nickel Oxide");
        assert_eq!(parsed["sources"][0]["title"], "Paper");
    }

    #[test]
    fn parse_research_response_body_accepts_sse_events() {
        let parsed = parse_research_response_body(
            "event: step\n\
data: {\"step\":\"started\",\"session_id\":\"abc\"}\n\
\n\
event: step\n\
data: {\"step\":\"answer\",\"answer\":\"Nickel oxide\"}\n\
\n\
event: step\n\
data: {\"step\":\"complete\",\"graph_queries\":2}\n\
\n",
        )
        .unwrap();

        assert_eq!(parsed["answer"], "Nickel oxide");
        assert_eq!(parsed["events"].as_array().unwrap().len(), 3);
        assert_eq!(parsed["complete"]["step"], "complete");
    }

    #[test]
    fn parse_research_response_body_prefers_final_answer_step() {
        let parsed = parse_research_response_body(
            "event: step\n\
data: {\"step\":\"reasoning\",\"data\":{\"text\":\"thinking\"}}\n\
\n\
event: step\n\
data: {\"step\":\"answer\",\"data\":{\"text\":\"final answer\"}}\n\
\n",
        )
        .unwrap();

        assert_eq!(parsed["answer"], "final answer");
    }

    #[test]
    fn parse_sse_json_events_accepts_line_delimited_data_events() {
        let events = parse_sse_json_events(
            "data: {\"step\":\"started\",\"instance_id\":\"abc\"}\n\
data: {\"step\":\"agent_turn\",\"agent_id\":\"metallurgist\"}\n\
data: {\"step\":\"complete\",\"total_turns\":2}\n",
        )
        .unwrap();

        assert_eq!(events.len(), 3);
        assert_eq!(events[0]["step"], "started");
        assert_eq!(events[1]["agent_id"], "metallurgist");
        assert_eq!(events[2]["step"], "complete");
    }

    #[test]
    fn normalize_stream_events_unwraps_text_wrapped_data_payloads() {
        let normalized = normalize_stream_events(vec![
            serde_json::json!({"text": "data: {\"step\":\"started\",\"instance_id\":\"abc\"}"}),
            serde_json::json!({"text": "plain text"}),
        ]);

        assert_eq!(normalized[0]["step"], "started");
        assert_eq!(normalized[0]["instance_id"], "abc");
        assert_eq!(normalized[1]["text"], "plain text");
    }

    #[test]
    fn cli_parses_models_list_command() {
        let cli = Cli::try_parse_from(["prism", "models", "list", "--provider", "google"]).unwrap();
        match cli.command.unwrap() {
            Commands::Models {
                command: ModelsCommands::List { provider, json },
            } => {
                assert_eq!(provider.as_deref(), Some("google"));
                assert!(!json);
            }
            _ => panic!("expected Models::List command"),
        }
    }

    #[test]
    fn cli_parses_deploy_create_command() {
        let cli = Cli::try_parse_from([
            "prism",
            "deploy",
            "create",
            "--name",
            "serve-demo",
            "--image",
            "marc27/mace:latest",
            "--target",
            "local",
            "--env",
            "MODEL_PATH=/models/demo",
        ])
        .unwrap();
        match cli.command.unwrap() {
            Commands::Deploy {
                command:
                    DeployCommands::Create {
                        name,
                        image,
                        resource_slug,
                        target,
                        env_vars,
                        ..
                    },
            } => {
                assert_eq!(name, "serve-demo");
                assert_eq!(image.as_deref(), Some("marc27/mace:latest"));
                assert!(resource_slug.is_none());
                assert_eq!(target, "local");
                assert_eq!(env_vars, vec!["MODEL_PATH=/models/demo".to_string()]);
            }
            _ => panic!("expected Deploy::Create command"),
        }
    }

    #[test]
    fn cli_parses_discourse_run_command() {
        let cli = Cli::try_parse_from([
            "prism",
            "discourse",
            "run",
            "123e4567-e89b-12d3-a456-426614174000",
            "--param",
            "alloy=IN718",
            "--json",
        ])
        .unwrap();
        match cli.command.unwrap() {
            Commands::Discourse {
                command:
                    DiscourseCommands::Run {
                        spec_id,
                        params,
                        json,
                    },
            } => {
                assert_eq!(spec_id, "123e4567-e89b-12d3-a456-426614174000");
                assert_eq!(params, vec!["alloy=IN718".to_string()]);
                assert!(json);
            }
            _ => panic!("expected Discourse::Run command"),
        }
    }
}
