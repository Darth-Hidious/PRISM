//! PRISM CLI — the main entry point for the `prism` binary.
//!
//! Handles command routing (setup, login, node, workflow, etc.), auth bootstrap
//! via device-flow OAuth, Python worker supervision, and dynamic workflow
//! discovery from `~/.prism/workflows/`.

mod boot;
mod boot_checks;
mod chat_config;
mod doctor;
mod mcp_server_native;
mod notebook;
mod onboarding;
mod pyiron_cmd;
mod tool_sync;
mod use_command;

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
// std::process::Stdio removed — old Ink TUI launcher no longer needed
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use clap::{Parser, Subcommand};
use prism_client::DeviceFlowAuth;
use prism_client::api::PlatformClient;
use prism_client::auth::{DeviceCodeResponse, TokenResponse};
use prism_proto::NodeCapabilities;
use prism_python_bridge::{ToolServer, ensure_venv};
use prism_runtime::{PlatformEndpoints, PrismPaths, StoredCredentials};
use prism_workflows::{
    WorkflowRunResult, WorkflowSpec, discover_workflows, execute_workflow, find_workflow,
    load_workflow_from_str, parse_workflow_command_args,
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
    /// Resume a previous conversation by ID. With no value, shows the
    /// session picker. Shortcut for `prism resume [id]`.
    #[arg(long, global = false)]
    resume: Option<Option<String>>,
    /// Override the LLM model for this session (e.g. --model gemma-4-12b).
    #[arg(long, global = false)]
    model: Option<String>,
    /// Auto-approve all tool calls without prompting.
    #[arg(long, global = false)]
    auto_approve: bool,
    /// Run in offline mode (no MARC27 platform connection).
    #[arg(long, global = false)]
    offline: bool,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Run first-time native setup and platform login.
    Setup,
    /// Launch the interactive AI agent TUI.
    Tui {
        /// Use a deterministic fake backend instead of spawning `prism
        /// backend`.  No subprocess, no network, no LLM.  Used for
        /// testing and PTY verification.
        #[arg(long)]
        fake_backend: bool,

        /// Fake backend scenario name (only used with --fake-backend).
        /// Available: basic_chat, streaming_answer, thinking_stream,
        /// tool_success, tool_error, approval_required, cost_metrics,
        /// backend_warning_error, ansi_injection
        #[arg(long, default_value = "basic_chat")]
        scenario: String,
    },
    /// Resume a previous conversation.
    ///
    /// With no argument, opens the conversation picker (last-N sessions
    /// shown by title + how-long-ago) so you can pick one. With a
    /// conversation id, jumps straight back into that conversation.
    /// The id was printed when you exited the previous session.
    Resume {
        /// Conversation UUID to resume directly. Omit to get the picker.
        id: Option<String>,
    },
    /// Authenticate against the MARC27 platform.
    ///
    /// Default: device-flow login that opens a browser to approve the
    /// session. For HPC nodes / SSH sessions / any environment without
    /// a browser, two extra modes are supported:
    ///
    ///   prism login --no-browser
    ///       Run the device flow but DON'T try to open a browser
    ///       automatically. The URL + user code are printed; approve
    ///       from any other machine's browser, then return to the
    ///       terminal — the poll continues until you do.
    ///
    ///   prism login --token <PAT>
    ///       Skip the device flow entirely. Use a Personal Access
    ///       Token created on the MARC27 website. The token is
    ///       written to ~/.prism/credentials.json with no further
    ///       interaction. This is the right path for headless
    ///       servers and CI environments.
    Login {
        /// Bypass the device flow. Use a pre-issued Personal Access
        /// Token from the MARC27 website. Headless / CI / SSH-only
        /// use. Token is read from this flag, env var
        /// `PRISM_LOGIN_TOKEN`, or stdin (in that priority order)
        /// so the token never has to appear in shell history.
        #[arg(long, value_name = "PAT", env = "PRISM_LOGIN_TOKEN")]
        token: Option<String>,

        /// Run the device flow but skip the browser auto-open. Prints
        /// the verification URL + user code and waits for approval.
        /// Useful when on a headless HPC node — you copy the URL to
        /// your laptop's browser and approve there.
        #[arg(long, conflicts_with = "token")]
        no_browser: bool,
    },
    /// Show runtime paths, endpoints, and auth status.
    Status,
    /// List, show, and run YAML-defined workflows.
    Workflow {
        #[command(subcommand)]
        command: WorkflowCommands,
    },
    /// Run an autonomous materials discovery campaign. The campaign agent
    /// loops: propose → evaluate → rank → narrow, with budget limits,
    /// checkpointing, and human approval gates.
    Campaign {
        #[command(subcommand)]
        command: CampaignCommands,
    },
    /// Start the agent backend (JSON-RPC server for TUI frontend).
    Backend {
        #[arg(long, default_value = ".")]
        project_root: PathBuf,
        #[arg(long, default_value = "python3")]
        python: PathBuf,
    },
    /// Serve the agent over JSON-RPC on stdio for external frontends
    /// (PRISM Desktop, IDE extensions) — the LSP-server role. Same-user
    /// stdio trust boundary; opens no network socket.
    IpcServe {
        #[arg(long, default_value = ".")]
        project_root: PathBuf,
        #[arg(long, default_value = "python3")]
        python: PathBuf,
    },
    /// Launch and manage Jupyter notebooks (local or remote compute).
    Notebook {
        #[command(subcommand)]
        command: NotebookCommands,
    },
    /// Manage PyIron (simulation framework) in the PRISM venv.
    Pyiron {
        #[command(subcommand)]
        command: PyironCommands,
    },
    /// List available Python tools.
    Tools,
    /// Run the native (Rust) MCP server — exposes PRISM's Rust-side tools
    /// (query, ingest, mesh, workflow, …) over stdio JSON-RPC. Forge spawns
    /// this as a subprocess so the LLM can call Rust tools without going
    /// through Python.
    #[command(name = "mcp-server-native", hide = true)]
    McpServerNative,
    /// Diagnostic snapshot — checks llama-server, models, Python venv, auth,
    /// MCP config and tool index. Run this first when chat misbehaves.
    Doctor,
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
    /// PRISM Fabric — cross-org federation primitives (read-only). Trust is
    /// managed in the MARC27 platform UI; the CLI only inspects state.
    Federation {
        #[command(subcommand)]
        command: FederationCommands,
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
    /// Run a marketplace model on the cloud, one call: ensure a deployment
    /// exists (reuse a running one, else create + wait ready), POST the
    /// inputs to its /predict endpoint, print the model's real result.
    ///
    /// Deployments this command CREATES are auto-stopped after the result
    /// (no silent per-minute billing) unless `--keep` is passed; reused
    /// deployments are never stopped. Default target lets the platform pick
    /// a node; `--node-id` pins a specific mesh target (the dashboard's
    /// "select a target from the mesh" case). Prints one JSON document.
    Predict {
        /// Marketplace model slug (e.g. "mace-mh-1", "chgnet").
        model: String,
        /// Model task, e.g. "single_point", "relax", "md".
        #[arg(long, default_value = "single_point")]
        task: String,
        /// Model inputs as a JSON object (e.g. '{"structure": {...}}').
        #[arg(long, default_value = "{}")]
        input: String,
        /// Pin the deployment to a specific PRISM node UUID (mesh target).
        #[arg(long)]
        node_id: Option<String>,
        /// GPU type to request for a NEW deployment (omit for CPU).
        #[arg(long)]
        gpu: Option<String>,
        /// Budget cap (USD) for a NEW deployment.
        #[arg(long)]
        budget: Option<f64>,
        /// Seconds to wait for a new deployment to become ready.
        #[arg(long, default_value_t = 900)]
        ready_timeout_secs: u64,
        /// Keep a newly-created deployment running after the result
        /// (it keeps billing per minute until stopped).
        #[arg(long)]
        keep: bool,
    },
    /// List GPU offers purchasable through the MARC27 compute platform.
    ///
    /// Prints the live catalog (type, VRAM, region, provider, $/hr) as one
    /// raw JSON array on stdout — machine-readable by design: the TUI
    /// `/gpus` picker and agents parse this output. Failures print
    /// `{"error": "..."}` and still exit 0 so callers always get exactly
    /// one JSON document.
    Gpus,
    /// One-shot compute-broker jobs (GPU/CPU) on the MARC27 platform.
    ///
    /// Read actions (gpus/providers/estimate/status) and cancel are safe;
    /// `submit` dispatches a real, billable job. Every subcommand prints one
    /// JSON document on stdout — machine-readable by design for agents.
    Compute {
        #[command(subcommand)]
        command: ComputeCommands,
    },
    /// Knowledge-plane reads + platform ingest (MARC27 knowledge graph).
    ///
    /// entity/paths/corpora are read-only graph/catalog lookups; `ingest`
    /// submits a background extraction job. Every subcommand prints one JSON
    /// document on stdout. Graph search + semantic search live under
    /// `prism query --platform`; graph stats under `prism ingest --status`.
    Knowledge {
        #[command(subcommand)]
        command: KnowledgeCommands,
    },
    /// Run multi-agent discourse workflows backed by the MARC27 platform.
    Discourse {
        #[command(subcommand)]
        command: DiscourseCommands,
    },
    /// Pick where chat turns are routed: MARC27 cloud (default), a
    /// local OpenAI-compatible LLM, or a direct vendor (Anthropic /
    /// OpenAI / etc). MARC27 platform tools — knowledge graph,
    /// discourse, marketplace, materials project — stay available
    /// regardless of which chat target is selected.
    ///
    /// Identical to the in-chat `/use` slash command — both write the
    /// same `~/.prism/config.toml`.
    Use {
        #[command(subcommand)]
        command: UseCommands,
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
    /// View credit balance, usage, and top up.
    Billing {
        #[command(subcommand)]
        command: Option<BillingCommands>,
    },
    #[command(external_subcommand)]
    External(Vec<String>),
}

#[derive(Debug, Subcommand)]
enum BillingCommands {
    /// Show usage breakdown by service.
    Usage,
    /// Show transaction history.
    History,
    /// Show credit pricing table.
    Prices,
    /// Buy credits — lists packs; with a slug, opens the checkout in a browser.
    Topup {
        /// Package slug: starter, standard, pro, enterprise. Omit to just
        /// list the available packs (no checkout is created).
        package: Option<String>,
    },
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
enum CampaignCommands {
    /// Start a new discovery campaign from a goal description.
    Start {
        /// Natural-language description of what to discover.
        #[arg(long)]
        goal: String,
        /// Comma-separated allowed elements (e.g. "W,Mo,Ta,Nb").
        #[arg(long)]
        elements: Option<String>,
        /// What to optimize (e.g. "maximize creep resistance").
        #[arg(long)]
        objective: Option<String>,
        /// Maximum number of discovery iterations.
        #[arg(long, default_value_t = 50)]
        max_iterations: usize,
        /// Candidates per iteration.
        #[arg(long, default_value_t = 10)]
        batch_size: usize,
        /// Optional USD budget cap.
        #[arg(long)]
        budget: Option<f64>,
        /// Checkpoint every N iterations.
        #[arg(long, default_value_t = 10)]
        checkpoint_every: usize,
        /// Pause for human approval at these iterations (comma-separated).
        #[arg(long)]
        approval_gates: Option<String>,
        /// Detach: write the initial checkpoint, hand the loop to a
        /// background process, and return the goal id immediately. Poll with
        /// `campaign status` / GET /api/goals.
        #[arg(long)]
        detach: bool,
    },
    /// Resume a paused campaign from its checkpoint.
    Resume {
        /// Campaign ID to resume.
        id: String,
        /// Detach: hand the resumed loop to a background process and return
        /// immediately.
        #[arg(long)]
        detach: bool,
    },
    /// Continue a campaign loop in the foreground from its checkpoint
    /// (the worker half of `--detach`; also usable directly).
    Continue {
        /// Campaign ID to continue.
        id: String,
    },
    /// Show the status of a campaign (from its checkpoint).
    Status {
        /// Campaign ID.
        id: String,
    },
    /// List all campaign checkpoints on this machine.
    List,
}

#[derive(Debug, Subcommand)]
enum NotebookCommands {
    /// Launch a Jupyter Lab server in the PRISM venv.
    Start {
        /// Port (default: auto).
        #[arg(long)]
        port: Option<u16>,
    },
    /// List active notebook sessions.
    List,
    /// Stop a notebook by PID, port, or "all".
    Stop {
        /// PID, port number, or "all".
        target: String,
    },
}

#[derive(Debug, Subcommand)]
enum PyironCommands {
    /// Show PyIron installation status (version, venv health).
    Status,
    /// Install PyIron into the PRISM venv.
    Install,
    /// Update PyIron to the latest pinned-compatible version.
    Update,
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
    /// Fetch another node's registered public key from the platform.
    Fetch {
        node_id: String,
        #[arg(long)]
        json: bool,
    },
    /// Exchange this node's public key for another node's key through the platform.
    Exchange {
        node_id: String,
        #[arg(long)]
        json: bool,
    },
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
    /// Quick health check: online status, node ID, peer count.
    Health {
        /// Dashboard URL of the running node.
        #[arg(long, default_value = "http://127.0.0.1:7327")]
        dashboard_url: String,
    },
}

/// Read-only commands for inspecting PRISM Fabric state.
///
/// **Trust is managed in the MARC27 platform UI, not from this CLI.** The
/// platform owns org / project / role definitions; PRISM nodes are clients
/// that use the platform-signed token to make cross-org requests. This
/// command surface is for *inspecting* what other nodes will see when
/// they verify your requests, not for granting trust.
///
/// See [crates/mesh/src/federation.rs] for the verify_peer() flow.
#[derive(Debug, Subcommand)]
enum FederationCommands {
    /// Print the identity that other nodes see when they verify your
    /// cross-org requests. Read-only; sourced from your local platform
    /// credentials.
    Whoami {
        /// Emit JSON instead of the human-readable summary.
        #[arg(long)]
        json: bool,
    },
    /// List known peer organizations the current user can interact with
    /// across the Fabric. Sourced from the MARC27 platform; trust is
    /// transitive via the platform root CA.
    Peers {
        /// Emit JSON instead of the human-readable summary.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum MarketplaceCommands {
    /// Search the MARC27 marketplace for tools and workflows.
    /// Aliases: `list`, `browse` — shorthand for an empty search.
    #[command(alias = "list", alias = "browse")]
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
    /// Semantic discovery — find marketplace tools/models/datasets by what
    /// they do, not by exact name. Wraps `POST /marketplace/find` which
    /// does RBAC-aware cosine search over the prism-resource-registry
    /// corpus.
    ///
    /// Use this when the curated tool list doesn't have what you need —
    /// the marketplace has the long tail (custom predictors, vendor MCPs,
    /// user-uploaded skills) that isn't worth listing in the prompt.
    Find {
        /// Natural-language description of what you're looking for.
        /// E.g. `"predict elastic moduli of a Ti-Al alloy"`.
        query: String,
        /// Restrict to specific resource_type values. Pass multiple times
        /// for an OR. Omit to search every type.
        #[arg(long = "type", value_name = "TYPE")]
        types: Vec<String>,
        /// Max number of hits to return. Typical: 3–10.
        #[arg(long, default_value_t = 5)]
        limit: usize,
        /// Return the raw JSON response instead of the human-readable
        /// summary. Useful from agent tools.
        #[arg(long)]
        json: bool,
    },
    /// Pull tool updates from the MARC27 marketplace. Re-downloads any
    /// tool whose marketplace version differs from the locally-installed
    /// one. Remote wins: locally-edited files are overwritten. Use
    /// `--dry-run` to see what would change without modifying anything.
    #[command(alias = "pull")]
    Update {
        /// Show what would be updated without downloading anything.
        #[arg(long)]
        dry_run: bool,
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
        /// GPU type to request. Omit for CPU-only deployments — a silent GPU
        /// default would claim hardware the target may not have and price the
        /// deployment at that GPU's rate.
        #[arg(long)]
        gpu: Option<String>,
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

/// Subcommands of `prism use`. See `chat_config::ChatTarget` for what
/// each variant ends up as in `~/.prism/config.toml`.
///
/// `Marc27` stays on the default route (MARC27 cloud) but pins which
/// upstream model MARC27 should serve. `Local` and `Provider` are the
/// two non-MARC27 chat targets. `Show` prints the current state (chat
/// target + tools auth state). `Reset` goes back to MARC27 cloud
/// without a pinned model (PRISM's compiled-in default).
#[derive(Debug, Subcommand)]
enum UseCommands {
    /// Stay on MARC27 cloud, but pin a specific upstream model
    /// (`gpt-5.5`, `claude-sonnet-4`, `mistral-large-latest`, …).
    /// MARC27's own vendor keys stay on the platform — PRISM only
    /// passes the model id forward.
    Marc27 {
        /// Upstream model id MARC27 should serve. If omitted,
        /// PRISM uses its compiled-in default.
        #[arg(long)]
        model: Option<String>,
    },
    /// Route chat turns to an OpenAI-compatible local server (Ollama,
    /// llama.cpp, vLLM, etc.). MARC27 platform tools stay available
    /// when the user is logged in.
    Local {
        /// Base URL of the local server, including `/v1`. Examples:
        /// `http://localhost:11434/v1`, `http://127.0.0.1:8080/v1`.
        #[arg(long)]
        url: String,
        /// Model name to send in chat requests (whatever the local
        /// server advertises — `llama-3.1-70b`, `mistral-7b-instruct`,
        /// `qwen2.5-coder`, etc.).
        #[arg(long)]
        model: String,
        /// Optional API key. Most local servers accept any non-empty
        /// string or none at all. Stored in plaintext in
        /// `~/.prism/config.toml` — only set this for trusted local
        /// servers; never put a cloud-vendor key here (use `provider`
        /// for that).
        #[arg(long)]
        api_key: Option<String>,
    },
    /// Route chat turns direct to a cloud vendor using the user's own
    /// API key (read from an env var, never persisted to disk).
    /// MARC27 platform tools stay available when the user is logged in.
    Provider {
        /// Vendor slug: `anthropic`, `openai`, `mistral`, `gemini`,
        /// `cohere`, …
        provider: String,
        /// Model id to send (e.g. `claude-sonnet-4`, `gpt-4o`).
        #[arg(long)]
        model: String,
        /// Override the env var name PRISM reads the API key from.
        /// Defaults to the vendor's standard env var
        /// (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, …).
        #[arg(long)]
        api_key_env: Option<String>,
    },
    /// Print the current chat target and the tools-auth state.
    Show,
    /// Reset chat target back to MARC27 cloud (the default).
    Reset,
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
    // Tracing goes to STDERR, never stdout: `prism backend` speaks JSON-RPC
    // over stdout, and any log line there corrupts the protocol (the TUI
    // deadlocks at "Igniting core..."). Stderr is captured to
    // ~/.prism/logs/backend.log by the TUI's spawn.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    // Project `.env` (the documented `.env.example` contract: provider API
    // keys, LLM_PROVIDER, LLM_MODEL) becomes env-var fallbacks for every
    // subcommand. dotenvy never overrides already-set vars, so real env
    // always wins. This file was previously dead — dotenvy sat unused in the
    // workspace deps and nothing loaded it, so `.env` settings silently did
    // nothing while compiled-in defaults took over.
    let _ = dotenvy::dotenv();

    // Provider keys saved via the TUI API-key window (~/.prism/api_keys.json)
    // become env-var fallbacks for every subcommand — chat, backend, doctor.
    // Real env vars always win.
    prism_ingest::llm::hydrate_env_from_api_keys();

    let mut cli = Cli::parse();
    let project_root = cli.project_root.clone();
    let endpoints = PlatformEndpoints::from_env();
    let paths = PrismPaths::discover()?;

    // Resolve Python: explicit --python wins; then the PRISM_PYTHON env
    // override (used by CI/the smoke harness to point at a pre-seeded
    // interpreter so an isolated $HOME never triggers venv provisioning,
    // which needs the network); otherwise manage ~/.prism/venv/.
    let python = if cli.python.as_os_str() != "python3" {
        cli.python.clone()
    } else if let Some(p) = std::env::var_os("PRISM_PYTHON").filter(|p| !p.is_empty()) {
        PathBuf::from(p)
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let prism_dir = PathBuf::from(&home).join(".prism");
        ensure_venv(&prism_dir, &project_root).await?
    };

    // ── Env-var mutations happen HERE, before ANY task is detached ──
    // POSIX setenv is not thread-safe against concurrent getenv; the first
    // detached task (tool auto-sync below) could read env while we write
    // (audit T3d: UB/torn reads). Hoisting also FIXES --offline for the
    // sync task itself: it checks PRISM_OFFLINE, which used to be set only
    // AFTER the task had already spawned.
    //
    // --offline applies to EVERY command, not just the bare-TUI shortcut:
    // it sets PRISM_OFFLINE=1, which PlatformClient's request helpers and
    // resolve_agent_auth() check before any platform HTTP. Pre-fix the flag
    // was parsed but never enforced — `prism --offline billing` happily hit
    // the live API (break-test defect H-3). The env var also propagates to
    // the spawned Python tool server so its platform client obeys too.
    if cli.offline {
        unsafe {
            std::env::set_var("PRISM_OFFLINE", "1");
        }
    }

    // Top-level flag shortcuts (--resume, --model, --auto-approve) when no
    // subcommand is given: they launch the TUI with the specified options.
    if cli.command.is_none() {
        if let Some(resume_id) = cli.resume.take() {
            // `prism --resume` or `prism --resume <id>` → acts like `prism resume`
            unsafe {
                match resume_id.as_deref() {
                    Some(raw_id) => std::env::set_var("PRISM_RESUME_ID", raw_id),
                    None => std::env::set_var("PRISM_RESUME_PICKER", "1"),
                }
            }
        }
        // --model override: set env var that build_llm_config reads
        if let Some(ref model) = cli.model {
            unsafe {
                std::env::set_var("LLM_MODEL", model);
            }
        }
        // --auto-approve: set env var that the backend reads
        if cli.auto_approve {
            unsafe {
                std::env::set_var("PRISM_AUTO_APPROVE", "1");
            }
        }
    }

    // Tool auto-sync: on every prism invocation, kick off a background
    // task that pulls tool updates from the MARC27 marketplace. This is
    // non-blocking — the actual sync happens in a detached tokio task
    // so startup (TUI/backend/CLI) isn't delayed by network I/O. If the
    // marketplace is unreachable, the task fails silently. The full
    // sync logic lives in `tool_sync::sync_tools`.
    //
    // Only fire for interactive commands (tui, backend, resume, chat)
    // where long-running sessions benefit from fresh tools. Skip for
    // one-shot commands like `marketplace`, `billing`, `doctor` to
    // avoid a network call on every trivial invocation.
    if matches!(
        cli.command,
        Some(Commands::Tui { .. })
            | Some(Commands::Backend { .. })
            | Some(Commands::Resume { .. })
            | Some(Commands::Campaign { .. })
            | None
    ) && let Ok(state) = paths.load_cli_state()
    {
        let token = state.credentials.as_ref().map(|c| c.access_token.clone());
        let platform = if let Some(t) = &token {
            prism_client::api::PlatformClient::new(&endpoints.api_base).with_token(t)
        } else {
            prism_client::api::PlatformClient::new(&endpoints.api_base)
        };
        crate::tool_sync::spawn_background_sync_owned(platform);
    }

    match cli.command.unwrap_or(Commands::Tui {
        fake_backend: false,
        scenario: "basic_chat".to_string(),
    }) {
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
                if (creds.user_id.is_none() || creds.display_name.is_none())
                    && let Ok(profile) = platform.fetch_current_user().await
                {
                    creds.user_id = Some(profile.id);
                    creds.display_name = profile.display_name;
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
            // Proactive token refresh.
            //
            // Two triggers, both attempted silently before the boot
            // checklist runs:
            //
            //   1. **Local expiry** — `expires_at` is in the past.
            //      The classic case; we know the token is stale.
            //
            //   2. **Within 5 min of expiry** — refresh early so the
            //      user doesn't watch the token expire mid-session.
            //
            // 401 from the platform during the boot check is handled
            // separately below — if /users/me rejects the token but
            // we have a refresh_token, we try refresh once more before
            // giving up and showing "run prism login".
            if let Some(creds) = state.credentials.as_ref()
                && !creds.refresh_token.is_empty()
                && creds
                    .expires_at
                    .is_some_and(|exp| chrono::Utc::now() + chrono::Duration::minutes(5) >= exp)
            {
                match refresh_access_token(&endpoints, creds).await {
                    Ok(new_creds) => {
                        state.credentials = Some(new_creds);
                        paths.save_cli_state(&state)?;
                        tracing::info!("access token refreshed proactively");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "proactive token refresh failed");
                        // Don't yell here — the boot check below will
                        // surface a precise message if the token is
                        // actually rejected.
                    }
                }
            }
            // Boot checklist. If Auth shows "token rejected" and we
            // still have a refresh_token we haven't tried yet (local
            // expires_at said fresh but server disagreed → server-side
            // rotation), try one more refresh + redo the check.
            let mut boot_checks =
                boot_checks::run_boot_checks(state.credentials.as_ref(), &endpoints).await;
            let auth_rejected = boot_checks
                .iter()
                .any(|c| c.name == "Auth" && c.result.starts_with("token rejected"));
            if auth_rejected
                && let Some(creds) = state.credentials.as_ref()
                && !creds.refresh_token.is_empty()
            {
                match refresh_access_token(&endpoints, creds).await {
                    Ok(new_creds) => {
                        tracing::info!("access token refreshed after server-side rejection");
                        state.credentials = Some(new_creds);
                        paths.save_cli_state(&state)?;
                        // Redo the boot checks with the new token.
                        boot_checks =
                            boot_checks::run_boot_checks(state.credentials.as_ref(), &endpoints)
                                .await;
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "refresh-on-rejection failed; user must re-login"
                        );
                    }
                }
            }
            boot::boot_sequence(&boot_checks);
            // Setup complete → drop straight into the interactive TUI, exactly
            // like `prism tui`: the native prism_tui frontend spawns `prism
            // backend` (the prism_agent loop). (Previously launched the
            // vendored forge chat surface.)
            let prism_bin =
                std::env::current_exe().context("failed to locate current prism executable")?;
            let platform = state.credentials.as_ref().map(|c| prism_tui::PlatformAuth {
                base_url: endpoints.api_base.clone(),
                token: c.access_token.clone(),
            });
            let config = prism_tui::RunConfig {
                backend_mode: prism_tui::BackendMode::Real {
                    prism_binary: prism_bin.to_str().unwrap().to_string(),
                    project_root: project_root.to_string_lossy().to_string(),
                    python_bin: python.to_string_lossy().to_string(),
                },
                platform,
                resume: None,
            };
            prism_tui::run_with_config(config).await?;
        }
        Commands::Login { token, no_browser } => {
            let mode = match token {
                Some(pat) => LoginMode::Token(pat),
                None => LoginMode::Device { no_browser },
            };
            perform_full_login(&paths, &endpoints, &python, mode).await?;
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
                        // User-facing surface is "prism"; the underlying chat
                        // harness is forge_main but that's an internal detail
                        // that doesn't belong in machine-readable output.
                        "chat_surface": "prism",
                        "workflow_runtime": "rust",
                    }
                }))?
            );
        }
        Commands::Workflow { command } => {
            handle_workflow_command(command, &project_root, &paths).await?;
        }
        Commands::Campaign { command } => {
            use prism_campaign::{Campaign, CampaignConfig, CampaignGoal};

            match command {
                CampaignCommands::Start {
                    goal,
                    elements,
                    objective,
                    max_iterations,
                    batch_size,
                    budget,
                    checkpoint_every,
                    approval_gates,
                    detach,
                } => {
                    let elements_vec = elements
                        .as_ref()
                        .map(|s| {
                            s.split(',')
                                .map(|e| e.trim().to_string())
                                .filter(|e| !e.is_empty())
                                .collect()
                        })
                        .unwrap_or_default();

                    let gates_vec = approval_gates
                        .as_ref()
                        .map(|s| {
                            s.split(',')
                                .filter_map(|g| g.trim().parse::<usize>().ok())
                                .collect()
                        })
                        .unwrap_or_default();

                    let campaign_goal = CampaignGoal {
                        description: goal.clone(),
                        elements: elements_vec,
                        objective: objective.clone().unwrap_or_default(),
                        constraints: Vec::new(),
                        seeds: Vec::new(),
                    };

                    let config = CampaignConfig {
                        max_iterations,
                        batch_size,
                        budget_usd: budget,
                        checkpoint_every,
                        approval_gate_at: gates_vec,
                        ..Default::default()
                    };

                    let campaign_id =
                        format!("campaign-{}", chrono::Utc::now().format("%Y%m%d-%H%M%S"));

                    println!("Starting campaign: {campaign_id}");
                    println!("Goal: {goal}");
                    if let Some(obj) = objective {
                        println!("Objective: {obj}");
                    }
                    println!("Max iterations: {max_iterations}, batch size: {batch_size}");
                    if let Some(b) = budget {
                        println!("Budget cap: ${b:.2}");
                    }
                    println!();

                    let mut campaign = Campaign::new(campaign_goal, config, campaign_id.clone());

                    if detach {
                        // Long-research mode: the goal id must exist on disk
                        // (and thus at GET /api/goals) before we return, then
                        // a background worker owns the loop. The caller —
                        // agent tool, HTTP endpoint, or a human — polls
                        // `campaign status` instead of blocking for hours.
                        campaign.checkpoint()?;
                        spawn_campaign_worker(&campaign_id)?;
                        println!("Detached: {campaign_id}");
                        println!("Checkpoint: ~/.prism/campaigns/{campaign_id}.json");
                        println!("Poll: prism campaign status {campaign_id}");
                    } else {
                        let result = campaign.run().await?;
                        println!("\n{}", result.summary);
                        println!("\nCheckpoint: ~/.prism/campaigns/{campaign_id}.json");
                    }
                }
                CampaignCommands::Resume { id, detach } => {
                    let home = std::env::var("HOME").unwrap_or_default();
                    let path = PathBuf::from(&home)
                        .join(".prism")
                        .join("campaigns")
                        .join(format!("{id}.json"));
                    let mut campaign = Campaign::from_checkpoint(&path)?;
                    if detach {
                        // Validate resumability BEFORE detaching so the
                        // caller gets the honest error, not a dead worker.
                        if campaign.state().completed {
                            anyhow::bail!(
                                "campaign '{id}' is completed ({}) — nothing to resume",
                                campaign.state().completion_reason
                            );
                        }
                        spawn_campaign_worker(&id)?;
                        println!("Detached: {id}");
                        println!("Poll: prism campaign status {id}");
                    } else {
                        println!("Resuming campaign: {id}");
                        let result = campaign.resume().await?;
                        println!("\n{}", result.summary);
                    }
                }
                CampaignCommands::Continue { id } => {
                    let home = std::env::var("HOME").unwrap_or_default();
                    let path = PathBuf::from(&home)
                        .join(".prism")
                        .join("campaigns")
                        .join(format!("{id}.json"));
                    let mut campaign = Campaign::from_checkpoint(&path)?;
                    if campaign.state().completed {
                        println!(
                            "Campaign '{id}' already completed: {}",
                            campaign.state().completion_reason
                        );
                    } else if campaign.state().paused {
                        let result = campaign.resume().await?;
                        println!("\n{}", result.summary);
                    } else {
                        let result = campaign.run().await?;
                        println!("\n{}", result.summary);
                    }
                }
                CampaignCommands::Status { id } => {
                    let home = std::env::var("HOME").unwrap_or_default();
                    let path = PathBuf::from(&home)
                        .join(".prism")
                        .join("campaigns")
                        .join(format!("{id}.json"));
                    let campaign = Campaign::from_checkpoint(&path)?;
                    let state = campaign.state();
                    println!("Campaign: {}", state.campaign_id);
                    println!("Goal: {}", state.goal.description);
                    println!(
                        "Status: {}",
                        if state.completed {
                            &state.completion_reason
                        } else if state.paused {
                            "paused (approval gate)"
                        } else {
                            "incomplete"
                        }
                    );
                    println!(
                        "Iterations: {} / {}",
                        state.current_iteration, state.config.max_iterations
                    );
                    println!("Candidates evaluated: {}", state.total_evaluated());
                    println!("Avg reward: {:.4}", state.avg_reward());
                    if let Some(best) = state.best() {
                        println!("Best: {} (reward={:.4})", best.composition, best.reward);
                    }
                }
                CampaignCommands::List => {
                    let home = std::env::var("HOME").unwrap_or_default();
                    let dir = PathBuf::from(&home).join(".prism").join("campaigns");
                    if !dir.is_dir() {
                        println!("No campaigns found ({} doesn't exist)", dir.display());
                        return Ok(());
                    }
                    let mut found = false;
                    for entry in std::fs::read_dir(&dir)? {
                        let entry = entry?;
                        let path = entry.path();
                        if path.extension().is_none_or(|e| e != "json") {
                            continue;
                        }
                        match Campaign::from_checkpoint(&path) {
                            Ok(c) => {
                                let s = c.state();
                                let status = if s.completed {
                                    &s.completion_reason
                                } else if s.paused {
                                    "paused"
                                } else {
                                    "incomplete"
                                };
                                println!(
                                    "  {} — {} — iter {}/{} — {} candidates — {}",
                                    s.campaign_id,
                                    status,
                                    s.current_iteration,
                                    s.config.max_iterations,
                                    s.total_evaluated(),
                                    s.goal.description
                                );
                                found = true;
                            }
                            Err(e) => {
                                tracing::warn!(path = %path.display(), error = %e, "skipping unreadable campaign file");
                            }
                        }
                    }
                    if !found {
                        println!("No campaigns found in {}", dir.display());
                    }
                }
            }
        }
        Commands::Notebook { command } => match command {
            NotebookCommands::Start { port } => {
                let session = notebook::start(port, None)?;
                println!("Notebook started:");
                println!("  URL:   {}", session.url);
                println!("  PID:   {}", session.pid);
                println!("  Port:  {}", session.port);
                println!("  Token: {}", session.token);
                println!("\nOpen the URL in your browser or IDE.");
            }
            NotebookCommands::List => {
                let sessions = notebook::list()?;
                if sessions.is_empty() {
                    println!("No active notebooks.");
                } else {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs_f64();
                    println!("{:<8} {:<8} {:<10} URL", "PID", "PORT", "UPTIME");
                    for s in &sessions {
                        let up = now - s.started_at;
                        let h = (up as u64) / 3600;
                        let m = ((up as u64) % 3600) / 60;
                        let up_s = if h > 0 {
                            format!("{h}h{m}m")
                        } else {
                            format!("{m}m")
                        };
                        println!("{:<8} {:<8} {:<10} {}", s.pid, s.port, up_s, s.url);
                    }
                }
            }
            NotebookCommands::Stop { target } => {
                let count = notebook::stop(&target)?;
                if count > 0 {
                    println!("Stopped {count} notebook(s).");
                } else {
                    println!("No matching notebooks found.");
                }
            }
        },
        Commands::Pyiron { command } => match command {
            PyironCommands::Status => match pyiron_cmd::status()? {
                Some(v) => println!("PyIron {v} (venv: ~/.prism/venv)"),
                None => println!(
                    "PyIron is not installed. Run `prism pyiron install` — \
                     simulation tools will also auto-install it on first use."
                ),
            },
            PyironCommands::Install => println!("{}", pyiron_cmd::install()?),
            PyironCommands::Update => println!("{}", pyiron_cmd::update()?),
        },
        Commands::Backend {
            project_root: backend_pr,
            python: backend_py,
        } => {
            use prism_ingest::LlmConfig;

            // Load from prism.toml [llm] section, env vars as overrides
            let node_config = prism_core::config::NodeConfig::load(Some(&backend_pr));
            let cfg_llm = &node_config.llm;

            // Also load ~/.prism/config.toml [chat] — the user-visible
            // chat target set by `prism use local/provider/marc27`.
            // If the user configured a local or direct-provider target,
            // that takes precedence over prism.toml [llm] for the agent
            // backend's LLM endpoint. This unifies the two config worlds
            // so `prism use local` actually affects `prism backend`.
            let chat_target = crate::chat_config::load().unwrap_or_default().chat;

            // The session's platform JWT — the credential the MARC27 LLM
            // proxy authenticates.
            let platform_token = paths
                .load_cli_state()
                .ok()
                .and_then(|s| s.credentials)
                .map(|c| c.access_token);

            // Generic key chain for the local/direct-provider targets.
            // Provider keys (ANTHROPIC/OPENAI) belong ONLY here — never on
            // the marc27 arm: now that the project `.env` is actually
            // loaded, an ANTHROPIC_API_KEY in it would otherwise shadow the
            // platform JWT and 401 every platform LLM call.
            let api_key = std::env::var("LLM_API_KEY")
                .or_else(|_| std::env::var("MARC27_TOKEN"))
                .or_else(|_| std::env::var("ANTHROPIC_API_KEY"))
                .or_else(|_| std::env::var("OPENAI_API_KEY"))
                .ok()
                .or_else(|| cfg_llm.resolve_api_key())
                .or_else(|| platform_token.clone());

            // Platform model catalog, fetched ONCE (fail-open: empty when
            // offline). Serves both marc27 model resolution and the limits
            // lookup below.
            let catalog = fetch_model_catalog(&paths).await;

            // Resolve base_url, model, and api_key from the chat target
            // when it overrides the prism.toml [llm] defaults.
            let (base_url, model, api_key) = match &chat_target {
                crate::chat_config::ChatTarget::Local {
                    url,
                    model,
                    api_key: local_key,
                } => (url.clone(), model.clone(), local_key.clone().or(api_key)),
                crate::chat_config::ChatTarget::Provider {
                    provider,
                    model,
                    api_key_env,
                } => {
                    let env_name = api_key_env.as_deref().unwrap_or_else(|| {
                        crate::chat_config::ChatTarget::default_api_key_env(provider)
                    });
                    let provider_key = std::env::var(env_name).ok();
                    (
                        format!(
                            "https://api.{provider}.com/v1",
                            provider = provider.to_ascii_lowercase()
                        ),
                        model.clone(),
                        provider_key.or(api_key),
                    )
                }
                // Marc27 cloud. Two bugs were hiding here:
                //
                //   1. The picked model was discarded (`Marc27 { .. }`), so
                //      the backend served the default even when `/use show`
                //      reported a selection (status bar said gpt-5.5 while
                //      the palette said sonnet).
                //   2. The base URL fell through to `cfg_llm.url`, whose
                //      default is `http://localhost:8080` (llama.cpp). With
                //      no prism.toml the "cloud" chat therefore ran on a
                //      LOCAL model — silently, with the header still showing
                //      the cloud model and credits never moving. It only
                //      "worked" because a local llama.cpp happened to be up;
                //      its 16k context is what produced the mystery
                //      exceed_context_size_error.
                //
                // Now the base URL is the signed-in project's MARC27 LLM
                // endpoint. The agent's LLM client recognises the `/llm`
                // segment and drives it over MARC27's native `/stream` SSE
                // protocol, so the cloud picks the real model and enforces
                // its real context + output limits.
                //
                // The model is resolved on TWO separate axes — LLM_PROVIDER
                // and LLM_MODEL (from the env / project .env) — against the
                // platform catalog, because the same model can be served by
                // more than one provider at different prices and billing
                // paths (claude-sonnet-5 direct-anthropic vs the OpenRouter
                // entry). Nothing is hardcoded: with no preference anywhere
                // the platform's own `default` catalog alias decides.
                crate::chat_config::ChatTarget::Marc27 {
                    model: target_model,
                } => {
                    let preference = resolve_marc27_model(
                        std::env::var("LLM_MODEL").ok(),
                        target_model.as_deref(),
                        cfg_llm.model.as_deref(),
                    );
                    let provider = std::env::var("LLM_PROVIDER").ok();
                    let model =
                        resolve_catalog_model(&catalog, provider.as_deref(), preference.as_deref())
                            .unwrap_or_else(|| {
                                // Catalog unavailable (offline) or no match: send
                                // the preference verbatim; with none at all, send
                                // the literal `default` alias — the platform
                                // resolves it server-side.
                                preference.unwrap_or_else(|| "default".to_string())
                            });
                    // Bearer for the platform: deliberate overrides
                    // (LLM_API_KEY, MARC27_TOKEN) → the session JWT.
                    // Provider keys are NOT platform credentials.
                    let marc27_key = std::env::var("LLM_API_KEY")
                        .or_else(|_| std::env::var("MARC27_TOKEN"))
                        .ok()
                        .or_else(|| platform_token.clone());
                    (
                        marc27_llm_base_url(&paths, &endpoints.api_base, &cfg_llm.url),
                        model,
                        marc27_key,
                    )
                }
            };

            // The model's real limits from the platform catalog. Drives
            // the agent's context budget: compaction fires on token
            // pressure against THIS window, not a guessed constant.
            // (None, None) for unknown models (local llama.cpp, offline)
            // → the agent falls back to turn-count compaction.
            let (context_window, max_output_tokens) = model_limits(&catalog, &model);
            tracing::info!(?context_window, ?max_output_tokens, model = %model, "model limits");

            let llm_config = LlmConfig {
                base_url,
                model,
                api_key,
                embedding_model: cfg_llm.embedding_model.clone(),
                context_window,
                max_output_tokens,
                ..Default::default()
            };

            let mut tool_server_env = std::collections::BTreeMap::new();
            tool_server_env.insert("PRISM_ENABLE_MCP".to_string(), "1".to_string());

            // Platform auth for the Python tool server: do NOT export the
            // session JWT as MARC27_API_KEY. That env var is the X-API-Key
            // channel (stable `m27_*` keys); the server rejects a JWT there
            // with 401 "invalid API key", which broke every Python platform
            // tool (knowledge semantic/graph, research) while Rust-side
            // Bearer calls kept working. The Python side reads the rotating
            // JWT itself from ~/.prism/credentials.json (kept in sync by
            // login/refresh) and re-reads it on 401, which a frozen env
            // snapshot can never do. A genuine user-set MARC27_API_KEY still
            // reaches the tool server through normal env inheritance.
            tool_server_env.insert("MARC27_API_URL".to_string(), endpoints.api_base.clone());

            // Pass through any API keys the user has set
            for key in &[
                "MP_API_KEY",
                "LENS_API_TOKEN",
                "OPENAI_API_KEY",
                "ANTHROPIC_API_KEY",
                "FIRECRAWL_API_KEY",
            ] {
                if let Ok(val) = std::env::var(key) {
                    tool_server_env.insert(key.to_string(), val);
                }
            }

            let tool_server = prism_python_bridge::ToolServer {
                python_bin: backend_py,
                project_root: backend_pr,
                env: tool_server_env,
            };

            prism_agent::protocol::run_server(llm_config, tool_server).await?;
        }
        Commands::IpcServe {
            project_root: ipc_pr,
            python: ipc_py,
        } => {
            // Thin adapter: spawn `prism backend` (this same binary) and expose
            // its native protocol to an external frontend as a minimal JSON-RPC
            // surface on our stdin/stdout. Tracing already goes to stderr, so
            // stdout stays a clean protocol channel.
            let prism_bin =
                std::env::current_exe().context("failed to locate current prism executable")?;
            let bridge = prism_ipc::BackendBridge::spawn(
                prism_bin
                    .to_str()
                    .context("prism executable path is not valid UTF-8")?,
                &ipc_pr.to_string_lossy(),
                &ipc_py.to_string_lossy(),
            )
            .await?;
            prism_ipc::serve_stdio(bridge).await?;
        }
        Commands::Tools => {
            let mut tool_server_env = std::collections::BTreeMap::new();
            tool_server_env.insert("PRISM_ENABLE_MCP".to_string(), "1".to_string());
            let server = ToolServer {
                python_bin: python.clone(),
                project_root: project_root.clone(),
                env: tool_server_env,
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
        Commands::McpServerNative => {
            mcp_server_native::run(project_root.clone(), python.clone()).await?;
        }
        Commands::Doctor => {
            doctor::run(&project_root, &python).await?;
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
                    unsafe {
                        std::env::set_var("PRISM_DATA_PATHS", combined);
                    }
                }
                if !model_paths.is_empty() {
                    let existing = std::env::var("PRISM_MODEL_PATHS").unwrap_or_default();
                    let combined = if existing.is_empty() {
                        model_paths.join(",")
                    } else {
                        format!("{},{}", existing, model_paths.join(","))
                    };
                    unsafe {
                        std::env::set_var("PRISM_MODEL_PATHS", combined);
                    }
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
                    unsafe {
                        std::env::set_var("PRISM_NODE_SERVE_MODEL", model);
                    }
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
                        || svc_config.spark.is_some()
                        || svc_config.firecrawl.is_some();

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

                // Wire core databases (RBAC + audit + sessions).
                //
                // Bug #21: in `--offline` mode, leaving session_db_path
                // configured forces every request to validate against
                // an empty SessionManager → 401 on /api/mesh/publish,
                // /api/mesh/subscribe, /api/audit. The middleware in
                // crates/server/src/middleware/auth.rs already has a
                // localhost-only fallback path (line 99-104) when no
                // session DB is configured: any non-empty token is
                // accepted as the user_id. We use that path in offline
                // mode so `tests/test_mesh_e2e.sh` and similar scripts
                // can pass `Authorization: Bearer test-token` (any
                // value works) and exercise the API surface end-to-end.
                let state_dir = &paths.state_dir;
                std::fs::create_dir_all(state_dir)?;
                server_node_state.audit_db_path = Some(state_dir.join("audit.db"));

                // Two-layer auth (session + RBAC) is bypassed in
                // `--offline` mode: any non-empty token works, and
                // the resolve_role middleware grants synthetic
                // NodeAdmin. See Bug #21 in docs/SHIPPED.md.
                if offline {
                    server_node_state.rbac_db_path = None;
                    server_node_state.session_db_path = None;
                } else {
                    server_node_state.rbac_db_path = Some(state_dir.join("rbac.db"));
                    server_node_state.session_db_path = Some(state_dir.join("sessions.db"));
                }

                // Subscription store: persist when connected to platform,
                // ephemeral (in-memory) in offline mode.
                //
                // Bug #20: pre-fix, EVERY `prism node up --offline` opened
                // the same SQLite file at state_dir/subscriptions.db, so a
                // dataset published in one test run showed up as a phantom
                // entry in the next fresh run's `/api/mesh/subscriptions`.
                // `--offline` now uses an in-memory SubscriptionManager —
                // each offline run starts clean. Persistent state is only
                // for runs that are part of an actual federated mesh.
                let subscription_mgr = if offline {
                    prism_mesh::subscription::SubscriptionManager::new()
                } else {
                    prism_mesh::subscription::SubscriptionManager::open(
                        &state_dir.join("subscriptions.db"),
                    )
                    .unwrap_or_else(|e| {
                        eprintln!(
                            "  Warning: Failed to open subscription store: {e} (using in-memory state)"
                        );
                        prism_mesh::subscription::SubscriptionManager::new()
                    })
                };
                server_node_state.subscriptions =
                    std::sync::Arc::new(std::sync::RwLock::new(subscription_mgr));

                // Scan for tools
                let tools_dir = paths.config_dir.join("tools");
                if tools_dir.is_dir()
                    && let Ok(mut reg) = server_node_state.tool_registry.write()
                {
                    let _ = reg.scan_directory(&tools_dir);
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

                // Wire LLM config from config.toml [chat] (prism use local)
                // falling back to prism.toml [indexer], then defaults.
                {
                    let chat_target = crate::chat_config::load().unwrap_or_default().chat;
                    let (base_url, model, api_key) = match &chat_target {
                        crate::chat_config::ChatTarget::Local {
                            url,
                            model,
                            api_key: local_key,
                        } => (
                            url.clone(),
                            model.clone(),
                            local_key.clone().or_else(|| {
                                prism_core::config::NodeConfig::resolve_api_key(
                                    &node_config.indexer,
                                )
                            }),
                        ),
                        crate::chat_config::ChatTarget::Provider {
                            provider,
                            model,
                            api_key_env,
                        } => {
                            let env_name = api_key_env.as_deref().unwrap_or_else(|| {
                                crate::chat_config::ChatTarget::default_api_key_env(provider)
                            });
                            (
                                format!(
                                    "https://api.{provider}.com/v1",
                                    provider = provider.to_ascii_lowercase()
                                ),
                                model.clone(),
                                std::env::var(env_name).ok().or_else(|| {
                                    prism_core::config::NodeConfig::resolve_api_key(
                                        &node_config.indexer,
                                    )
                                }),
                            )
                        }
                        crate::chat_config::ChatTarget::Marc27 { .. } => {
                            let api_key = prism_core::config::NodeConfig::resolve_api_key(
                                &node_config.indexer,
                            );
                            let base_url =
                                node_config.indexer.uri.clone().unwrap_or_else(
                                    || match node_config.indexer.mode.as_str() {
                                        "platform" | "marc27" | "external" => {
                                            node_config.platform.url.clone() + "/llm"
                                        }
                                        _ => "http://localhost:8080".into(),
                                    },
                                );
                            let model = node_config
                                .indexer
                                .model
                                .clone()
                                .unwrap_or_else(|| "gemma-3-27b".into());
                            (base_url, model, api_key)
                        }
                    };
                    server_node_state.llm = Some(prism_ingest::LlmConfig {
                        base_url,
                        model,
                        api_key,
                        embedding_model: node_config.indexer.embedding_model.clone(),
                        ..Default::default()
                    });
                }

                // ── Platform registration (unless --offline) ──
                let mut daemon_platform_client: Option<PlatformClient> = None;
                let mut daemon_platform_node_id: Option<String> = None;
                let mut daemon_org_id: Option<String> = None;

                // Load CLI state for mesh auth — even in offline mode,
                // we need credentials to join the mesh. The mesh refuses
                // to start without an auth token (RBAC gate).
                let cli_state = paths.load_cli_state().ok().unwrap_or_default();
                let mesh_auth_token = cli_state
                    .credentials
                    .as_ref()
                    .map(|c| c.access_token.clone());
                let mesh_has_auth = mesh_auth_token.is_some();

                if !offline {
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
                        eprintln!(
                            "  Warning: No credentials — run `prism setup` first to register with platform."
                        );
                    }
                }

                // ── Cross-org audit envelopes (F5) ──
                // Reuse the node's own Ed25519 identity key (the same one
                // that signs federation + SSH claims) so envelopes verify
                // against the node's platform-signed identity — never a
                // second identity. One emitter is shared by the HTTP
                // federation middleware (via NodeState) and the daemon's
                // platform-relay handler (via DaemonOptions), so both
                // cross-org receive paths write one identity + one
                // append-only log. `audit.enabled = false` opts out.
                let audit_emitter: Option<std::sync::Arc<prism_audit::AuditEmitter>> =
                    if node_config.audit.enabled {
                        match prism_node::crypto::load_or_generate_signing_key(state_dir) {
                            Ok((signing_key, _)) => {
                                let audit_node_id = daemon_platform_node_id
                                    .clone()
                                    .unwrap_or_else(|| node_name.clone());
                                let audit_org_id =
                                    daemon_org_id.clone().unwrap_or_else(|| "local".to_string());
                                Some(std::sync::Arc::new(prism_audit::AuditEmitter::new(
                                    audit_node_id,
                                    audit_org_id,
                                    signing_key,
                                    state_dir.join("audit-envelopes.jsonl"),
                                    true,
                                )))
                            }
                            Err(e) => {
                                eprintln!(
                                    "  Warning: audit envelopes disabled (signing key load failed: {e})"
                                );
                                None
                            }
                        }
                    } else {
                        None
                    };
                server_node_state.federation_audit = audit_emitter.clone();

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

                // ── Conversational agent service (POST /api/chat) ──
                // Chat-app parity: the SAME agent loop the TUI backend runs
                // (prism_agent::service::ChatService shares build_agent_seed
                // + agent_loop::run_turn with `prism backend`), exposed as an
                // HTTP service on this node. Spawned in the background AFTER
                // the listener is up so the command-tool catalog's node probe
                // sees the dashboard live and a slow Python spawn never
                // delays node boot. On failure /api/chat answers 503.
                if server_state.llm.is_some() {
                    println!(
                        "  ~ {:<12} http://localhost:{}/api/chat (starting)",
                        "Chat", dashboard_port
                    );
                    let chat_state = server_state.clone();
                    let chat_python = python.clone();
                    let chat_project_root = project_root.clone();
                    let chat_api_base = endpoints.api_base.clone();
                    tokio::spawn(async move {
                        let llm_config = chat_state
                            .llm
                            .clone()
                            .expect("checked is_some before spawn");
                        let tool_server = prism_python_bridge::ToolServer {
                            python_bin: chat_python,
                            project_root: chat_project_root,
                            env: prism_agent::service::default_tool_server_env(&chat_api_base),
                        };
                        match prism_agent::service::ChatService::spawn(
                            llm_config,
                            tool_server,
                            None,
                        )
                        .await
                        {
                            Ok(service) => {
                                let _ = chat_state.chat.set(std::sync::Arc::new(service));
                                tracing::info!("chat service ready — POST /api/chat");
                            }
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    "chat service failed to start — /api/chat returns 503"
                                );
                            }
                        }
                    });
                } else {
                    tracing::warn!("no LLM configured — /api/chat disabled (503)");
                }
                println!();

                // ── Tool-call relay executor ──────────────────────────────
                // The daemon (prism-node) stays thin: it forwards each relayed
                // `InvokeTool` over this channel. Here — where we own the
                // ChatService — we run the named tool through the SAME executor
                // the agent loop uses (`invoke_tool`) and reply. This is the
                // "someone in Poland runs a tool on my Mac through the node"
                // path: platform gates node visibility, the tool runs locally,
                // the real caller is propagated for audit.
                let (tool_invoke_tx, mut tool_invoke_rx) =
                    tokio::sync::mpsc::channel::<prism_node::daemon::ToolInvocationRequest>(32);
                let relay_state = server_state.clone();
                tokio::spawn(async move {
                    while let Some(req) = tool_invoke_rx.recv().await {
                        let caller = req.caller_user_id.to_string();
                        let result = match relay_state.chat.get() {
                            // approve=false ALWAYS: a remote relay caller can
                            // never run approval-gated tools on this machine.
                            Some(chat) => chat
                                .invoke_tool(&req.tool, req.args, Some(caller.as_str()), false)
                                .await
                                .map_err(|e| e.to_string()),
                            None => Err("tool executor not ready \
                                 (chat service still starting, or node has no LLM configured)"
                                .to_string()),
                        };
                        // Receiver gone (daemon task cancelled / timed out) is fine.
                        let _ = req.reply.send(result);
                    }
                });

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
                    offline,
                    tool_invoker: Some(tool_invoke_tx),
                    audit_emitter,
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
                        auth_token: mesh_auth_token,
                    },
                    mesh_cancel.clone(),
                );
                // Initialize federated query client for cross-mesh searches
                let _ = server_state
                    .federation
                    .set(prism_mesh::federated_query::FederatedQuery::default());

                if !mesh_has_auth {
                    println!("  \u{26A0} Mesh: disabled (no auth — run `prism login` to enable)");
                } else if broadcast {
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
                KeyCommands::Fetch { node_id, json } => {
                    let (api_base, auth) = resolve_agent_auth()?;
                    let client = reqwest::Client::builder()
                        .timeout(Duration::from_secs(30))
                        .build()?;
                    let response: serde_json::Value = auth
                        .apply(client.get(format!("{api_base}/nodes/{node_id}/public-key")))
                        .send()
                        .await?
                        .error_for_status()?
                        .json()
                        .await?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&response)?);
                    } else {
                        println!(
                            "Node: {}",
                            value_string(&response, &["name"]).unwrap_or("unknown")
                        );
                        println!(
                            "Node ID: {}",
                            value_string(&response, &["node_id", "id"]).unwrap_or(&node_id)
                        );
                        println!(
                            "Algorithm: {}",
                            value_string(&response, &["algorithm"]).unwrap_or("x25519")
                        );
                        println!(
                            "Public key: {}",
                            value_string(&response, &["public_key"]).unwrap_or("")
                        );
                    }
                }
                KeyCommands::Exchange { node_id, json } => {
                    let (_secret, public) =
                        prism_node::crypto::load_or_generate_key(&paths.state_dir)?;
                    let our_public_key = prism_node::crypto::encode_public_key(&public);
                    let (api_base, auth) = resolve_agent_auth()?;
                    let client = reqwest::Client::builder()
                        .timeout(Duration::from_secs(30))
                        .build()?;
                    let response: serde_json::Value = auth
                        .apply(client.post(format!("{api_base}/nodes/{node_id}/exchange-key")))
                        .json(&serde_json::json!({
                            "public_key": our_public_key,
                        }))
                        .send()
                        .await?
                        .error_for_status()?
                        .json()
                        .await?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&response)?);
                    } else {
                        println!(
                            "Target node ID: {}",
                            value_string(&response, &["target_node_id", "node_id", "id"])
                                .unwrap_or(&node_id)
                        );
                        println!(
                            "Algorithm: {}",
                            value_string(&response, &["algorithm"]).unwrap_or("x25519")
                        );
                        println!(
                            "Target public key: {}",
                            value_string(&response, &["target_public_key", "public_key"])
                                .unwrap_or("")
                        );
                        println!(
                            "Public key sent: {}",
                            value_string(&response, &["your_public_key_received"]).unwrap_or("")
                        );
                    }
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
                    path,
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
                    path,
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
                // LLM config only needed for --semantic (embedding generation).
                // --cypher and NL neighbor queries hit Neo4j directly.
                let llm_cfg = if semantic {
                    Some(build_llm_config(
                        &project_root,
                        llm_url.as_deref(),
                        model.as_deref(),
                        api_key.as_deref(),
                    )?)
                } else {
                    build_llm_config(
                        &project_root,
                        llm_url.as_deref(),
                        model.as_deref(),
                        api_key.as_deref(),
                    )
                    .ok()
                };
                handle_query(
                    &text,
                    cypher,
                    semantic,
                    &neo4j_url,
                    &neo4j_user,
                    &neo4j_pass,
                    &qdrant_url,
                    llm_cfg.as_ref(),
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
        Commands::Federation { command } => {
            handle_federation_command(command, &paths).await?;
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
                    // Empty query → list all resources instead of searching
                    let tools = if query.as_deref().is_none_or(|q| q.is_empty()) {
                        marketplace.list_tools(None).await?
                    } else {
                        marketplace.list_tools(query.as_deref()).await?
                    };
                    if tools.is_empty() {
                        println!("No results found.");
                    } else {
                        println!("Marketplace resources:\n");
                        for t in &tools {
                            let author = t.author.as_deref().unwrap_or("MARC27");
                            // Print the slug — it's the identifier install/info
                            // take; the footer told users to install "<slug>"
                            // without ever showing one.
                            println!(
                                "  {} [{}] ({})  by {}  [{}]",
                                t.name, t.slug, t.resource_type, author, t.pricing
                            );
                            println!("    {}", t.description);
                            if !t.tags.is_empty() {
                                println!("    tags: {}", t.tags.join(", "));
                            }
                            println!();
                        }
                        println!(
                            "{} resources found. Install with: prism marketplace install <slug>",
                            tools.len()
                        );
                    }
                }
                MarketplaceCommands::Install { name, workflow } => {
                    // Reject names containing path separators / parent refs.
                    // Without this, `prism marketplace install ../../../foo`
                    // would write to ~/.prism/tools/../../../foo.py — a self-
                    // inflicted path traversal. Marketplace slugs are always
                    // simple identifiers in practice, so the restriction is
                    // safe and surfaces typos early.
                    if name.contains('/')
                        || name.contains('\\')
                        || name.contains("..")
                        || name.starts_with('.')
                    {
                        anyhow::bail!(
                            "Invalid marketplace name '{name}'. Names must be simple slugs \
                             (no `/`, `\\`, `..`, or leading `.`)."
                        );
                    }

                    let url = marketplace.install_url(&name).await?;
                    let client = reqwest::Client::new();
                    // error_for_status() converts 4xx/5xx into Err so a 404
                    // doesn't end up saved as a Python file. Previously the
                    // download path happily wrote HTML 404 pages as `.py`,
                    // which then auto-loaded on next launch and crashed the
                    // tool router with a syntax error.
                    let content = client
                        .get(&url)
                        .send()
                        .await?
                        .error_for_status()
                        .with_context(|| format!("downloading {name} from {url}"))?
                        .text()
                        .await?;

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

                    // Refuse to silently overwrite a local edit. Marketplace
                    // installs are expected to be additive; if the user
                    // wants the upstream version they can `rm` the file
                    // first or pass a future `--force` flag.
                    if dest.exists() {
                        anyhow::bail!(
                            "Refusing to overwrite existing file at {}. \
                             Remove it first if you want the marketplace version.",
                            dest.display()
                        );
                    }

                    std::fs::write(&dest, &content)?;
                    let kind = if workflow { "workflow" } else { "tool" };
                    println!("Installed {kind} '{name}' to {}", dest.display());
                    println!("It will be auto-discovered on next prism run.");
                }
                MarketplaceCommands::Find {
                    query,
                    types,
                    limit,
                    json,
                } => {
                    let hits = marketplace.find_tool(&query, &types, limit).await?;
                    if json {
                        // Stable structured output for agent-tool consumption.
                        println!("{}", serde_json::to_string_pretty(&hits)?);
                    } else if hits.is_empty() {
                        println!("No semantic matches for `{query}`.");
                        println!(
                            "Try a different phrasing, or `prism marketplace search <query>` for \
                             lexical search."
                        );
                    } else {
                        println!("Top {} marketplace matches for `{query}`:\n", hits.len());
                        for hit in &hits {
                            let display = if hit.display_name.is_empty() {
                                &hit.canonical_name
                            } else {
                                &hit.display_name
                            };
                            // score uses 2-decimal width so all rows line up under "score=0.91"
                            println!(
                                "  score={:.2}  {}  [{}]  ← {}",
                                hit.score, hit.canonical_name, hit.category, display,
                            );
                            if !hit.description.is_empty() {
                                println!("    {}", hit.description);
                            }
                            if !hit.execution_target.is_empty() {
                                println!("    execution_target: {}", hit.execution_target);
                            }
                            println!();
                        }
                        println!(
                            "Invoke a hit by its canonical_name. Cite both name and score in your \
                             final answer."
                        );
                    }
                }
                MarketplaceCommands::Info { name } => {
                    // Slugs are lowercase; users copy display names like
                    // "MACE-MH-1" from search output, so retry lowercased
                    // before surfacing a 404.
                    let tool = match marketplace.get_tool(&name).await {
                        Ok(t) => t,
                        Err(e) if name.chars().any(|c| c.is_ascii_uppercase()) => {
                            match marketplace.get_tool(&name.to_ascii_lowercase()).await {
                                Ok(t) => t,
                                Err(_) => return Err(e),
                            }
                        }
                        Err(e) => return Err(e),
                    };
                    println!("Name:        {}", tool.name);
                    println!("Slug:        {}", tool.slug);
                    println!("Type:        {}", tool.resource_type);
                    println!("Version:     {}", tool.version);
                    println!(
                        "Author:      {}",
                        tool.author.as_deref().unwrap_or("MARC27")
                    );
                    println!("Description: {}", tool.description);
                    println!("Pricing:     {}", tool.pricing);
                    println!("Downloads:   {}", tool.download_count);
                    if !tool.tags.is_empty() {
                        println!("Tags:        {}", tool.tags.join(", "));
                    }
                }
                MarketplaceCommands::Update { dry_run } => {
                    if dry_run {
                        let pending = crate::tool_sync::check_for_updates(&marketplace).await?;
                        if pending.is_empty() {
                            println!("All installed tools are up to date.");
                        } else {
                            println!("{} tool update(s) available:", pending.len());
                            for (slug, local, remote) in &pending {
                                let from = if local.is_empty() {
                                    "(not installed)"
                                } else {
                                    local
                                };
                                println!("  {slug}: {from} → {remote}");
                            }
                        }
                    } else {
                        // Also prune stale manifest entries before syncing.
                        if let Err(e) = crate::tool_sync::prune_manifest() {
                            tracing::warn!(error = %e, "manifest prune failed (non-fatal)");
                        }
                        let report = crate::tool_sync::sync_tools(&marketplace).await?;
                        crate::tool_sync::print_report(&report);
                    }
                }
            }
        }
        Commands::Research { query, depth, json } => {
            let (api_base, auth) = resolve_agent_auth()?;
            // Target /agent-runs — the durable research orchestrator that IS
            // deployed and goes through marc27_core::research::engine (all
            // safety gates included). The `/research` verb-shim this command
            // originally targeted was PR #50, which was CLOSED unmerged —
            // the endpoint never existed in prod (every call 404'd). The
            // Python tool layer (app/tools/agent_runs.py) already uses
            // /agent-runs; this mirrors it: create run, poll until terminal.
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()?;
            let created: serde_json::Value = auth
                .apply(client.post(format!("{api_base}/agent-runs")))
                // Keep smoke tests cheap by always making depth explicit.
                .json(&serde_json::json!({ "question": query, "depth": depth }))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            let run_id = created
                .get("id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .ok_or_else(|| anyhow::anyhow!("platform did not return a run id: {created}"))?;
            eprintln!("research run {run_id} started; waiting for completion…");

            // Poll until terminal ("completed" | "failed" | "canceled").
            // 10 min ceiling matches the server-side run budget; dots to
            // stderr so stdout stays a single clean JSON/answer document.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(630);
            let resp = loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                let run: serde_json::Value = auth
                    .apply(client.get(format!("{api_base}/agent-runs/{run_id}")))
                    .send()
                    .await?
                    .error_for_status()?
                    .json()
                    .await?;
                let state = run.get("state").and_then(|s| s.as_str()).unwrap_or("");
                match state {
                    "completed" => {
                        break serde_json::json!({
                            "run_id": run_id,
                            "answer": run.get("answer").cloned().unwrap_or(serde_json::Value::Null),
                            "sources": run.get("params").and_then(|p| p.get("sources")).cloned()
                                .unwrap_or_else(|| serde_json::json!([])),
                        });
                    }
                    "failed" | "canceled" => {
                        let err = run
                            .get("error")
                            .and_then(|e| e.as_str())
                            .unwrap_or("(no error detail)");
                        anyhow::bail!("research run {run_id} {state}: {err}");
                    }
                    _ => {
                        if std::time::Instant::now() >= deadline {
                            anyhow::bail!(
                                "research run {run_id} still '{state}' after 10 min; \
                                 check later with: prism agent (check_background_research)"
                            );
                        }
                        eprint!(".");
                        use std::io::Write as _;
                        let _ = std::io::stderr().flush();
                    }
                }
            };
            eprintln!();

            if json {
                println!("{}", serde_json::to_string_pretty(&resp)?);
            } else {
                if let Some(answer) = resp.get("answer").and_then(|a| a.as_str()) {
                    println!("{answer}");
                }
                if let Some(sources) = resp.get("sources").and_then(|s| s.as_array())
                    && !sources.is_empty()
                {
                    println!("\nSources:");
                    for src in sources {
                        if let Some(title) = src.get("title").and_then(|t| t.as_str()) {
                            let url = src.get("url").and_then(|u| u.as_str()).unwrap_or("");
                            println!("  - {title} {url}");
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
        Commands::Predict {
            model,
            task,
            input,
            node_id,
            gpu,
            budget,
            ready_timeout_secs,
            keep,
        } => {
            handle_predict(
                &model,
                &task,
                &input,
                node_id.as_deref(),
                gpu.as_deref(),
                budget,
                ready_timeout_secs,
                keep,
            )
            .await?;
        }
        Commands::Models { command } => {
            handle_models_command(&paths, command).await?;
        }
        Commands::Gpus => {
            handle_gpus_command().await;
        }
        Commands::Compute { command } => {
            handle_compute_command(command).await?;
        }
        Commands::Knowledge { command } => {
            handle_knowledge_command(command).await?;
        }
        Commands::Discourse { command } => {
            handle_discourse_command(command).await?;
        }
        Commands::Use { command } => {
            handle_use_command(command).await?;
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
        Commands::Tui {
            fake_backend,
            scenario,
        } => {
            // --fake-backend: deterministic test mode, no real backend.
            if fake_backend {
                let scen = prism_tui::backend::FakeScenario::from_name(&scenario)?;
                let config = prism_tui::RunConfig {
                    backend_mode: prism_tui::BackendMode::Fake { scenario: scen },
                    platform: None,
                    resume: None,
                };
                prism_tui::run_with_config(config).await?;
                return Ok(());
            }

            // --offline: skip auth refresh + boot checks entirely.
            // The TUI launches directly with local tools only.
            if cli.offline {
                let _ = &python;
                let prism_bin =
                    std::env::current_exe().context("failed to locate current prism executable")?;
                prism_tui::run(
                    prism_bin.to_str().unwrap(),
                    project_root.to_string_lossy().as_ref(),
                    python.to_string_lossy().as_ref(),
                )
                .await?;
                return Ok(());
            }

            // First-run onboarding. A brand-new user (no credentials on
            // disk) gets a guided sign-in + model pick instead of being
            // dropped into the TUI on silent defaults — the reason a
            // fresh install used to show `gpt-5.5` with nobody logged in.
            // No-ops on repeat launches and in non-interactive contexts.
            onboarding::run_if_first_launch(&paths, &endpoints, &python).await?;

            // Auto-refresh + refresh-on-rejection. Mirrors the `prism
            // setup` path — see comments there for design notes.
            // The two triggers (proactive expiry + reactive 401) keep
            // users out of the "log in again every session" loop.
            let mut state = paths.load_cli_state().ok().unwrap_or_default();
            if let Some(creds) = state.credentials.as_ref()
                && !creds.refresh_token.is_empty()
                && creds
                    .expires_at
                    .is_some_and(|exp| chrono::Utc::now() + chrono::Duration::minutes(5) >= exp)
            {
                match refresh_access_token(&endpoints, creds).await {
                    Ok(new_creds) => {
                        state.credentials = Some(new_creds);
                        let _ = paths.save_cli_state(&state);
                        tracing::info!("access token refreshed proactively");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "proactive token refresh failed");
                    }
                }
            }
            let mut boot_checks =
                boot_checks::run_boot_checks(state.credentials.as_ref(), &endpoints).await;
            let auth_rejected = boot_checks
                .iter()
                .any(|c| c.name == "Auth" && c.result.starts_with("token rejected"));
            if auth_rejected
                && let Some(creds) = state.credentials.as_ref()
                && !creds.refresh_token.is_empty()
                && let Ok(new_creds) = refresh_access_token(&endpoints, creds).await
            {
                tracing::info!("access token refreshed after server-side rejection");
                state.credentials = Some(new_creds);
                let _ = paths.save_cli_state(&state);
                boot_checks =
                    boot_checks::run_boot_checks(state.credentials.as_ref(), &endpoints).await;
            }
            // Both proactive AND reactive refresh have failed → the
            // refresh token itself is expired. Run the full Login
            // recipe inline so the user doesn't have to abandon the
            // session. Same code path as `prism login` — device flow,
            // project picker, state save, SDK creds sync.
            if boot_checks
                .iter()
                .any(|c| c.name == "Auth" && c.result.starts_with("token rejected"))
            {
                eprintln!();
                eprintln!("\x1b[33mYour MARC27 session has expired — re-authenticating…\x1b[0m");
                eprintln!();
                if let Err(e) = perform_full_login(
                    &paths,
                    &endpoints,
                    &python,
                    LoginMode::Device { no_browser: false },
                )
                .await
                {
                    eprintln!("\x1b[31mInline re-login failed:\x1b[0m {e}");
                    eprintln!();
                    eprintln!(
                        "  Run \x1b[1mprism login\x1b[0m manually, then start \x1b[1mprism tui\x1b[0m again."
                    );
                    eprintln!();
                    return Ok(());
                }
                state = paths.load_cli_state().ok().unwrap_or_default();
                boot_checks =
                    boot_checks::run_boot_checks(state.credentials.as_ref(), &endpoints).await;
            }
            boot::boot_sequence(&boot_checks);
            let _ = &python;
            // Launch the new Ratatui full-screen TUI. It spawns
            // `prism backend` as a subprocess and talks JSON-RPC.
            let prism_bin =
                std::env::current_exe().context("failed to locate current prism executable")?;
            // Give the TUI the platform bearer so it can poll the org credit
            // balance at turn boundaries (status bar). None → no credits shown.
            let platform = state.credentials.as_ref().map(|c| prism_tui::PlatformAuth {
                base_url: endpoints.api_base.clone(),
                token: c.access_token.clone(),
            });
            let config = prism_tui::RunConfig {
                backend_mode: prism_tui::BackendMode::Real {
                    prism_binary: prism_bin.to_str().unwrap().to_string(),
                    project_root: project_root.to_string_lossy().to_string(),
                    python_bin: python.to_string_lossy().to_string(),
                },
                platform,
                resume: None,
            };
            prism_tui::run_with_config(config).await?;
        }
        Commands::Resume { id } => {
            // Reuses the same Tui setup path (auth refresh + boot checklist),
            // then launches the native prism_tui with a resume request:
            // `prism resume` (no id) opens the session picker; `prism resume
            // <id>` jumps straight into that conversation.

            // Same auth-refresh + boot-check flow as the Tui branch.
            let mut state = paths.load_cli_state().ok().unwrap_or_default();
            if let Some(creds) = state.credentials.as_ref()
                && !creds.refresh_token.is_empty()
                && creds
                    .expires_at
                    .is_some_and(|exp| chrono::Utc::now() + chrono::Duration::minutes(5) >= exp)
            {
                match refresh_access_token(&endpoints, creds).await {
                    Ok(new_creds) => {
                        state.credentials = Some(new_creds);
                        let _ = paths.save_cli_state(&state);
                        tracing::info!("access token refreshed proactively");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "proactive token refresh failed");
                    }
                }
            }
            let mut boot_checks =
                boot_checks::run_boot_checks(state.credentials.as_ref(), &endpoints).await;
            let auth_rejected = boot_checks
                .iter()
                .any(|c| c.name == "Auth" && c.result.starts_with("token rejected"));
            if auth_rejected
                && let Some(creds) = state.credentials.as_ref()
                && !creds.refresh_token.is_empty()
                && let Ok(new_creds) = refresh_access_token(&endpoints, creds).await
            {
                tracing::info!("access token refreshed after server-side rejection");
                state.credentials = Some(new_creds);
                let _ = paths.save_cli_state(&state);
                boot_checks =
                    boot_checks::run_boot_checks(state.credentials.as_ref(), &endpoints).await;
            }
            // Same inline re-login as the Tui branch — see comment
            // there. Resuming on dead creds is even more confusing
            // because the user expects their old conversation to load.
            if boot_checks
                .iter()
                .any(|c| c.name == "Auth" && c.result.starts_with("token rejected"))
            {
                eprintln!();
                eprintln!("\x1b[33mYour MARC27 session has expired — re-authenticating…\x1b[0m");
                eprintln!();
                if let Err(e) = perform_full_login(
                    &paths,
                    &endpoints,
                    &python,
                    LoginMode::Device { no_browser: false },
                )
                .await
                {
                    eprintln!("\x1b[31mInline re-login failed:\x1b[0m {e}");
                    eprintln!();
                    eprintln!(
                        "  Run \x1b[1mprism login\x1b[0m manually, then resume with \
                         \x1b[1mprism resume{}\x1b[0m.",
                        id.as_deref().map(|s| format!(" {s}")).unwrap_or_default()
                    );
                    eprintln!();
                    return Ok(());
                }
                state = paths.load_cli_state().ok().unwrap_or_default();
                boot_checks =
                    boot_checks::run_boot_checks(state.credentials.as_ref(), &endpoints).await;
            }
            boot::boot_sequence(&boot_checks);
            let prism_bin =
                std::env::current_exe().context("failed to locate current prism executable")?;
            let platform = state.credentials.as_ref().map(|c| prism_tui::PlatformAuth {
                base_url: endpoints.api_base.clone(),
                token: c.access_token.clone(),
            });
            let config = prism_tui::RunConfig {
                backend_mode: prism_tui::BackendMode::Real {
                    prism_binary: prism_bin.to_str().unwrap().to_string(),
                    project_root: project_root.to_string_lossy().to_string(),
                    python_bin: python.to_string_lossy().to_string(),
                },
                platform,
                resume: Some(id.unwrap_or_default()),
            };
            prism_tui::run_with_config(config).await?;
        }
        Commands::Billing { command } => {
            let (api_base, auth) = resolve_agent_auth()?;
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()?;

            match command {
                None => {
                    // Default: show balance
                    let raw = auth
                        .apply(client.get(format!("{api_base}/billing/balance")))
                        .send()
                        .await?;
                    let resp: serde_json::Value =
                        friendly_status(raw, "view billing balance")?.json().await?;
                    println!("\nMARC27 Credits");
                    println!(
                        "\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}"
                    );
                    println!(
                        "  Balance:  {:.1} credits (${:.2})",
                        resp["credits"].as_f64().unwrap_or(0.0),
                        resp["dollar_value"].as_f64().unwrap_or(0.0),
                    );
                    println!(
                        "  Org:      {}",
                        resp["org_name"].as_str().unwrap_or("unknown")
                    );
                    println!(
                        "\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\n"
                    );
                }
                Some(BillingCommands::Usage) => {
                    let resp: serde_json::Value = auth
                        .apply(client.get(format!("{api_base}/billing/usage?period=monthly")))
                        .send()
                        .await?
                        .error_for_status()?
                        .json()
                        .await?;
                    println!("\nUsage (current period)\n");
                    if let Some(services) = resp["by_service"].as_array() {
                        for svc in services {
                            println!(
                                "  {:<30} {:.2} credits  {} calls",
                                svc["metric"].as_str().unwrap_or("?"),
                                svc["credits_spent"].as_f64().unwrap_or(0.0),
                                svc["request_count"].as_u64().unwrap_or(0),
                            );
                        }
                    }
                    println!(
                        "\n  Total: {:.2} credits\n",
                        resp["total"].as_f64().unwrap_or(0.0)
                    );
                }
                Some(BillingCommands::History) => {
                    let resp: serde_json::Value = auth
                        .apply(client.get(format!("{api_base}/billing/history?page=1&per_page=20")))
                        .send()
                        .await?
                        .error_for_status()?
                        .json()
                        .await?;
                    println!("\nTransaction History\n");
                    if let Some(txns) = resp["transactions"].as_array() {
                        for tx in txns {
                            println!(
                                "  {} {:+.2} credits  {}",
                                tx["created_at"].as_str().unwrap_or("?"),
                                tx["amount_credits"].as_f64().unwrap_or(0.0),
                                tx["description"].as_str().unwrap_or(""),
                            );
                        }
                        if txns.is_empty() {
                            println!("  No transactions yet.");
                        }
                    }
                    println!();
                }
                Some(BillingCommands::Prices) => {
                    let resp: serde_json::Value = client
                        .get(format!("{api_base}/billing/prices"))
                        .send()
                        .await?
                        .error_for_status()?
                        .json()
                        .await?;
                    println!("\nCredit Prices\n");
                    if let Some(prices) = resp["prices"].as_array() {
                        for p in prices {
                            println!(
                                "  {:<30} {:.4} credits/{}  ({}% markup)",
                                p["metric"].as_str().unwrap_or("?"),
                                p["credits_per_unit"].as_f64().unwrap_or(0.0),
                                p["unit_label"].as_str().unwrap_or("unit"),
                                p["markup_pct"].as_f64().unwrap_or(0.0),
                            );
                        }
                    }
                    println!();
                }
                Some(BillingCommands::Topup { package }) => {
                    // Always show the available packs first.
                    let pkgs: serde_json::Value = client
                        .get(format!("{api_base}/billing/packages"))
                        .send()
                        .await?
                        .error_for_status()?
                        .json()
                        .await?;
                    println!("\nAvailable credit packs:\n");
                    if let Some(packages) = pkgs["packages"].as_array() {
                        for (i, p) in packages.iter().enumerate() {
                            println!(
                                "  {}. {:<12} \u{2014} {} credits  ${:.2}",
                                i + 1,
                                p["slug"].as_str().unwrap_or("?"),
                                p["credits"].as_u64().unwrap_or(0),
                                p["price_usd"].as_f64().unwrap_or(0.0),
                            );
                        }
                    }

                    // No slug: list-only. Never create a checkout the user
                    // didn't ask for (the old default silently bought starter).
                    let Some(package) = package else {
                        println!("\nRun `/billing topup <slug>` (e.g. pro) to open a checkout.");
                        return Ok(());
                    };

                    println!("\nOpening checkout for '{package}'...");
                    let resp: serde_json::Value = auth
                        .apply(client.post(format!("{api_base}/billing/topup")))
                        .json(&serde_json::json!({"package": package}))
                        .send()
                        .await?
                        .error_for_status()?
                        .json()
                        .await?;

                    if let Some(url) = resp["checkout_url"].as_str() {
                        println!("Checkout: {url}\n");
                        if let Err(e) = open_browser(url) {
                            eprintln!("Could not open browser: {e}");
                            println!("Open the URL above manually.");
                        }
                    } else {
                        eprintln!(
                            "Error: {}",
                            resp["error"]["message"].as_str().unwrap_or("unknown error")
                        );
                    }
                }
            }
        }
        Commands::External(args) => {
            if try_run_workflow_alias(&project_root, &paths, &args).await? {
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

async fn handle_workflow_command(
    command: WorkflowCommands,
    project_root: &Path,
    paths: &prism_runtime::PrismPaths,
) -> Result<()> {
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
            // Accept ONE self-contained .yaml fed directly (owner: "one .yaml
            // file that has to be fed" — no dropping it in ~/.prism/workflows
            // first). If `name` is a path to an existing yaml/yml, load that
            // spec wholesale; otherwise resolve it as a registered name.
            let file_spec = load_workflow_file(&name)?;
            let spec = match &file_spec {
                Some(s) => s,
                None => find_workflow(&specs, &name)
                    .ok_or_else(|| anyhow!("Workflow not found: {name}"))?,
            };
            let mut values = parse_set_pairs(&pairs)?;
            // Only execute-mode runs actually call tools (dry runs plan only).
            if execute && let Some(token) = mint_workflow_node_token(paths).await {
                values.insert("_node_token".to_string(), token);
            }
            let result = execute_workflow(spec, &values, execute).await?;
            render_workflow_result(spec, &result);
        }
    }
    Ok(())
}

async fn try_run_workflow_alias(
    project_root: &Path,
    paths: &prism_runtime::PrismPaths,
    args: &[String],
) -> Result<bool> {
    if args.is_empty() {
        return Ok(false);
    }
    let specs = discover_workflows(Some(project_root))?;
    let request = parse_workflow_command_args(args)?;
    let Some(spec) = find_workflow(&specs, &request.name) else {
        return Ok(false);
    };
    let mut values = request.values;
    // Only execute-mode runs actually call tools (dry runs plan only).
    if request.execute
        && let Some(token) = mint_workflow_node_token(paths).await
    {
        values.insert("_node_token".to_string(), token);
    }
    let result = execute_workflow(spec, &values, request.execute).await?;
    render_workflow_result(spec, &result);
    Ok(true)
}

/// If `name` points at an existing `.yaml`/`.yml` file, load and parse it as a
/// single self-contained workflow spec. Returns `None` when it's not a yaml
/// path (so the caller falls back to registry-name lookup). A path that looks
/// like yaml but fails to read/parse is a hard error — the user clearly meant
/// to feed that file.
fn load_workflow_file(name: &str) -> Result<Option<WorkflowSpec>> {
    let path = Path::new(name);
    let is_yaml = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("yaml") || e.eq_ignore_ascii_case("yml"))
        .unwrap_or(false);
    if !is_yaml || !path.is_file() {
        return Ok(None);
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading workflow file {name}"))?;
    let spec = load_workflow_from_str(&text, name)
        .with_context(|| format!("parsing workflow file {name}"))?;
    Ok(Some(spec))
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

    // Two valid invocation paths exist for any workflow. Showing only
    // the `--<arg>` table without the example invocations confused
    // users into trying `prism workflow run <name> --<arg> <value>`,
    // which clap rejects (the `run` subcommand takes `--set k=v` for
    // forwarding). Make both paths visible.
    let required_args: Vec<&str> = spec
        .arguments
        .iter()
        .filter(|a| a.required)
        .map(|a| a.name.as_str())
        .collect();
    if !required_args.is_empty() {
        let top_level: String = required_args
            .iter()
            .map(|n| format!(" --{n} <{n}>"))
            .collect();
        let set_pairs: String = required_args
            .iter()
            .map(|n| format!(" --set {n}=<{n}>"))
            .collect();
        println!();
        println!("usage:");
        println!("  prism {}{}", spec.command_name, top_level);
        println!("  prism workflow run {}{}", spec.command_name, set_pairs);
        println!("  add `--execute` to run for real (default is dry-run plan)");
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
        // Surface the tool's REAL output for completed steps — a compute step
        // that only prints "HTTP 200" hides the very result the run exists to
        // produce. The node wraps it as {status_code, output:{tool, result}};
        // show `result` compactly so `workflow run --execute` is self-evidently
        // real (and legible in a recording), not a silent success.
        if let Some(out) = step
            .data
            .get("output")
            .and_then(|o| o.get("result"))
            .filter(|r| !r.is_null())
        {
            let rendered = match out.as_str() {
                Some(s) => s.to_string(),
                None => serde_json::to_string(out).unwrap_or_default(),
            };
            let trimmed = rendered.trim();
            if !trimmed.is_empty() {
                let shown: String = trimmed.chars().take(600).collect();
                let ellipsis = if trimmed.chars().count() > 600 {
                    " …"
                } else {
                    ""
                };
                println!("    ↳ {shown}{ellipsis}");
            }
        }
    }
}

// ── prism mesh ─────────────────────────────────────────────────────────

/// Render the cross-org identity that other Fabric nodes will see
/// when verifying this user's requests. Sourced from local credentials
/// — no platform call needed.
async fn handle_federation_command(
    command: FederationCommands,
    paths: &prism_runtime::PrismPaths,
) -> Result<()> {
    match command {
        FederationCommands::Whoami { json } => {
            let state = paths.load_cli_state().ok().unwrap_or_default();
            let creds = state.credentials.as_ref().ok_or_else(|| {
                anyhow!("Not logged in. Run `prism login` to set up your platform identity.")
            })?;

            let now = chrono::Utc::now();
            let expired = creds.expires_at.is_some_and(|exp| now >= exp);

            if json {
                let out = serde_json::json!({
                    "org_id": creds.org_id,
                    "org_name": creds.org_name,
                    "project_id": creds.project_id,
                    "project_name": creds.project_name,
                    "user_id": creds.user_id,
                    "display_name": creds.display_name,
                    "platform_url": creds.platform_url,
                    "valid_until": creds.expires_at.map(|d| d.to_rfc3339()),
                    "expired": expired,
                });
                println!("{}", serde_json::to_string_pretty(&out)?);
                return Ok(());
            }

            // Human-readable. The shape mirrors what
            // crates/mesh/src/federation.rs::PeerIdentity emits over
            // the wire so a user inspecting their own identity sees
            // (roughly) what a remote node verifies.
            println!("\nFabric identity (this is what peer nodes see)");
            println!("───────────────────────────────────────────────");
            println!(
                "  Display name : {}",
                creds.display_name.as_deref().unwrap_or("(unset)")
            );
            println!(
                "  User ID      : {}",
                creds.user_id.as_deref().unwrap_or("(unset)")
            );
            println!(
                "  Org          : {} ({})",
                creds.org_name.as_deref().unwrap_or("(unset)"),
                creds.org_id.as_deref().unwrap_or("(unset)")
            );
            println!(
                "  Project      : {} ({})",
                creds.project_name.as_deref().unwrap_or("(unset)"),
                creds.project_id.as_deref().unwrap_or("(unset)")
            );
            println!("  Platform     : {}", creds.platform_url);
            match creds.expires_at {
                Some(exp) => {
                    let label = if expired { "EXPIRED" } else { "valid until" };
                    println!("  Token        : {} {}", label, exp.to_rfc3339());
                }
                None => println!("  Token        : no expiry set"),
            }
            if expired {
                println!();
                println!(
                    "  Token has expired. Run `prism login` to refresh — \
                     cross-org requests will be rejected by other nodes \
                     until you re-auth."
                );
            }
            println!();
        }
        FederationCommands::Peers { json } => {
            // Peer-org listing requires a platform endpoint that
            // returns the orgs THIS user can interact with across
            // Fabric. The MARC27 platform doesn't expose this yet
            // — tracked as F1 chunk 3 (platform pubkey fetcher +
            // peer enumeration). Until that lands, return a clean
            // empty result rather than fake data.
            //
            // The protocol contract is: trust is transitive via the
            // platform root CA; the platform owns peer enumeration;
            // PRISM clients only consume that list. Inventing peers
            // client-side would let an adversarial CLI fork generate
            // tokens that don't exist platform-side, which is the
            // exact attack vector the root-CA model prevents.
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "peers": [],
                        "platform_supported": false,
                        "note": "Peer listing requires platform-side enumeration (F1 chunk 3). Trust is transitive via the platform root CA — see docs/prism_fabric_v1_spec.md."
                    }))?
                );
            } else {
                println!("\nFabric peers");
                println!("─────────────");
                println!("  (no peers — platform enumeration coming in F1 chunk 3)");
                println!();
                println!("  Trust is transitive via the MARC27 platform root CA.");
                println!("  Run `prism federation whoami` to see your own identity.");
                println!("  See docs/prism_fabric_v1_spec.md for the full design.");
                println!();
            }
        }
    }
    Ok(())
}

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
        MeshCommands::Health { dashboard_url } => {
            let url = format!("{dashboard_url}/api/mesh/nodes");
            let resp = reqwest::get(&url)
                .await
                .with_context(|| format!("Failed to reach node at {url}"))?;
            let body = resp.text().await?;
            let status: serde_json::Value = serde_json::from_str(&body)?;

            if !status["online"].as_bool().unwrap_or(false) {
                println!("Mesh: offline");
                return Ok(());
            }
            let peer_count = status["peers"].as_array().map(|p| p.len()).unwrap_or(0);
            println!(
                "Mesh: online — node {} — {} peer(s)",
                status["node_id"].as_str().unwrap_or("?"),
                peer_count,
            );
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
/// Precedence: CLI flags > config.toml [chat] > prism.toml [llm] > built-in defaults.
/// Returns a helpful error if no model is configured anywhere.
fn build_llm_config(
    project_root: &Path,
    url_override: Option<&str>,
    model_override: Option<&str>,
    api_key_override: Option<&str>,
) -> Result<prism_ingest::LlmConfig> {
    let node_config = prism_core::config::NodeConfig::load(Some(project_root));
    let llm = &node_config.llm;

    // Also load ~/.prism/config.toml [chat] — the user-visible chat target
    // set by `prism use local/provider/marc27`. When set to Local or Provider,
    // it takes precedence over prism.toml [llm] for the LLM endpoint.
    let chat_target = crate::chat_config::load().unwrap_or_default().chat;

    // Resolve base_url, model, and api_key with the same precedence as
    // Commands::Backend: CLI flags > chat target > prism.toml [llm].
    let (base_url, model, api_key) = match &chat_target {
        crate::chat_config::ChatTarget::Local {
            url,
            model: local_model,
            api_key: local_key,
        } => (
            url_override
                .map(str::to_string)
                .unwrap_or_else(|| url.clone()),
            model_override
                .map(str::to_string)
                .unwrap_or_else(|| local_model.clone()),
            api_key_override
                .map(str::to_string)
                .or_else(|| local_key.clone())
                .or_else(|| llm.resolve_api_key()),
        ),
        crate::chat_config::ChatTarget::Provider {
            provider,
            model: prov_model,
            api_key_env,
        } => {
            let env_name = api_key_env
                .as_deref()
                .unwrap_or_else(|| crate::chat_config::ChatTarget::default_api_key_env(provider));
            (
                url_override.map(str::to_string).unwrap_or_else(|| {
                    format!(
                        "https://api.{provider}.com/v1",
                        provider = provider.to_ascii_lowercase()
                    )
                }),
                model_override
                    .map(str::to_string)
                    .unwrap_or_else(|| prov_model.clone()),
                api_key_override
                    .map(str::to_string)
                    .or_else(|| std::env::var(env_name).ok())
                    .or_else(|| llm.resolve_api_key()),
            )
        }
        // Marc27 cloud: use prism.toml [llm] as before.
        crate::chat_config::ChatTarget::Marc27 { .. } => {
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
            (base_url, model, api_key)
        }
    };

    Ok(prism_ingest::LlmConfig {
        base_url,
        model,
        api_key,
        embedding_model: llm.embedding_model.clone(),
        timeout_secs: llm.timeout_secs,
        ..Default::default()
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
    doc_id: &str,
    corpus: Option<&str>,
    model: Option<&str>,
) -> Result<serde_json::Value> {
    let (api_base, auth) = resolve_agent_auth()?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;

    // "text" = inline document ingestion (IngestSource::Text). The old
    // "query" type made the platform WEB-SEARCH the document's text instead
    // of ingesting it — documents never landed (ingestion audit, critical #1).
    let mut body = serde_json::json!({
        "source": { "type": "text", "text": chunk, "doc_id": doc_id },
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

#[allow(clippy::too_many_arguments)]
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
            provenance_db: None,
        }
    } else {
        let llm_cfg = build_llm_config(project_root, llm_url, model, api_key)?;
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
            provenance_db: None,
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
            let doc_id = format!("{}#{}", path.display(), index);
            let mut job = submit_platform_ingest_chunk(chunk, &doc_id, corpus, model).await?;
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

    // Honest closing: a pipeline step failure means data did NOT land for
    // that step — never print a clean "Done." over it (audit critical #2:
    // dead backends used to print "Done." with exit 0 having stored nothing).
    let step_errors: Vec<&str> = summary
        .get("result")
        .and_then(|r| r.get("errors"))
        .or_else(|| summary.get("errors"))
        .and_then(|e| e.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    if step_errors.is_empty() {
        println!("\n  Done.");
    } else {
        println!("\n  FAILED STEPS ({}):", step_errors.len());
        for e in &step_errors {
            println!("    ! {e}");
        }
        println!("\n  Completed WITH ERRORS — data for the failed steps was NOT stored.");
    }
}

/// Step failures from an ingest summary (either local-pipeline shape
/// `{result:{errors:[..]}}` or top-level). Non-empty ⇒ exit non-zero.
fn ingest_summary_errors(summary: &serde_json::Value) -> usize {
    summary
        .get("result")
        .and_then(|r| r.get("errors"))
        .or_else(|| summary.get("errors"))
        .and_then(|e| e.as_array())
        .map(|a| a.len())
        .unwrap_or(0)
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

    let total_step_errors: usize = summaries.iter().map(ingest_summary_errors).sum();

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

    // Exit non-zero when configured steps failed — agents and scripts key
    // off the exit code, and the old exit-0-having-stored-nothing was the
    // audit's #2 critical.
    if total_step_errors > 0 {
        bail!("{total_step_errors} ingest step(s) failed — see errors above/in JSON");
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
        if let Ok(meta) = path.metadata()
            && let Ok(modified) = meta.modified()
        {
            seen.insert(path.clone(), modified);
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
    // Primary: read from PrismPaths (cli-state.json)
    if let Ok(paths) = prism_runtime::PrismPaths::discover()
        && let Ok(state) = paths.load_cli_state()
        && let Some(creds) = &state.credentials
    {
        let raw_url = &creds.platform_url;
        let api_base = if raw_url.ends_with("/api/v1") {
            raw_url.clone()
        } else {
            format!("{}/api/v1", raw_url.trim_end_matches('/'))
        };
        return Ok((api_base, format!("Bearer {}", creds.access_token)));
    }

    // Fallback: legacy ~/.prism/credentials.json
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

/// Friendlier replacement for `.error_for_status()` on platform calls.
///
/// Default reqwest error on 401 reads "HTTP status client error (401
/// Unauthorized) for url ...", which leaves the user wondering what
/// to do. This wrapper replaces that with an actionable message
/// pointing at `prism login` and `prism status`. Other non-2xx
/// responses fall through to the normal reqwest error.
fn friendly_status(resp: reqwest::Response, action: &str) -> Result<reqwest::Response> {
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        let url = resp.url().to_string();
        bail!(
            "Not authorized to {action}.\n\
             \n\
             Your platform token may be expired or missing the required \
             scope for this endpoint. Try:\n\
             \x20 prism login              # refresh your platform session\n\
             \x20 prism status             # check current auth state\n\
             \n\
             Endpoint: {url}"
        );
    }
    Ok(resp.error_for_status()?)
}

/// Resolve platform auth for agents (MARC27_API_KEY env var → X-API-Key header).
/// Decoupled from user auth — agents use API keys, users use JWT.
/// Falls back to user auth if no API key is set (backward compat).
fn resolve_agent_auth() -> Result<(String, PlatformAuth)> {
    // Commands that build raw reqwest calls (research, discourse, …) all
    // resolve auth here first — so this is the offline chokepoint for the
    // paths that bypass PlatformClient's own guard.
    if std::env::var("PRISM_OFFLINE").is_ok_and(|v| v == "1") {
        anyhow::bail!(
            "offline mode: this command needs the MARC27 platform \
             (remove --offline to use it)"
        );
    }
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
                let total_rounds = event
                    .get("total_rounds")
                    .and_then(|value| value.as_i64())
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "?".to_string());
                println!(
                    "\n  \u{2501}\u{2501}\u{2501} {spec_name} \u{2501}\u{2501}\u{2501} {total_rounds} round(s) \u{2501}\u{2501}\u{2501}"
                );
                println!("  instance: {instance_id}\n");
            }
            "round_started" => {
                let round = event
                    .get("round")
                    .and_then(|value| value.as_i64())
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "?".to_string());
                // Engine emits the round category under `round_type`,
                // not `type` — earlier code looked up the wrong key
                // and rendered `[?]` for every round.
                let round_type = value_string(event, &["round_type", "type"]).unwrap_or("?");
                let agents: Vec<String> = event
                    .get("agents")
                    .and_then(|value| value.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                let agents_label = if agents.is_empty() {
                    "(all agents)".to_string()
                } else {
                    agents.join(", ")
                };
                println!(
                    "\n  \u{25BC} Round {round} \u{2014} {round_type} \u{2014} {agents_label}\n"
                );
            }
            "agent_turn" => {
                let agent = value_string(event, &["agent_id"]).unwrap_or("?");
                let content = value_string(event, &["content"]).unwrap_or("");
                let turn_num = event
                    .get("turn_num")
                    .and_then(|value| value.as_i64())
                    .map(|value| format!("turn {value}"))
                    .unwrap_or_default();
                // Indent every line of the agent's reply by 4 spaces so
                // the reader's eye groups it under the agent header. No
                // truncation — the whole point of a discourse is to read
                // what the agents actually said.
                let indented: String = content
                    .lines()
                    .map(|line| format!("    {line}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                println!("  \u{2022} {agent}  {turn_num}");
                println!("{indented}\n");
            }
            "round_complete" => {
                let round = event
                    .get("round")
                    .and_then(|value| value.as_i64())
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "?".to_string());
                println!("  \u{2514} round {round} complete\n");
            }
            "gate_check" => {
                let metric = value_string(event, &["metric"]).unwrap_or("?");
                let value = event
                    .get("value")
                    .and_then(|v| v.as_f64())
                    .map(|v| format!("{v:.3}"))
                    .unwrap_or_else(|| "?".to_string());
                let passed = event
                    .get("passed")
                    .and_then(|v| v.as_bool())
                    .map(|v| if v { "PASS" } else { "FAIL" })
                    .unwrap_or("?");
                println!("  gate: {metric}={value} \u{2192} {passed}\n");
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
                println!(
                    "  \u{2501}\u{2501}\u{2501} complete \u{2501} {turns} turn(s) \u{2501} {cost} \u{2501}\u{2501}\u{2501}\n"
                );
            }
            "error" => {
                let msg = value_string(event, &["message"]).unwrap_or("(no detail)");
                println!("  \u{26A0}  error: {msg}\n");
            }
            other => {
                println!("  ? {other}: {}", event);
            }
        }
    }
}

/// `prism predict` — the agent's one-call "run this marketplace model on the
/// cloud" path, riding the PROVEN deployment spine (deploy → /predict HTTP):
/// ensure a deployment (reuse running, else create + wait ready), POST the
/// inputs, return the model's real result, auto-stop what we created unless
/// `--keep`. Always prints exactly one JSON document on stdout (agents parse
/// it); hard failures return an `{"error": ...}` document with exit 1.
#[allow(clippy::too_many_arguments)]
async fn handle_predict(
    model: &str,
    task: &str,
    input_json: &str,
    node_id: Option<&str>,
    gpu: Option<&str>,
    budget: Option<f64>,
    ready_timeout_secs: u64,
    keep: bool,
) -> Result<()> {
    let inputs: serde_json::Value = serde_json::from_str(input_json)
        .with_context(|| "--input is not valid JSON".to_string())?;
    if !inputs.is_object() {
        bail!("--input must be a JSON object");
    }

    let (api_base, auth) = resolve_agent_auth()?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?;

    // 1. Reuse a RUNNING deployment of this model when one exists — repeat
    //    predictions then cost one HTTP call, not a container start. Match on
    //    the deployment name (this command names its deployments = the slug).
    let list: serde_json::Value = auth
        .apply(client.get(format!("{api_base}/compute/deployments")))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let running = list
        .as_array()
        .or_else(|| list.get("deployments").and_then(|d| d.as_array()))
        .map(|deployments| {
            deployments
                .iter()
                .filter(|d| d.get("name").and_then(|n| n.as_str()) == Some(model))
                .filter(|d| d.get("status").and_then(|s| s.as_str()) == Some("running"))
                .find(|d| {
                    d.get("endpoint_url")
                        .and_then(|u| u.as_str())
                        .is_some_and(|u| !u.is_empty())
                })
                .cloned()
        })
        .unwrap_or(None);

    let (deployment_id, endpoint_url, reused) = if let Some(dep) = running {
        (
            dep["id"].as_str().unwrap_or_default().to_string(),
            dep["endpoint_url"].as_str().unwrap_or_default().to_string(),
            true,
        )
    } else {
        // 2. Create a deployment from the marketplace slug. Default target
        //    lets the platform pick any registered node; --node-id pins one.
        let mut body = serde_json::json!({
            "resource_slug": model,
            "name": model,
            "target": "prism_node",
        });
        if let Some(nid) = node_id {
            body["node_id"] = serde_json::json!(nid);
        }
        if let Some(gpu) = gpu {
            body["gpu_type"] = serde_json::json!(gpu);
        }
        if let Some(budget) = budget {
            body["budget_max_usd"] = serde_json::json!(budget);
        }

        let created: serde_json::Value = auth
            .apply(client.post(format!("{api_base}/compute/deployments")))
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let id = created["id"]
            .as_str()
            .or_else(|| created["deployment_id"].as_str())
            .context("deployment create response has no id")?
            .to_string();

        // 3. Wait (bounded) for running + endpoint. Failed/stopped is a
        //    terminal honest error, not a wait-forever.
        let deadline = std::time::Instant::now() + Duration::from_secs(ready_timeout_secs);
        let endpoint = loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let status: serde_json::Value = auth
                .apply(client.get(format!("{api_base}/compute/deployments/{id}")))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            let state = status["status"].as_str().unwrap_or("unknown");
            match state {
                "running" => {
                    if let Some(url) = status["endpoint_url"].as_str().filter(|u| !u.is_empty()) {
                        break url.to_string();
                    }
                }
                "failed" | "stopped" | "unhealthy" => {
                    bail!(
                        "deployment {id} for model '{model}' ended in state '{state}' \
                         before serving — check `prism deploy status {id}`"
                    );
                }
                _ => {}
            }
            if std::time::Instant::now() > deadline {
                bail!(
                    "deployment {id} for model '{model}' not ready after \
                     {ready_timeout_secs}s (last state '{state}'); it is still \
                     provisioning — poll `prism deploy status {id}` or re-run \
                     with a larger --ready-timeout-secs"
                );
            }
        };
        (id, endpoint, false)
    };

    // 4. The actual prediction: same JSON body as the serving images' batch
    //    mode — {"task": ..., ...inputs} → POST {endpoint}/predict.
    let mut payload = inputs.clone();
    payload["task"] = serde_json::json!(task);
    payload["model"] = serde_json::json!(model);
    let predict_url = format!("{}/predict", endpoint_url.trim_end_matches('/'));
    let result: serde_json::Value = client
        .post(&predict_url)
        .json(&payload)
        .timeout(Duration::from_secs(600))
        .send()
        .await
        .with_context(|| format!("prediction request to {predict_url} failed"))?
        .error_for_status()?
        .json()
        .await?;

    // 5. Auto-stop what WE created (no silent per-minute billing) unless
    //    --keep; a reused deployment belongs to whoever started it.
    let mut auto_stopped = false;
    if !reused && !keep {
        auto_stopped = auth
            .apply(client.delete(format!("{api_base}/compute/deployments/{deployment_id}")))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false);
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "model": model,
            "task": task,
            "deployment_id": deployment_id,
            "reused_running_deployment": reused,
            "auto_stopped": auto_stopped,
            "kept_running": !auto_stopped,
            "result": result,
        }))?
    );
    Ok(())
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
            if let Some(gpu) = gpu {
                body.insert("gpu_type".to_string(), serde_json::Value::String(gpu));
            }
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
                friendly_status(request.send().await?, "list compute deployments")?
                    .json()
                    .await?;

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

#[derive(Debug, Subcommand)]
enum ComputeCommands {
    /// List purchasable GPU offers (type, VRAM, region, provider, $/hr).
    Gpus,
    /// List registered compute providers/backends.
    Providers,
    /// Preview the cost of a job without dispatching it (FREE).
    Estimate {
        /// Container image or marketplace slug to price.
        #[arg(long)]
        image: String,
        /// GPU class, e.g. A100-80GB. Omit to let the broker choose.
        #[arg(long)]
        gpu: Option<String>,
        /// Wall-time cap in seconds (default 3600).
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Poll one compute job by ID.
    Status {
        /// Job ID returned by `compute submit`.
        job_id: String,
    },
    /// Cancel a queued/running compute job by ID (idempotent).
    Cancel {
        /// Job ID to cancel.
        job_id: String,
    },
    /// Dispatch a real, BILLABLE containerized GPU/CPU job.
    Submit {
        /// Container image or marketplace slug.
        #[arg(long)]
        image: String,
        /// JSON input payload for the container (default '{}').
        #[arg(long, default_value = "{}")]
        inputs: String,
        /// GPU class, e.g. A100-80GB.
        #[arg(long)]
        gpu: Option<String>,
        /// Hard cost cap in USD; broker refuses dispatch if the estimate exceeds it.
        #[arg(long)]
        budget: Option<f64>,
        /// Routing: cheapest (default), fastest, or a provider name.
        #[arg(long)]
        provider: Option<String>,
        /// Wall-time cap in seconds (default 3600).
        #[arg(long)]
        timeout: Option<u64>,
        /// Environment variables (repeatable): --env KEY=VALUE.
        #[arg(long = "env")]
        env: Vec<String>,
    },
}

/// `prism compute …` — one-shot compute-broker jobs. Every subcommand prints
/// exactly one JSON document on stdout (agent-facing / machine-readable); the
/// broker endpoints live under `{api_base}/compute/*`. This is the Rust CLI
/// home for compute dispatch — the old Python `compute`/`compute_submit` tools
/// (which needed an uninstalled `marc27` SDK) were retired in its favour.
async fn handle_compute_command(command: ComputeCommands) -> Result<()> {
    let (api_base, auth) = resolve_agent_auth()?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    let response: serde_json::Value = match command {
        ComputeCommands::Gpus => {
            auth.apply(client.get(format!("{api_base}/compute/gpus")))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?
        }
        ComputeCommands::Providers => {
            auth.apply(client.get(format!("{api_base}/compute/providers")))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?
        }
        ComputeCommands::Estimate {
            image,
            gpu,
            timeout,
        } => {
            let mut body = serde_json::Map::new();
            body.insert("image".to_string(), serde_json::Value::String(image));
            body.insert("inputs".to_string(), serde_json::json!({}));
            if let Some(gpu) = gpu {
                body.insert("gpu_type".to_string(), serde_json::Value::String(gpu));
            }
            if let Some(timeout) = timeout {
                body.insert("timeout_seconds".to_string(), serde_json::json!(timeout));
            }
            auth.apply(client.post(format!("{api_base}/compute/estimate")))
                .json(&serde_json::Value::Object(body))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?
        }
        ComputeCommands::Status { job_id } => {
            auth.apply(client.get(format!("{api_base}/compute/{job_id}")))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?
        }
        ComputeCommands::Cancel { job_id } => {
            auth.apply(client.post(format!("{api_base}/compute/{job_id}/cancel")))
                .send()
                .await?
                .error_for_status()?;
            serde_json::json!({ "job_id": job_id, "status": "cancel_requested" })
        }
        ComputeCommands::Submit {
            image,
            inputs,
            gpu,
            budget,
            provider,
            timeout,
            env,
        } => {
            let inputs_value: serde_json::Value = serde_json::from_str(&inputs)
                .map_err(|e| anyhow!("--inputs must be valid JSON: {e}"))?;
            let mut body = serde_json::Map::new();
            body.insert("image".to_string(), serde_json::Value::String(image));
            body.insert("inputs".to_string(), inputs_value);
            if let Some(gpu) = gpu {
                body.insert("gpu_type".to_string(), serde_json::Value::String(gpu));
            }
            if let Some(budget) = budget {
                body.insert("budget_max_usd".to_string(), serde_json::json!(budget));
            }
            if let Some(provider) = provider {
                body.insert(
                    "provider_preference".to_string(),
                    serde_json::Value::String(provider),
                );
            }
            if let Some(timeout) = timeout {
                body.insert("timeout_seconds".to_string(), serde_json::json!(timeout));
            }
            if !env.is_empty() {
                body.insert(
                    "env_vars".to_string(),
                    serde_json::Value::Object(parse_string_map_arg(&env, "--env")?),
                );
            }
            auth.apply(client.post(format!("{api_base}/compute/submit")))
                .json(&serde_json::Value::Object(body))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?
        }
    };

    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

#[derive(Debug, Subcommand)]
enum KnowledgeCommands {
    /// Look up one entity plus its 1-hop neighbors in the knowledge graph.
    Entity {
        /// Entity name to resolve.
        name: String,
        /// Max neighbors to return.
        #[arg(long, default_value = "10")]
        limit: usize,
    },
    /// Shortest hop-paths between two entities ("how does X relate to Y?").
    Paths {
        /// Start entity.
        from: String,
        /// End entity.
        to: String,
        /// Max path length in hops.
        #[arg(long, default_value = "3")]
        max_hops: usize,
    },
    /// List available corpora from the platform catalog.
    Corpora {
        /// Filter by domain (materials/chemistry/biomedical/physics).
        #[arg(long)]
        domain: Option<String>,
        /// Filter by kind (structured_db/knowledge_graph/literature/ontology).
        #[arg(long)]
        kind: Option<String>,
        /// Max results.
        #[arg(long, default_value = "50")]
        limit: usize,
    },
    /// Submit a background extraction job from a URL or free-text query.
    Ingest {
        /// Source URL to fetch and extract.
        #[arg(long)]
        url: Option<String>,
        /// Free-text query to extract entities/embeddings from.
        #[arg(long)]
        query: Option<String>,
        /// Extraction mode: graph, embed, or full.
        #[arg(long, default_value = "full")]
        mode: String,
    },
}

/// `prism knowledge …` — knowledge-graph reads + platform ingest. Every
/// subcommand prints exactly one JSON document on stdout (agent-facing /
/// machine-readable); the endpoints live under `{api_base}/knowledge/*`. This
/// is the Rust CLI home for the knowledge plane — the old Python `knowledge`
/// tool (which drove a thin `_platform_client` and needed the uninstalled
/// `marc27` SDK for some paths) was retired in its favour. Graph + semantic
/// search stay under `prism query --platform`; graph stats under
/// `prism ingest --status`.
async fn handle_knowledge_command(command: KnowledgeCommands) -> Result<()> {
    let (api_base, auth) = resolve_agent_auth()?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    let response: serde_json::Value = match command {
        KnowledgeCommands::Entity { name, limit } => {
            auth.apply(client.get(format!("{api_base}/knowledge/graph/entity/{name}")))
                .query(&[("limit", limit.to_string())])
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?
        }
        KnowledgeCommands::Paths { from, to, max_hops } => {
            auth.apply(client.get(format!("{api_base}/knowledge/graph/paths")))
                .query(&[
                    ("from", from),
                    ("to", to),
                    ("max_hops", max_hops.to_string()),
                ])
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?
        }
        KnowledgeCommands::Corpora {
            domain,
            kind,
            limit,
        } => {
            let mut params: Vec<(&str, String)> = vec![("limit", limit.to_string())];
            if let Some(domain) = domain {
                params.push(("domain", domain));
            }
            if let Some(kind) = kind {
                params.push(("kind", kind));
            }
            auth.apply(client.get(format!("{api_base}/knowledge/catalog")))
                .query(&params)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?
        }
        KnowledgeCommands::Ingest { url, query, mode } => {
            let source = match (url, query) {
                (Some(url), _) => serde_json::json!({ "type": "url", "url": url }),
                (None, Some(query)) => serde_json::json!({ "type": "query", "query": query }),
                (None, None) => bail!("`knowledge ingest` requires --url or --query"),
            };
            let body = serde_json::json!({ "mode": mode, "source": source });
            auth.apply(client.post(format!("{api_base}/knowledge/ingest-job")))
                .json(&body)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?
        }
    };

    println!("{}", serde_json::to_string_pretty(&response)?);
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
        ModelsCommands::Info { model_id, json: _ } => {
            let model = models
                .into_iter()
                .find(|model| {
                    value_string(model, &["model_id", "id"])
                        .map(|value| value == model_id)
                        .unwrap_or(false)
                })
                .ok_or_else(|| anyhow!("Model not found in project catalog: {model_id}"))?;

            println!("{}", serde_json::to_string_pretty(&model)?)
        }
    }

    Ok(())
}

/// `prism gpus` — the live GPU procurement catalog.
///
/// Fetches `GET {api_base}/compute/gpus` (user JWT auth) and prints the
/// raw JSON array as one document on stdout. Always exits 0: failures
/// print `{"error": "..."}` instead, because the backend's `/gpus` slash
/// handler parses stdout and a nonzero exit would swallow the message.
async fn handle_gpus_command() {
    let value = match fetch_gpu_catalog().await {
        Ok(value) => value,
        Err(err) => serde_json::json!({ "error": format!("{err:#}") }),
    };
    match serde_json::to_string_pretty(&value) {
        Ok(text) => println!("{text}"),
        Err(err) => println!("{}", serde_json::json!({ "error": format!("{err:#}") })),
    }
}

async fn fetch_gpu_catalog() -> Result<serde_json::Value> {
    let (api_base, auth_header) = resolve_user_auth()?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let response = client
        .get(format!("{api_base}/compute/gpus"))
        .header("Authorization", auth_header)
        .send()
        .await?;
    let value = friendly_status(response, "list GPU compute offers")?
        .json()
        .await?;
    Ok(value)
}

/// Translate clap-parsed `UseCommands` into the surface-agnostic
/// `UseAction` consumed by `use_command::apply`. The CLI surface and
/// the in-chat `/use` slash command both build `UseAction`s (the
/// slash command parses raw text into one); keeping the action type
/// independent of clap means the slash command doesn't pull in any
/// CLI-only types.
async fn handle_use_command(command: UseCommands) -> Result<()> {
    let action = match command {
        UseCommands::Marc27 { model } => use_command::UseAction::Marc27 { model },
        UseCommands::Local {
            url,
            model,
            api_key,
        } => use_command::UseAction::Local {
            url,
            model,
            api_key,
        },
        UseCommands::Provider {
            provider,
            model,
            api_key_env,
        } => use_command::UseAction::Provider {
            provider,
            model,
            api_key_env,
        },
        UseCommands::Show => use_command::UseAction::Show,
        UseCommands::Reset => use_command::UseAction::Reset,
    };
    // We're running before prism boots, so there's no live bridge to
    // hot-swap — only the persisted config matters. Detect whether the
    // user has run `prism login` so the message can correctly state
    // whether platform tools are available.
    let logged_in = paths_credentials_present();
    let outcome = use_command::apply(action, None, logged_in).await?;
    println!("{}", outcome.message);
    Ok(())
}

/// Best-effort check for whether `prism login` has been completed and
/// the credentials file exists with a non-empty token. Used by
/// `prism use` and the in-chat `/use` to render the "Tools" line
/// honestly. Doesn't validate the token (no network) — that would
/// move to a separate `prism status --tools` check if we want it.
fn paths_credentials_present() -> bool {
    let home = match std::env::var_os("HOME") {
        Some(h) => h,
        None => return false,
    };
    let path = std::path::PathBuf::from(home)
        .join(".prism")
        .join("credentials.json");
    if !path.exists() {
        return false;
    }
    match std::fs::read_to_string(&path) {
        Ok(text) => !text.trim().is_empty() && text.contains("access_token"),
        Err(_) => false,
    }
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
            let raw = auth
                .apply(client.get(format!("{api_base}/discourse/specs")))
                .send()
                .await?;
            let response: serde_json::Value =
                friendly_status(raw, "list discourse specs")?.json().await?;

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

/// Parse a Server-Sent-Events stream body into JSON event objects. Each
/// `data:` line is decoded as one JSON value (our backends emit one compact
/// JSON object per `data:` line); `[DONE]` sentinels, comments, and non-JSON
/// keep-alive lines are skipped.
///
/// If the body carries no SSE `data:` events at all, it is treated as a plain
/// JSON document (array → its elements, object → a single event) so a
/// non-streamed error/response body still surfaces to the caller instead of
/// silently vanishing.
fn parse_sse_json_events(body: &str) -> Result<Vec<serde_json::Value>> {
    let mut events = Vec::new();
    for line in body.lines() {
        let Some(payload) = line.trim_end().strip_prefix("data:") else {
            continue;
        };
        let payload = payload.trim();
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) {
            events.push(value);
        }
    }

    if events.is_empty() && !body.trim().is_empty() {
        match serde_json::from_str::<serde_json::Value>(body.trim()) {
            Ok(serde_json::Value::Array(items)) => events = items,
            Ok(value) => events.push(value),
            Err(err) => return Err(anyhow!("stream response was neither SSE nor JSON: {err}")),
        }
    }

    Ok(events)
}

/// Unwrap events that arrived as `{ "text": "data: {...}" }` — the shape the
/// marc27 text-marker stream path emits — into the inner JSON object. Events
/// without a JSON-object-bearing `text` field pass through unchanged.
fn normalize_stream_events(events: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
    events
        .into_iter()
        .map(|event| {
            let unwrapped = value_string(&event, &["text"]).and_then(|text| {
                let payload = text
                    .strip_prefix("data:")
                    .map(str::trim_start)
                    .unwrap_or(text)
                    .trim();
                serde_json::from_str::<serde_json::Value>(payload)
                    .ok()
                    .filter(serde_json::Value::is_object)
            });
            unwrapped.unwrap_or(event)
        })
        .collect()
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

    // The local CLI is the node operator on this machine, so the FIRST
    // user to run it gets bootstrapped as NodeAdmin to manage their
    // own dashboard routes without a separate bootstrap dance.
    //
    // **Only assign if no role already exists** — see Bug #49. The
    // earlier code unconditionally `assign_role(NodeAdmin)`, which
    // uses `INSERT ... ON CONFLICT DO UPDATE`, so any explicit
    // downgrade by an admin (e.g. someone running an admin tool to
    // demote user-X to Viewer) would be silently undone the next
    // time user-X ran any CLI command that called this function.
    // That defeats the RBAC model on shared machines.
    let rbac_db_path = paths.state_dir.join("rbac.db");
    let rbac_engine = prism_core::rbac::RbacEngine::new(&rbac_db_path)?;
    if rbac_engine.get_role(user_id)?.is_none() {
        rbac_engine.assign_role(user_id, prism_core::rbac::LocalRole::NodeAdmin)?;
    }

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

/// Spawn the detached background worker that owns a campaign loop:
/// `prism campaign continue <id>` with stdio detached, in its own process
/// group so it survives the parent CLI (or an agent tool call) exiting. The
/// checkpoint file is the only communication channel — the worker updates
/// it, everyone else polls it.
fn spawn_campaign_worker(campaign_id: &str) -> Result<()> {
    let exe = std::env::current_exe().context("failed to locate current prism executable")?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["campaign", "continue", campaign_id])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let child = cmd
        .spawn()
        .context("failed to spawn detached campaign worker")?;
    tracing::info!(
        campaign_id,
        worker_pid = child.id(),
        "campaign worker detached"
    );
    Ok(())
}

/// Mint a loopback session token so a workflow's `tool` steps can authenticate
/// to the local node's `/api/tools/{name}/run` endpoint, which is auth- and
/// `ExecuteTools`-gated when the node is online. The token is injected into the
/// workflow context under the reserved `_node_token` key; `run_tool_step`
/// forwards it as a Bearer credential and strips it from the returned context.
///
/// Best-effort: returns `None` when the caller isn't logged in or the node
/// isn't reachable. A tokenless workflow still runs — tool-free workflows are
/// unaffected, and an online tool step fails honestly with 401 rather than
/// silently.
async fn mint_workflow_node_token(paths: &prism_runtime::PrismPaths) -> Option<String> {
    match create_dashboard_session("http://127.0.0.1:7327", paths).await {
        Ok(token) => Some(token),
        Err(e) => {
            tracing::debug!(error = %e, "workflow: no local node session (running tokenless)");
            None
        }
    }
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
    llm_cfg: Option<&prism_ingest::LlmConfig>,
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
        let llm_cfg = llm_cfg.ok_or_else(|| {
            anyhow!("--semantic queries require an LLM for embeddings. Run: prism configure --model <name>")
        })?;
        println!("Generating query embedding...");
        let llm_client = prism_ingest::llm::LlmClient::new(llm_cfg.clone());
        let query_vec = match llm_client.embed_text(text).await {
            Ok(v) => v,
            Err(e) => {
                // Detect the most common misconfiguration: prism.toml's
                // `[llm].url` points at MARC27's project-scoped LLM proxy
                // (`/api/v1/projects/{id}/llm`) which doesn't expose
                // `/v1/embeddings`. Without this hint a real user thinks
                // the platform is down. Steer them to the working path.
                let msg = e.to_string();
                let looks_like_marc27_404 =
                    msg.contains("404") && msg.contains("/projects/") && msg.contains("/llm/");
                if looks_like_marc27_404 {
                    bail!(
                        "Local --semantic search needs an LLM that exposes \
                         OpenAI-compatible /v1/embeddings, but the configured \
                         URL ({}) is the MARC27 project-scoped LLM proxy \
                         which serves chat-streaming only.\n\n\
                         For platform knowledge-graph search, use:\n\
                         \x20 prism query --platform --semantic \"{}\"\n\n\
                         For local embeddings, run a server like Ollama or \
                         llama.cpp and point prism at it via:\n\
                         \x20 prism configure --llm-provider llamacpp\n\n\
                         Underlying error: {e}",
                        llm_cfg.base_url,
                        text
                    );
                }
                return Err(e).context("failed to generate query embedding");
            }
        };

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

/// Mode flag for [`perform_full_login`] — picks the credential source
/// (PAT vs interactive device flow) without committing the caller to
/// the structure of [`Commands::Login`]'s arguments.
enum LoginMode {
    /// Personal Access Token — non-interactive, suitable for headless
    /// scripts and CI. Skips the device-flow polling step.
    Token(String),
    /// Interactive device flow. `no_browser=true` renders a paste-this-
    /// URL block instead of auto-launching the browser.
    Device { no_browser: bool },
}

/// Run the full login recipe used by `prism login` AND by the inline
/// relogin path in `prism tui` / `prism resume` when both refreshes
/// fail.
///
/// Steps:
/// 1. Mint fresh credentials (token or device flow).
/// 2. Fetch the user profile.
/// 3. Pick org + project (auto-selects when only one exists — see
///    [`select_project`]).
/// 4. Persist `StoredCredentials` to `cli_state.json`.
/// 5. Mirror the access/refresh tokens to `~/.prism/credentials.json`
///    (0600 on unix) for the Python SDK.
///
/// On success, the caller can call `paths.load_cli_state()` to read
/// the freshly-saved credentials and continue.
///
/// Extracted so [`Commands::Tui`] and [`Commands::Resume`] can do
/// inline relogin instead of dropping the user with "open a new
/// terminal and run `prism login`" (the previous behaviour, see
/// [`Commands::Tui`] fail-fast block).
async fn perform_full_login(
    paths: &PrismPaths,
    endpoints: &PlatformEndpoints,
    python: &std::path::Path,
    mode: LoginMode,
) -> Result<()> {
    let mut state = paths.load_cli_state().unwrap_or_default();
    let credentials = match mode {
        LoginMode::Token(pat) => run_token_login(endpoints, &pat).await?,
        LoginMode::Device { no_browser } => {
            run_device_login_with_opts(endpoints, no_browser).await?
        }
    };
    let platform = PlatformClient::new(&endpoints.api_base).with_token(&credentials.access_token);
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

    // Sync credentials to ~/.prism/credentials.json for the Python SDK.
    //
    // 0600 because the file holds an access_token + refresh_token. Plain
    // `fs::write` would inherit the user's umask (typically 0644 =
    // world-readable on most Linux distros), which would let any other
    // local user read the tokens. cli_state.json (saved via
    // PrismPaths::save_cli_state) already uses 0600 for the same reason.
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
                if let Some(parent) = sdk_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                #[cfg(unix)]
                {
                    use std::io::Write;
                    use std::os::unix::fs::OpenOptionsExt;
                    if let Ok(mut file) = std::fs::OpenOptions::new()
                        .write(true)
                        .create(true)
                        .truncate(true)
                        .mode(0o600)
                        .open(&sdk_path)
                    {
                        let _ = file.write_all(json.as_bytes());
                    }
                }
                #[cfg(not(unix))]
                {
                    let _ = std::fs::write(&sdk_path, json);
                }
            }
        }
    }

    Ok(())
}

async fn run_device_login(endpoints: &PlatformEndpoints) -> Result<StoredCredentials> {
    // Default behaviour preserves the original "open a browser" UX
    // for users with a desktop GUI. HPC / SSH paths reach the new
    // headless variant via `prism login --no-browser`.
    run_device_login_with_opts(endpoints, false).await
}

/// Headless variant of `run_device_login`.
///
/// `no_browser=true` skips the auto-open and renders an explicit
/// instruction block so the user can paste the URL into ANY browser
/// (their laptop's, their phone's, …) and approve. The polling loop
/// is identical — once the device is approved, the token comes back
/// and gets stored exactly the same way.
async fn run_device_login_with_opts(
    endpoints: &PlatformEndpoints,
    no_browser: bool,
) -> Result<StoredCredentials> {
    let platform = PlatformClient::new(&endpoints.api_base);
    let http = platform.inner().clone();

    let start: DeviceCodeResponse =
        DeviceFlowAuth::start_device_flow(&http, &endpoints.api_base).await?;

    println!();
    if no_browser {
        // Headless block — no auto-open, structure the output so the
        // user can clearly see what to do on a different machine.
        println!("\u{2501}\u{2501} PRISM headless login \u{2501}\u{2501}");
        println!();
        println!("  1. Open this URL on any browser (laptop, phone, …):");
        println!("       {}", start.verification_uri);
        println!("  2. Enter this code:");
        println!("       {}", start.user_code);
        println!("  3. Approve the session.");
        println!();
        println!("  Waiting here until you approve. Ctrl+C to abort.");
    } else {
        println!("PRISM setup needs MARC27 platform login.");
        println!("Open: {}", start.verification_uri);
        println!("Code: {}", start.user_code);
        println!();
        if let Err(err) = open_browser(&start.verification_uri) {
            eprintln!("warning: failed to open browser automatically: {err}");
        }
        println!("Approve the device in your browser, then return here.");
    }

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

    // Store server config (default model, MP API key) if provided
    if let Some(config) = &token.config {
        if let Some(ref default_model) = config.default_model {
            tracing::info!(model = %default_model, "server config: default model");
        }
        if config.mp_api_key.is_some() {
            tracing::info!("server config: Materials Project API key received");
        }
        // Write config to prism.toml and env for the current process
        if let Some(ref mp_key) = config.mp_api_key {
            unsafe {
                std::env::set_var("MP_API_KEY", mp_key);
            }
        }
        if let Some(ref fc_key) = config.firecrawl_api_key {
            unsafe {
                std::env::set_var("FIRECRAWL_API_KEY", fc_key);
            }
            tracing::info!("server config: Firecrawl API key received");
        }
        // Write default model to prism.toml if user hasn't set one
        if let Some(ref model) = config.default_model {
            let node_config = prism_core::config::NodeConfig::load(None);
            if node_config.llm.model.is_none() {
                // User hasn't set a model — use server default
                if let Ok(home) = std::env::var("HOME") {
                    let toml_path = format!("{home}/.prism/prism.toml");
                    if let Ok(existing) = std::fs::read_to_string(&toml_path)
                        && !existing.contains("model =")
                    {
                        let updated = if existing.contains("[llm]") {
                            existing.replace("[llm]", &format!("[llm]\nmodel = \"{model}\""))
                        } else {
                            format!("{existing}\n[llm]\nmodel = \"{model}\"\n")
                        };
                        let _ = std::fs::write(&toml_path, updated);
                        tracing::info!(model = %model, "set default model from server config");
                    }
                }
            }
        }
    }

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

/// Headless / CI / SSH-only login path. Skips the device flow and
/// validates a Personal Access Token (PAT) the user issued on the
/// MARC27 website. The token is the only credential needed; org +
/// project selection is deferred to the post-login interactive step
/// (Commands::Login handler), which prompts only if multiple orgs/
/// projects are visible — and falls back to env var `MARC27_PROJECT_ID`
/// when the prompt isn't appropriate (CI, scripted setup).
///
/// Validation: we hit `GET /api/v1/me` (or whatever `fetch_current_user`
/// resolves to) to verify the token is alive AND grab the user's
/// display name + id at the same time. Bad token → fast fail with a
/// clear error pointing the user at the website's PAT page; we don't
/// silently store a token that doesn't authenticate.
async fn run_token_login(endpoints: &PlatformEndpoints, token: &str) -> Result<StoredCredentials> {
    let token = token.trim();
    if token.is_empty() {
        bail!(
            "Empty --token. Pass a Personal Access Token from {}/settings/tokens, \
             or set $PRISM_LOGIN_TOKEN before running.",
            endpoints
                .api_base
                .trim_end_matches("/api/v1")
                .trim_end_matches('/')
        );
    }

    // Validate by fetching the user profile; this also gives us the
    // display name + user_id we'd otherwise have to ask the website
    // for separately.
    let platform = PlatformClient::new(&endpoints.api_base).with_token(token);
    let profile = platform.fetch_current_user().await.with_context(|| {
        format!(
            "Token rejected by MARC27 ({}). Check the PAT is correct and not revoked. \
             Issue a new one at {}/settings/tokens.",
            endpoints.api_base,
            endpoints
                .api_base
                .trim_end_matches("/api/v1")
                .trim_end_matches('/')
        )
    })?;

    // PATs don't have a refresh token (long-lived by design) and
    // expires_at depends on when the user issued it; the website tells
    // them. Leaving expires_at = None means PRISM treats it as
    // never-expiring locally; on a 401 from MARC27 we fall through
    // to the existing visible-failure detector with a clear message.
    println!();
    println!(
        "\x1b[32m\u{2713}\x1b[0m Authenticated as {} (token login, headless)",
        profile.display_name.as_deref().unwrap_or("(unnamed)")
    );

    Ok(StoredCredentials {
        access_token: token.to_string(),
        // PATs are long-lived and have no refresh token — store
        // empty string so the StoredCredentials shape stays stable
        // and the rest of PRISM doesn't need to learn a new
        // "no-refresh" branch. On 401, the visible-failure detector
        // will tell the user to re-issue the PAT and re-run
        // `prism login --token …`.
        refresh_token: String::new(),
        platform_url: endpoints.api_base.trim_end_matches("/api/v1").to_string(),
        user_id: Some(profile.id.clone()),
        display_name: profile.display_name,
        org_id: None,
        org_name: None,
        project_id: None,
        project_name: None,
        expires_at: None,
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

    // Skip the picker when there's only one choice — silent prompts on a
    // single-org/single-project account were the loudest auth friction.
    let selected_org = if orgs.len() == 1 {
        let only = &orgs[0];
        println!("Using organization: {} ({})", only.name, only.slug);
        only
    } else {
        prompt_select("Select organization", &orgs, |org| {
            format!("{} ({})", org.name, org.slug)
        })?
    };

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

    let selected_project = if projects.len() == 1 {
        let only = &projects[0];
        println!("Using project: {} ({})", only.name, only.slug);
        only
    } else {
        prompt_select("Select project", &projects, |project| {
            format!("{} ({})", project.name, project.slug)
        })?
    };

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

// Old Ink/TypeScript TUI launcher removed — native Ratatui TUI is in crates/cli/src/tui/

fn open_browser(url: &str) -> Result<()> {
    // Defense-in-depth: only http(s) URLs.
    //
    // The two callers pass `verification_uri` from the OAuth device-flow
    // response and `checkout_url` from the billing topup response. Both
    // come from the MARC27 platform and should always be https. Validating
    // here means a compromised or buggy platform can't slip in
    // `--version`, `file:///etc/passwd`, or a `javascript:` payload that
    // some browser opener would happily execute.
    let trimmed = url.trim();
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
        bail!("refusing to open non-http(s) URL: {url}");
    }

    let status = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(trimmed).status()
    } else if cfg!(target_os = "windows") {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", trimmed])
            .status()
    } else {
        std::process::Command::new("xdg-open").arg(trimmed).status()
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

    // Step 3: Query each peer.
    //
    // The early-return on `peer_count == 0` above means peer_list must be
    // Some(non-empty) here, but expressing that with `unwrap()` is brittle —
    // a future refactor of the early-return path would crash production. Use
    // the explicit form so a regression is at most an empty iteration.
    let Some(peers) = peer_list else {
        return Ok(());
    };
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
    use prism_compute::ExperimentPlan;
    use prism_compute::backend::ComputeRouter;
    use prism_compute::byoc::ByocTarget;

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
    let _user_id = creds.and_then(|c| c.user_id.as_deref()).unwrap_or("");
    let _project_id = creds.and_then(|c| c.project_id.as_deref()).unwrap_or("");

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
    if let Some(c) = creds
        && !c.access_token.is_empty()
    {
        print!("Sending to MARC27 platform... ");
        let platform_body = serde_json::json!({
            "title": format!("bug report: {}", &description[..description.len().min(60)]),
            "description": format!(
                "{description}\n\nPRISM v{version}, Python {python_version}, {os_info}, {} cores, {} GB RAM",
                caps.cpu_cores, caps.ram_gb / 1024,
            ),
            "severity": "medium",
        });

        let url = format!("{}/support/tickets", endpoints.api_base);
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

    println!("\nReport submitted. We'll follow up on GitHub and your MARC27 dashboard.");
    Ok(())
}

/// The user's MARC27-cloud model *preference*, with explicit precedence:
/// `LLM_MODEL` env (incl. the project `.env`) → the model selected via
/// `prism use marc27 --model …` (persisted on the chat target) →
/// prism.toml `[llm].model`. `None` = no preference anywhere — the caller
/// falls back to the platform catalog's `default` alias, so NO model name
/// is compiled into the client. Pure so the precedence is unit-testable.
fn resolve_marc27_model(
    env_model: Option<String>,
    target_model: Option<&str>,
    cfg_model: Option<&str>,
) -> Option<String> {
    env_model
        .or_else(|| target_model.map(str::to_string))
        .or_else(|| cfg_model.map(str::to_string))
}

/// Pick the exact platform-catalog entry for a (provider, model) pair and
/// return its `model_id` — the string the platform bills by.
///
/// Provider and model are SEPARATE axes on purpose: the same model can be
/// served by more than one provider at different prices and different
/// billing paths (e.g. `claude-sonnet-5` provider=anthropic vs
/// `anthropic/claude-sonnet-5` provider=openrouter). A single conflated
/// slug picked between those silently; making the provider an explicit
/// filter (`LLM_PROVIDER`) puts that routing choice in configuration where
/// it belongs.
///
/// Matching, within the provider filter (no filter = all entries):
/// 1. exact `model_id`
/// 2. catalog alias (e.g. `default`)
/// 3. bare-name suffix — `claude-sonnet-5` matches a router's
///    `anthropic/claude-sonnet-5`
///
/// No preference at all → the entry carrying the platform's `default`
/// alias. `None` = no catalog match (caller decides the fallback).
fn resolve_catalog_model(
    models: &[serde_json::Value],
    provider: Option<&str>,
    preference: Option<&str>,
) -> Option<String> {
    let id_of = |m: &serde_json::Value| value_string(m, &["model_id", "id"]).map(str::to_string);
    let has_alias = |m: &serde_json::Value, alias: &str| {
        m.get("aliases")
            .and_then(|a| a.as_array())
            .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(alias)))
    };
    let candidates: Vec<&serde_json::Value> = models
        .iter()
        .filter(|m| {
            provider.is_none_or(|p| value_string(m, &["provider_slug", "provider"]) == Some(p))
        })
        .collect();

    match preference {
        Some(pref) => candidates
            .iter()
            .find(|m| value_string(m, &["model_id", "id"]) == Some(pref))
            .or_else(|| candidates.iter().find(|m| has_alias(m, pref)))
            .or_else(|| {
                let suffix = format!("/{pref}");
                candidates.iter().find(|m| {
                    value_string(m, &["model_id", "id"]).is_some_and(|id| id.ends_with(&suffix))
                })
            })
            .and_then(|m| id_of(m)),
        None => candidates
            .iter()
            .find(|m| has_alias(m, "default"))
            .and_then(|m| id_of(m)),
    }
}

/// The per-project MARC27 LLM base URL. The agent's LLM client recognises
/// the trailing `/llm` and drives it over MARC27's native `/stream` SSE
/// endpoint (`{api_base}/projects/{id}/llm/stream`). Pure so the join is
/// unit-testable.
fn marc27_llm_url_for_project(api_base: &str, project_id: &str) -> String {
    format!(
        "{}/projects/{}/llm",
        api_base.trim_end_matches('/'),
        project_id
    )
}

/// Look up the active model's context/token limits in the platform
/// catalog (`GET /projects/{id}/llm/models`, same endpoint as `prism
/// models list`). Fail-open on every path — offline, no auth, network
/// error, model not in catalog — returning `(None, None)`: the agent
/// then uses conservative fallback behavior instead of a wrong number.
/// Bounded by a short timeout so backend startup is never held hostage.
/// Fetch the platform model catalog once (`GET /projects/{id}/llm/models`,
/// same endpoint as `prism models list`). Fail-open on every path —
/// offline, no auth, network error → empty Vec. One fetch serves BOTH
/// model resolution (`resolve_catalog_model`) and limits (`model_limits`).
async fn fetch_model_catalog(paths: &PrismPaths) -> Vec<serde_json::Value> {
    if std::env::var("PRISM_OFFLINE").as_deref() == Ok("1") {
        return Vec::new();
    }
    let Ok((api_base, auth)) = resolve_agent_auth() else {
        return Vec::new();
    };
    let Ok(project_id) = resolve_active_project_id(paths) else {
        return Vec::new();
    };
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    else {
        return Vec::new();
    };
    let response: serde_json::Value = match auth
        .apply(client.get(format!("{api_base}/projects/{project_id}/llm/models")))
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
    {
        Ok(resp) => match resp.json().await {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        },
        Err(_) => return Vec::new(),
    };
    value_array(&response, &["models", "items", "data"])
        .cloned()
        .unwrap_or_default()
}

/// The model's context/output limits from an already-fetched catalog.
/// (None, None) for unknown models (local llama.cpp, offline) → the agent
/// falls back to turn-count compaction.
fn model_limits(models: &[serde_json::Value], model_id: &str) -> (Option<u64>, Option<u64>) {
    let Some(entry) = models
        .iter()
        .find(|m| value_string(m, &["model_id", "id"]) == Some(model_id))
    else {
        return (None, None);
    };
    (
        entry.get("context_window").and_then(|v| v.as_u64()),
        entry.get("max_output_tokens").and_then(|v| v.as_u64()),
    )
}

/// Resolve the base URL for the MARC27-cloud chat target.
///
/// `LLM_BASE_URL` overrides everything (power users, local-model dev, the
/// micro-server test). Otherwise route at the signed-in project's MARC27
/// LLM endpoint so the cloud enforces each model's real context window +
/// output cap. Only when there is no usable session (no creds / no
/// project) do we fall back to `cfg_llm.url` — whose default is
/// `http://localhost:8080` (llama.cpp). Using that fallback
/// unconditionally is exactly what made "MARC27 cloud" chat run on a
/// local 16k model.
fn marc27_llm_base_url(paths: &PrismPaths, api_base: &str, fallback_url: &str) -> String {
    if let Ok(explicit) = std::env::var("LLM_BASE_URL") {
        return explicit;
    }
    if let Some(project_id) = paths
        .load_cli_state()
        .ok()
        .and_then(|s| s.credentials)
        .and_then(|c| c.project_id)
    {
        return marc27_llm_url_for_project(api_base, &project_id);
    }
    fallback_url.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marc27_model_prefers_user_selection_over_default() {
        // The reported bug: user picked sonnet, backend served gpt-5.5.
        assert_eq!(
            resolve_marc27_model(None, Some("claude-sonnet-4"), None).as_deref(),
            Some("claude-sonnet-4"),
            "the user's `prism use marc27 --model` pick must win over the default"
        );
        // No selection anywhere → None: NO model name is compiled in; the
        // caller falls back to the platform catalog's `default` alias.
        assert_eq!(resolve_marc27_model(None, None, None), None);
        // prism.toml [llm].model used when the target carries no model.
        assert_eq!(
            resolve_marc27_model(None, None, Some("mistral-large-latest")).as_deref(),
            Some("mistral-large-latest")
        );
        // Explicit LLM_MODEL env overrides everything.
        assert_eq!(
            resolve_marc27_model(Some("gpt-5.5".into()), Some("claude-sonnet-4"), None).as_deref(),
            Some("gpt-5.5")
        );
    }

    /// Fixture mirroring the live catalog shape that caused the billing
    /// bug: the SAME model served by two providers under different ids.
    fn catalog_fixture() -> Vec<serde_json::Value> {
        vec![
            serde_json::json!({
                "model_id": "claude-sonnet-5",
                "provider_slug": "anthropic",
                "aliases": ["default"],
            }),
            serde_json::json!({
                "model_id": "anthropic/claude-sonnet-5",
                "provider_slug": "openrouter",
                "aliases": [],
            }),
            serde_json::json!({
                "model_id": "gpt-5.5",
                "provider_slug": "openai",
                "aliases": [],
            }),
        ]
    }

    #[test]
    fn catalog_model_provider_axis_disambiguates_same_model() {
        let cat = catalog_fixture();
        // Same LLM_MODEL, different LLM_PROVIDER → different catalog entry.
        // This is the whole point of the two-axis config: the provider
        // choice (billing route + price) is explicit, never inferred from
        // an opaque combined slug.
        assert_eq!(
            resolve_catalog_model(&cat, Some("anthropic"), Some("claude-sonnet-5")).as_deref(),
            Some("claude-sonnet-5")
        );
        assert_eq!(
            resolve_catalog_model(&cat, Some("openrouter"), Some("claude-sonnet-5")).as_deref(),
            Some("anthropic/claude-sonnet-5"),
            "bare name must suffix-match the router's prefixed id"
        );
        // Provider filter with no matching model → None (caller falls back).
        assert_eq!(
            resolve_catalog_model(&cat, Some("openai"), Some("claude-sonnet-5")),
            None
        );
    }

    #[test]
    fn catalog_model_no_preference_uses_platform_default_alias() {
        let cat = catalog_fixture();
        // Nothing configured anywhere → the platform's `default` alias
        // decides. No model name lives in the client.
        assert_eq!(
            resolve_catalog_model(&cat, None, None).as_deref(),
            Some("claude-sonnet-5")
        );
        // Alias is also resolvable as an explicit preference.
        assert_eq!(
            resolve_catalog_model(&cat, None, Some("default")).as_deref(),
            Some("claude-sonnet-5")
        );
        // Exact id beats alias/suffix when no provider filter is given.
        assert_eq!(
            resolve_catalog_model(&cat, None, Some("anthropic/claude-sonnet-5")).as_deref(),
            Some("anthropic/claude-sonnet-5")
        );
        // Empty catalog (offline) → None.
        assert_eq!(
            resolve_catalog_model(&[], None, Some("claude-sonnet-5")),
            None
        );
    }

    #[test]
    fn marc27_llm_url_targets_the_project_stream_endpoint() {
        // `{api_base}/projects/{id}/llm` — the agent client appends
        // `/stream`, giving the real route the API exposes.
        assert_eq!(
            marc27_llm_url_for_project("https://api.marc27.com/api/v1", "proj-123"),
            "https://api.marc27.com/api/v1/projects/proj-123/llm"
        );
        // A trailing slash on the api_base must not double up.
        assert_eq!(
            marc27_llm_url_for_project("https://api.marc27.com/api/v1/", "proj-123"),
            "https://api.marc27.com/api/v1/projects/proj-123/llm"
        );
    }

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
        unsafe {
            std::env::remove_var("MARC27_PROJECT_ID");
        }
        assert_eq!(env_project_override(), None);
        unsafe {
            std::env::set_var("MARC27_PROJECT_ID", "   ");
        }
        assert_eq!(env_project_override(), None);
        unsafe {
            std::env::set_var("MARC27_PROJECT_ID", "project-123");
        }
        assert_eq!(env_project_override(), Some("project-123".to_string()));
        unsafe {
            std::env::remove_var("MARC27_PROJECT_ID");
        }
    }

    #[test]
    fn default_project_slug_has_prism_prefix() {
        let slug = default_project_slug();
        assert!(slug.starts_with("prism-"));
        assert!(slug.len() > "prism-".len());
    }

    #[test]
    fn default_ssh_user_ignores_empty_values() {
        unsafe {
            std::env::remove_var("USER");
        }
        assert_eq!(default_ssh_user(), None);
        unsafe {
            std::env::set_var("USER", "   ");
        }
        assert_eq!(default_ssh_user(), None);
        unsafe {
            std::env::set_var("USER", "sid");
        }
        assert_eq!(default_ssh_user(), Some("sid".to_string()));
        unsafe {
            std::env::remove_var("USER");
        }
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
