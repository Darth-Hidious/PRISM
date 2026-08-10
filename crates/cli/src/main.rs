// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! PRISM CLI — the main entry point for the `prism` binary.
//!
//! Handles command routing (setup, login, node, workflow, etc.), auth bootstrap
//! via device-flow OAuth, Python worker supervision, and dynamic workflow
//! discovery from `~/.prism/workflows/`.

mod boot;
mod boot_checks;
use prism_core::brand;
use prism_core::chat_config;
mod doctor;
mod local_llm;
mod mcp_server_native;
mod notebook;
mod onboarding;
mod papers;
use prism_core::providers;
mod pyiron_cmd;
mod tool_sync;
mod use_command;

use std::collections::BTreeMap;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
// std::process::Stdio removed — old Ink TUI launcher no longer needed
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use clap::{Parser, Subcommand};
use prism_client::DeviceFlowAuth;
use prism_client::PlatformResponseExt;
use prism_client::api::PlatformClient;
use prism_client::auth::{DeviceCodeResponse, TokenResponse};
use prism_proto::NodeCapabilities;
use prism_python_bridge::{ToolServer, ensure_venv};
use prism_runtime::auth::{self, AuthSurface, PlatformAuth};
use prism_runtime::platform_env::PlatformVar;
use prism_runtime::{PlatformEndpoints, PrismPaths, StoredCredentials};

// Loopback detection lives with the local-server probe that also needs it,
// so ingest locality and discovery cannot disagree about what "on this
// machine" means.
use crate::local_llm::is_loopback_url;
use prism_workflows::{
    WorkflowExecutionOptions, WorkflowRunResult, WorkflowSpec, discover_workflows,
    execute_workflow_with_policy_and_options, find_workflow, load_workflow_from_str,
    parse_workflow_command_args,
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
    /// Run without contacting the hosted platform.
    #[arg(long, global = false)]
    offline: bool,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Run first-time native setup and platform login.
    Setup {
        /// Explicitly allow the retained device flow in a real TTY.
        #[arg(long)]
        interactive_auth: bool,
    },
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
    /// Authenticate against the configured hosted platform.
    ///
    /// Non-interactive by default: use `--token <PAT>` or configure
    /// `MARC27_API_KEY`. The retained device flow requires the explicit
    /// `--interactive-auth` opt-in and a real TTY; PRISM never opens a browser.
    Login {
        /// Use a pre-issued Personal Access Token from the platform website.
        /// This is non-interactive and suitable for headless runs.
        #[arg(long, value_name = "PAT", env = "PRISM_LOGIN_TOKEN")]
        token: Option<String>,

        /// Retained compatibility flag. Device auth is always manual; PRISM
        /// never launches a browser.
        #[arg(long, conflicts_with = "token")]
        no_browser: bool,

        /// Explicitly allow the retained device flow in a real TTY.
        #[arg(long, conflicts_with = "token")]
        interactive_auth: bool,
    },
    /// Show runtime paths, endpoints, and auth status.
    Status,
    /// Inspect the local provenance ledger (verified run history).
    /// VS3: surfaces `stats()` (ok/error/other counts) and `query_failures()`
    /// to a HUMAN — the store was queryable by code but no one could actually
    /// ask "which runs failed?" from the command line. All queries are local
    /// (`~/.prism/provenance.db`); no network.
    Provenance {
        #[command(subcommand)]
        command: ProvenanceCommands,
    },
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
    /// Durable schedules and watchers that wake a long-running goal back up —
    /// on a clock, on a cron expression, or when a condition becomes true —
    /// so a goal survives reboots and crashes without a human restarting it.
    Schedule {
        #[command(subcommand)]
        command: ScheduleCommands,
    },
    /// Start the agent backend (JSON-RPC server for TUI frontend).
    //
    // No `--python` of its own. It used to declare one, defaulting to the
    // literal "python3", which shadowed nothing and served only to hand the
    // handler a sentinel instead of the resolved interpreter — see the
    // global `--python` on `Cli`.
    Backend {
        #[arg(long, default_value = ".")]
        project_root: PathBuf,
    },
    /// Serve the agent over JSON-RPC on stdio for external frontends
    /// (PRISM Desktop, IDE extensions) — the LSP-server role. Same-user
    /// stdio trust boundary; opens no network socket.
    IpcServe {
        #[arg(long, default_value = ".")]
        project_root: PathBuf,
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
    /// Provision science Python extras or vendor wheels for an offline node.
    Provision {
        #[command(subcommand)]
        command: ProvisionCommands,
    },
    /// List available Python tools.
    Tools,
    /// Run the native (Rust) MCP server — exposes PRISM's Rust-side tools
    /// (query, ingest, mesh, workflow, …) over stdio JSON-RPC. Forge spawns
    /// this as a subprocess so the LLM can call Rust tools without going
    /// through Python.
    #[command(name = "mcp-server-native", hide = true)]
    McpServerNative,
    /// Diagnostic snapshot — checks llama-server, models, Python venv, auth
    /// and platform connectivity. Run this first when something feels off.
    Doctor {
        /// Repair what can be repaired (rebuild the Python venv, warm the
        /// embedding model cache) and print the exact command for everything
        /// else. Nothing is reported as fixed unless the check that failed
        /// passes on re-run.
        #[arg(long)]
        fix: bool,
    },
    /// PRISM node lifecycle commands.
    Node {
        #[command(subcommand)]
        command: NodeCommands,
    },
    /// Fast literature retrieval engine (arXiv, OpenAlex, Crossref, PubMed,
    /// Semantic Scholar, Europe PMC preprints, ChemRxiv, DOAJ). Concurrent,
    /// polite, resumable; output is JSON aimed at EMMO-typed ingestion.
    Papers {
        #[command(subcommand)]
        command: crate::papers::PapersCommands,
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
        /// Skip LLM extraction (schema detection only).
        #[arg(long)]
        schema_only: bool,
        /// Show current ingest/job status instead of ingesting a path.
        #[arg(long)]
        status: bool,
        /// Send the file to the hosted platform instead of extracting locally.
        ///
        /// Without this, `prism ingest` runs the LOCAL pipeline — it needs a
        /// local LLM and a local runtime, and it writes to the local Turso
        /// store. There was no way to put a PDF into your hosted knowledge
        /// graph from the CLI at all, which is the thing most people actually
        /// want; the only route was calling the API by hand.
        #[arg(long)]
        platform: bool,
        /// Watch a directory for new/modified files and ingest continuously.
        #[arg(long)]
        watch: bool,
        /// Runtime URL for local PDF text extraction. PRISM starts a runtime
        /// here automatically when the URL is on this machine and none is up.
        #[arg(long, default_value = prism_node::runtime_service::DEFAULT_RUNTIME_URL)]
        runtime_url: String,
        /// Output JSON instead of human-readable progress.
        #[arg(long)]
        json: bool,
        /// Path to a YAML ontology mapping file (custom entity/relationship rules).
        #[arg(long)]
        mapping: Option<PathBuf>,
    },
    /// MatKG reference knowledge graph (Venugopal & Olivetti 2024, CC BY 4.0).
    Matkg {
        #[command(subcommand)]
        command: MatkgCommands,
    },
    /// Query the knowledge graph.
    Query {
        /// Entity name or search text.
        text: String,
        /// Semantic vector search.
        #[arg(long)]
        semantic: bool,
        /// Use the hosted platform API instead of the local graph.
        #[arg(long)]
        platform: bool,
        /// Output as JSON (for piping to other tools / agents).
        #[arg(long)]
        json: bool,
        /// Query all known mesh peers and merge results.
        #[arg(long)]
        federated: bool,
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
    /// Submit a compute job (local Docker, the hosted platform, or BYOC).
    Run {
        /// Container image to run, or a pre-staged .sif path for SLURM.
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
        /// Platform API URL (for the `marc27` backend). Defaults to the public
        /// gateway; override only to point at a staging/self-hosted control plane.
        #[arg(long, default_value = "https://api.marc27.com/api/v1")]
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
        /// SLURM allocation account.
        #[arg(long)]
        slurm_account: Option<String>,
        /// SLURM wall time, for example 02:00:00.
        #[arg(long)]
        slurm_time: Option<String>,
        /// SLURM generic resources, for example gpu:a100:1.
        #[arg(long)]
        slurm_gres: Option<String>,
        /// Total SLURM memory per node, for example 64G.
        #[arg(long, conflicts_with = "slurm_mem_per_cpu")]
        slurm_mem: Option<String>,
        /// SLURM memory per allocated CPU, for example 8G.
        #[arg(long, conflicts_with = "slurm_mem")]
        slurm_mem_per_cpu: Option<String>,
        /// SLURM CPUs per task.
        #[arg(long)]
        slurm_cpus_per_task: Option<u32>,
        /// SLURM node count.
        #[arg(long)]
        slurm_nodes: Option<u32>,
        /// SLURM task count.
        #[arg(long)]
        slurm_ntasks: Option<u32>,
        /// SLURM array expression, for example 0-15%4.
        #[arg(long)]
        slurm_array: Option<String>,
        /// Run only after this SLURM job id completes successfully.
        #[arg(long)]
        slurm_dependency_afterok: Option<u64>,
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
    /// managed in the platform UI; the CLI only inspects state.
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
        /// Don't open a GitHub issue (only send to the hosted platform).
        #[arg(long)]
        no_github: bool,
    },
    /// Browse and install tools and workflows from the platform marketplace.
    Marketplace {
        #[command(subcommand)]
        command: MarketplaceCommands,
    },
    /// Start a hosted research loop for a materials-science goal.
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
    /// Deploy a model or service to the hosted compute platform.
    Deploy {
        #[command(subcommand)]
        command: DeployCommands,
    },
    /// Discover hosted LLM models available for the active platform project.
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
    /// Deploy any container image or marketplace resource and invoke it,
    /// one call: create the deployment → poll until `running` (timeout) →
    /// POST the invoke payload → stop the deployment. The stop is always
    /// attempted (unless `--keep`), even when the invoke request itself
    /// fails — no orphaned billable deployment left behind by a failed
    /// call. Unlike `predict`, this never reuses an already-running
    /// deployment of the same name; it always creates fresh (general
    /// image/resource deploys aren't safely reusable the way named
    /// marketplace slugs are). Prints one JSON document; a failed invoke
    /// is a real error exit, never a success document.
    DeployAndInvoke {
        /// Deployment name shown in the platform UI.
        #[arg(long)]
        name: String,
        /// Container image to deploy directly.
        #[arg(long)]
        image: Option<String>,
        /// Marketplace resource slug to deploy instead of a raw image.
        #[arg(long)]
        resource_slug: Option<String>,
        /// Target deployment backend: `local`, `mesh`, `runpod`, `lambda`.
        #[arg(long, default_value = "local")]
        target: String,
        /// GPU type to request (omit for CPU).
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
        /// Path on the deployment endpoint to POST the invoke payload to.
        #[arg(long, default_value = "/predict")]
        invoke_path: String,
        /// JSON body posted to `invoke_path` (e.g. '{"task": "single_point"}').
        #[arg(long, default_value = "{}")]
        input: String,
        /// Seconds to wait for the deployment to become ready.
        #[arg(long, default_value_t = 900)]
        ready_timeout_secs: u64,
        /// Keep the deployment running after the result (skips auto-stop).
        #[arg(long)]
        keep: bool,
    },
    /// List GPU offers purchasable through the hosted compute platform.
    ///
    /// Prints the live catalog (type, VRAM, region, provider, $/hr) as one
    /// raw JSON array on stdout — machine-readable by design: the TUI
    /// `/gpus` picker and agents parse this output. Failures print
    /// `{"error": "..."}` and still exit 0 so callers always get exactly
    /// one JSON document.
    Gpus,
    /// One-shot compute-broker jobs (GPU/CPU) on the hosted platform.
    ///
    /// Read actions (gpus/providers/estimate/status) and cancel are safe;
    /// `submit` dispatches a real, billable job. Every subcommand prints one
    /// JSON document on stdout — machine-readable by design for agents.
    Compute {
        #[command(subcommand)]
        command: ComputeCommands,
    },
    /// Run a compute-broker job to completion, one call: submit → poll
    /// status → return the real result. A failed/cancelled job is a real
    /// error exit, never a success document. Price a job for free first
    /// with `prism compute estimate` before dispatching it with this
    /// command — `estimate` stays its own cheap, non-billable command.
    ComputeRun {
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
        /// Wall-time cap in seconds for the job itself (default 3600).
        #[arg(long)]
        timeout: Option<u64>,
        /// Environment variables (repeatable): --env KEY=VALUE.
        #[arg(long = "env")]
        env: Vec<String>,
        /// Seconds to wait for the job to reach a terminal state.
        #[arg(long, default_value_t = 1800)]
        poll_timeout_secs: u64,
    },
    /// Knowledge-plane reads + platform ingest (hosted knowledge graph).
    ///
    /// entity/paths/corpora are read-only graph/catalog lookups; `ingest`
    /// submits a background extraction job. Every subcommand prints one JSON
    /// document on stdout. Graph search + semantic search live under
    /// `prism query --platform`; graph stats under `prism ingest --status`.
    Knowledge {
        #[command(subcommand)]
        command: KnowledgeCommands,
    },
    /// Submit a knowledge-graph ingest job and wait for it to finish, one
    /// call: POST the ingest job → poll until done → return the resulting
    /// graph references. A failed job is a real error exit, never a
    /// success document.
    ///
    /// NOTE: the platform's only wired ingest-job endpoints in this
    /// codebase are `POST /knowledge/ingest-job` (submit) and
    /// `GET /knowledge/ingest-jobs` (list ALL jobs) — there is no per-job
    /// GET, so this polls the list and matches the returned job id.
    IngestAndWait {
        /// Source URL to fetch and extract.
        #[arg(long)]
        url: Option<String>,
        /// Free-text query to extract entities/embeddings from.
        #[arg(long)]
        query: Option<String>,
        /// Extraction mode: graph, embed, or full.
        #[arg(long, default_value = "full")]
        mode: String,
        /// Seconds to wait for the ingest job to finish.
        #[arg(long, default_value_t = 1800)]
        poll_timeout_secs: u64,
    },
    /// Run multi-agent discourse workflows backed by the hosted platform.
    Discourse {
        #[command(subcommand)]
        command: DiscourseCommands,
    },
    /// Pick where chat turns are routed: the hosted platform
    /// (default), a local OpenAI-compatible LLM, or a direct vendor
    /// (Anthropic / OpenAI / etc). Hosted tools — knowledge graph,
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
enum MatkgCommands {
    /// Stream MatKG's reified SUBRELOBJ N-Triples (.nt, .nt.gz, or .tar.gz)
    /// into the local knowledge graph (`~/.prism/provenance.db`) under the
    /// isolated `local@matkg` tenant, as evidence class Research (ORANGE),
    /// PROV-O-attributed to the dataset DOI. Bounded by default; every
    /// skipped row is reported. Re-running does not inflate anything.
    Load {
        /// Path to SUBRELOBJ.nt, SUBRELOBJ.nt.gz, or SUBRELOBJ.nt.tar.gz.
        path: PathBuf,
        /// Skip rows whose co-occurrence count is below this. The pinned
        /// MatKG 1.4 minimum is 25, so the default filters nothing — it
        /// exists to be raised.
        #[arg(long, default_value_t = prism_ingest::matkg::DEFAULT_MIN_COUNT)]
        min_count: u32,
        /// Load at most this many rows, strongest counts first.
        #[arg(long, default_value_t = prism_ingest::matkg::DEFAULT_LIMIT)]
        limit: usize,
        /// Deliberately load EVERY row passing --min-count. The full file
        /// is 5.4M rows — millions of store writes; hours on a laptop.
        #[arg(long, conflicts_with = "limit")]
        all: bool,
        /// Output the full load report as JSON (includes the required
        /// CC-BY attribution).
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum BillingCommands {
    /// Show usage breakdown by service.
    Usage,
    /// Show transaction history.
    History,
    /// Show credit pricing table.
    Prices,
    /// Buy credits — lists packs; with a slug, prints the checkout URL for manual opening.
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
enum ProvenanceCommands {
    /// Print aggregate counts: total records and the ok/error/other breakdown.
    Stats,
    /// List failed tool runs (status='error'), newest first.
    Failures {
        /// Optional session id to scope to (defaults to all sessions).
        #[arg(long)]
        session_id: Option<String>,
        /// Max failures to list (default 20; the store caps at 1000).
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
}

fn format_classified_reward(reward: f64, evidence_class: prism_campaign::EvidenceClass) -> String {
    format!(
        "{reward:.4} [{} {}]",
        evidence_class.color().to_ascii_uppercase(),
        evidence_class.as_str()
    )
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
    /// Run PRISM's campaign workload as a signal-aware batch entrypoint.
    /// Reads the campaign specification from PRISM_INPUTS and addresses its
    /// durable checkpoint by PRISM_TASK_ID.
    BatchEntrypoint,
    /// Show the status of a campaign (from its checkpoint).
    Status {
        /// Campaign ID.
        id: String,
    },
    /// List all campaign checkpoints on this machine.
    List,
}

#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
enum BatchStringList {
    List(Vec<String>),
    CommaSeparated(String),
}

impl Default for BatchStringList {
    fn default() -> Self {
        Self::List(Vec::new())
    }
}

impl BatchStringList {
    fn into_vec(self) -> Vec<String> {
        let values = match self {
            Self::List(values) => values,
            Self::CommaSeparated(values) => values.split(',').map(str::to_string).collect(),
        };
        values
            .into_iter()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect()
    }
}

#[derive(Debug, serde::Deserialize)]
struct BatchCampaignInputs {
    goal: String,
    #[serde(default)]
    elements: BatchStringList,
    #[serde(default)]
    objective: String,
    #[serde(default)]
    constraints: BatchStringList,
    #[serde(default)]
    seeds: BatchStringList,
    max_iterations: Option<usize>,
    batch_size: Option<usize>,
    budget: Option<f64>,
    checkpoint_every: Option<usize>,
    #[serde(default)]
    approval_gates: Vec<usize>,
}

/// Arguments for `schedule create`. A named struct (rather than inline
/// variant fields) so the enum stays small next to its one-word variants.
#[derive(Debug, clap::Args)]
struct ScheduleCreateArgs {
    /// Goal (campaign) id to wake up.
    #[arg(long)]
    goal: String,
    /// Recurring interval, e.g. 30s, 15m, 6h, 2d.
    #[arg(long, group = "trigger")]
    every: Option<String>,
    /// Cron expression. 5-field crontab ("0 */6 * * *") or 6-field
    /// seconds-first.
    #[arg(long, group = "trigger")]
    cron: Option<String>,
    /// One-shot: fire once at this unix timestamp.
    #[arg(long, group = "trigger")]
    at: Option<i64>,
    /// Watcher: fire when this path appears.
    #[arg(long, group = "trigger")]
    watch_file: Option<PathBuf>,
    /// Watcher: fire when another goal reaches --watch-goal-status.
    #[arg(long, group = "trigger")]
    watch_goal: Option<String>,
    /// Status the watched goal must reach (default: completed).
    #[arg(long, default_value = "completed")]
    watch_goal_status: String,
    /// Watcher: fire when this local graph database has grown to
    /// --corpus-at-least entities.
    #[arg(long, group = "trigger")]
    watch_corpus: Option<PathBuf>,
    /// Entity count the watched corpus must reach.
    #[arg(long)]
    corpus_at_least: Option<i64>,
    /// Scope the corpus count to one tenant. Omit only on a single-tenant
    /// node — an unscoped count mixes every tenant's rows.
    #[arg(long)]
    corpus_tenant: Option<String>,
    /// Hard ceiling on how many times this schedule may resume the goal.
    /// The one spend guard that works even when nothing reports a cost.
    #[arg(long, default_value_t = 100)]
    max_fires: u32,
    /// Stop and report after this many consecutive wake-ups that produced
    /// no progress.
    #[arg(long, default_value_t = 3)]
    max_no_progress: u32,
}

#[derive(Debug, Subcommand)]
enum ScheduleCommands {
    /// Create a schedule that wakes a goal back up. Exactly one trigger.
    Create(Box<ScheduleCreateArgs>),
    /// List every schedule with its state and last outcome.
    List,
    /// Cancel a schedule (it never fires again).
    Cancel {
        /// Schedule id from `schedule list`.
        id: String,
    },
    /// Evaluate every schedule once and resume whatever is due. This is the
    /// command an OS timer (launchd / systemd / cron) runs.
    Tick,
    /// Run `tick` in a loop in the foreground — for containers and pods where
    /// the runtime's restart policy is the supervisor. Dies with this
    /// process; on a normal host prefer `schedule install` + `tick`.
    Daemon {
        /// Seconds between ticks.
        #[arg(long, default_value_t = 60)]
        interval: u64,
    },
    /// Print (or write) the OS unit that owns the tick heartbeat.
    Install {
        /// Seconds between ticks.
        #[arg(long, default_value_t = 60)]
        interval: u64,
        /// Write the unit to the user's agent/unit directory instead of
        /// printing it, and print the command that activates it.
        #[arg(long)]
        write: bool,
    },
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
enum ProvisionCommands {
    /// Install one science extra into the active PRISM Python environment.
    Extra {
        /// Extra name: qe, calphad, mace, precipitation, lpbf, simulation, or ml.
        name: String,
        /// Offline wheelhouse; defaults to ~/.prism/wheelhouse.
        #[arg(long)]
        wheelhouse: Option<PathBuf>,
    },
    /// Vendor the core package and science extra wheels on a connected machine.
    Wheels {
        /// Directory to receive the wheelhouse.
        #[arg(long)]
        output: PathBuf,
        /// One or more science extras, comma-separated or repeated.
        #[arg(long = "extra", value_delimiter = ',', required = true)]
        extras: Vec<String>,
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
        /// Run in offline mode (no platform registration; mesh networking
        /// disabled).
        #[arg(long)]
        offline: bool,
        /// Dashboard HTTP port (default: 7327).
        #[arg(long, default_value_t = 7327)]
        dashboard_port: u16,
        /// Skip starting managed services (Kafka, Spark).
        #[arg(long)]
        no_services: bool,
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
    /// Stream logs from a managed service (kafka, spark, firecrawl).
    Logs {
        /// Service name: e.g. kafka, spark, or firecrawl.
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
    /// Manage the durable node token — a stable, non-rotating API key that
    /// keeps `node up` alive across session refresh-token rotation (the
    /// rotating session token is what kills long-running nodes today).
    Token {
        #[command(subcommand)]
        command: TokenCommands,
    },
}

/// Durable node-token subcommands.
#[derive(Debug, Subcommand)]
enum TokenCommands {
    /// Mint a stable node token (node-scoped API key) for a project and store
    /// it locally so `node up` uses it instead of the rotating session token.
    /// Uses the current session to mint once; the token itself never rotates.
    Mint {
        /// Project to scope the token to. Defaults to the active project.
        #[arg(long)]
        project: Option<String>,
    },
    /// Revoke the stored node token (deletes the platform key + the local file).
    Revoke,
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
    /// Pull a dataset from a peer node NOW — no Kafka broker required.
    /// The peer's facts land in the local store under the peer's own
    /// tenant (`mesh:{peer node id}`), attributable and separable.
    Sync {
        /// Dataset name to pull.
        dataset_name: String,
        /// Base URL of the peer node (e.g. http://192.168.1.20:7327).
        #[arg(long)]
        peer: String,
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
/// **Trust is managed in the platform UI, not from this CLI.** The
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
    /// across the Fabric. Sourced from the hosted platform; trust is
    /// transitive via the platform root CA.
    Peers {
        /// Emit JSON instead of the human-readable summary.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum MarketplaceCommands {
    /// Search the platform marketplace for tools and workflows.
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
    /// Pull tool updates from the platform marketplace. Re-downloads any
    /// tool whose marketplace version differs from the locally-installed
    /// one. Remote wins: locally-edited files are overwritten. Use
    /// `--dry-run` to see what would change without modifying anything.
    #[command(alias = "pull")]
    Update {
        /// Show what would be updated without downloading anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Publish PRISM's own materials tools to the MARC27 marketplace so
    /// they are discoverable without installing all of PRISM.
    ///
    /// The catalog is `app/tools/marketplace_catalog.json`, held to account
    /// against the live tool registry by `tests/test_marketplace_catalog.py`.
    /// Each entry is created as a draft, has its tags/license set, then is
    /// submitted for review — a platform reviewer still has to approve it
    /// before it appears in the public listing.
    Publish {
        /// Show what would be published without calling the platform.
        #[arg(long)]
        dry_run: bool,
        /// Publish only this slug (default: every entry in the catalog).
        #[arg(long)]
        slug: Option<String>,
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
    /// List hosted models available to the active platform project.
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
    /// Register a custom or self-hosted model in ~/.prism/models.toml
    /// (e.g. z.ai GLM, vLLM, Ollama). No network or login required. The
    /// entry overrides the platform catalog for cost/context accounting.
    Register {
        /// Model ID as the endpoint expects it, e.g. `glm-5.2`.
        model_id: String,
        /// Provider label, e.g. `zhipu`, `local`, `openai`.
        #[arg(long)]
        provider: String,
        /// Custom endpoint base URL (self-hosted / direct vendor).
        #[arg(long)]
        base_url: Option<String>,
        /// Name of the env var holding the API key (never the key itself).
        #[arg(long)]
        api_key_env: Option<String>,
        /// USD per 1M input tokens (0 for free/local models).
        #[arg(long)]
        input_price: f64,
        /// USD per 1M output tokens (0 for free/local models).
        #[arg(long)]
        output_price: f64,
        /// Context window in tokens.
        #[arg(long)]
        context_window: usize,
        /// Max output tokens per response (default 16384).
        #[arg(long)]
        max_output_tokens: Option<usize>,
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
/// `Marc27` — the frozen wire id for the hosted route — stays on the
/// default route but pins which upstream model the platform should
/// serve. `Local` and `Provider` are the two chat targets that need no
/// platform at all. `Show` prints the current state (chat target +
/// tools auth state). `Reset` goes back to the hosted route without a
/// pinned model (PRISM's compiled-in default).
#[derive(Debug, Subcommand)]
enum UseCommands {
    /// Stay on the hosted route, but pin a specific upstream model
    /// (`gpt-5.5`, `claude-sonnet-4`, `mistral-large-latest`, …).
    /// The platform's own vendor keys stay there — PRISM only passes
    /// the model id forward.
    Marc27 {
        /// Upstream model id the platform should serve. If omitted,
        /// PRISM uses its compiled-in default.
        #[arg(long)]
        model: Option<String>,
    },
    /// Route chat turns to an OpenAI-compatible local server (Ollama,
    /// llama.cpp, vLLM, etc.). Hosted platform tools stay available
    /// when the user is logged in.
    Local {
        /// Base URL of a local server, or `gguf://local` for PRISM's embedded
        /// runtime. Examples: `http://localhost:11434/v1`, `gguf://local`.
        #[arg(long)]
        url: String,
        /// Model name to send in chat requests (whatever the local
        /// server advertises — `llama-3.1-70b`, `mistral-7b-instruct`,
        /// `qwen2.5-coder`, etc.). Omit it and PRISM asks the server:
        /// if it is serving exactly one model, that one is used.
        #[arg(long)]
        model: Option<String>,
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
    /// Hosted platform tools stay available when the user is logged in.
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
    /// List every provider PRISM can route chat to, and which ones have
    /// credentials present right now. Read-only.
    List,
    /// Print the current chat target and the tools-auth state.
    Show,
    /// Reset chat target back to the hosted route (the default).
    Reset,
}

#[derive(Debug, Clone)]
struct SelectedContext {
    org_id: Option<String>,
    org_name: Option<String>,
    project_id: Option<String>,
    project_name: Option<String>,
}

/// Whether this command will actually reach the Python tool server.
///
/// Deliberately a deny-list of the pure-Rust commands rather than an
/// allow-list of the Python ones: anything unrecognised falls through to
/// `true` and provisions the venv exactly as before. The worst case of a
/// stale list is therefore the old (slow) behaviour, never a command handed
/// a path to an interpreter nobody built.
fn preflight_command_auth(command: Option<&Commands>) -> Result<()> {
    if let Some(Commands::Node {
        command: NodeCommands::Up { offline, .. },
    }) = command
        && !(*offline || prism_runtime::offline::enabled())
    {
        // `node up` needs Python later, but missing auth must be reported
        // before venv provisioning can block or touch the network.
        let _ = resolve_agent_auth()?;
    }
    Ok(())
}

fn command_needs_python(command: Option<&Commands>) -> bool {
    match command {
        // Rust-side state only: the config TOML, the provider registry, the
        // local provenance ledger, paths and endpoints.
        Some(
            Commands::Status
            | Commands::Use { .. }
            | Commands::Agent
            | Commands::Provenance { .. }
            | Commands::Federation { .. }
            | Commands::Publish { .. },
        ) => false,
        // Reports on the venv — including its absence — rather than using it.
        Some(Commands::Doctor { .. }) => false,
        // Pure platform HTTP: these talk to the API over reqwest and print
        // the answer. None of their handlers takes the interpreter path.
        Some(
            Commands::Billing { .. }
            | Commands::Marketplace { .. }
            | Commands::Mesh { .. }
            | Commands::Workflow { .. }
            | Commands::Models { .. }
            | Commands::Gpus,
        ) => false,
        // `prism login` is the FIRST command a new user runs, and it must
        // work on a machine with no Python at all: the device flow is pure
        // HTTP, and `perform_full_login` only records the interpreter path
        // as a string in `preferred_python` — it never executes it. Before
        // this, a fresh install on a box without Python 3.11+ died on "No
        // Python 3.11+ found" before the browser ever opened.
        Some(Commands::Login { .. }) => false,
        // An unrecognised word — `prism verison` — reaches clap's external
        // subcommand catch-all and is about to be told it is unknown.
        // Building a venv to print a typo message is the most obviously
        // wasted ~30 s in the product.
        Some(Commands::External(_)) => false,
        // NOT listed, deliberately: `Commands::Node`. `node up` spawns the
        // Python tool server for the node's /api/chat service, and this
        // match cannot see which subcommand was given.
        //
        // Bare `prism` is the TUI, which spawns the tool server.
        _ => true,
    }
}

/// Whether this invocation should kick off the background marketplace
/// tool-sync.
///
/// Two gates, and the second one used to be missing. The command has to be
/// one of the long-running ones — that part was always here. But the sync
/// also fired with no credential of any kind, so `prism` or `prism resume`
/// on a fresh install with no account issued an unauthenticated
/// `GET /api/v1/marketplace/resources` to the hosted platform. For a tool
/// that advertises working fully locally, that is a phone-home on first run.
///
/// [`boot_checks::platform_configured`] is the same test the boot screen
/// uses to decide whether the platform exists for this user at all, so the
/// two surfaces cannot drift apart. `--offline` is already handled further
/// up: it sets `PRISM_OFFLINE` before any task is spawned, and
/// `PlatformClient` checks it before every request.
fn should_sync_tools(
    command: Option<&Commands>,
    credentials: Option<&prism_runtime::StoredCredentials>,
) -> bool {
    if prism_runtime::offline::enabled() {
        return false;
    }
    matches!(
        command,
        Some(
            Commands::Tui { .. }
                | Commands::Backend { .. }
                | Commands::Resume { .. }
                | Commands::Campaign { .. }
        ) | None
    ) && boot_checks::platform_configured(credentials)
}

#[tokio::main]
async fn main() -> Result<()> {
    // Install the process-wide rustls CryptoProvider before ANY TLS can happen.
    // `prism node up` makes its first HTTPS call in register_node (well before
    // the node daemon, which was the only place this was installed), and rustls
    // panics on the first handshake if no process default is set (#131). ok():
    // a second install elsewhere returns Err harmlessly.
    let _ = rustls::crypto::ring::default_provider().install_default();

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
    // Apply the environment policy before resolving Python or constructing
    // any detached task. The environment variable is the hard-offline
    // control plane; the flag is only a convenient way to set it.
    if cli.offline || prism_runtime::offline::enabled() {
        cli.offline = true;
        unsafe {
            std::env::set_var(prism_runtime::offline::ENV, "1");
        }
    }
    preflight_command_auth(cli.command.as_ref())?;
    let project_root = cli.project_root.clone();
    let endpoints = PlatformEndpoints::from_env();
    let paths = PrismPaths::discover()?;

    // Resolve Python: explicit --python wins; then the PRISM_PYTHON env
    // override (used by CI/the smoke harness to point at a pre-seeded
    // interpreter so an isolated $HOME never triggers venv provisioning,
    // which needs the network); otherwise manage ~/.prism/venv/.
    //
    // Provisioning is LAZY. This used to run for every invocation, before
    // command dispatch — so `prism use list`, which only reads a TOML file,
    // spent ~30 s building a venv on a fresh machine, and on a box without
    // Python 3.11+ it failed outright with nothing printed. Commands that
    // touch no Python now get the path the venv *would* live at, without
    // creating it; `doctor` reports that absence as a check result instead
    // of dying on it. `command_needs_python` defaults to TRUE, so a command
    // added later keeps today's eager behaviour rather than silently
    // receiving a path to an interpreter that was never built.
    let python = if cli.python.as_os_str() != "python3" {
        cli.python.clone()
    } else if let Some(p) = std::env::var_os("PRISM_PYTHON").filter(|p| !p.is_empty()) {
        PathBuf::from(p)
    } else {
        // HOME is not set on stock Windows, where the equivalent is
        // USERPROFILE. Falling through to "." would silently put the venv in
        // whatever directory the user happened to be in — and this runs
        // before EVERY subcommand, so it is the one home-dir lookup that
        // cannot be allowed to guess wrong.
        // NOTE: the other `env::var("HOME")` sites in this file are still
        // Unix-only; Windows support is not complete until they are too.
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| ".".to_string());
        let prism_dir = PathBuf::from(&home).join(".prism");
        if command_needs_python(cli.command.as_ref()) {
            ensure_venv(&prism_dir, &project_root).await?
        } else {
            // Pure-Rust commands, including `doctor`, must start even when the
            // managed venv is absent or broken. Use the platform-specific path
            // doctor should inspect without attempting to provision it.
            prism_python_bridge::venv::venv_layout(&prism_dir.join("venv")).0
        }
    };

    // The offline policy was applied immediately after argument parsing,
    // before venv resolution and before this startup sync decision.

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
    if let Ok(state) = paths.load_cli_state()
        && should_sync_tools(cli.command.as_ref(), state.credentials.as_ref())
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
        Commands::Setup { interactive_auth } => {
            let mut state = paths.load_cli_state()?;
            state.preferred_python = Some(python.display().to_string());
            if state.credentials.is_none() {
                let credentials = run_device_login(&endpoints, interactive_auth).await?;
                let platform =
                    PlatformClient::new(&endpoints.api_base).with_token(&credentials.access_token);
                let profile = platform.fetch_current_user().await.ok();
                let selected = select_project(
                    &platform,
                    profile
                        .as_ref()
                        .and_then(|user| user.display_name.as_deref()),
                    interactive_auth,
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
                    let selected =
                        select_project(&platform, creds.display_name.as_deref(), interactive_auth)
                            .await?;
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
                match refresh_access_token(&paths, &endpoints, creds).await {
                    Ok(new_creds) => {
                        state.credentials = Some(new_creds);
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
                match refresh_access_token(&paths, &endpoints, creds).await {
                    Ok(new_creds) => {
                        tracing::info!("access token refreshed after server-side rejection");
                        state.credentials = Some(new_creds);
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
        Commands::Login {
            token,
            no_browser,
            interactive_auth,
        } => {
            let mode = match token {
                Some(pat) => LoginMode::Token(pat),
                None => LoginMode::Device {
                    interactive_auth,
                    no_browser,
                },
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
                        // The module `ToolServer` actually runs. This said
                        // "app.backend" — a module that does not exist in
                        // the tree, so anyone trusting `prism status` to
                        // tell them what to look at was sent nowhere.
                        "python_worker": prism_python_bridge::TOOL_SERVER_MODULE,
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
        Commands::Provenance { command } => {
            handle_provenance_command(command).await?;
        }
        Commands::Workflow { command } => {
            handle_workflow_command(command, &project_root, &paths).await?;
        }
        Commands::Campaign { command } => {
            use prism_campaign::{Campaign, CampaignConfig, CampaignGoal, GoalStatus};

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
                        project_root: Some(project_root.clone()),
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
                        // The worker (`campaign continue`) attaches the
                        // provenance store and records the full trail.
                        campaign.checkpoint()?;
                        spawn_campaign_worker(&campaign_id)?;
                        println!("Detached: {campaign_id}");
                        println!("Checkpoint: ~/.prism/campaigns/{campaign_id}.json");
                        println!("Poll: prism campaign status {campaign_id}");
                    } else {
                        if let Some(store) = open_campaign_provenance().await {
                            campaign = campaign.with_provenance(store);
                        }
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
                        if campaign.state().status == GoalStatus::Completed {
                            anyhow::bail!(
                                "campaign '{id}' is completed ({}) — nothing to resume",
                                campaign.state().completion_reason
                            );
                        }
                        spawn_campaign_worker(&id)?;
                        println!("Detached: {id}");
                        println!("Poll: prism campaign status {id}");
                    } else {
                        if let Some(store) = open_campaign_provenance().await {
                            campaign = campaign.with_provenance(store);
                        }
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
                    if campaign.state().status == GoalStatus::Completed {
                        println!(
                            "Campaign '{id}' already completed: {}",
                            campaign.state().completion_reason
                        );
                    } else {
                        // Whoever actually runs the loop holds the goal's
                        // worker lock, so the scheduler can tell "still
                        // working" from "its process died", and a second
                        // worker over a healthy one is refused rather than
                        // doubling the goal's spend.
                        let _worker_lock = acquire_worker_lock(&id)?;
                        if let Some(store) = open_campaign_provenance().await {
                            campaign = campaign.with_provenance(store);
                        }
                        let result = if campaign.state().status == GoalStatus::Paused {
                            campaign.resume().await?
                        } else {
                            // Submitted, Running (stale after a crash), or
                            // Failed (retry) — all re-enter the loop.
                            campaign.run().await?
                        };
                        println!("\n{}", result.summary);
                    }
                }
                CampaignCommands::BatchEntrypoint => {
                    if run_batch_campaign_entrypoint(&project_root).await? {
                        // The BYOC sbatch wrapper interprets 140 as "the
                        // checkpoint is durable; requeue this allocation".
                        std::process::exit(140);
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
                    println!("Status: {}", state.status.as_str());
                    if !state.completion_reason.is_empty() {
                        println!("Reason: {}", state.completion_reason);
                    }
                    println!(
                        "Iterations: {} / {}",
                        state.current_iteration, state.config.max_iterations
                    );
                    println!("Candidates evaluated: {}", state.total_evaluated());
                    // Say what the USD ceiling can and cannot actually do for
                    // this goal. A ceiling nothing is billing against reads
                    // as "$0.00 of $25.00" otherwise, which is a green light
                    // for a limit that cannot fire.
                    println!("Budget: {}", state.budget_status());
                    println!(
                        "Avg reward: {}",
                        format_classified_reward(state.avg_reward(), state.evidence_class)
                    );
                    if let Some(best) = state.best() {
                        println!(
                            "Best: {} (reward={})",
                            best.composition,
                            format_classified_reward(best.reward, best.evidence_class)
                        );
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
                                let status = if s.completion_reason.is_empty() {
                                    s.status.as_str().to_string()
                                } else {
                                    format!("{} ({})", s.status.as_str(), s.completion_reason)
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
        Commands::Schedule { command } => {
            handle_schedule_command(command).await?;
        }
        Commands::Notebook { command } => match command {
            NotebookCommands::Start { port } => {
                let session = notebook::start(port, None)?;
                println!("Notebook started:");
                println!("  URL:   {}", session.url);
                println!("  PID:   {}", session.pid);
                println!("  Port:  {}", session.port);
                println!("  Token: {}", session.token);
                println!("\nOpen the URL manually in a browser or IDE.");
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
                    "PyIron is not installed; the pyiron extra provides it — \
                     simulation tools will also auto-install it on first use."
                ),
            },
            PyironCommands::Install => println!("{}", pyiron_cmd::install()?),
            PyironCommands::Update => println!("{}", pyiron_cmd::update()?),
        },
        Commands::Provision { command } => match command {
            ProvisionCommands::Extra { name, wheelhouse } => {
                prism_python_bridge::venv::install_extra(
                    &python,
                    &project_root,
                    &name,
                    wheelhouse.as_deref(),
                )
                .await?;
                println!("Provisioned PRISM extra [{name}].");
            }
            ProvisionCommands::Wheels { output, extras } => {
                prism_python_bridge::venv::pre_stage_wheels(
                    &python,
                    &project_root,
                    &output,
                    &extras,
                )
                .await?;
                println!(
                    "Pre-staged PRISM wheels for [{}] in {}.",
                    extras.join(", "),
                    output.display()
                );
            }
        },
        Commands::Backend {
            project_root: backend_pr,
        } => {
            let backend_py = python.clone();
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
                .ok()
                .or_else(|| PlatformVar::TOKEN.get())
                .ok_or(std::env::VarError::NotPresent)
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
                    let registry = crate::providers::Registry::load();
                    let env_name = api_key_env.clone().unwrap_or_else(|| {
                        crate::providers::default_api_key_env(&registry, provider)
                    });
                    let provider_key = std::env::var(&env_name).ok();
                    (
                        provider_endpoint(&registry, provider),
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
                    // Credential for the platform LLM proxy, in precedence
                    // order: explicit LLM_API_KEY override → the stable
                    // `m27_*` API key (MARC27_API_KEY — no login, no expiry,
                    // the headless-server/agent path) → MARC27_TOKEN → the
                    // logged-in session JWT. The LLM client routes an `m27_*`
                    // value onto X-API-Key and a JWT onto Bearer automatically.
                    // Provider keys are NOT platform credentials.
                    let marc27_key = std::env::var("LLM_API_KEY")
                        .ok()
                        .or_else(|| PlatformVar::API_KEY.get())
                        .or_else(|| PlatformVar::TOKEN.get())
                        .or_else(|| platform_token.clone());
                    (
                        marc27_llm_base_url(&paths, &endpoints.api_base, &cfg_llm.url)?,
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
            if let Ok(binary) = std::env::current_exe() {
                tool_server_env.insert(
                    "PRISM_BINARY".to_string(),
                    binary.to_string_lossy().into_owned(),
                );
            }
            tool_server_env.insert(
                "PRISM_PROJECT_ROOT".to_string(),
                backend_pr.to_string_lossy().into_owned(),
            );

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
        } => {
            let ipc_py = python.clone();
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
        Commands::Doctor { fix } => {
            doctor::run(&project_root, &python, fix).await?;
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
                            // Only the PULL is refused. `check_ollama_model`
                            // above is a loopback call and an already-cached
                            // model still serves fine offline — so this guard
                            // sits here, on the miss, not on the whole branch.
                            //
                            // It also has to sit BEFORE the spawn rather than
                            // relying on the `offline` flag computed later in
                            // this same function (first read ~40 lines down,
                            // for a print): by then the pull has happened.
                            if prism_runtime::offline::enabled() {
                                bail!(
                                    "offline mode: model '{model}' is not in the local \
                                     Ollama cache and `ollama pull` would fetch it from \
                                     the registry. Pull it before going offline, or pick \
                                     a model already cached."
                                );
                            }
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
                    if with_kafka {
                        svc_config.kafka = Some(prism_orch::services::KafkaConfig::default());
                    }
                    if with_spark {
                        svc_config.spark = Some(prism_orch::services::SparkConfig::default());
                    }

                    let wants_managed_services = svc_config.kafka.is_some()
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
                            let registry = crate::providers::Registry::load();
                            let env_name = api_key_env.clone().unwrap_or_else(|| {
                                crate::providers::default_api_key_env(&registry, provider)
                            });
                            (
                                provider_endpoint(&registry, provider),
                                model.clone(),
                                std::env::var(&env_name).ok().or_else(|| {
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
                // Client for platform-mediated peer discovery (kept out of
                // DaemonOptions, which consumes daemon_platform_client).
                let mut platform_discovery_client: Option<PlatformClient> = None;

                // Durable mesh identity, persisted beside the node keys.
                // Subscriptions are keyed on `publisher_node` and the Kafka
                // consumer group derives from this id — a fresh UUID per
                // boot dangled every subscription and made each restart a
                // new consumer group starting at `latest`, losing every
                // publish that happened during downtime.
                let mesh_node_id_persisted = prism_mesh::load_or_create_node_id(&paths.state_dir)?;

                // Resolve auth before registration. API-key-only users do
                // not have cli-state metadata and must still be accepted.
                let cli_state = paths.load_cli_state().ok().unwrap_or_default();
                let resolved_platform_auth = if !offline {
                    Some(resolve_agent_auth()?)
                } else {
                    None
                };
                let mesh_auth_token = resolved_platform_auth
                    .as_ref()
                    .map(|(_, auth)| auth.secret().to_string());
                let mesh_has_auth = mesh_auth_token.is_some();

                if !offline {
                    let (resolved_api_base, resolved_auth) = resolved_platform_auth
                        .as_ref()
                        .expect("non-offline node auth was preflighted");
                    let creds = cli_state.credentials.as_ref();
                    daemon_org_id = creds.and_then(|value| value.org_id.clone());
                    let (token, maybe_refreshed) = if resolved_auth.is_api_key() {
                        (resolved_auth.secret().to_string(), None)
                    } else if let Some(creds) = creds {
                        resolve_node_token(&paths, &endpoints, creds).await?
                    } else {
                        (resolved_auth.secret().to_string(), None)
                    };
                    let effective_creds = maybe_refreshed.or_else(|| creds.cloned());

                    // Resolve the platform token through the SAME priority
                    // + refresh path the WS daemon uses
                    // (daemon::load_access_token): durable node token →
                    // MARC27_API_KEY → cli-state creds, refreshed when
                    // expired. Previously this built the client straight
                    // from `creds.access_token` with NO refresh, so once the
                    // session token aged past 24h (and the SDK mirror held a
                    // stale copy) `POST /nodes/register` 401'd and the node
                    // silently fell back to offline mode while the platform
                    // still listed it online.
                    //
                    // `resolve_node_token` returns the rotated creds when it
                    // refreshed (it consumes the single-use refresh token);
                    // we MUST keep those as the effective creds for any later
                    // refresh, never the stale startup `creds` binding, or we
                    // replay a now-REVOKED token and trip token-family
                    // invalidation.
                    // The EFFECTIVE creds — `resolve_node_token`'s rotation
                    // if it refreshed, else the startup creds. The 401-retry
                    // arm refreshes from THIS (never the stale startup
                    // binding), so a single-use refresh token already
                    // consumed by resolve_node_token is never replayed.
                    // `mut`: the 401-retry arm reassigns this to a client
                    // built from the refreshed token, so the value stored
                    // into the daemon state below carries the LIVE token
                    // (not the one that just 401'd — a prior bug stored the
                    // stale client and the daemon then heartbeat/role-sync/
                    // deregister'd on the dead token → all 401 → stale
                    // "online" record).
                    let mut platform = PlatformClient::new(resolved_api_base).with_token(&token);
                    let mut caps = serde_json::json!({
                        "compute": !no_compute,
                        "storage": !no_storage,
                        "dashboard_port": dashboard_port,
                        // Mesh reachability for platform-mediated discovery
                        // (prism_mesh::platform_discovery reads these back
                        // out of the registry's node profiles).
                        "mesh_node_id": mesh_node_id_persisted.to_string(),
                    });
                    // Advertised only when a LAN address is actually
                    // knowable — an address is never invented.
                    if let Some(url) = prism_mesh::platform_discovery::advertise_url(dashboard_port)
                    {
                        caps["mesh_advertise_url"] = serde_json::Value::String(url);
                    }

                    // register_node_inspect returns a typed ApiError
                    // carrying the HTTP status + parsed `code`, so a stale
                    // token (401 / token_expired) can be recovered with a
                    // refresh + single retry instead of the opaque
                    // "returned error status" that hid the cause.
                    let reg = {
                        let registry =
                            prism_client::node_registry::NodeRegistryClient::new(&platform);
                        match registry.register_node_inspect(&node_name, &caps).await {
                            Ok(reg) => reg,
                            Err(api_err)
                                if api_err.is_token_expired() && !resolved_auth.is_api_key() =>
                            {
                                tracing::info!(
                                    "node register rejected with token_expired — refreshing and retrying once"
                                );
                                // Refresh from the EFFECTIVE creds (rotated
                                // by resolve_node_token if it already
                                // refreshed), never the stale startup
                                // binding.
                                let effective_creds = effective_creds.as_ref().ok_or_else(|| {
                                        anyhow!("token expired without a stored session; re-authentication or MARC27_API_KEY is required")
                                    })?;
                                let refreshed = refresh_access_token(
                                    &paths,
                                    &endpoints,
                                    effective_creds,
                                )
                                .await
                                .context(
                                    "token expired and refresh failed — re-authentication required",
                                )?;
                                // Reassign the OUTER client so the daemon
                                // state (stored below) carries the live token.
                                platform = PlatformClient::new(resolved_api_base)
                                    .with_token(&refreshed.access_token);
                                let registry =
                                    prism_client::node_registry::NodeRegistryClient::new(&platform);
                                registry
                                    .register_node_inspect(&node_name, &caps)
                                    .await
                                    .map_err(|e| {
                                        anyhow!(
                                            "platform registration failed after token refresh: {e}"
                                        )
                                    })?
                            }
                            Err(e) => {
                                // Fail LOUD: a non-offline `node up` that cannot
                                // register leaves the node in a dangerous
                                // half-state — the dashboard/mesh run, but the
                                // node never receives broker-dispatched jobs, and
                                // the platform may still list a stale record as
                                // online. Fail with a clear message + non-zero
                                // exit instead of silently dropping to "offline
                                // mode". (Pass --offline to run without the
                                // platform.)
                                return Err(anyhow!(
                                    "platform registration failed: {e}\n\
                                         re-authenticate with `prism login`, or pass --offline \
                                         to run without platform dispatch."
                                ));
                            }
                        }
                    };

                    // NOTE: the in-app node supervisor
                    // (prism_agent::node_supervisor) parses this
                    // line out of the daemon log to learn the
                    // platform node id — keep the
                    // "(node_id: …)" shape if rewording.
                    println!(
                        "  \u{2713} Registered with platform (node_id: {})",
                        reg.node_id
                    );
                    daemon_platform_node_id = Some(reg.node_id);
                    // `platform` is the LIVE client: either the original
                    // (register succeeded first try) or the reassigned
                    // refreshed one (after a 401-retry). Storing the stale
                    // client here was the bug that made the daemon's REST
                    // calls all 401 silently.
                    server_node_state.platform_client = Some(platform.clone());
                    platform_discovery_client = Some(platform.clone());
                    daemon_platform_client = Some(platform);
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
                        let mut tool_server_env =
                            prism_agent::service::default_tool_server_env(&chat_api_base);
                        if let Ok(binary) = std::env::current_exe() {
                            tool_server_env.insert(
                                "PRISM_BINARY".to_string(),
                                binary.to_string_lossy().into_owned(),
                            );
                        }
                        tool_server_env.insert(
                            "PRISM_PROJECT_ROOT".to_string(),
                            chat_project_root.to_string_lossy().into_owned(),
                        );
                        let tool_server = prism_python_bridge::ToolServer {
                            python_bin: chat_python,
                            project_root: chat_project_root,
                            env: tool_server_env,
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
                    org_id: daemon_org_id.clone(),
                    // Merge the subcommand flag with the process-wide policy.
                    // `--offline` on `node up` is its own arg (see NodeCommands::Up)
                    // and was passed through raw, so `PRISM_OFFLINE=1 prism node up`
                    // left this false: the daemon resolved a real credential and
                    // opened `wss://…?token=<token>` (node/daemon.rs:525). Same
                    // shape as main.rs:1640's `cli.offline || offline::enabled()`.
                    offline: offline || prism_runtime::offline::enabled(),
                    tool_invoker: Some(tool_invoke_tx),
                    audit_emitter,
                };

                // ── Start mesh networking (mDNS discovery + optional broadcast) ──
                // One resolution of "did the operator ask for offline?" — the
                // `--offline` flag or `PRISM_OFFLINE=1` — shared by the mesh
                // refusal and the boot line (same merge as daemon_options.offline).
                let mesh_offline = offline || prism_runtime::offline::enabled();
                let mesh_cancel = tokio_util::sync::CancellationToken::new();
                // Resolve Kafka brokers: explicit flag > implicit from --with-kafka
                let kafka_requested = kafka_brokers.is_some() || with_kafka;
                let resolved_kafka_brokers =
                    resolve_kafka_brokers(kafka_brokers.as_deref(), with_kafka);
                if kafka_requested && resolved_kafka_brokers.is_none() {
                    eprintln!("  ⚠ Kafka mesh transport disabled: offline mode.");
                }

                let mesh_config = prism_mesh::MeshConfig {
                    node_name: daemon_options.name.clone(),
                    publish_port: dashboard_port,
                    discovery: vec![prism_mesh::DiscoveryMethod::Mdns],
                    kafka_brokers: resolved_kafka_brokers.clone(),
                };
                let mesh_start_options = prism_mesh::MeshStartOptions {
                    node_name: daemon_options.name.clone(),
                    publish_port: dashboard_port,
                    broadcast,
                    capabilities: Vec::new(),
                    discovery_interval_secs: 30,
                    event_tx: Some(server_state.ws_broadcast.clone()),
                    auth_token: mesh_auth_token.clone(),
                    offline: mesh_offline,
                };
                // The handle the REST API reports is minted from the SAME
                // decision `start_mesh` acts on. An `Online` handle was
                // previously written unconditionally, so `/api/mesh/nodes`
                // answered `"online": true` (and `mesh peers` printed
                // "Mesh: online") while the boot line in the same process
                // said "Mesh disabled" — and `mesh sync` against such a node
                // "succeeded" with 0 entities instead of failing at its
                // publisher-lookup guard.
                let mesh_handle = match prism_mesh::mesh_start_refusal(&mesh_start_options) {
                    None => prism_mesh::init_mesh_with_id(mesh_config, mesh_node_id_persisted)?,
                    Some(_) => prism_mesh::MeshHandle::Offline,
                };
                let mesh_peers_shared = mesh_handle.peers_shared();
                *server_state.mesh.write().unwrap_or_else(|e| e.into_inner()) = mesh_handle.clone();
                let mesh_task =
                    prism_mesh::start_mesh(mesh_handle, mesh_start_options, mesh_cancel.clone());
                // Initialize federated query client for cross-mesh
                // searches. It carries the owner's platform token so each
                // peer can VERIFY who is querying and mint a session —
                // `/api/query` sits behind the peers' auth stacks, so the
                // tokenless default could only ever collect 401s.
                let _ = server_state.federation.set(
                    prism_mesh::federated_query::FederatedQuery::with_platform_token(
                        std::time::Duration::from_secs(10),
                        mesh_auth_token.clone(),
                    ),
                );

                // ── Platform-mediated peer discovery ──
                // The org's node registry is an AUTHENTICATED peer
                // directory (both machines register there at `node up`),
                // unlike mDNS, whose "authenticated" flag keys on the mere
                // presence of a non-cryptographic TXT hash. Registered
                // peers that advertised a mesh identity + URL are merged
                // into the peer list; mDNS stays the zero-config LAN path.
                if let (Some(discovery_client), Some(peers_shared)) =
                    (platform_discovery_client, mesh_peers_shared.clone())
                {
                    let org = daemon_org_id.clone();
                    let cancel = mesh_cancel.clone();
                    tokio::spawn(async move {
                        let mut interval =
                            tokio::time::interval(std::time::Duration::from_secs(300));
                        loop {
                            tokio::select! {
                                _ = cancel.cancelled() => break,
                                _ = interval.tick() => {}
                            }
                            match prism_mesh::platform_discovery::discover_platform_peers(
                                &discovery_client,
                                org.as_deref(),
                                mesh_node_id_persisted,
                            )
                            .await
                            {
                                Ok(discovered) => {
                                    let mut list =
                                        peers_shared.write().unwrap_or_else(|e| e.into_inner());
                                    for peer in discovered {
                                        if !list.iter().any(|p| p.node_id == peer.node_id) {
                                            tracing::info!(
                                                peer = %peer.name,
                                                id = %peer.node_id,
                                                "peer discovered via platform registry"
                                            );
                                            list.push(peer);
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::debug!(error = %e, "platform peer discovery failed");
                                }
                            }
                        }
                    });
                }

                if mesh_offline {
                    // `resolved_platform_auth` is forced to None under
                    // offline, so keying this line on auth alone blamed
                    // "not authenticated" for the operator's own --offline.
                    println!("  \u{26A0} Mesh: disabled (offline mode)");
                } else if !mesh_has_auth {
                    println!("  \u{26A0} Mesh: disabled (not authenticated)");
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
                        // The persisted identity, not the handle's: when the
                        // mesh was refused the handle is Offline (node_id
                        // None), but the Kafka transport is started
                        // separately and must keep its durable consumer
                        // group either way.
                        group_id: format!("prism-{mesh_node_id_persisted}"),
                    };

                    match prism_mesh::kafka::MeshKafkaConsumer::new(&kafka_cfg) {
                        Ok(consumer) => {
                            let (tx, rx) = tokio::sync::mpsc::channel(256);

                            // Use shared state extracted from mesh handle before it was moved
                            let peers_arc = mesh_peers_shared.clone().unwrap_or_else(|| {
                                std::sync::Arc::new(std::sync::RwLock::new(Vec::new()))
                            });
                            // Persisted identity, not the handle's: `nil`
                            // here would break the sync handler's self-skip.
                            let our_node_id = mesh_node_id_persisted;
                            let subscriptions = server_state.subscriptions.clone();

                            // Peer-synced facts land in the bundled Turso
                            // store under the publisher's own tenant
                            // ("mesh:{node id}") — always available, no
                            // external graph service required.
                            let sync_home =
                                std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
                            let sync_config = Some(prism_mesh::sync::SyncConfig {
                                provenance_db: std::path::PathBuf::from(sync_home)
                                    .join(".prism/provenance.db"),
                            });

                            // Spawn consumer loop
                            tokio::spawn(async move {
                                if let Err(e) = consumer.run(tx).await {
                                    tracing::error!(error = %e, "Kafka consumer loop exited with error");
                                }
                            });

                            // Spawn sync handler. It authenticates every
                            // peer pull by minting a session with the
                            // owner's platform token — without one the
                            // peer's auth stack answers 401 and nothing
                            // ever arrives.
                            let sync_sessions =
                                std::sync::Arc::new(prism_mesh::peer_session::PeerSessions::new(
                                    mesh_auth_token.clone(),
                                ));
                            tokio::spawn(async move {
                                prism_mesh::sync::run_sync_handler(
                                    rx,
                                    peers_arc,
                                    subscriptions,
                                    our_node_id,
                                    sync_config,
                                    sync_sessions,
                                )
                                .await;
                            });

                            println!("  \u{2713} Kafka: pub/sub active ({brokers})");
                        }
                        Err(e) => {
                            // Name what actually stops working:
                            // `run_sync_handler` is spawned only in the
                            // branch above and drains a channel only the
                            // Kafka consumer feeds, so without Kafka
                            // nothing syncs AUTOMATICALLY on publish.
                            // `prism mesh sync <dataset> --peer <url>` is
                            // the Kafka-free manual pull.
                            eprintln!("  Warning: Kafka consumer failed to start: {e}");
                            eprintln!(
                                "  Peer discovery (mDNS) still works, but publishes will NOT \
                                 sync automatically. Pull on demand with \
                                 `prism mesh sync <dataset> --peer <url>`."
                            );
                        }
                    }

                    match prism_mesh::kafka::MeshKafkaProducer::new(&kafka_cfg) {
                        Ok(producer) => {
                            let producer = std::sync::Arc::new(producer);
                            // Store producer in server state so mesh handlers can publish
                            let _ = server_state.kafka_producer.set(producer.clone());
                            let _ = server_state.node_id.set(mesh_node_id_persisted);
                            tracing::info!("Kafka producer ready and wired to mesh handlers");

                            // Announce this node on the mesh via Kafka
                            {
                                let announce_producer = producer.clone();
                                let node_name = daemon_options.name.clone();
                                tokio::spawn(async move {
                                    let msg = prism_mesh::protocol::MeshMessage::Announce {
                                        node_id: mesh_node_id_persisted,
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
                let outcome = prism_node::daemon::stop_daemon(&paths)?;
                println!("{outcome}");
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
                        .platform_error_for_status()
                        .await?
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
                        .platform_error_for_status()
                        .await?
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
            NodeCommands::Token { command } => match command {
                TokenCommands::Mint { project } => {
                    handle_node_token_mint(&paths, project.as_deref()).await?;
                }
                TokenCommands::Revoke => {
                    handle_node_token_revoke(&paths).await?;
                }
            },
        },
        Commands::Papers { command } => {
            crate::papers::handle(command, &cli.project_root).await?;
        }
        Commands::Ingest {
            path,
            corpus,
            model,
            llm_url,
            api_key,
            schema_only,
            status,
            platform,
            watch,
            runtime_url,
            json,
            mapping,
        } => {
            if status {
                handle_ingest_status(corpus.as_deref(), json).await?;
            } else if platform {
                let path = path.as_deref().ok_or_else(|| {
                    anyhow!("`prism ingest --platform` requires a file to upload.")
                })?;
                handle_ingest_platform(path, json).await?;
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
                    schema_only,
                    &runtime_url,
                    corpus.as_deref(),
                    json,
                    mapping.as_deref(),
                )
                .await?;
            }
        }
        Commands::Matkg { command } => match command {
            MatkgCommands::Load {
                path,
                min_count,
                limit,
                all,
                json,
            } => {
                handle_matkg_load(&path, min_count, if all { None } else { Some(limit) }, json)
                    .await?;
            }
        },
        Commands::Query {
            text,
            semantic,
            platform,
            json: json_output,
            federated,
            llm_url: _,
            model: _,
            api_key: _,
            limit,
            dashboard_url,
        } => {
            if platform {
                // Route through MARC27 platform API
                handle_platform_query(&text, semantic, json_output, limit).await?;
            } else if federated {
                handle_federated_query(&text, &dashboard_url, &paths).await?;
            } else {
                handle_query(&text, semantic, limit).await?;
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
            slurm_account,
            slurm_time,
            slurm_gres,
            slurm_mem,
            slurm_mem_per_cpu,
            slurm_cpus_per_task,
            slurm_nodes,
            slurm_ntasks,
            slurm_array,
            slurm_dependency_afterok,
            json,
        } => {
            handle_run(
                &paths.data_dir,
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
                slurm_account.as_deref(),
                slurm_time.as_deref(),
                slurm_gres.as_deref(),
                slurm_mem.as_deref(),
                slurm_mem_per_cpu.as_deref(),
                slurm_cpus_per_task,
                slurm_nodes,
                slurm_ntasks,
                slurm_array.as_deref(),
                slurm_dependency_afterok,
                json,
            )
            .await?;
        }
        Commands::JobStatus { job_id } => {
            handle_job_status(&paths, &job_id).await?;
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
                            let author = t
                                .author
                                .as_deref()
                                .unwrap_or(&crate::brand::brand().display_name);
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
                            "{} resources found. Install any by its [slug] — `marketplace install <slug>`, or ask the agent.",
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

                    // Resources whose capability ships inside PRISM (the
                    // materials tools) hold no artifact — `/install` 422s for
                    // them. Say what to `pip install` instead of failing with
                    // the platform's 422; an entry that only 422'd would be
                    // worse than no entry at all.
                    if let Ok(resource) = marketplace.get_tool(&name).await
                        && let Some((command, note)) = resource.install_instructions()
                    {
                        println!("'{name}' ships inside PRISM — there is no artifact to download.");
                        println!("\n    {command}\n");
                        if let Some(note) = note {
                            println!("{note}");
                        }
                        return Ok(());
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
                            "Try a different phrasing, or `marketplace search <query>` for \
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
                        tool.author
                            .as_deref()
                            .unwrap_or(&crate::brand::brand().display_name)
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
                MarketplaceCommands::Publish { dry_run, slug } => {
                    let catalog = prism_client::marketplace::builtin_catalog()?;
                    let selected: Vec<_> = catalog
                        .entries
                        .iter()
                        .filter(|e| slug.as_ref().is_none_or(|s| *s == e.slug))
                        .collect();
                    if selected.is_empty() {
                        anyhow::bail!(
                            "no catalog entry matches '{}'. Known slugs: {}",
                            slug.unwrap_or_default(),
                            catalog
                                .entries
                                .iter()
                                .map(|e| e.slug.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        );
                    }
                    for entry in &selected {
                        let extras = if entry.requires_extras.is_empty() {
                            "no extra required".to_string()
                        } else {
                            format!("needs [{}]", entry.requires_extras.join(", "))
                        };
                        println!(
                            "  {} [{}]  {} — {}",
                            entry.name, entry.slug, entry.license, extras
                        );
                    }
                    if dry_run {
                        println!(
                            "\n{} entr(ies) would be published. \
                             {} tool(s) stay bundled on purpose (see the catalog).",
                            selected.len(),
                            catalog.bundled.len()
                        );
                    } else {
                        if token.is_none() {
                            anyhow::bail!("publishing requires authentication");
                        }
                        for entry in &selected {
                            marketplace.publish_entry(entry).await?;
                            println!("published {} (draft → pending_review)", entry.slug);
                        }
                        println!(
                            "\n{} entr(ies) submitted. They stay invisible to the public \
                             listing until a platform reviewer approves them.",
                            selected.len()
                        );
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
                .platform_error_for_status()
                .await?
                .json()
                .await?;
            let run_id = created
                .get("id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .ok_or_else(|| anyhow::anyhow!("platform did not return a run id: {created}"))?;
            eprintln!("research run {run_id} started; waiting for completion…");

            // Poll until terminal ("completed" | "failed" | "canceled").
            // Dots to stderr so stdout stays a single clean JSON/answer document.
            //
            // The ceiling is deliberately well past the server's run budget
            // (RESEARCH_MAX_WALL_SECS, 600s by default but raised in
            // deployments): if the client gives up first it reports a hang on a
            // run that is still working, and the user loses an answer they have
            // already paid for.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2100);

            // A poll failure is NOT a run failure. The run lives server-side;
            // this loop only reads it. Aborting on one bad response threw away
            // ten to twenty minutes of billable work every time the API
            // restarted underneath it — observed three times in one afternoon,
            // as a 502 mid-poll and as a truncated stream. Transient errors are
            // therefore tolerated until they stop looking transient.
            const MAX_CONSECUTIVE_POLL_FAILURES: u32 = 12; // ~1 min at 5s
            let mut consecutive_failures: u32 = 0;

            let resp = loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;

                let polled = auth
                    .apply(client.get(format!("{api_base}/agent-runs/{run_id}")))
                    .send()
                    .await;

                let run: serde_json::Value = match polled {
                    Ok(response) if response.status().is_success() => match response.json().await {
                        Ok(value) => {
                            consecutive_failures = 0;
                            value
                        }
                        Err(error) => {
                            consecutive_failures += 1;
                            if consecutive_failures >= MAX_CONSECUTIVE_POLL_FAILURES {
                                anyhow::bail!(
                                    "lost contact with the platform while polling run {run_id} \
                                     ({consecutive_failures} consecutive failures, last: {error}). \
                                     The run may still be going — check with: prism agent"
                                );
                            }
                            continue;
                        }
                    },
                    Ok(response) => {
                        let transient = response.status().is_server_error();
                        let error = response
                            .platform_error_for_status()
                            .await
                            .expect_err("non-success response must produce a platform error");
                        if !transient {
                            return Err(error);
                        }
                        consecutive_failures += 1;
                        if consecutive_failures >= MAX_CONSECUTIVE_POLL_FAILURES {
                            anyhow::bail!(
                                "lost contact with the platform while polling run {run_id} \
                                 ({consecutive_failures} consecutive failures, last: {error:#}). \
                                 The run may still be going — check with: prism agent"
                            );
                        }
                        continue;
                    }
                    Err(error) => {
                        consecutive_failures += 1;
                        if consecutive_failures >= MAX_CONSECUTIVE_POLL_FAILURES {
                            anyhow::bail!(
                                "lost contact with the platform while polling run {run_id} \
                                 ({consecutive_failures} consecutive failures, last: {error}). \
                                 The run may still be going — check with: prism agent"
                            );
                        }
                        continue;
                    }
                };
                // Read the terminal state from `state` (primary) or `status`
                // (fallback), and accept the full success/failure vocabulary the
                // platform uses. This mirrors the sibling `run_ingest_job` poll
                // loop and the verified Python client (`app/tools/agent_runs.py`,
                // shape verified 2026-07-02): the `/agent-runs` orchestrator may
                // report success as "succeeded"/"done" (not only "completed"),
                // and cancel as "cancelled". Matching only "completed" here would
                // hang the research leg until the 10-min deadline on a run that
                // actually finished.
                let state = run
                    .get("state")
                    .and_then(|s| s.as_str())
                    .or_else(|| run.get("status").and_then(|s| s.as_str()))
                    .unwrap_or("");
                match state {
                    "completed" | "succeeded" | "done" => {
                        break serde_json::json!({
                            "run_id": run_id,
                            "answer": run.get("answer").cloned().unwrap_or(serde_json::Value::Null),
                            "sources": run.get("params").and_then(|p| p.get("sources")).cloned()
                                .unwrap_or_else(|| serde_json::json!([])),
                        });
                    }
                    "failed" | "canceled" | "cancelled" => {
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
        Commands::DeployAndInvoke {
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
            invoke_path,
            input,
            ready_timeout_secs,
            keep,
        } => {
            handle_deploy_and_invoke(
                &name,
                image.as_deref(),
                resource_slug.as_deref(),
                &target,
                gpu.as_deref(),
                budget,
                node_id.as_deref(),
                &env_vars,
                port,
                &health_path,
                &invoke_path,
                &input,
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
        Commands::ComputeRun {
            image,
            inputs,
            gpu,
            budget,
            provider,
            timeout,
            env,
            poll_timeout_secs,
        } => {
            handle_compute_run(
                &image,
                &inputs,
                gpu.as_deref(),
                budget,
                provider.as_deref(),
                timeout,
                &env,
                poll_timeout_secs,
            )
            .await?;
        }
        Commands::Knowledge { command } => {
            handle_knowledge_command(command).await?;
        }
        Commands::IngestAndWait {
            url,
            query,
            mode,
            poll_timeout_secs,
        } => {
            handle_ingest_and_wait(url.as_deref(), query.as_deref(), &mode, poll_timeout_secs)
                .await?;
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
                    // `hf` uploads to huggingface.co using the user's cached HF
                    // token. Egress by subprocess, so no URL-shaped guard here
                    // covered it, and `PRISM_OFFLINE=1 prism publish --to hf`
                    // created a public repo and pushed the artifact anyway.
                    //
                    // Refused, not skipped: publishing IS this command. Silently
                    // doing nothing would report success for work never done.
                    // (Contrast `prism report`, where filing is one optional
                    // step of several and degrades to `--no-github`.)
                    if prism_runtime::offline::enabled() {
                        anyhow::bail!(
                            "offline mode: refusing to publish to huggingface.co \
                             — `hf` would upload {path} and send your Hugging Face \
                             token. Unset PRISM_OFFLINE to publish."
                        );
                    }
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
                    let (api_base, auth) = resolve_agent_auth()?;
                    let client = reqwest::Client::builder()
                        .timeout(Duration::from_secs(30))
                        .build()?;

                    println!(
                        "Publishing to {} marketplace...",
                        crate::brand::brand().display_name
                    );
                    let name = repo.unwrap_or_else(|| {
                        artifact_path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("artifact")
                            .to_string()
                    });
                    let resp: serde_json::Value = auth
                        .apply(client.post(format!("{api_base}/marketplace")))
                        .json(&serde_json::json!({
                            "name": name,
                            "path": path,
                            "private": private,
                        }))
                        .send()
                        .await?
                        .platform_error_for_status()
                        .await?
                        .json()
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
                    // `marc27` here is the frozen CLI value token, not the
                    // brand: it is what shipped scripts pass to `--to`.
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
                match refresh_access_token(&paths, &endpoints, creds).await {
                    Ok(new_creds) => {
                        state.credentials = Some(new_creds);
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
                && let Ok(new_creds) = refresh_access_token(&paths, &endpoints, creds).await
            {
                tracing::info!("access token refreshed after server-side rejection");
                state.credentials = Some(new_creds);
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
                eprintln!("\x1b[33mYour platform session has expired — re-authenticating…\x1b[0m");
                eprintln!();
                if let Err(e) = perform_full_login(
                    &paths,
                    &endpoints,
                    &python,
                    LoginMode::Device {
                        interactive_auth: false,
                        no_browser: true,
                    },
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
                match refresh_access_token(&paths, &endpoints, creds).await {
                    Ok(new_creds) => {
                        state.credentials = Some(new_creds);
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
                && let Ok(new_creds) = refresh_access_token(&paths, &endpoints, creds).await
            {
                tracing::info!("access token refreshed after server-side rejection");
                state.credentials = Some(new_creds);
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
                eprintln!("\x1b[33mYour platform session has expired — re-authenticating…\x1b[0m");
                eprintln!();
                if let Err(e) = perform_full_login(
                    &paths,
                    &endpoints,
                    &python,
                    LoginMode::Device {
                        interactive_auth: false,
                        no_browser: true,
                    },
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
                        raw.platform_error_for_status().await?.json().await?;
                    println!("\nCredits");
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
                        .platform_error_for_status()
                        .await?
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
                        .platform_error_for_status()
                        .await?
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
                        .platform_error_for_status()
                        .await?
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
                        .platform_error_for_status()
                        .await?
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
                        .platform_error_for_status()
                        .await?
                        .json()
                        .await?;

                    if let Some(url) = resp["checkout_url"].as_str() {
                        println!("Checkout URL (open manually): {url}\n");
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
            let caller_supplied_llm_base_url = values.contains_key("llm_base_url");
            // Only execute-mode runs actually call tools (dry runs plan only).
            // The launcher-issued token replaces any caller-supplied reserved
            // value; the workflow engine separately binds it to port 7327.
            let node_token = if execute {
                mint_workflow_node_token(paths).await
            } else {
                None
            };
            if let Some(token) = &node_token {
                values.insert("_node_token".to_string(), token.clone());
            }
            // Point `llm_*` steps at the resolved chat endpoint.
            inject_workflow_llm_endpoint(&mut values, project_root, paths);
            let options = resolve_workflow_llm_options(
                project_root,
                paths,
                caller_supplied_llm_base_url,
                node_token,
            );
            let result = execute_workflow_with_policy_and_options(
                spec, &values, execute, None, None, None, &options,
            )
            .await?;
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
    let caller_supplied_llm_base_url = values.contains_key("llm_base_url");
    // Only execute-mode runs actually call tools (dry runs plan only).
    let node_token = if request.execute {
        mint_workflow_node_token(paths).await
    } else {
        None
    };
    if let Some(token) = &node_token {
        values.insert("_node_token".to_string(), token.clone());
    }
    // Point `llm_*` steps at the resolved chat endpoint.
    inject_workflow_llm_endpoint(&mut values, project_root, paths);
    let options = resolve_workflow_llm_options(
        project_root,
        paths,
        caller_supplied_llm_base_url,
        node_token,
    );
    let result = execute_workflow_with_policy_and_options(
        spec,
        &values,
        request.execute,
        None,
        None,
        None,
        &options,
    )
    .await?;
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
            // This is local reporting, not an authenticated platform call.
            // API-key-only users and fresh installs therefore get a truthful
            // identity report instead of an invented login requirement.
            let state = paths.load_cli_state().ok().unwrap_or_default();
            let creds = state.credentials.as_ref();
            let endpoints = PlatformEndpoints::from_env();
            // Report the name that actually supplied the key, not a fixed
            // string -- an operator with both spellings set otherwise cannot
            // tell which one the process read.
            let credential_source = if let Some(name) = PlatformVar::API_KEY.source() {
                name
            } else if creds.is_some() {
                "cli-state session"
            } else {
                "none"
            };
            let expired = creds
                .and_then(|value| value.expires_at)
                .is_some_and(|exp| chrono::Utc::now() >= exp);
            let platform_url = creds
                .map(|value| value.platform_url.as_str())
                .filter(|value| !value.is_empty())
                .unwrap_or(&endpoints.api_base);

            if json {
                let out = serde_json::json!({
                    "org_id": creds.and_then(|value| value.org_id.as_deref()),
                    "org_name": creds.and_then(|value| value.org_name.as_deref()),
                    "project_id": creds.and_then(|value| value.project_id.as_deref()),
                    "project_name": creds.and_then(|value| value.project_name.as_deref()),
                    "user_id": creds.and_then(|value| value.user_id.as_deref()),
                    "display_name": creds.and_then(|value| value.display_name.as_deref()),
                    "platform_url": platform_url,
                    "credential_source": credential_source,
                    "valid_until": creds.and_then(|value| value.expires_at.map(|d| d.to_rfc3339())),
                    "expired": expired,
                });
                println!("{}", serde_json::to_string_pretty(&out)?);
                return Ok(());
            }

            println!("\nFabric identity (local state)");
            println!("───────────────────────────────────────────────");
            println!(
                "  Display name : {}",
                creds
                    .and_then(|value| value.display_name.as_deref())
                    .unwrap_or("(not stored locally)")
            );
            println!(
                "  User ID      : {}",
                creds
                    .and_then(|value| value.user_id.as_deref())
                    .unwrap_or("(not stored locally)")
            );
            println!(
                "  Org          : {} ({})",
                creds
                    .and_then(|value| value.org_name.as_deref())
                    .unwrap_or("(not stored locally)"),
                creds
                    .and_then(|value| value.org_id.as_deref())
                    .unwrap_or("(not stored locally)")
            );
            println!(
                "  Project      : {} ({})",
                creds
                    .and_then(|value| value.project_name.as_deref())
                    .unwrap_or("(not stored locally)"),
                creds
                    .and_then(|value| value.project_id.as_deref())
                    .unwrap_or("(not stored locally)")
            );
            println!("  Platform     : {platform_url}");
            println!("  Credential   : {credential_source}");
            match creds.and_then(|value| value.expires_at) {
                Some(exp) => {
                    let label = if expired { "EXPIRED" } else { "valid until" };
                    println!("  Token        : {label} {}", exp.to_rfc3339());
                }
                None => println!("  Token        : no stored expiry"),
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
                println!("  Trust is transitive via the platform root CA.");
                println!("  Your own identity is shown by federation whoami.");
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
            // This command never discovered anything.
            //
            // `init_mesh` only allocates an empty peer list — the mDNS listener
            // lives in `start_mesh`, which was never called. So the old body
            // slept for `timeout` and then read a list nothing could ever have
            // filled, printing "No peers found." whether or not peers existed.
            // A false negative is worse than a missing feature: it answers the
            // question the user asked, wrongly.
            //
            // Both refusal conditions are checked HERE and reported, because
            // `start_mesh` refuses on a background task whose reason the user
            // would never see — leaving the same silent empty result.
            if prism_runtime::offline::enabled() {
                println!(
                    "Discovery not run: offline mode. mDNS is LAN multicast, \
                     which hard offline blocks."
                );
                return Ok(());
            }
            let auth_token = resolve_agent_auth()
                .ok()
                .map(|(_, auth)| auth.secret().to_string());
            if auth_token.is_none() {
                println!(
                    "Discovery not run: not authenticated. The mesh gates peer \
                     interaction on platform RBAC, so discovery needs a session."
                );
                return Ok(());
            }

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
            // `MeshHandle` is Clone and its peer list is an Arc, so the clone
            // reads the same list the mDNS task fills.
            let probe = handle.clone();
            let cancel = tokio_util::sync::CancellationToken::new();
            let task = prism_mesh::start_mesh(
                handle,
                prism_mesh::MeshStartOptions {
                    node_name: "discovery-probe".into(),
                    publish_port: 0,
                    // Passive: listen for peers, do not advertise this probe as
                    // a node. `prism mesh discover` is a question, not a join.
                    broadcast: false,
                    capabilities: Vec::new(),
                    discovery_interval_secs: 5,
                    event_tx: None,
                    auth_token,
                    // No --offline flag on `mesh discover`; PRISM_OFFLINE=1
                    // still refuses inside mesh_start_refusal.
                    offline: false,
                },
                cancel.clone(),
            );
            tokio::time::sleep(Duration::from_secs(timeout)).await;
            let peers = probe.peers();

            // Cancel and AWAIT — never `abort()` straight after `cancel()`.
            //
            // `start_mesh` promises a clean shutdown: on cancellation it breaks
            // its select loop and calls `mdns.stop()`, which unregisters the
            // service and shuts the mDNS daemon down. `abort()` drops the task
            // future before the cancellation branch is ever polled, so that
            // cleanup never runs — a reviewer measured 0 graceful shutdowns in
            // 200 trials of this exact idiom.
            //
            // `mdns_sd::ServiceDaemon` has no `Drop` impl and owns an OS thread
            // holding the bound UDP socket, which exits only on an explicit
            // shutdown command. So aborting abandons a thread and a socket. In
            // this one-shot CLI the process exits immediately and the OS
            // reclaims both — but the idiom leaks once per call anywhere
            // longer-lived, and it silently made the cleanup path dead code.
            //
            // Bounded, so a wedged daemon cannot hang the command.
            cancel.cancel();
            let _ = tokio::time::timeout(Duration::from_secs(3), task).await;
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
        MeshCommands::Sync { dataset_name, peer } => {
            let peer = peer.trim_end_matches('/').to_string();
            // The peer URL is user-typed, but hard offline still applies —
            // `sync_dataset_from_peer` gates it too; failing here first
            // gives the reason before any session mint is attempted.
            prism_runtime::offline::check_url(&peer).map_err(|r| anyhow!(r))?;

            // The tenant is keyed on the PEER's node identity, which its
            // public discovery route reports. Refusing without one beats
            // inventing a tenant the counting layer would misattribute.
            let nodes_url = format!("{peer}/api/mesh/nodes");
            let status: serde_json::Value = reqwest::get(&nodes_url)
                .await
                .with_context(|| format!("Failed to reach peer at {nodes_url}"))?
                .json()
                .await
                .with_context(|| format!("Peer at {nodes_url} did not answer with JSON"))?;
            let publisher = status["node_id"]
                .as_str()
                .and_then(|id| uuid::Uuid::parse_str(id).ok())
                .ok_or_else(|| {
                    anyhow!(
                        "peer at {peer} reports no mesh node id (is its mesh online?) — \
                         cannot attribute the synced data to a publisher"
                    )
                })?;

            // Self-peer guard. The Kafka path skips its own publishes
            // (sync.rs: `node_id == our_node_id`); without the same check
            // here, a node pointed at its own dashboard syncs its own facts
            // into `mesh:{its own id}` and then serves them back as peer
            // knowledge.
            let our_node_id = prism_mesh::load_or_create_node_id(&paths.state_dir)?;
            if publisher == our_node_id {
                bail!(
                    "peer at {peer} is this node itself (mesh node {publisher}) — \
                     a node cannot pull its own dataset as peer knowledge. \
                     Point --peer at another node's dashboard."
                );
            }

            // The owner's platform token lets the peer VERIFY who is
            // pulling. Without a login, a loopback peer still works (it
            // mints an anonymous-local session); a remote peer will refuse.
            let platform_token = paths
                .load_cli_state()
                .ok()
                .and_then(|s| s.credentials)
                .map(|c| c.access_token);
            if platform_token.is_none() {
                println!(
                    "  ⚠ Not logged in: a remote peer will refuse this pull. \
                     Loopback peers still answer."
                );
            }
            let sessions = prism_mesh::peer_session::PeerSessions::new(platform_token);
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            let sync_config = Some(prism_mesh::sync::SyncConfig {
                provenance_db: std::path::PathBuf::from(home).join(".prism/provenance.db"),
            });

            println!("Pulling dataset '{dataset_name}' from {peer} (node {publisher})...");
            let client = prism_mesh::sync::sync_http_client();
            let synced = prism_mesh::sync::sync_dataset_from_peer(
                &client,
                // The HUMAN typed this address, which is what makes it
                // eligible to be shown the platform credential.
                &prism_mesh::peer_session::PeerAddress::operator_named(&peer),
                &dataset_name,
                publisher,
                &sync_config,
                &sessions,
            )
            .await?;
            println!(
                "✓ {synced} entit{} synced under tenant '{}'",
                if synced == 1 { "y" } else { "ies" },
                prism_mesh::sync::mesh_tenant(&publisher)
            );
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
        eprintln!("Current config is shown by configure --show.");
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

/// The chat endpoint for a direct-provider target, from the provider
/// registry (`providers.toml` + `~/.prism/providers.toml`).
///
/// Every chat surface resolves the endpoint here so the four of them can
/// never drift apart again. Before the registry they each carried their own
/// copy of `format!("https://api.{provider}.com/v1")` — a guess that only
/// happens to be right for OpenAI and DeepSeek, and that quietly aimed
/// Mistral at `api.mistral.com` (the real host is `.ai`), Groq at the wrong
/// path, and Google at a host that does not serve chat at all.
///
/// An id in nobody's registry keeps that historical guess rather than
/// erroring: it is the only thing we can do for an unknown slug, it is what
/// the code did before, and `prism use provider` warns at selection time
/// (the moment the user can act on it) instead of failing mid-turn.
fn provider_endpoint(registry: &crate::providers::Registry, provider: &str) -> String {
    crate::providers::base_url_for(registry, provider)
        .unwrap_or_else(|| crate::providers::legacy_guess_base_url(provider))
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
    let api_key_override = api_key_override.filter(|key| !key.trim().is_empty());

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
                .or_else(|| llm.resolve_api_key().filter(|key| !key.trim().is_empty())),
        ),
        crate::chat_config::ChatTarget::Provider {
            provider,
            model: prov_model,
            api_key_env,
        } => {
            let registry = crate::providers::Registry::load();
            let env_name = api_key_env
                .clone()
                .unwrap_or_else(|| crate::providers::default_api_key_env(&registry, provider));
            (
                url_override
                    .map(str::to_string)
                    .unwrap_or_else(|| provider_endpoint(&registry, provider)),
                model_override
                    .map(str::to_string)
                    .unwrap_or_else(|| prov_model.clone()),
                api_key_override
                    .map(str::to_string)
                    .or_else(|| {
                        std::env::var(&env_name)
                            .ok()
                            .filter(|key| !key.trim().is_empty())
                    })
                    .or_else(|| llm.resolve_api_key().filter(|key| !key.trim().is_empty())),
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
                .or_else(|| llm.resolve_api_key().filter(|key| !key.trim().is_empty()));
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

/// Decide the `(llm_base_url, llm_model)` a workflow run should inject into its
/// context so `llm_*` steps reach the real model, mirroring the
/// `Commands::Backend` chat resolution per configured target. `None` ⇒ inject
/// nothing (the workflow falls back to its own env resolution).
///
/// This deliberately does NOT reuse `build_llm_config`, whose Marc27 arm
/// resolves the raw `[llm].url` (whose default is the localhost llama.cpp
/// endpoint) and errors when no `[llm].model` is set — both wrong for a
/// signed-in cloud user and a re-open of the #132 silent-localhost class.
///
/// For the Marc27 cloud target the base URL is the outcome of the shared
/// `marc27_llm_base_url` resolver (LLM_BASE_URL env → signed-in project `/llm`
/// endpoint → explicit `[llm].url`, refusing the localhost default); `None`
/// there ⇒ skip, so an unauthenticated user with no explicit endpoint never
/// gets the dead localhost default and a stray localhost-ingest `[llm].url`
/// never shadows a working env override. The model honors `LLM_MODEL` (env) →
/// `/use marc27 --model` → `[llm].model`, else the platform's `default` alias.
fn resolve_workflow_llm_endpoint(
    registry: &crate::providers::Registry,
    chat_target: &crate::chat_config::ChatTarget,
    cfg_model: Option<&str>,
    env_model: Option<String>,
    marc27_base_url: Option<String>,
) -> Option<(String, String)> {
    use crate::chat_config::ChatTarget;
    match chat_target {
        ChatTarget::Local { url, model, .. } => {
            (!url.is_empty()).then(|| (url.clone(), model.clone()))
        }
        ChatTarget::Provider {
            provider, model, ..
        } => Some((provider_endpoint(registry, provider), model.clone())),
        ChatTarget::Marc27 {
            model: target_model,
        } => {
            let base_url = marc27_base_url?;
            let model = resolve_marc27_model(env_model, target_model.as_deref(), cfg_model)
                .unwrap_or_else(|| "default".to_string());
            Some((base_url, model))
        }
    }
}

/// Gather the (global) config the workflow LLM endpoint resolves from and hand
/// it to the pure [`resolve_workflow_llm_endpoint`]. Shared by the CLI
/// `prism workflow run` paths and the native MCP server so every headless
/// launch surface points `llm_*` steps at the same endpoint the chat path uses.
fn resolve_workflow_llm_pair(project_root: &Path, paths: &PrismPaths) -> Option<(String, String)> {
    let node_config = prism_core::config::NodeConfig::load(Some(project_root));
    let cfg_llm = &node_config.llm;
    let chat_target = crate::chat_config::load().unwrap_or_default().chat;

    // Marc27 base URL via the SAME resolver the chat backend uses; `.ok()`
    // turns the honest #132 error (unauthenticated + no explicit endpoint)
    // into "skip injection" rather than a dead localhost default.
    let marc27_base_url = if matches!(chat_target, crate::chat_config::ChatTarget::Marc27 { .. }) {
        let endpoints = PlatformEndpoints::from_env();
        marc27_llm_base_url(paths, &endpoints.api_base, &cfg_llm.url).ok()
    } else {
        None
    };

    resolve_workflow_llm_endpoint(
        &crate::providers::Registry::load(),
        &chat_target,
        cfg_llm.model.as_deref(),
        std::env::var("LLM_MODEL").ok(),
        marc27_base_url,
    )
}

/// Resolve the trusted LLM endpoint and its paired credential for a CLI
/// workflow launch. The endpoint is kept in `values` for workflow rendering,
/// while the key travels only through `WorkflowExecutionOptions`.
fn resolve_workflow_llm_options(
    project_root: &Path,
    paths: &PrismPaths,
    caller_supplied_llm_base_url: bool,
    node_token: Option<String>,
) -> WorkflowExecutionOptions {
    let resolved = resolve_workflow_llm_pair(project_root, paths);
    let trusted_llm_api_key = resolved
        .as_ref()
        .and_then(|_| resolve_workflow_llm_api_key(project_root, paths));
    WorkflowExecutionOptions {
        trusted_llm_base_url: resolved.as_ref().map(|(base_url, _)| base_url.clone()),
        trusted_llm_api_key,
        caller_supplied_llm_base_url,
        trusted_node_port: node_token.as_ref().map(|_| 7327),
        trusted_node_token: node_token,
    }
}

fn resolve_workflow_llm_api_key(project_root: &Path, paths: &PrismPaths) -> Option<String> {
    let node_config = prism_core::config::NodeConfig::load(Some(project_root));
    let chat_target = crate::chat_config::load().unwrap_or_default().chat;
    let platform_token = paths
        .load_cli_state()
        .ok()
        .and_then(|state| state.credentials)
        .map(|credentials| credentials.access_token);
    resolve_workflow_llm_api_key_for_target(&chat_target, &node_config.llm, platform_token)
}

fn resolve_workflow_llm_api_key_for_target(
    chat_target: &crate::chat_config::ChatTarget,
    cfg_llm: &prism_core::config::LlmSection,
    platform_token: Option<String>,
) -> Option<String> {
    let non_empty = |key: Option<String>| key.filter(|value| !value.trim().is_empty());

    match chat_target {
        crate::chat_config::ChatTarget::Local { api_key, .. } => {
            non_empty(api_key.clone()).or_else(|| non_empty(cfg_llm.resolve_api_key()))
        }
        crate::chat_config::ChatTarget::Provider {
            provider,
            api_key_env,
            ..
        } => {
            let registry = crate::providers::Registry::load();
            let env_name = api_key_env
                .clone()
                .unwrap_or_else(|| crate::providers::default_api_key_env(&registry, provider));
            non_empty(std::env::var(env_name).ok()).or_else(|| non_empty(cfg_llm.resolve_api_key()))
        }
        crate::chat_config::ChatTarget::Marc27 { .. } => non_empty(
            std::env::var("LLM_API_KEY")
                .ok()
                .or_else(|| PlatformVar::API_KEY.get())
                .or_else(|| PlatformVar::TOKEN.get())
                .or_else(|| cfg_llm.resolve_api_key())
                .or(platform_token),
        ),
    }
}

/// Inject the resolved chat LLM endpoint into a workflow's `values`. A `--set`
/// override always wins (`or_insert`); an unresolved endpoint injects nothing.
fn inject_workflow_llm_endpoint(
    values: &mut BTreeMap<String, String>,
    project_root: &Path,
    paths: &PrismPaths,
) {
    let Some((base_url, model)) = resolve_workflow_llm_pair(project_root, paths) else {
        return;
    };
    if !base_url.is_empty() {
        values.entry("llm_base_url".to_string()).or_insert(base_url);
    }
    if !model.is_empty() {
        values.entry("llm_model".to_string()).or_insert(model);
    }
}

// ── prism ingest ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IngestBackend {
    LocalTabular,
    PlatformText,
}

/// Text-document formats the platform's holistic ingest accepts — that
/// backend's own surface, NOT a shadow of the connector registry.
const PLATFORM_TEXT_EXTENSIONS: &[&str] = &["pdf", "json", "jsonl", "owl", "cif", "txt", "md"];

fn ingest_backend(path: &Path) -> Option<IngestBackend> {
    // Tabular formats are whatever the connector registry claims — the one
    // place that owns the extension→connector decision. A new file
    // connector routes here with zero edits.
    if prism_ingest::connectors::registry().claims(path) {
        return Some(IngestBackend::LocalTabular);
    }

    let ext = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    // Text documents go to the platform's holistic ingest, not to a local
    // file connector.
    if PLATFORM_TEXT_EXTENSIONS.contains(&ext.as_str()) {
        Some(IngestBackend::PlatformText)
    } else {
        None
    }
}

/// Every format `ingest_backend` routes somewhere: the connector registry's
/// claims (so a newly registered connector is ADVERTISED with zero edits
/// here, exactly as it is routed), then the platform text formats. The
/// unsupported-format error must never advertise a route `ingest_backend`
/// will not take, nor hide one it will.
fn supported_ingest_formats() -> String {
    let mut formats: Vec<&str> = prism_ingest::connectors::registry().extensions();
    formats.extend(PLATFORM_TEXT_EXTENSIONS);
    formats.join(", ")
}

fn ingest_format(path: &Path) -> String {
    path.extension()
        .and_then(|value| value.to_str())
        .unwrap_or("unknown")
        .to_ascii_lowercase()
}

/// Where text-document ingest runs — and, when it lands on the hosted
/// platform, whether the user actually asked for that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextLocality {
    /// On-device extraction into the bundled Turso store.
    Local,
    /// The platform, because the user said so (`locality = "cloud"`).
    Cloud,
    /// The platform only because `auto` found no on-device model to extract
    /// with. Nobody chose the cloud here, so a missing account is a dead end
    /// to report — not a routing decision to announce. See [`handle_ingest`].
    CloudNoLocalModel,
}

impl TextLocality {
    fn is_local(self) -> bool {
        matches!(self, Self::Local)
    }
}

/// Decide locality from the two inputs that matter: the configured
/// `[ontology] locality` and the base URL of the LLM that would do the
/// extraction (`None` = no usable LLM config at all). Split out from
/// [`resolve_text_ingest_locality`] so the decision is testable without
/// a `$HOME` and a config file.
fn text_locality_for(configured: &str, llm_base_url: Option<&str>) -> TextLocality {
    match configured {
        "local" => TextLocality::Local,
        "cloud" => TextLocality::Cloud,
        // auto: local iff the extraction model runs on this machine — a
        // loopback HTTP server, or `gguf://local`, PRISM's own embedded
        // engine. The gguf sentinel is not a network URL at all, so it is
        // matched here by the LLM crate's own predicate rather than by
        // widening `is_loopback_url`, which answers a different (security)
        // question — "does this URL target the loopback interface" — for
        // the offline gate and the local-server sweep.
        _ => match llm_base_url {
            Some(url) if is_loopback_url(url) || prism_ingest::llm::is_local_gguf_url(url) => {
                TextLocality::Local
            }
            _ => TextLocality::CloudNoLocalModel,
        },
    }
}

/// Resolve where text-document ingest runs: `[ontology] locality` in
/// prism.toml ("local"/"cloud" honored as-is); "auto" → local iff the LLM
/// endpoint `build_llm_config` resolves is loopback (an on-device model).
fn resolve_text_ingest_locality(
    project_root: &Path,
    llm_url: Option<&str>,
    model: Option<&str>,
    api_key: Option<&str>,
) -> TextLocality {
    let node_config = prism_core::config::NodeConfig::load(Some(project_root));
    let llm_base_url = build_llm_config(project_root, llm_url, model, api_key)
        .ok()
        .map(|cfg| cfg.base_url);
    text_locality_for(&node_config.ontology.locality, llm_base_url.as_deref())
}

/// What to print when `prism ingest` has neither a configured on-device
/// model nor a platform account. Local comes first: `prism ingest` with no
/// flags is a local-first command, and the old error ("Not logged in. Run
/// `prism login`.") named the one remedy the user was trying to avoid.
///
/// `discovered` is what a sweep of this machine actually found. When it is
/// non-empty the message stops being a template: PRISM saw the server, so
/// it prints the command that uses it, model id and all. Telling a user
/// with Ollama already serving two models to go and find a model id was
/// the friction this replaces — the information was a 400ms probe away.
fn no_ingest_backend_message(discovered: &[local_llm::LocalServer]) -> String {
    let ready: Vec<&local_llm::LocalServer> = discovered
        .iter()
        .filter(|s| s.state == local_llm::ServerState::Ready && !s.models.is_empty())
        .collect();

    if ready.is_empty() {
        // Nothing is serving. Keep the template — it is the honest answer
        // when there is no reality to report — but note anything we found
        // mid-startup rather than pretending the machine is empty.
        let loading: Vec<String> = discovered
            .iter()
            .filter(|s| s.state == local_llm::ServerState::Loading)
            .map(|s| {
                format!(
                    "\n\n{} at {} is up but still loading a model.",
                    s.name, s.base_url
                )
            })
            .collect();
        return format!(
            "no on-device model to extract with, and the hosted platform needs an \
             account.{loading}\n\n\
             Ingest these documents locally — nothing leaves your machine:\n  \
             prism use local --url http://localhost:11434/v1 --model <model>   (Ollama)\n  \
             prism use local --url http://localhost:8080 --model <model>       (llama.cpp)\n\
             then re-run this command.\n\n\
             Or use the hosted platform instead:\n  \
             prism login",
            loading = loading.join(""),
        );
    }

    // One server, one model: there is exactly one thing the user can mean,
    // so hand them that command complete. We still do not run it for them —
    // where inference happens is their call, not ours.
    let single = match ready.as_slice() {
        [only] => only.sole_model().map(|model| (*only, model)),
        _ => None,
    };
    if let Some((server, model)) = single {
        return format!(
            "no on-device model configured for extraction, and the hosted platform \
             needs an account.\n\n\
             {name} is already running here with {model}. Use it — nothing leaves \
             your machine:\n  {command}\n\
             then re-run this command.\n\n\
             Or use the hosted platform instead:\n  prism login",
            name = server.name,
            command = server.use_command(model),
        );
    }

    // Several candidates. List them; picking would be a guess.
    let options: Vec<String> = ready
        .iter()
        .flat_map(|server| {
            server
                .models
                .iter()
                .map(move |model| format!("  {}", server.use_command(model)))
        })
        .collect();
    format!(
        "no on-device model configured for extraction, and the hosted platform \
         needs an account.\n\n\
         These are already running on this machine — pick one, and nothing leaves \
         your machine:\n{options}\n\
         then re-run this command.\n\n\
         Or use the hosted platform instead:\n  prism login",
        options = options.join("\n"),
    )
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
    // Step 1 of ingest runs on this machine and needs the local runtime.
    // Nothing else starts it, so start it here — or explain, once, why we
    // can't. The alternative (and the old behaviour) is a raw connect error
    // against a port no PRISM code path ever binds.
    prism_node::runtime_service::ensure_running(runtime_url, |msg| eprintln!("  {msg}"))
        .await
        .with_context(|| {
            format!(
                "local text extraction failed for {} (nothing was sent to the platform)",
                path.display()
            )
        })?;

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
        .with_context(|| {
            format!(
                "local text extraction failed for {} — the runtime at {runtime_url} stopped \
                 responding (nothing was sent to the platform)",
                path.display()
            )
        })?;

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

    let response = response.platform_error_for_status().await?;

    Ok(response.json().await?)
}

/// Resolve the active ontology id for local ingest from `prism.toml`
/// (`[ontology] id`, default "emmo"), refusing unimplemented `[ontology]
/// engine` values loudly — that knob used to be read by nothing, so any
/// value silently behaved like "llm".
fn active_ontology_from_config(project_root: &Path) -> Result<String> {
    let node_config = prism_core::config::NodeConfig::load(Some(project_root));
    let engine = node_config.ontology.engine;
    if engine != "llm" {
        bail!(
            "[ontology] engine = \"{engine}\" is not implemented — only \"llm\" is. \
             Remove the setting or set engine = \"llm\"."
        );
    }
    Ok(node_config.ontology.id)
}

async fn run_local_ingest_file(
    path: &Path,
    project_root: &Path,
    model: Option<&str>,
    llm_url: Option<&str>,
    api_key: Option<&str>,
    schema_only: bool,
    mapping_path: Option<&Path>,
) -> Result<serde_json::Value> {
    use prism_ingest::pipeline::{IngestPipeline, PipelineConfig};

    let mapping = mapping_path
        .map(prism_ingest::mapping::OntologyMapping::from_file)
        .transpose()?;

    // The `[ontology] id` knob selects which vocabulary this ingest
    // extracts and validates with; an unregistered id fails the run loudly
    // inside the pipeline instead of silently ingesting as EMMO.
    let ontology = Some(active_ontology_from_config(project_root)?);

    let config = if schema_only {
        PipelineConfig {
            llm: None,
            max_sample_rows: 10,
            mapping: None,
            provenance_db: None,
            ontology,
        }
    } else {
        let llm_cfg = build_llm_config(project_root, llm_url, model, api_key)?;
        PipelineConfig {
            llm: Some(llm_cfg),
            max_sample_rows: 10,
            mapping,
            provenance_db: None,
            ontology,
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

/// Ingest a text document entirely on-device: extract EMMO facts with the
/// local LLM and write them (with one PROV-O activity) into the bundled
/// Turso provenance store. Nothing leaves the machine.
#[allow(clippy::too_many_arguments)]
async fn run_local_text_ingest_file(
    path: &Path,
    project_root: &Path,
    model: Option<&str>,
    llm_url: Option<&str>,
    api_key: Option<&str>,
    runtime_url: &str,
    schema_only: bool,
    mapping_path: Option<&Path>,
) -> Result<serde_json::Value> {
    // Text-document extraction is wired to the built-in EMMO ontology only
    // (EMMO prompt, QUDT-typed MaterialFacts). Refuse honestly under any
    // other active ontology rather than extracting with the wrong
    // vocabulary — only the tabular pipeline consults the ontology registry
    // today.
    let ontology_id = active_ontology_from_config(project_root)?;
    if ontology_id != prism_ingest::ontologies::DEFAULT_ONTOLOGY_ID {
        bail!(
            "text-document ingest currently extracts with the built-in EMMO ontology only; \
             the active ontology '{ontology_id}' has no text extractor. Ingest tabular data \
             (CSV/Parquet), or set [ontology] id = \"emmo\"."
        );
    }
    let ontology = prism_ingest::ontologies::active(Some(&ontology_id))?;

    if mapping_path.is_some() {
        eprintln!(
            "Warning: --mapping is only applied to the local tabular ingest pipeline and is ignored for {}.",
            path.display()
        );
    }

    // Local PDF parsing isn't wired yet. Be honest and skip — never quietly
    // fall back to the cloud against the user's locality choice.
    if ingest_format(path) == "pdf" {
        let reason = "local PDF parsing isn't available yet — ingest text/markdown/json locally, or set locality = \"cloud\" for PDFs";
        eprintln!("  Skipping {}: {reason}", path.display());
        return Ok(serde_json::json!({
            "backend": "local_text",
            "path": path.display().to_string(),
            "format": "pdf",
            "skipped": true,
            "reason": reason,
        }));
    }

    // Non-PDF text formats just read the file — the runtime sidecar is
    // never contacted (the PDF branch is excluded above).
    let (text, _pages, warning) = extract_platform_ingest_text(path, runtime_url).await?;
    let chars = text.chars().count();
    if text.trim().is_empty() {
        bail!("No ingestable text found in {}", path.display());
    }

    if schema_only {
        return Ok(serde_json::json!({
            "backend": "local_text",
            "path": path.display().to_string(),
            "format": ingest_format(path),
            "schema_only": true,
            "chars": chars,
            "warning": warning,
        }));
    }

    let llm_cfg = build_llm_config(project_root, llm_url, model, api_key)?;
    let agent_id = if llm_cfg.model.is_empty() {
        "prism-ingest".to_string()
    } else {
        llm_cfg.model.clone()
    };
    let llm = prism_ingest::llm::LlmClient::new(llm_cfg);
    let title = path
        .file_stem()
        .or_else(|| path.file_name())
        .and_then(|value| value.to_str())
        .unwrap_or("untitled");

    let extraction =
        prism_ingest::text_extract::extract_facts_from_text(&llm, title, &text).await?;
    let parse_error = extraction.parse_error.clone();
    let facts = extraction.facts;

    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let db_path = PathBuf::from(home).join(".prism/provenance.db");
    let store = prism_provenance::ProvenanceStore::open(&db_path).await?;

    let now = chrono::Utc::now().to_rfc3339();
    let prov = prism_provenance::LocalProvenance {
        activity_id: uuid::Uuid::new_v4().to_string(),
        agent_id: agent_id.clone(),
        agent_kind: "SoftwareAgent".into(),
        source_entity_id: path.display().to_string(),
        source_kind: "Document".into(),
        // Local single-user store — no per-run tenancy (yet).
        tenant: "local".into(),
        started_at: now.clone(),
        ended_at: now,
        locality: "local".into(),
        // Local ingest reads the source itself — the locator IS the origin.
        origin_source_id: None,
    };
    // Peer-echo tripwire, BEFORE the writes: an agent that read a peer
    // fact out of `prism query` and fed it back through `prism ingest`
    // re-asserts it under "local" — laundering peer knowledge into local,
    // which corroboration then counts as independent evidence. The store
    // cannot block that write (it is indistinguishable from a genuinely
    // independent source stating the same fact), so the collision is
    // reported LOUDLY instead of absorbed silently. This is DETECTION,
    // not prevention: the write below proceeds either way.
    let (peer_echoes, peer_echo_check_errors) = collect_peer_echoes(&store, &facts).await;
    if !peer_echoes.is_empty() {
        eprintln!(
            "  WARNING: {} extracted fact(s) already exist under mesh peer tenant(s).",
            peer_echoes.len()
        );
        eprintln!(
            "           If this document restates knowledge you received over the mesh \
             (e.g. it was produced from a `prism query` result), this ingest launders \
             peer knowledge into your local tenant and future corroboration will count \
             it as independent evidence. If the document is a genuinely independent \
             source, no action is needed. The write proceeds either way; the echoes \
             are listed under `peer_echoes` in the ingest summary."
        );
    }
    if !peer_echo_check_errors.is_empty() {
        eprintln!(
            "  WARNING: {} fact(s) could not be checked against mesh tenants — their \
             laundering status is UNKNOWN, not clean (see `peer_echo_check_errors`).",
            peer_echo_check_errors.len()
        );
    }

    store.record_activity(&prov).await?;
    for fact in &facts {
        store
            .write_fact_with_classification(
                fact,
                &prov,
                prism_provenance::OntologyClassification {
                    version_iri: ontology.version_iri().as_str(),
                    artifact_sha256: ontology.artifact_sha256(),
                },
            )
            .await?;
    }
    // Best-effort: vectorize the freshly written entity names into the same
    // Turso store so `prism query --semantic` works without Qdrant.
    // Failures are logged inside and never fail the ingest.
    store.embed_entities_best_effort(&facts, &prov.tenant).await;

    Ok(serde_json::json!({
        "backend": "local_text",
        "path": path.display().to_string(),
        "format": ingest_format(path),
        "schema_only": false,
        "chars": chars,
        "facts_written": facts.len(),
        "model": agent_id,
        "store": db_path.display().to_string(),
        "warning": warning,
        // A partial read has to be reported here, not left to a log line: the
        // subscriber is built with `EnvFilter::from_default_env()`, whose
        // default directive is ERROR, so `tracing::warn!` reaches nobody
        // unless RUST_LOG is set.
        "truncated_bytes": extraction.dropped_bytes,
        // Zero facts because the model returned garbage is a different outcome
        // from zero facts because the document held none. Only this field
        // tells them apart on the user's side.
        "parse_error": parse_error,
        // Facts that already exist under a mesh peer tenant — the loud
        // half of the laundering tripwire (see the WARNING above).
        "peer_echoes": peer_echoes,
        // Facts whose echo check FAILED: unknown status, not clean.
        "peer_echo_check_errors": peer_echo_check_errors,
    }))
}

/// Peer-echo scan for facts about to be written under the local tenant:
/// which of them already exist under a mesh tenant (see
/// `ProvenanceStore::peer_tenants_asserting_among`). Returns the echo rows
/// for the ingest summary plus any check FAILURES — a failed check means a
/// fact whose laundering status is unknown, which callers must report
/// rather than swallow (a `tracing::warn!` here would be dark by default:
/// the subscriber uses `EnvFilter::from_default_env()`, whose default
/// directive is ERROR). Mesh tenants are discovered once for the whole
/// batch, not per fact.
async fn collect_peer_echoes(
    store: &prism_provenance::ProvenanceStore,
    facts: &[prism_provenance::MaterialFact],
) -> (Vec<serde_json::Value>, Vec<String>) {
    let mut echoes = Vec::new();
    let mut errors = Vec::new();
    if facts.is_empty() {
        return (echoes, errors);
    }
    let tenants = match store.default_read_tenants().await {
        Ok(tenants) => tenants,
        Err(e) => {
            errors.push(format!(
                "mesh tenant discovery failed — no fact could be checked: {e:#}"
            ));
            return (echoes, errors);
        }
    };
    for fact in facts {
        match store
            .peer_tenants_asserting_among(&tenants, &fact.subject, &fact.predicate, &fact.object)
            .await
        {
            Ok(holders) if !holders.is_empty() => {
                echoes.push(serde_json::json!({
                    "subject": fact.subject,
                    "predicate": fact.predicate,
                    "object": fact.object,
                    "peer_tenants": holders,
                }));
            }
            Ok(_) => {}
            Err(e) => errors.push(format!(
                "'{} {} {}': {e:#}",
                fact.subject, fact.predicate, fact.object
            )),
        }
    }
    (echoes, errors)
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
                println!(
                    "  Graph: {nodes} nodes, {edges} edges written to the local knowledge graph"
                );
            }
            if let Some(report) = dropped_entities_report(result) {
                println!("{report}");
            }
            if let Some(report) = dropped_relationships_report(result) {
                println!("{report}");
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
        "local_text" => {
            if summary
                .get("skipped")
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
            {
                let reason = value_string(summary, &["reason"]).unwrap_or("skipped");
                println!("  Skipped: {reason}");
            } else {
                let chars = summary
                    .get("chars")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0);
                println!("  Text: {chars} chars extracted on-device");
                if let Some(warning) = summary.get("warning").and_then(|value| value.as_str()) {
                    println!("  Warning: {warning}");
                }
                if let Some(parse_error) =
                    summary.get("parse_error").and_then(|value| value.as_str())
                {
                    println!("  Warning: no facts extracted \u{2014} {parse_error}");
                }
                let truncated = summary
                    .get("truncated_bytes")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0);
                if truncated > 0 {
                    println!(
                        "  Warning: {truncated} bytes were NOT read. The document exceeds the \
                         extraction budget and there is no chunking, so these facts come from \
                         the start of it only."
                    );
                }
                if summary
                    .get("schema_only")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false)
                {
                    println!("  (schema-only mode — text was measured but not extracted)");
                } else {
                    let facts = summary
                        .get("facts_written")
                        .and_then(|value| value.as_u64())
                        .unwrap_or(0);
                    let store =
                        value_string(summary, &["store"]).unwrap_or("~/.prism/provenance.db");
                    println!("  Facts: {facts} written to local store ({store})");
                    if let Some(model) = value_string(summary, &["model"]) {
                        println!("  Model: {model}");
                    }
                    // The laundering tripwire must reach STDOUT with the
                    // summary — the earlier stderr warning is lost to any
                    // caller that captures stdout only (exactly the shape
                    // of an agent-driven ingest).
                    let echoes = summary
                        .get("peer_echoes")
                        .and_then(|value| value.as_array())
                        .map(Vec::len)
                        .unwrap_or(0);
                    if echoes > 0 {
                        println!(
                            "  Warning: {echoes} fact(s) already exist under mesh peer \
                             tenant(s) — if this document was produced from a mesh read, \
                             this ingest laundered peer knowledge into 'local' \
                             (details: `peer_echoes` in --json output)."
                        );
                    }
                    let unchecked = summary
                        .get("peer_echo_check_errors")
                        .and_then(|value| value.as_array())
                        .map(Vec::len)
                        .unwrap_or(0);
                    if unchecked > 0 {
                        println!(
                            "  Warning: {unchecked} peer-echo check(s) failed — those \
                             facts' laundering status is unknown, not clean."
                        );
                    }
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

/// The referential-containment drop report for one local-tabular ingest
/// result: how many extracted relationships were dropped for referencing
/// entities the extraction never declared, and why — `None` when nothing
/// was dropped. A drop is a PARTIAL result (the valid remainder was stored,
/// exit stays 0), never a silent one: these lines are the user's only
/// window on it, so they are kept out of `print_ingest_summary` where the
/// honesty of the report is testable without capturing stdout.
fn dropped_relationships_report(result: &serde_json::Value) -> Option<String> {
    let dropped = result.get("dropped_relationships")?.as_array()?;
    if dropped.is_empty() {
        return None;
    }
    let extracted = result
        .get("entities")
        .and_then(|value| value.get("relationships"))
        .and_then(|value| value.as_array())
        .map(Vec::len);
    let mut out = format!(
        "  Dropped: {}{} relationship(s) referencing entities the extraction never \
         declared — NOT stored (an endpoint is never invented to patch an edge):",
        dropped.len(),
        extracted
            .map(|total| format!(" of {total}"))
            .unwrap_or_default(),
    );
    for reason in dropped.iter().filter_map(|value| value.as_str()) {
        out.push_str(&format!("\n    ! {reason}"));
    }
    Some(out)
}

/// The unmapped-type drop report, same contract as
/// [`dropped_relationships_report`]: entities whose declared type the
/// active ontology maps to no storage label are dropped and REPORTED —
/// stored under an invented label never, silently dropped never. `None`
/// when nothing was dropped.
fn dropped_entities_report(result: &serde_json::Value) -> Option<String> {
    let dropped = result.get("dropped_entities")?.as_array()?;
    if dropped.is_empty() {
        return None;
    }
    let extracted = result
        .get("entities")
        .and_then(|value| value.get("entities"))
        .and_then(|value| value.as_array())
        .map(Vec::len);
    let mut out = format!(
        "  Dropped: {}{} entity(ies) of types the active ontology maps to no \
         storage label — NOT stored (a label is never invented):",
        dropped.len(),
        extracted
            .map(|total| format!(" of {total}"))
            .unwrap_or_default(),
    );
    for reason in dropped.iter().filter_map(|value| value.as_str()) {
        out.push_str(&format!("\n    ! {reason}"));
    }
    Some(out)
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
        .platform_error_for_status()
        .await?
        .json()
        .await?;

    let embedding_stats: serde_json::Value = auth
        .apply(client.get(format!("{api_base}/knowledge/embeddings/stats")))
        .send()
        .await?
        .platform_error_for_status()
        .await?
        .json()
        .await?;

    let jobs: serde_json::Value = auth
        .apply(client.get(format!("{api_base}/knowledge/ingest-jobs")))
        .send()
        .await?
        .platform_error_for_status()
        .await?
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
            .platform_error_for_status()
            .await?
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

/// Upload a document to the MARC27 platform's holistic ingest pipeline.
///
/// The local pipeline (`prism ingest` with no flags) needs a local LLM and a
/// local runtime and writes to the local Turso store. This is the other half:
/// the platform extracts, and the result lands in your MARC27 knowledge graph.
///
/// The endpoint streams Server-Sent Events and holds the connection until
/// extraction finishes, which is minutes for a real paper. The CLI has no
/// streaming HTTP dependency, so the body is read whole and the events are
/// replayed afterwards — the user is told that up front rather than left
/// staring at a silent terminal wondering whether it hung.
///
/// A PROV-O activity is recorded locally on success. Without it the client
/// keeps no record that this machine ingested anything: the facts live in
/// FalkorDB server-side and the local provenance spine stays empty, so
/// `prism query` can never answer "what did I put in, and when".
async fn handle_ingest_platform(path: &Path, json_output: bool) -> Result<()> {
    let bytes =
        std::fs::read(path).with_context(|| format!("could not read {}", path.display()))?;

    // Check the magic here rather than let the server reject it after the
    // upload — on a large file that is a pointless round trip, and the local
    // error can name the actual path.
    if !bytes.starts_with(b"%PDF") {
        bail!(
            "{} is not a PDF (no %PDF magic bytes). The platform ingest \
             endpoint accepts PDFs; for an HTML paper use `prism ingest-and-wait --url <URL>`.",
            path.display()
        );
    }

    let (api_base, auth) = resolve_agent_auth()?;
    let client = reqwest::Client::builder()
        // Generous: extraction of a full paper routinely runs into minutes,
        // and a timeout here would abandon work the platform has already
        // started paying for.
        .timeout(Duration::from_secs(1800))
        .build()?;

    if !json_output {
        println!(
            "Uploading {} ({} KB) to the platform…",
            path.display(),
            bytes.len() / 1024
        );
        println!(
            "The platform extracts while this connection is held open — this can take several minutes."
        );
    }

    let resp = auth
        .apply(
            client
                .post(format!("{api_base}/knowledge/ingest/holistic/upload"))
                .header("Content-Type", "application/pdf")
                .body(bytes.clone()),
        )
        .send()
        .await
        .context("upload request failed")?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("platform ingest failed ({status}): {}", body.trim());
    }

    // Replay the SSE stream. `error` is a real failure even though the HTTP
    // status was 200 — the pipeline reports stage failures inside the stream,
    // so trusting the status code alone would call a failed ingest a success.
    let mut steps: Vec<(String, serde_json::Value)> = Vec::new();
    let mut failure: Option<String> = None;
    for line in body.lines() {
        let Some(payload) = line.strip_prefix("data: ") else {
            continue;
        };
        let Ok(event) = serde_json::from_str::<serde_json::Value>(payload) else {
            continue;
        };
        let step = event
            .get("step")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        let data = event
            .get("data")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        if step == "error" {
            failure = Some(
                data.get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error")
                    .to_string(),
            );
        }
        steps.push((step, data));
    }

    if let Some(message) = failure {
        bail!("platform ingest failed during extraction: {message}");
    }
    if steps.is_empty() {
        bail!("platform returned no ingest events — nothing was ingested");
    }

    record_platform_ingest_provenance(path, &steps).await;

    if json_output {
        let out = serde_json::json!({
            "path": path.display().to_string(),
            "steps": steps.iter().map(|(s, d)| serde_json::json!({"step": s, "data": d})).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!();
        for (step, data) in &steps {
            let detail = data
                .as_object()
                .map(|o| {
                    o.iter()
                        .filter(|(k, _)| {
                            matches!(k.as_str(), "chars" | "count" | "title" | "warning")
                        })
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            println!("  {step:<24} {detail}");
        }
        // "degraded" is not "failed", and the difference matters: a degraded
        // run really did ingest, just without the GPU document model.
        if steps.iter().any(|(s, _)| s.contains("degraded")) {
            println!();
            println!("Note: the run was DEGRADED — the GPU document model was unavailable,");
            println!("so text was extracted without figure segmentation. The facts landed;");
            println!("figures and their captions did not.");
        }
        println!();
        println!("Check the graph with:  prism ingest --status");
    }
    Ok(())
}

/// Best-effort local PROV-O record of a platform ingest.
///
/// Best-effort on purpose: the document IS in the platform graph by the time
/// this runs, so failing the command because a local bookkeeping write failed
/// would report a successful ingest as an error.
async fn record_platform_ingest_provenance(path: &Path, steps: &[(String, serde_json::Value)]) {
    let Some(store) = open_campaign_provenance().await else {
        return;
    };
    let now = chrono::Utc::now().to_rfc3339();
    let title = steps
        .iter()
        .find_map(|(_, d)| d.get("title").and_then(|v| v.as_str()))
        .unwrap_or_else(|| {
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("uploaded document")
        })
        .to_string();

    let prov = prism_provenance::LocalProvenance {
        activity_id: uuid::Uuid::new_v4().to_string(),
        // The platform did the extraction, so it is the agent — attributing it
        // to this CLI would misname who produced the facts.
        agent_id: "marc27-platform/holistic-ingest".to_string(),
        agent_kind: "SoftwareAgent".to_string(),
        source_entity_id: path.display().to_string(),
        source_kind: "Document".to_string(),
        tenant: "local".to_string(),
        started_at: now.clone(),
        ended_at: now,
        // Distinguishes this from a locally-extracted document: the facts are
        // NOT in the local store, they are in the platform graph.
        locality: "platform".to_string(),
        // The platform extracted from the document itself — not a relay.
        origin_source_id: None,
    };
    if let Err(e) = store.record_activity(&prov).await {
        tracing::warn!(error = %e, title = %title, "platform ingest succeeded but the local provenance record failed");
    }
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

    // Resolve text-ingest locality once per run and say where document text
    // is processed BEFORE anything runs. Banner goes to stderr so `--json`
    // stdout stays parseable.
    let text_locality = if ingest_targets
        .iter()
        .any(|target| ingest_backend(target) == Some(IngestBackend::PlatformText))
    {
        let locality = resolve_text_ingest_locality(project_root, llm_url, model, api_key);
        // `auto` fell through to the platform only because no on-device model
        // was found — the user never asked to send their documents anywhere.
        // If the platform can't authenticate either, both doors are shut, so
        // say that once, up front. The old path announced "☁ CLOUD — sent to
        // the platform" (a claim about documents the user wanted kept local)
        // and then died on "Not logged in. Run `prism login`." — pointing a
        // no-account user at an account instead of at the local ingest that
        // was there all along.
        if locality == TextLocality::CloudNoLocalModel && resolve_agent_auth().is_err() {
            // Both doors look shut — so look before saying so. This runs
            // only on the failure path, so the probe costs a user with a
            // working setup nothing.
            bail!(
                "{}",
                no_ingest_backend_message(&local_llm::discover().await)
            );
        }
        if locality.is_local() {
            eprintln!("⚑ LOCAL — extracting on-device, nothing leaves your machine");
        } else {
            // Cloud ingest is two stages and the first one is LOCAL: document
            // text is extracted by the runtime on this machine, and only that
            // text is uploaded. Saying just "sent to the platform" made a
            // localhost failure in stage 1 impossible to place.
            eprintln!(
                "☁ CLOUD — step 1: text extracted on-device · step 2: that text sent to the platform"
            );
        }
        locality
    } else {
        TextLocality::Cloud
    };

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
                    schema_only,
                    mapping_path,
                )
                .await?
            }
            Some(IngestBackend::PlatformText) if text_locality.is_local() => {
                run_local_text_ingest_file(
                    &target,
                    project_root,
                    model,
                    llm_url,
                    api_key,
                    runtime_url,
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
                "Unsupported ingest format for {}. Supported: {}",
                target.display(),
                supported_ingest_formats()
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

KNOWLEDGE GRAPH (--platform queries the hosted graph; omit it to stay local):
  prism query --platform --semantic "creep resistant superalloy"     # semantic search
  prism query --platform "Inconel 718"                               # graph search
  prism query --platform --semantic "yield strength titanium" --json # JSON output for piping
  prism query --platform --json "fatigue life" | grep MAT            # grep for materials only
  prism status | grep nodes                                          # quick stats

COMPUTE:
  prism run <image> --backend local                    # run container locally
  prism run <image> --backend marc27                   # run on the hosted platform
  prism job-status <job-id>                            # check job status
  prism deploy create --name serve --image ghcr.io/example/mace:latest --target local
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
  --platform: route through the hosted API instead of the local graph

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

/// Default `--platform-url` for `prism run --backend marc27`. Kept as a const so
/// `handle_run` can tell an explicit override from the default and pick the
/// agent-resolved base otherwise.
const DEFAULT_RUN_PLATFORM_URL: &str = "https://api.marc27.com/api/v1";

/// Map the shared auth seam onto the compute crate's decoupled `Marc27Auth`.
fn marc27_auth_from(auth: PlatformAuth) -> prism_compute::Marc27Auth {
    match auth {
        PlatformAuth::ApiKey(key) => prism_compute::Marc27Auth::ApiKey(key),
        PlatformAuth::Bearer(token) => prism_compute::Marc27Auth::Bearer(token),
    }
}

/// Resolve platform auth for every CLI command that reaches MARC27. This is
/// the single CLI auth chokepoint: it accepts API-key-only users, stored
/// sessions, and legacy credentials, and never starts interactive auth.
fn resolve_agent_auth() -> Result<(String, PlatformAuth)> {
    // `offline::enabled()`, not a re-derived `== "1"`. This function is the
    // gate ~25 platform commands rely on, and it trimmed nothing — so
    // `PRISM_OFFLINE=" 1"` (a routine shell/CI artifact) was honoured by every
    // `offline::enabled()` caller and ignored here. It was not exploitable
    // through `prism` only because main.rs:1640 canonicalises the var first,
    // which is an accidental safety net: `prism-node` skipped that preamble
    // and was online under hard offline until f91917a2.
    if prism_runtime::offline::enabled() {
        anyhow::bail!(
            "offline mode: this command needs the hosted platform \
             (remove --offline to use it)"
        );
    }

    let default_api_base = PlatformVar::API_URL
        .get()
        .unwrap_or_else(|| "https://api.marc27.com/api/v1".to_string());
    let paths = PrismPaths::discover().ok();
    let resolved = auth::resolve_from_environment(paths.as_ref(), &default_api_base)?;
    Ok((resolved.api_base, resolved.credential))
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
            anyhow!("No active project selected; authenticate or set MARC27_PROJECT_ID.")
        })
}

/// Mint a durable node token (a node-scoped API key) and store it locally so
/// `node up` authenticates with a stable credential instead of the rotating
/// session token. The current session is used once to mint; the token itself
/// never rotates and is revocable via `node token revoke` (or the dashboard).
async fn handle_node_token_mint(paths: &PrismPaths, project: Option<&str>) -> Result<()> {
    let project_id = match project {
        Some(p) => p.to_string(),
        None => resolve_active_project_id(paths)?,
    };

    let (api_base, auth) = resolve_agent_auth()?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    // POST /api-keys with node scope + no expiry → a stable, revocable token.
    // (marc27-core's WS auth accepts API keys; the node scope marks it as a
    // node credential for display/revocation.)
    let created: serde_json::Value = auth
        .apply(client.post(format!("{api_base}/api-keys")))
        .json(&serde_json::json!({
            "project_id": project_id,
            "scopes": ["node"],
        }))
        .send()
        .await?
        .platform_error_for_status()
        .await?
        .json()
        .await?;

    let token = prism_runtime::StoredNodeToken {
        key: created["key"]
            .as_str()
            .context("platform did not return a key")?
            .to_string(),
        id: created["id"].as_str().unwrap_or("").to_string(),
        prefix: created["prefix"].as_str().unwrap_or("").to_string(),
    };

    paths.save_node_token(&token)?;

    println!("Minted durable node token (prefix: {}).", token.prefix);
    println!(
        "Stored at {}. `prism node up` will now use it and survive",
        paths.node_token_path().display()
    );
    println!("session token rotation. Revoke with `prism node token revoke`.");
    Ok(())
}

/// Revoke the stored durable node token: delete the platform key, then remove
/// the local file. Falls back to deleting just the local file if the platform
/// id is unknown or the call fails (so a leaked token is still revocable).
async fn handle_node_token_revoke(paths: &PrismPaths) -> Result<()> {
    let Some(token) = paths.load_node_token() else {
        println!("No durable node token stored.");
        return Ok(());
    };

    let revoked_on_platform = match resolve_agent_auth() {
        Ok((api_base, auth)) => {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()?;
            match auth
                .apply(client.delete(format!("{api_base}/api-keys/{}", token.id)))
                .send()
                .await
            {
                Ok(resp) => resp.status().is_success(),
                Err(e) => {
                    println!("Could not reach platform to revoke ({e}); removing local file only.");
                    false
                }
            }
        }
        Err(e) => {
            println!("Not authenticated to revoke on platform ({e}); removing local file only.");
            false
        }
    };

    let removed = paths.clear_node_token();
    if revoked_on_platform && removed {
        println!("Node token revoked on platform and removed locally.");
    } else if removed {
        println!("Removed local node-token file (platform revoke skipped/failed).");
    } else {
        println!("Local node-token file was already gone.");
    }
    Ok(())
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
        .platform_error_for_status()
        .await?
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
            .platform_error_for_status()
            .await?
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
                .platform_error_for_status()
                .await?
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

#[allow(clippy::too_many_arguments)]
async fn handle_deploy_and_invoke(
    name: &str,
    image: Option<&str>,
    resource_slug: Option<&str>,
    target: &str,
    gpu: Option<&str>,
    budget: Option<f64>,
    node_id: Option<&str>,
    env_vars: &[String],
    port: u16,
    health_path: &str,
    invoke_path: &str,
    input_json: &str,
    ready_timeout_secs: u64,
    keep: bool,
) -> Result<()> {
    let input: serde_json::Value = serde_json::from_str(input_json)
        .with_context(|| "--input is not valid JSON".to_string())?;

    let (api_base, auth) = resolve_agent_auth()?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?;

    let result = run_deploy_and_invoke(
        &client,
        &api_base,
        &auth,
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
        invoke_path,
        input,
        ready_timeout_secs,
        keep,
        Duration::from_secs(5),
    )
    .await?;

    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

/// Core of `deploy-and-invoke`: create a deployment (always fresh — no
/// reuse, unlike `predict`'s marketplace-slug shortcut), poll (bounded) for
/// `running` + an endpoint, POST the invoke payload, then ALWAYS attempt
/// to stop the deployment (unless `keep`) — even when the invoke itself
/// failed, so a failed call never leaks a billable deployment. Factored
/// out of `handle_deploy_and_invoke` so tests can drive it against a
/// mocked HTTP transport instead of `resolve_agent_auth()`'s real env
/// vars / on-disk credentials.
#[allow(clippy::too_many_arguments)]
async fn run_deploy_and_invoke(
    client: &reqwest::Client,
    api_base: &str,
    auth: &PlatformAuth,
    name: &str,
    image: Option<&str>,
    resource_slug: Option<&str>,
    target: &str,
    gpu: Option<&str>,
    budget: Option<f64>,
    node_id: Option<&str>,
    env_vars: &[String],
    port: u16,
    health_path: &str,
    invoke_path: &str,
    input: serde_json::Value,
    ready_timeout_secs: u64,
    keep: bool,
    poll_interval: Duration,
) -> Result<serde_json::Value> {
    let has_image = image.is_some();
    let has_resource = resource_slug.is_some();
    if has_image == has_resource {
        bail!("Specify exactly one of `--image` or `--resource-slug`.");
    }
    let normalized_target = normalize_deploy_target(target, node_id)?;

    let mut body = serde_json::Map::new();
    body.insert(
        "name".to_string(),
        serde_json::Value::String(name.to_string()),
    );
    body.insert(
        "target".to_string(),
        serde_json::Value::String(normalized_target.to_string()),
    );
    if let Some(gpu) = gpu {
        body.insert(
            "gpu_type".to_string(),
            serde_json::Value::String(gpu.to_string()),
        );
    }
    body.insert(
        "deploy_config".to_string(),
        serde_json::json!({ "port": port, "health_path": health_path }),
    );
    if let Some(image) = image {
        body.insert(
            "image".to_string(),
            serde_json::Value::String(image.to_string()),
        );
    }
    if let Some(resource_slug) = resource_slug {
        body.insert(
            "resource_slug".to_string(),
            serde_json::Value::String(resource_slug.to_string()),
        );
    }
    if let Some(budget) = budget {
        body.insert("budget_max_usd".to_string(), serde_json::json!(budget));
    }
    if let Some(node_id) = node_id {
        body.insert(
            "node_id".to_string(),
            serde_json::Value::String(node_id.to_string()),
        );
    }
    if !env_vars.is_empty() {
        body.insert(
            "env_vars".to_string(),
            serde_json::Value::Object(parse_string_map_arg(env_vars, "--env")?),
        );
    }

    let created: serde_json::Value = auth
        .apply(client.post(format!("{api_base}/compute/deployments")))
        .json(&serde_json::Value::Object(body))
        .send()
        .await?
        .platform_error_for_status()
        .await?
        .json()
        .await?;
    let deployment_id = created["id"]
        .as_str()
        .or_else(|| created["deployment_id"].as_str())
        .context("deployment create response has no id")?
        .to_string();

    // Poll (bounded) for running + endpoint. Failed/stopped is a terminal
    // honest error, not a wait-forever — mirrors `predict`'s wait loop.
    let deadline = std::time::Instant::now() + Duration::from_secs(ready_timeout_secs);
    let endpoint_url = loop {
        tokio::time::sleep(poll_interval).await;
        let status: serde_json::Value = auth
            .apply(client.get(format!("{api_base}/compute/deployments/{deployment_id}")))
            .send()
            .await?
            .platform_error_for_status()
            .await?
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
                    "deployment {deployment_id} for '{name}' ended in state '{state}' \
                     before serving — check `prism deploy status {deployment_id}`"
                );
            }
            _ => {}
        }
        if std::time::Instant::now() > deadline {
            bail!(
                "deployment {deployment_id} for '{name}' not ready after \
                 {ready_timeout_secs}s (last state '{state}'); it is still \
                 provisioning — poll `prism deploy status {deployment_id}` or \
                 re-run with a larger --ready-timeout-secs"
            );
        }
    };

    // Invoke — captured as a Result (not `?`'d away) so a failed call
    // still reaches the auto-stop step below.
    let invoke_url = format!("{}{}", endpoint_url.trim_end_matches('/'), invoke_path);
    let send_result = client
        .post(&invoke_url)
        .json(&input)
        .timeout(Duration::from_secs(600))
        .send()
        .await;
    let invoke_result: Result<serde_json::Value> = match send_result {
        Ok(resp) => match resp.error_for_status() {
            Ok(resp) => resp
                .json::<serde_json::Value>()
                .await
                .context("invoke response was not valid JSON"),
            Err(e) => Err(e).context("invoke request returned an error status"),
        },
        Err(e) => Err(e).with_context(|| format!("invoke request to {invoke_url} failed")),
    };

    // Auto-stop what WE created (no silent per-minute billing) unless
    // --keep — runs REGARDLESS of invoke outcome (HARD requirement: never
    // leak a billable deployment because the invoke call itself failed).
    let auto_stopped = if !keep {
        auth.apply(client.delete(format!("{api_base}/compute/deployments/{deployment_id}")))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    } else {
        false
    };

    match invoke_result {
        Ok(result) => Ok(serde_json::json!({
            "deployment_id": deployment_id,
            "endpoint_url": endpoint_url,
            "auto_stopped": auto_stopped,
            "kept_running": !auto_stopped,
            "result": result,
        })),
        Err(e) => Err(anyhow!(
            "deploy-and-invoke: invoke failed for deployment {deployment_id} \
             (auto_stopped={auto_stopped}, kept_running={}): {e}",
            !auto_stopped
        )),
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
                .platform_error_for_status()
                .await?
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
            let response: serde_json::Value = request
                .send()
                .await?
                .platform_error_for_status()
                .await?
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
                .platform_error_for_status()
                .await?
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
                .platform_error_for_status()
                .await?
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
                .platform_error_for_status()
                .await?
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
                .platform_error_for_status()
                .await?
                .json()
                .await?
        }
        ComputeCommands::Providers => {
            auth.apply(client.get(format!("{api_base}/compute/providers")))
                .send()
                .await?
                .platform_error_for_status()
                .await?
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
                .platform_error_for_status()
                .await?
                .json()
                .await?
        }
        ComputeCommands::Status { job_id } => {
            auth.apply(client.get(format!("{api_base}/compute/{job_id}")))
                .send()
                .await?
                .platform_error_for_status()
                .await?
                .json()
                .await?
        }
        ComputeCommands::Cancel { job_id } => {
            auth.apply(client.post(format!("{api_base}/compute/{job_id}/cancel")))
                .send()
                .await?
                .platform_error_for_status()
                .await?;
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
                .platform_error_for_status()
                .await?
                .json()
                .await?
        }
    };

    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn handle_compute_run(
    image: &str,
    inputs_json: &str,
    gpu: Option<&str>,
    budget: Option<f64>,
    provider: Option<&str>,
    timeout: Option<u64>,
    env: &[String],
    poll_timeout_secs: u64,
) -> Result<()> {
    let inputs: serde_json::Value = serde_json::from_str(inputs_json)
        .map_err(|e| anyhow!("--inputs must be valid JSON: {e}"))?;

    let (api_base, auth) = resolve_agent_auth()?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    let result = run_compute_job(
        &client,
        &api_base,
        &auth,
        image,
        inputs,
        gpu,
        budget,
        provider,
        timeout,
        env,
        poll_timeout_secs,
        Duration::from_secs(5),
    )
    .await?;

    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

/// Core of `compute-run`: POST `{api_base}/compute/submit`, then poll
/// `GET {api_base}/compute/{job_id}` (the single wired status endpoint —
/// see the doc comment on `ComputeCommands::Status`) until it reports a
/// terminal state, returning the real result embedded in that same
/// response. A failed/cancelled job is a real `Err`, never a fabricated
/// success. Factored out of `handle_compute_run` so tests can drive it
/// against a mocked HTTP transport.
#[allow(clippy::too_many_arguments)]
async fn run_compute_job(
    client: &reqwest::Client,
    api_base: &str,
    auth: &PlatformAuth,
    image: &str,
    inputs: serde_json::Value,
    gpu: Option<&str>,
    budget: Option<f64>,
    provider: Option<&str>,
    timeout: Option<u64>,
    env: &[String],
    poll_timeout_secs: u64,
    poll_interval: Duration,
) -> Result<serde_json::Value> {
    let mut body = serde_json::Map::new();
    body.insert(
        "image".to_string(),
        serde_json::Value::String(image.to_string()),
    );
    body.insert("inputs".to_string(), inputs);
    if let Some(gpu) = gpu {
        body.insert(
            "gpu_type".to_string(),
            serde_json::Value::String(gpu.to_string()),
        );
    }
    if let Some(budget) = budget {
        body.insert("budget_max_usd".to_string(), serde_json::json!(budget));
    }
    if let Some(provider) = provider {
        body.insert(
            "provider_preference".to_string(),
            serde_json::Value::String(provider.to_string()),
        );
    }
    if let Some(timeout) = timeout {
        body.insert("timeout_seconds".to_string(), serde_json::json!(timeout));
    }
    if !env.is_empty() {
        body.insert(
            "env_vars".to_string(),
            serde_json::Value::Object(parse_string_map_arg(env, "--env")?),
        );
    }

    let submitted: serde_json::Value = auth
        .apply(client.post(format!("{api_base}/compute/submit")))
        .json(&serde_json::Value::Object(body))
        .send()
        .await?
        .platform_error_for_status()
        .await?
        .json()
        .await?;
    let job_id = value_string(&submitted, &["job_id", "id"])
        .context("compute submit response has no job_id")?
        .to_string();

    let deadline = std::time::Instant::now() + Duration::from_secs(poll_timeout_secs);
    loop {
        tokio::time::sleep(poll_interval).await;
        let status: serde_json::Value = auth
            .apply(client.get(format!("{api_base}/compute/{job_id}")))
            .send()
            .await?
            .platform_error_for_status()
            .await?
            .json()
            .await?;
        let state = value_string(&status, &["status", "state"]).unwrap_or("unknown");
        match state {
            "completed" | "succeeded" | "done" => {
                let result = status
                    .get("result")
                    .or_else(|| status.get("output"))
                    .or_else(|| status.get("results"))
                    .cloned()
                    .unwrap_or_else(|| status.clone());
                return Ok(serde_json::json!({
                    "job_id": job_id,
                    "status": state,
                    "result": result,
                }));
            }
            "failed" | "error" | "cancelled" | "canceled" => {
                let err = value_string(&status, &["error", "error_message"])
                    .unwrap_or("(no error detail)");
                bail!("compute job {job_id} {state}: {err}");
            }
            _ => {
                if std::time::Instant::now() >= deadline {
                    bail!(
                        "compute job {job_id} still '{state}' after {poll_timeout_secs}s; \
                         check later with `prism compute status {job_id}`"
                    );
                }
            }
        }
    }
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
                .platform_error_for_status()
                .await?
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
                .platform_error_for_status()
                .await?
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
                .platform_error_for_status()
                .await?
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
                .platform_error_for_status()
                .await?
                .json()
                .await?
        }
    };

    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

async fn handle_ingest_and_wait(
    url: Option<&str>,
    query: Option<&str>,
    mode: &str,
    poll_timeout_secs: u64,
) -> Result<()> {
    let (api_base, auth) = resolve_agent_auth()?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    let result = run_ingest_job(
        &client,
        &api_base,
        &auth,
        url,
        query,
        mode,
        poll_timeout_secs,
        Duration::from_secs(5),
    )
    .await?;

    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

/// Core of `ingest-and-wait`: POST `{api_base}/knowledge/ingest-job` to
/// submit, then poll `GET {api_base}/knowledge/ingest-jobs` — the ONLY
/// wired job-status endpoint in this codebase; there is no per-job GET,
/// so this matches the submitted job id inside the list response — until
/// the matching entry reports a terminal state, returning its graph
/// references. A failed job is a real `Err`, never a fabricated success.
/// Factored out of `handle_ingest_and_wait` so tests can drive it against
/// a mocked HTTP transport.
#[allow(clippy::too_many_arguments)]
async fn run_ingest_job(
    client: &reqwest::Client,
    api_base: &str,
    auth: &PlatformAuth,
    url: Option<&str>,
    query: Option<&str>,
    mode: &str,
    poll_timeout_secs: u64,
    poll_interval: Duration,
) -> Result<serde_json::Value> {
    let source = match (url, query) {
        (Some(url), _) => serde_json::json!({ "type": "url", "url": url }),
        (None, Some(query)) => serde_json::json!({ "type": "query", "query": query }),
        (None, None) => bail!("`ingest-and-wait` requires --url or --query"),
    };
    let body = serde_json::json!({ "mode": mode, "source": source });

    let submitted: serde_json::Value = auth
        .apply(client.post(format!("{api_base}/knowledge/ingest-job")))
        .json(&body)
        .send()
        .await?
        .platform_error_for_status()
        .await?
        .json()
        .await?;
    let job_id = value_string(&submitted, &["job_id", "id"])
        .context("ingest-job submit response has no job_id")?
        .to_string();

    let deadline = std::time::Instant::now() + Duration::from_secs(poll_timeout_secs);
    loop {
        tokio::time::sleep(poll_interval).await;
        let jobs: serde_json::Value = auth
            .apply(client.get(format!("{api_base}/knowledge/ingest-jobs")))
            .send()
            .await?
            .platform_error_for_status()
            .await?
            .json()
            .await?;
        let job_entry = value_array(&jobs, &["jobs", "items", "data"]).and_then(|list| {
            list.iter()
                .find(|j| value_string(j, &["job_id", "id"]) == Some(job_id.as_str()))
        });

        let Some(job) = job_entry else {
            if std::time::Instant::now() >= deadline {
                bail!(
                    "ingest job {job_id} not found in `/knowledge/ingest-jobs` after \
                     {poll_timeout_secs}s; check later with `prism ingest --status`"
                );
            }
            continue;
        };

        let state = value_string(job, &["status", "state"]).unwrap_or("unknown");
        match state {
            "completed" | "succeeded" | "done" => {
                let graph_refs = job
                    .get("graph_refs")
                    .or_else(|| job.get("result"))
                    .or_else(|| job.get("entities"))
                    .cloned()
                    .unwrap_or_else(|| job.clone());
                return Ok(serde_json::json!({
                    "job_id": job_id,
                    "status": state,
                    "graph_refs": graph_refs,
                }));
            }
            "failed" | "error" | "cancelled" | "canceled" => {
                let err =
                    value_string(job, &["error", "error_message"]).unwrap_or("(no error detail)");
                bail!("ingest job {job_id} {state}: {err}");
            }
            _ => {
                if std::time::Instant::now() >= deadline {
                    bail!(
                        "ingest job {job_id} still '{state}' after {poll_timeout_secs}s; \
                         check later with `prism ingest --status`"
                    );
                }
            }
        }
    }
}

/// Fetch the live platform catalog and persist it to the local cache
/// (`~/.prism/model-catalog.json`) so lookups keep working offline.
async fn fetch_platform_catalog_live(paths: &PrismPaths) -> Result<Vec<serde_json::Value>> {
    let (api_base, auth) = resolve_agent_auth()?;
    let project_id = resolve_active_project_id(paths)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    let response: serde_json::Value = auth
        .apply(client.get(format!("{api_base}/projects/{project_id}/llm/models")))
        .send()
        .await?
        .platform_error_for_status()
        .await?
        .json()
        .await?;

    let models = value_array(&response, &["models", "items", "data"])
        .cloned()
        .unwrap_or_default();
    if let Err(err) = prism_agent::models::save_catalog_cache(&models) {
        eprintln!("[prism] warning: could not write model catalog cache: {err:#}");
    }
    Ok(models)
}

/// User-registered models first (they override the catalog), then the
/// platform catalog, deduped by id, ranked newest-first.
fn merge_and_rank_models(catalog: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
    let mut models = prism_agent::models::user_models_as_catalog_json();
    models.extend(catalog);
    let mut seen = std::collections::HashSet::new();
    models.retain(|m| match value_string(m, &["model_id", "id"]) {
        Some(id) => seen.insert(id.to_string()),
        None => false,
    });
    models.sort_by(|a, b| {
        let key = |m: &serde_json::Value| {
            prism_agent::models::recency_key(value_string(m, &["model_id", "id"]).unwrap_or(""))
        };
        key(b).cmp(&key(a)).then_with(|| {
            value_string(a, &["model_id", "id"]).cmp(&value_string(b, &["model_id", "id"]))
        })
    });
    models
}

async fn handle_models_command(paths: &PrismPaths, command: ModelsCommands) -> Result<()> {
    // Register is purely local: no auth, no network.
    let command = match command {
        ModelsCommands::Register {
            model_id,
            provider,
            base_url,
            api_key_env,
            input_price,
            output_price,
            context_window,
            max_output_tokens,
            json,
        } => {
            let entry = prism_agent::models::UserModel {
                provider,
                base_url,
                api_key_env,
                input_price,
                output_price,
                context_window,
                max_output_tokens,
                supports_tools: None,
                supports_thinking: None,
                supports_caching: None,
            };
            let path = prism_agent::models::register_user_model(&model_id, entry.clone())?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "registered": model_id,
                        "path": path,
                        "model": entry,
                    }))?
                );
            } else {
                println!("Registered model: {model_id} [{}]", entry.provider);
                println!(
                    "  ctx={}  in=${:.2}/M  out=${:.2}/M",
                    entry.context_window, entry.input_price, entry.output_price
                );
                if let Some(url) = &entry.base_url {
                    println!("  endpoint: {url}");
                }
                if let Some(env) = &entry.api_key_env {
                    println!("  api key env: {env}");
                }
                println!("  saved to {}", path.display());
                println!("  Switch with `/model {model_id}` in the TUI.");
            }
            return Ok(());
        }
        other => other,
    };

    // Live catalog first; if the platform is unreachable, fall back to the
    // local cache so `prism models` keeps working offline. Provenance
    // (source + age) is carried through so the TUI can be honest about a
    // stale list instead of silently showing cached data as live.
    let (catalog, source, age_secs) = match fetch_platform_catalog_live(paths).await {
        Ok(models) => (models, "live", None),
        Err(err) => match prism_agent::models::load_catalog_cache() {
            Some(cache) => {
                let age = cache.age().as_secs();
                eprintln!(
                    "[prism] platform catalog unreachable ({err:#}); using cached catalog \
                     ({} models, {}m old)",
                    cache.models.len(),
                    age / 60
                );
                (cache.models, "cache", Some(age))
            }
            None => {
                return Err(err.context(
                    "platform catalog unreachable and no local cache yet — run once while \
                     online, or add models with `prism models register`",
                ));
            }
        },
    };
    let mut models = merge_and_rank_models(catalog);

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
                // Object (not bare array) so the TUI picker can render the
                // list's provenance; the protocol parser reads `models`.
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "models": models,
                        "source": source,
                        "age_secs": age_secs,
                    }))?
                );
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
        // Handled by the early-return above; the rebind can't carry it here.
        ModelsCommands::Register { .. } => unreachable!("register returns early"),
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
    let (api_base, auth) = resolve_agent_auth()?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let response = auth
        .apply(client.get(format!("{api_base}/compute/gpus")))
        .send()
        .await?;
    let value = response.platform_error_for_status().await?.json().await?;
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
        UseCommands::List => use_command::UseAction::List,
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
                .platform_error_for_status()
                .await?
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
            let response: serde_json::Value = raw.platform_error_for_status().await?.json().await?;

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
                .platform_error_for_status()
                .await?
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
                .platform_error_for_status()
                .await?
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
                .platform_error_for_status()
                .await?
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
                .platform_error_for_status()
                .await?
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

/// Chunk-safe Server-Sent-Events decoder for the platform discourse
/// (mirofish) event stream.
///
/// Two things make a naive "one JSON object per `data:` line" parser wrong
/// against the live platform:
///
/// 1. **Multi-line events.** Per the SSE spec, an event's `data` field can
///    span several consecutive `data:` lines — the values are joined with
///    `\n` and the event only ends at the next *blank* line. Treating every
///    `data:` line as an independent event breaks as soon as a payload (or
///    the framing around it) spans more than one line.
/// 2. **Double `data:` encoding.** The mirofish discourse engine pre-formats
///    each message as the literal string `"data: {json}\n\n"` *before*
///    handing it to axum's `Sse` response writer, which then SSE-encodes
///    that string *again* — splitting it on its embedded newlines and
///    re-prefixing every resulting line with `data:`. The wire bytes for one
///    logical event end up looking like:
///    ```text
///    data:data: {"step":"started"}
///    data:
///    data:
///
///    ```
///    A spec-correct multi-line join (rule 1) reconstructs the original
///    `"data: {json}\n\n"` string; this decoder then strips the surviving
///    inner `data:` prefix before parsing JSON.
///
/// The decoder is fed via [`SseDecoder::push_chunk`], which may be called
/// once per network read — a line, or an event's blank-line terminator, may
/// legally straddle two chunks, and partial state is buffered across calls.
#[derive(Default)]
struct SseDecoder {
    /// Bytes received but not yet resolved into a complete `\n`-terminated line.
    line_buf: String,
    /// `data:` field values accumulated for the event currently in progress.
    data_lines: Vec<String>,
}

impl SseDecoder {
    /// Feed one chunk of the response body (as much or as little as one
    /// network read happened to deliver) and return every event it
    /// completes. Incomplete lines/events are buffered for the next call.
    fn push_chunk(&mut self, chunk: &str) -> Vec<serde_json::Value> {
        self.line_buf.push_str(chunk);
        let mut out = Vec::new();
        while let Some(pos) = self.line_buf.find('\n') {
            let line = self.line_buf[..pos].trim_end_matches('\r').to_string();
            self.line_buf.drain(..=pos);
            self.feed_line(&line, &mut out);
        }
        out
    }

    /// Flush any state left after the stream has closed: a final line with
    /// no trailing newline, and/or an event with no trailing blank line.
    fn finish(&mut self) -> Vec<serde_json::Value> {
        let mut out = Vec::new();
        if !self.line_buf.is_empty() {
            let line = std::mem::take(&mut self.line_buf);
            self.feed_line(line.trim_end_matches('\r'), &mut out);
        }
        if let Some(value) = self.finish_event() {
            out.push(value);
        }
        out
    }

    fn feed_line(&mut self, line: &str, out: &mut Vec<serde_json::Value>) {
        if line.is_empty() {
            // Blank line: the SSE event boundary.
            if let Some(value) = self.finish_event() {
                out.push(value);
            }
            return;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            self.data_lines
                .push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
        }
        // Other fields (`event:`, `id:`, `retry:`) and `:`-comment
        // keep-alives carry no payload we need here — ignored.
    }

    fn finish_event(&mut self) -> Option<serde_json::Value> {
        if self.data_lines.is_empty() {
            return None;
        }
        let mut payload = self.data_lines.join("\n");
        self.data_lines.clear();

        // Undo the double `data:` encoding described above. Bounded so a
        // malformed payload that merely starts with the literal text
        // "data:" can never loop.
        for _ in 0..4 {
            let Some(rest) = payload.trim().strip_prefix("data:") else {
                break;
            };
            payload = rest.strip_prefix(' ').unwrap_or(rest).to_string();
        }

        let payload = payload.trim();
        if payload.is_empty() || payload == "[DONE]" {
            return None;
        }
        // Malformed/non-JSON keep-alive payloads are skipped rather than
        // failing the whole stream, matching real SSE keep-alive traffic.
        serde_json::from_str::<serde_json::Value>(payload).ok()
    }
}

/// Parse a Server-Sent-Events stream body into JSON event objects, tolerant
/// of multi-line events and the mirofish double `data:` encoding bug — see
/// [`SseDecoder`].
///
/// If the body carries no SSE `data:` events at all, it is treated as a plain
/// JSON document (array → its elements, object → a single event) so a
/// non-streamed error/response body still surfaces to the caller instead of
/// silently vanishing.
fn parse_sse_json_events(body: &str) -> Result<Vec<serde_json::Value>> {
    let mut decoder = SseDecoder::default();
    let mut events = decoder.push_chunk(body);
    events.extend(decoder.finish());

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
        .ok_or_else(|| anyhow::anyhow!("Not authenticated."))?;
    let user_id = creds.user_id.as_deref().ok_or_else(|| {
        anyhow::anyhow!("Stored credentials are missing user_id; re-authentication required.")
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

    create_dashboard_session_for_user_with_platform_token(
        dashboard_url,
        user_id,
        creds.display_name.as_deref(),
        Some(creds.access_token.as_str()),
    )
    .await
}

async fn create_dashboard_session_for_user(
    dashboard_url: &str,
    user_id: &str,
    display_name: Option<&str>,
) -> Result<String> {
    create_dashboard_session_for_user_with_platform_token(
        dashboard_url,
        user_id,
        display_name,
        None,
    )
    .await
}

/// Decide whether the platform credential may be sent to this dashboard.
///
/// Extracted so the RULE is testable. Asserting it through
/// `create_dashboard_session_*` cannot work: with nothing listening the
/// request fails before anything is transmitted, so such a test passes whether
/// the token was withheld or not — it proves only that the error text is
/// clean. This function is the decision itself.
fn platform_token_for<'a>(dashboard_url: &str, token: Option<&'a str>) -> Option<&'a str> {
    match token {
        Some(_) if !prism_runtime::offline::is_loopback_url(dashboard_url) => {
            eprintln!(
                "note: not sending your platform credential to {dashboard_url} \
                 — it is not a loopback address. The session is created without \
                 platform access."
            );
            None
        }
        other => other,
    }
}

/// Create a dashboard session, optionally handing the dashboard a platform
/// token so it can call the platform on the user's behalf.
///
/// `platform_token` is only ever sent to a LOOPBACK dashboard.
///
/// `--dashboard-url` documents itself as "Dashboard URL of the running node"
/// and defaults to `http://127.0.0.1:7327` — it exists to change YOUR node's
/// port, not to name a third party. But it is a free-form string on
/// `mesh publish/subscribe/unsubscribe` and on `query --federated`, and the
/// agent tool schemas for those (agent/src/command_tools.rs) expose it to the
/// model. A prompt injection setting
/// `--dashboard-url https://attacker.example` therefore POSTed the user's live
/// platform access token, read from `~/.prism/credentials.json`, to a host of
/// the attacker's choosing. `PermissionMode::LocalOnly`'s env-stripping does
/// not help: the credential comes off disk, not out of the environment.
///
/// Withholding it is a supported degradation rather than a new failure mode —
/// `create_dashboard_session_for_user` already calls this with `None`, so a
/// tokenless session is an existing, working shape. The session is still
/// created; only the platform capability is withheld, and the caller is told.
async fn create_dashboard_session_for_user_with_platform_token(
    dashboard_url: &str,
    user_id: &str,
    display_name: Option<&str>,
    platform_token: Option<&str>,
) -> Result<String> {
    // Hard offline: the dashboard may be remote, so this is a network call
    // like any other. Loopback stays permitted, matching llm/embed/workflows.
    prism_runtime::offline::check_url(dashboard_url).map_err(|reason| anyhow!(reason))?;

    let platform_token = platform_token_for(dashboard_url, platform_token);

    let url = format!("{dashboard_url}/api/sessions");
    // Never follow a redirect on this call.
    //
    // `reqwest`'s default is `Policy::limited(10)`, and its cross-host
    // scrubbing (`redirect.rs:244-247`) removes AUTHORIZATION / COOKIE /
    // PROXY_AUTHORIZATION — HEADERS only. The platform token here is in the
    // BODY, which that never touches, and on 307/308 the method and body are
    // preserved and resent. Both guards above run on `dashboard_url`, the
    // INITIAL url; a redirect target is never re-checked. So a process
    // answering the loopback dashboard port could 307 the credential off-box.
    //
    // A session mint has no legitimate reason to be redirected, so refusing
    // outright is both simpler and stricter than re-validating per hop.
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("failed to build the dashboard HTTP client")?;
    let resp = client
        .post(&url)
        .json(&serde_json::json!({
            "user_id": user_id,
            "display_name": display_name,
            "platform_token": platform_token,
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

/// Open the shared provenance store (`~/.prism/provenance.db`) so a campaign
/// loop persists every progress transition to Turso. Failure degrades to
/// checkpoint-only persistence with a loud warning — a locked or corrupt
/// store must not brick a discovery run.
async fn open_campaign_provenance() -> Option<prism_provenance::ProvenanceStore> {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let db_path = PathBuf::from(home).join(".prism").join("provenance.db");
    if let Some(parent) = db_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match prism_provenance::ProvenanceStore::open(&db_path).await {
        Ok(store) => Some(store),
        Err(e) => {
            tracing::warn!(
                error = %e,
                db = %db_path.display(),
                "campaign provenance store unavailable — transitions will only land in the checkpoint"
            );
            None
        }
    }
}

/// VS3: a human-facing inspection of the local provenance ledger. `stats`
/// prints the ok/error/other breakdown (the failure rate is otherwise
/// invisible); `failures` lists the actual failed runs. Local-only: opens
/// `~/.prism/provenance.db`, no network. If the store can't be opened we say so
/// plainly and exit non-zero rather than printing an empty/in misleading result.
async fn handle_provenance_command(command: ProvenanceCommands) -> anyhow::Result<()> {
    let Some(store) = open_campaign_provenance().await else {
        eprintln!("provenance store unavailable (could not open ~/.prism/provenance.db)");
        anyhow::bail!("provenance store unavailable");
    };
    match command {
        ProvenanceCommands::Stats => {
            let s = store.stats().await?;
            // Pretty JSON so it's both human-skimmable and machine-parsable.
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "total_records": s.total_records,
                    "ok_records": s.ok_records,
                    "error_records": s.error_records,
                    "other_records": s.other_records,
                    "note": "error_records counts tool runs recorded as status='error'; \
                             use `prism provenance failures` to list them"
                }))?
            );
        }
        ProvenanceCommands::Failures { session_id, limit } => {
            let failures = store.query_failures(session_id.as_deref(), limit).await?;
            let entries: Vec<serde_json::Value> = failures
                .into_iter()
                .map(|rec| {
                    let error = rec
                        .output_json
                        .as_ref()
                        .and_then(|o| o.get("error"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                        .or_else(|| {
                            rec.output_json
                                .as_ref()
                                .and_then(|o| o.get("stderr"))
                                .and_then(serde_json::Value::as_str)
                                .and_then(|s| s.lines().find(|l| !l.trim().is_empty()))
                                .map(str::to_string)
                        });
                    serde_json::json!({
                        "id": rec.id,
                        "session_id": rec.session_id,
                        "tool_name": rec.tool_name,
                        "exit_code": rec.exit_code,
                        "error": error,
                        "timestamp": rec.timestamp,
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "count": entries.len(),
                    "failures": entries,
                }))?
            );
        }
    }
    Ok(())
}

fn batch_task_id() -> Result<String> {
    let task_id = std::env::var("PRISM_TASK_ID")
        .context("PRISM_TASK_ID is required for the campaign batch entrypoint")?;
    if task_id.is_empty()
        || task_id.len() > 128
        || !task_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        || matches!(task_id.as_str(), "." | "..")
    {
        bail!(
            "invalid PRISM_TASK_ID {task_id:?}: expected 1-128 ASCII letters, digits, '.', '-', or '_'"
        );
    }
    Ok(task_id)
}

/// Run the PRISM-owned campaign entrypoint. `Ok(true)` means SIGUSR1 was
/// received and the existing atomic campaign checkpoint was published, so
/// the caller must exit 140 to request requeue. Arbitrary image entrypoints
/// remain governed by the separate BYOC contract in `prism-compute`.
#[cfg(unix)]
async fn run_batch_campaign_entrypoint(project_root: &Path) -> Result<bool> {
    use prism_campaign::{Campaign, CampaignConfig, CampaignGoal, GoalStatus};
    use tokio::signal::unix::{SignalKind, signal};

    // Register before loading or creating campaign state. Once campaign work
    // starts, USR1 is queued for this stream instead of taking the default
    // process-termination path.
    let mut usr1 = signal(SignalKind::user_defined1()).context("failed to install SIGUSR1 trap")?;
    let task_id = batch_task_id()?;
    let checkpoint_dir = prism_campaign::schedule::campaigns_dir();
    let checkpoint_path = checkpoint_dir.join(format!("{task_id}.json"));

    let mut campaign = if checkpoint_path.is_file() {
        Campaign::from_checkpoint(&checkpoint_path)?
    } else {
        let inputs_json = std::env::var("PRISM_INPUTS")
            .context("PRISM_INPUTS is required to start a batch campaign")?;
        let inputs: BatchCampaignInputs = serde_json::from_str(&inputs_json)
            .context("PRISM_INPUTS is not a valid PRISM campaign specification")?;
        let mut config = CampaignConfig::default();
        if let Some(max_iterations) = inputs.max_iterations {
            config.max_iterations = max_iterations;
        }
        if let Some(batch_size) = inputs.batch_size {
            config.batch_size = batch_size;
        }
        config.budget_usd = inputs.budget;
        if let Some(checkpoint_every) = inputs.checkpoint_every {
            config.checkpoint_every = checkpoint_every;
        }
        config.approval_gate_at = inputs.approval_gates;
        config.checkpoint_dir = Some(checkpoint_dir);
        config.project_root = Some(project_root.to_path_buf());
        Campaign::new(
            CampaignGoal {
                description: inputs.goal,
                elements: inputs.elements.into_vec(),
                objective: inputs.objective,
                constraints: inputs.constraints.into_vec(),
                seeds: inputs.seeds.into_vec(),
            },
            config,
            task_id.clone(),
        )
    };

    match campaign.state().status {
        GoalStatus::Completed => {
            println!(
                "Campaign '{task_id}' already completed: {}",
                campaign.state().completion_reason
            );
            return Ok(false);
        }
        GoalStatus::Paused => {
            println!(
                "Campaign '{task_id}' is paused at an approval gate; scheduler requeue cannot approve it"
            );
            return Ok(false);
        }
        _ => {}
    }

    let _worker_lock = acquire_worker_lock(&task_id)?;
    if let Some(store) = open_campaign_provenance().await {
        campaign = campaign.with_provenance(store);
    }
    let run_result = {
        let run = campaign.run();
        tokio::pin!(run);
        tokio::select! {
            result = &mut run => Some(result),
            received = usr1.recv() => {
                if received.is_none() {
                    bail!("SIGUSR1 trap closed before the campaign finished");
                }
                None
            }
        }
    };

    match run_result {
        Some(result) => {
            println!("\n{}", result?.summary);
            Ok(false)
        }
        None => {
            campaign
                .checkpoint()
                .context("SIGUSR1 received but campaign checkpoint failed; refusing requeue")?;
            eprintln!(
                "Campaign '{task_id}' checkpointed after SIGUSR1; requesting scheduler requeue"
            );
            Ok(true)
        }
    }
}

#[cfg(not(unix))]
async fn run_batch_campaign_entrypoint(_project_root: &Path) -> Result<bool> {
    bail!("the campaign batch entrypoint requires Unix signal support")
}

/// Take the goal's worker lock for the life of this process, refusing to
/// start if another worker already holds it. The lock — not a pid file the
/// OS might recycle out from under us — is what tells the scheduler whether
/// this goal is being worked on; the kernel releases it even on SIGKILL.
fn acquire_worker_lock(goal_id: &str) -> Result<prism_campaign::schedule::WorkerLock> {
    match prism_campaign::schedule::WorkerLock::acquire(goal_id) {
        Ok(Some(lock)) => Ok(lock),
        Ok(None) => anyhow::bail!(
            "goal '{goal_id}' already has a worker running — refusing to start a second one \
             (that would double its spend). Wait for it, or stop it first."
        ),
        Err(e) => Err(e.context(format!(
            "could not take the worker lock for goal '{goal_id}'"
        ))),
    }
}

/// `prism schedule …` — the durable wake-up surface for long-running goals.
///
/// The registry lives in pod-local embedded libSQL; the heartbeat lives in
/// whatever already supervises processes on this host. See
/// `prism_campaign::schedule` for why the ticking is delegated rather than
/// run in-process.
async fn handle_schedule_command(command: ScheduleCommands) -> Result<()> {
    use prism_campaign::schedule::{
        Decision, ScheduleStore, Trigger, WorkerResumer, launchd_plist, parse_duration,
        systemd_units, tick_once,
    };

    let store = ScheduleStore::open_default().await?;
    let resumer = WorkerResumer {
        exe: std::env::current_exe().context("failed to locate the prism executable")?,
    };

    match command {
        ScheduleCommands::Create(args) => {
            let ScheduleCreateArgs {
                goal,
                every,
                cron,
                at,
                watch_file,
                watch_goal,
                watch_goal_status,
                watch_corpus,
                corpus_at_least,
                corpus_tenant,
                max_fires,
                max_no_progress,
            } = *args;
            let trigger = if let Some(spec) = every {
                Trigger::Every {
                    seconds: parse_duration(&spec)?,
                }
            } else if let Some(expr) = cron {
                Trigger::Cron { expr }
            } else if let Some(unix_secs) = at {
                Trigger::At { unix_secs }
            } else if let Some(path) = watch_file {
                Trigger::WatchFile { path }
            } else if let Some(goal_id) = watch_goal {
                Trigger::WatchGoal {
                    goal_id,
                    status: watch_goal_status,
                }
            } else if let Some(db) = watch_corpus {
                let at_least = corpus_at_least.ok_or_else(|| {
                    anyhow::anyhow!("--watch-corpus needs --corpus-at-least <entity count>")
                })?;
                Trigger::WatchCorpus {
                    db,
                    at_least,
                    tenant: corpus_tenant,
                }
            } else {
                anyhow::bail!(
                    "a schedule needs a trigger: --every, --cron, --at, --watch-file, \
                     --watch-goal, or --watch-corpus"
                );
            };
            // Refuse to schedule a goal that does not exist — an id typo
            // would otherwise create a schedule that wedges on its first tick.
            let snapshot =
                <WorkerResumer as prism_campaign::schedule::GoalResumer>::snapshot(&resumer, &goal)
                    .with_context(|| format!("cannot schedule goal '{goal}'"))?;
            let sched = store
                .create(&goal, trigger, max_fires, max_no_progress)
                .await?;
            println!("Schedule created: {}", sched.id);
            println!(
                "  goal:      {} ({})",
                sched.goal_id,
                snapshot.status.as_str()
            );
            println!("  trigger:   {}", serde_json::to_string(&sched.trigger)?);
            if let Some(due) = sched.next_due_at {
                println!("  next due:  {due} (unix)");
            } else {
                println!("  next due:  on the watched condition becoming true");
            }
            println!(
                "  ceilings:  {max_fires} wake-ups, wedged after {max_no_progress} with no progress"
            );
            // Say plainly whether anything will ever run this schedule.
            // Derived from when a tick last actually ran, not from a unit
            // file existing on disk — a written-but-never-loaded unit would
            // have read as healthy.
            println!(
                "  {}",
                store.heartbeat_status(chrono::Utc::now().timestamp()).await
            );
        }
        ScheduleCommands::List => {
            // Lead with the heartbeat: a schedule that reads `active` but
            // that nothing is ticking is the whole reason someone runs this
            // command, and it looks identical to a healthy one otherwise.
            println!(
                "{}",
                store.heartbeat_status(chrono::Utc::now().timestamp()).await
            );
            let all = store.list().await?;
            if all.is_empty() {
                println!("No schedules.");
                return Ok(());
            }
            for s in all {
                println!(
                    "  {} — {} — goal {} — {} fires/{} — {}",
                    s.id,
                    s.state.as_str(),
                    s.goal_id,
                    s.fires,
                    s.max_fires,
                    serde_json::to_string(&s.trigger)?
                );
                if !s.last_outcome.is_empty() {
                    println!("      last: {}", s.last_outcome);
                }
            }
        }
        ScheduleCommands::Cancel { id } => {
            if store.cancel(&id).await? {
                println!("Cancelled {id}");
            } else {
                anyhow::bail!("no schedule '{id}' — nothing to cancel");
            }
        }
        ScheduleCommands::Tick => {
            let decisions = tick_once(&store, &resumer, chrono::Utc::now().timestamp()).await?;
            if decisions.is_empty() {
                println!("Nothing due.");
            }
            for (id, decision) in decisions {
                let tag = match decision {
                    Decision::Fired(_) => "FIRED",
                    Decision::Skipped(_) => "skip",
                    Decision::Stopped(_) => "STOP",
                };
                println!("{tag} {id}: {}", decision.reason());
            }
        }
        ScheduleCommands::Daemon { interval } => {
            if interval == 0 {
                anyhow::bail!("--interval must be greater than zero");
            }
            println!(
                "Schedule daemon running (tick every {interval}s). This loop dies with this \
                 process — it is meant for a supervised container. On a normal host use \
                 `prism schedule install`."
            );
            loop {
                match tick_once(&store, &resumer, chrono::Utc::now().timestamp()).await {
                    Ok(decisions) => {
                        for (id, decision) in decisions {
                            println!("{id}: {}", decision.reason());
                        }
                    }
                    // A failing tick must be loud and must not kill the loop —
                    // the next tick may well succeed.
                    Err(e) => eprintln!("tick failed: {e:#}"),
                }
                tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
            }
        }
        ScheduleCommands::Install { interval, write } => {
            let exe = std::env::current_exe()?;
            let unit_path = heartbeat_unit_path();
            let (contents, activate) = if cfg!(target_os = "macos") {
                // launchd opens StandardOutPath itself and refuses to spawn
                // the job if the directory is missing — the unit would sit
                // installed and never fire. Create it before writing.
                if write {
                    std::fs::create_dir_all(
                        PathBuf::from(std::env::var("HOME").unwrap_or_default())
                            .join(".prism")
                            .join("logs"),
                    )?;
                }
                (
                    launchd_plist(&exe, interval),
                    "launchctl bootstrap gui/$(id -u)".to_string(),
                )
            } else {
                let (service, timer) = systemd_units(&exe, interval);
                if write && let Some(dir) = unit_path.parent() {
                    std::fs::create_dir_all(dir)?;
                    std::fs::write(dir.join("prism-schedule.service"), &service)?;
                    println!("Wrote {}", dir.join("prism-schedule.service").display());
                }
                // systemd wants the unit NAME once the file is in the user
                // unit directory; a path works but is the awkward form.
                (
                    timer,
                    "systemctl --user daemon-reload && systemctl --user enable --now \
                     prism-schedule.timer #"
                        .to_string(),
                )
            };
            if write {
                if let Some(parent) = unit_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&unit_path, &contents)?;
                println!("Wrote {}", unit_path.display());
                println!("Activate it with:\n  {activate} {}", unit_path.display());
            } else {
                println!("# {}", unit_path.display());
                print!("{contents}");
                println!("# Activate with: {activate} {}", unit_path.display());
                println!("# Or re-run with --write to install it.");
            }
        }
    }
    Ok(())
}

/// Where this host's tick heartbeat unit lives.
fn heartbeat_unit_path() -> PathBuf {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    if cfg!(target_os = "macos") {
        home.join("Library/LaunchAgents/com.mirdyne.prism.schedule.plist")
    } else {
        home.join(".config/systemd/user/prism-schedule.timer")
    }
}

/// Spawn the detached background worker that owns a campaign loop:
/// `prism campaign continue <id>` with stdio detached, in its own process
/// group so it survives the parent CLI (or an agent tool call) exiting. The
/// checkpoint file is the polling channel — the worker updates it, everyone
/// else polls it — and the worker also records every progress transition to
/// the provenance store.
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
    // Record the live worker so the scheduler can tell "still running" from
    // "its process died" without spawning a duplicate. Every spawn site must
    // write this, or a scheduled tick would start a second worker alongside a
    // healthy one.
    if let Err(e) =
        prism_campaign::schedule::WorkerResumer::write_worker_pid(campaign_id, child.id())
    {
        tracing::warn!(campaign_id, error = %e, "could not record campaign worker pid — the scheduler may spawn a duplicate worker");
    }
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
            // Standalone/offline nodes have no account to verify. Their
            // anonymous local session is still useful for the local tool
            // boundary; linked nodes continue to reject it as non-owner.
            tracing::debug!(error = %e, "workflow: trying anonymous local node session");
            prism_client::node_session::mint_local_session(
                "http://127.0.0.1:7327",
                "anonymous-local",
                None,
            )
            .await
            .ok()
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
            .await?
            .platform_error_for_status()
            .await?;

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
            .await?
            .platform_error_for_status()
            .await?;

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

/// Load MatKG into the bundled local store (`~/.prism/provenance.db`) under
/// the isolated `local@matkg` tenant. `limit: None` is the deliberate
/// full-load choice (`--all`). The CC-BY attribution is a licence
/// condition and is surfaced on every load: printed in human output,
/// carried in the `attribution` field of `--json` output.
async fn handle_matkg_load(
    path: &Path,
    min_count: u32,
    limit: Option<usize>,
    json: bool,
) -> Result<()> {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let db_path = PathBuf::from(home).join(".prism/provenance.db");
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let store = prism_provenance::ProvenanceStore::open(&db_path).await?;
    let report = prism_ingest::matkg::load(
        path,
        &store,
        prism_ingest::matkg::MatkgLoadOptions { min_count, limit },
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("MatKG load complete → {}", db_path.display());
        println!("  Tenant: {}", report.tenant);
        println!(
            "  Facts written: {} (evidence class: research / ORANGE)",
            report.facts_written
        );
        println!("  Entities written: {}", report.entities_written);
        println!(
            "  Rows: {} total; skipped {} below --min-count {} and {} beyond --limit; {} selected",
            report.rows_total,
            report.rows_below_min_count,
            min_count,
            report.rows_beyond_limit,
            report.rows_selected
        );
        println!(
            "  Of selected: {} merged duplicate pairs, {} self pairs, {} incomplete, {} malformed, {} ambiguous",
            report.rows_merged_duplicates,
            report.rows_self_pair,
            report.rows_incomplete,
            report.rows_malformed,
            report.rows_ambiguous
        );
        if report.lines_unparsed > 0 {
            println!(
                "  Unparsed input lines: {} of {}",
                report.lines_unparsed, report.lines_total
            );
        }
        println!("  Input SHA-256: {}", report.data_sha256);
        println!(
            "  Ontology: {} (artifact sha256 {})",
            report.ontology_version_iri, report.ontology_artifact_sha256
        );
        println!("  PROV-O activity: {}", report.activity_id);
        println!();
        println!("{}", report.attribution);
    }
    Ok(())
}

/// Tenant every local single-user write uses (see the ingest pipeline's
/// `write_local_graph` and `handle_text_ingest` — both stamp `"local"`).
const LOCAL_ONTOLOGY_TENANT: &str = "local";

/// Locally-held EMMO ontology matches read from the bundled Turso
/// provenance store (`~/.prism/provenance.db`) — the store `prism ingest`
/// writes into and mesh sync lands peer knowledge in. This is what makes
/// a local ingest (or a peer sync) visible to `prism query` without any
/// external services running.
struct LocalOntologyResults {
    nodes: Vec<prism_provenance::GraphNode>,
    edges: Vec<prism_provenance::GraphEdge>,
    facts: Vec<prism_provenance::RecalledMaterialFact>,
}

/// The tenant set a CLI read spans: local plus every mesh tenant present
/// in the store (peer knowledge is shown BY DEFAULT, labelled — the
/// owner's decision, so the two-machine story needs no flag). Discovery
/// failure degrades to local-only rather than erroring — a broken
/// discovery must not take local query down with it — but it says so ON
/// STDERR: a silent narrowing would make peer knowledge invisible again
/// while looking exactly like "no peers exist", and `tracing::warn!` is
/// dark by default under `EnvFilter::from_default_env()`.
async fn read_scope(store: &prism_provenance::ProvenanceStore) -> Vec<String> {
    store.default_read_tenants().await.unwrap_or_else(|e| {
        eprintln!(
            "  warning: mesh tenant discovery failed — showing LOCAL knowledge only \
             (peer knowledge may exist but cannot be listed): {e:#}"
        );
        vec![LOCAL_ONTOLOGY_TENANT.to_string()]
    })
}

/// Query the bundled Turso store for locally-held ontology (local + mesh
/// tenants).
///
/// Never errors: any failure (store unopenable, query error) degrades to
/// `None`, which the caller renders as "no matches". `None` is also
/// returned when the store is fine but nothing matched (fresh install,
/// unknown term).
async fn local_ontology_lookup(
    db_path: &Path,
    text: &str,
    limit: usize,
) -> Option<LocalOntologyResults> {
    let store = match prism_provenance::ProvenanceStore::open(db_path).await {
        Ok(store) => store,
        Err(e) => {
            tracing::debug!("local ontology store open failed: {e:#}");
            return None;
        }
    };
    let limit = limit.max(1) as i64;
    let scope = read_scope(&store).await;
    let tenants: Vec<&str> = scope.iter().map(String::as_str).collect();

    // Exact/canonical entity name → 1-hop neighborhood (the Turso
    // counterpart of Neo4j `neighbors`).
    let (mut nodes, edges) = match store
        .get_neighbors_scoped(text, None, &tenants, limit)
        .await
    {
        Ok(traversal) => (traversal.nodes, traversal.edges),
        Err(e) => {
            tracing::debug!("local ontology neighbor read failed: {e:#}");
            (Vec::new(), Vec::new())
        }
    };

    // No exact center → substring search over entity names.
    if nodes.is_empty() {
        nodes = match store.graph_search_scoped(text, &tenants, limit).await {
            Ok(nodes) => nodes,
            Err(e) => {
                tracing::debug!("local ontology graph search failed: {e:#}");
                Vec::new()
            }
        };
    }

    // Provenance-backed assertions mentioning the query term — the
    // complete shape, so evidence class and owning tenant reach the
    // printer instead of being fetched and thrown away.
    let facts = match store
        .recall_with_context_scoped(text, &tenants, limit)
        .await
    {
        Ok(facts) => facts,
        Err(e) => {
            tracing::debug!("local ontology recall failed: {e:#}");
            Vec::new()
        }
    };

    if nodes.is_empty() && edges.is_empty() && facts.is_empty() {
        return None;
    }
    Some(LocalOntologyResults {
        nodes,
        edges,
        facts,
    })
}

/// Semantic entity search over the bundled Turso store, ranked by Turso's
/// native `vector_distance_cos()`, using the offline `prism-embed` backend
/// for the query vector (no Qdrant, no cloud) — the vectors that local
/// ingest writes via `embed_entities_best_effort`.
///
/// # Honesty contract
///
/// `Ok(vec![])` means **nothing is embedded locally yet**, and nothing
/// else. Anything that makes the index unusable — an unopenable store, a
/// missing embedding backend, a dimension mismatch — is an `Err` whose
/// message names the problem, so a broken index is never printed as "no
/// results". The store is counted BEFORE the backend is built, so a fresh
/// install never pays the embedding-model init just to return nothing.
async fn local_semantic_lookup(
    db_path: &Path,
    text: &str,
    limit: usize,
) -> Result<Vec<prism_provenance::SemanticEntityHit>> {
    // No store file at all ⇒ nothing was ever ingested. That is an empty
    // index, not a broken one, so it must not raise the alarm a fresh
    // install would otherwise trip on (opening a path under a missing
    // `~/.prism` fails outright).
    if !db_path.exists() {
        return Ok(Vec::new());
    }
    let store = prism_provenance::ProvenanceStore::open(db_path)
        .await
        .with_context(|| {
            format!(
                "local semantic store {} could not be opened",
                db_path.display()
            )
        })?;
    let scope = read_scope(&store).await;
    let tenants: Vec<&str> = scope.iter().map(String::as_str).collect();
    let embedded = store
        .entity_embedding_count_scoped(&tenants)
        .await
        .context("local semantic index could not be counted")?;
    if embedded == 0 {
        return Ok(Vec::new()); // nothing ingested yet — a real empty answer
    }

    // First ever native init may download the model — blocking pool.
    let backend = tokio::task::spawn_blocking(prism_embed::from_config)
        .await
        .context("embedding backend initialization panicked")?
        .context(
            "no embedding backend available, so the query cannot be embedded — set \
             PRISM_EMBED_BACKEND=native (the default) or =openai with \
             PRISM_EMBED_ENDPOINT_URL",
        )?;
    let query_vec = backend
        .embed(std::slice::from_ref(&text.to_string()))
        .await
        .context("embedding the query failed")?
        .into_iter()
        .next()
        .context("embedding backend returned no vector for the query")?;

    store
        .semantic_search_entities_scoped(&query_vec, &tenants, limit)
        .await
}

/// Origin marker appended to a row that came from a mesh peer rather than
/// this machine's own ingests. Empty for local rows, so a store with no
/// peer knowledge prints exactly what it always did.
///
/// Peer rows show the tenant verbatim (`[peer mesh:node-a]`, or
/// `[peer mesh]` for the legacy shared tenant): the whole point of NOT
/// merging tenants is that the reader can tell whose knowledge a line is,
/// so the label is the real storage identity, not a prettified alias. An
/// empty tenant (a pre-attribution serialized edge) is rendered as local
/// rather than invented.
fn peer_tag(tenant: &str) -> String {
    if tenant == LOCAL_ONTOLOGY_TENANT || tenant.is_empty() {
        String::new()
    } else if let Some(ontology) = tenant.strip_prefix("local@") {
        // Ontology-composed local tenant (`local@matkg`, …): reference
        // knowledge the user chose to load — labelling it `[peer …]` would
        // claim a mesh origin it does not have.
        format!("  [{ontology}]")
    } else {
        format!("  [peer {tenant}]")
    }
}

/// Render local-ontology matches in the same shape the retired Neo4j path
/// printed: one `[type] name` line per entity, plus relationship and fact
/// lines. Rows owned by a mesh tenant carry a trailing `[peer …]` marker,
/// and every fact line names its evidence class — a peer fact must be
/// visibly a peer fact, and a claim must be visibly classed.
fn format_local_ontology(results: &LocalOntologyResults) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    if !results.nodes.is_empty() {
        let mesh = results
            .nodes
            .iter()
            .filter(|node| node.tenant == "mesh" || node.tenant.starts_with("mesh:"))
            .count();
        let reference = results
            .nodes
            .iter()
            .filter(|node| node.tenant.starts_with("local@"))
            .count();
        if mesh == 0 && reference == 0 {
            let _ = writeln!(
                out,
                "  Found {} matching entities (local ontology):\n",
                results.nodes.len()
            );
        } else {
            let _ = writeln!(
                out,
                "  Found {} matching entities ({} local, {} reference, {} from mesh peers):\n",
                results.nodes.len(),
                results.nodes.len() - mesh - reference,
                reference,
                mesh
            );
        }
        for node in &results.nodes {
            let _ = writeln!(
                out,
                "  [{}] {}{}",
                node.entity_type,
                node.name,
                peer_tag(&node.tenant)
            );
        }
    }
    if !results.edges.is_empty() {
        let _ = writeln!(out, "\n  {} relationship(s):\n", results.edges.len());
        for edge in &results.edges {
            let _ = writeln!(
                out,
                "  {} -[{}]-> {}{}",
                edge.source,
                edge.rel_type,
                edge.target,
                peer_tag(&edge.tenant)
            );
        }
    }
    if !results.facts.is_empty() {
        let _ = writeln!(out, "\n  {} recalled fact(s):\n", results.facts.len());
        for fact in &results.facts {
            let _ = writeln!(
                out,
                "  {} -[{}]-> {}  (confidence {:.2}, evidence {}, source {}){}",
                fact.subject,
                fact.predicate,
                fact.object,
                fact.confidence,
                fact.evidence_class.as_str(),
                fact.source,
                peer_tag(&fact.tenant)
            );
        }
    }
    out
}

async fn handle_query(text: &str, semantic: bool, limit: usize) -> Result<()> {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let turso_db = PathBuf::from(home).join(".prism/provenance.db");

    if semantic {
        // Bundled Turso entity vectors written by local ingest, ranked by
        // the native `vector_distance_cos()` (offline prism-embed query
        // embedding — no services needed). An unusable index errors out
        // here rather than printing an empty, reassuring list.
        let results = local_semantic_lookup(&turso_db, text, limit).await?;
        println!("\nSemantic search results ({} matches):\n", results.len());
        for (i, hit) in results.iter().enumerate() {
            println!(
                "  {}. {}  (score: {:.4}){}",
                i + 1,
                hit.name,
                hit.similarity,
                peer_tag(&hit.tenant)
            );
        }
        if results.is_empty() {
            println!(
                "  (the local semantic index is empty — ingest data first with: \
                 prism ingest <path>)"
            );
        }
    } else {
        // Graph traversal over the bundled Turso provenance store
        // (~/.prism/provenance.db, tenant "local") — the sole graph
        // backend; no running services required.
        println!("Querying knowledge graph: \"{text}\"\n");

        if let Some(local) = local_ontology_lookup(&turso_db, text, limit).await {
            print!("{}", format_local_ontology(&local));
        } else {
            println!("  No direct matches. Try --semantic for vector search.");
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
    /// Retained device flow. It is usable only with explicit opt-in and a
    /// TTY; the URL is printed for the human to open manually and PRISM never
    /// launches a browser.
    Device {
        interactive_auth: bool,
        no_browser: bool,
    },
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
    let (credentials, interactive_auth) = match mode {
        LoginMode::Token(pat) => (run_token_login(endpoints, &pat).await?, false),
        LoginMode::Device {
            interactive_auth,
            no_browser,
        } => (
            run_device_login_with_opts(endpoints, interactive_auth, no_browser).await?,
            true,
        ),
    };
    let platform = PlatformClient::new(&endpoints.api_base).with_token(&credentials.access_token);
    let profile = platform.fetch_current_user().await.ok();
    let selected = select_project(
        &platform,
        profile
            .as_ref()
            .and_then(|user| user.display_name.as_deref()),
        interactive_auth,
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

async fn run_device_login(
    endpoints: &PlatformEndpoints,
    interactive_auth: bool,
) -> Result<StoredCredentials> {
    run_device_login_with_opts(endpoints, interactive_auth, true).await
}

/// Retained device-flow implementation. It is never entered without the
/// explicit interactive-auth opt-in and a real TTY. The verification URL is
/// printed for the human to open manually; PRISM never launches a browser.
async fn run_device_login_with_opts(
    endpoints: &PlatformEndpoints,
    interactive_auth: bool,
    _no_browser: bool,
) -> Result<StoredCredentials> {
    auth::require_interactive_auth(
        AuthSurface::Cli,
        interactive_auth,
        io::stdin().is_terminal(),
        io::stdout().is_terminal(),
    )?;

    let platform = PlatformClient::new(&endpoints.api_base);
    let http = platform.inner().clone();

    let start: DeviceCodeResponse =
        DeviceFlowAuth::start_device_flow(&http, &endpoints.api_base).await?;

    println!();
    println!("PRISM device login (manual approval)");
    println!("Open this URL manually: {}", start.verification_uri);
    println!("Code: {}", start.user_code);
    println!();
    println!("Approve the device, then wait here. Ctrl+C to abort.");

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
            "Token rejected by {} ({}). Check the PAT is correct and not revoked. \
             Issue a new one at {}/settings/tokens.",
            crate::brand::brand().display_name,
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
    interactive_auth: bool,
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
    } else if interactive_auth {
        prompt_select("Select organization", &orgs, |org| {
            format!("{} ({})", org.name, org.slug)
        })?
    } else {
        bail!(
            "multiple organizations require a selection; set MARC27_PROJECT_ID=<project_id>, then re-authenticate"
        );
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
    } else if interactive_auth {
        prompt_select("Select project", &projects, |project| {
            format!("{} ({})", project.name, project.slug)
        })?
    } else {
        bail!(
            "multiple projects require a selection; set MARC27_PROJECT_ID=<project_id>, then re-authenticate"
        );
    };

    Ok(SelectedContext {
        org_id: Some(selected_org.id.clone()),
        org_name: Some(selected_org.name.clone()),
        project_id: Some(selected_project.id.clone()),
        project_name: Some(selected_project.name.clone()),
    })
}

fn env_project_override() -> Option<String> {
    PlatformVar::PROJECT_ID
        .get()
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
    paths: &PrismPaths,
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

    // Persist rotated tokens to BOTH stores atomically: the authoritative
    // `cli-state.json` AND the `~/.prism/credentials.json` SDK mirror that the
    // Python platform tools read. Writing only one store left the other holding
    // a refresh token that single-use rotation had since REVOKED — replaying the
    // stale token tripped the server's token-family invalidation, forced a
    // device-flow re-login (the "re-login every ~24h" drift), and (for the SDK
    // mirror specifically) left node-up reading an expired access token that
    // 401'd on `POST /nodes/register` and dropped to silent offline mode.
    // `persist_credentials` is the single well-tested both-store writer.
    paths.persist_credentials(&new_creds)?;

    Ok(new_creds)
}

/// Resolve the platform credential for the node-up register call, mirroring the
/// priority order of `daemon::load_access_token` so the REST register path
/// never sends an expired session token:
///
/// 1. durable node token (`prism node token mint`) — non-rotating, no expiry;
/// 2. `MARC27_API_KEY` env — headless/agent path, no expiry;
/// 3. cli-state creds, refreshed once if `expires_at` has passed.
///
/// Returns `(token, refreshed_creds)`:
/// - `token` is the string to attach to the `PlatformClient`;
/// - `refreshed_creds` is `Some(...)` ONLY when this call refreshed (rotating
///   the single-use refresh token). The caller MUST use these refreshed creds
///   for any later refresh (e.g. the 401-retry) — never the stale startup
///   binding — or it will replay a now-REVOKED refresh token and trip the
///   server's token-family invalidation (forcing re-login + potentially
///   revoking the good tokens just issued).
async fn resolve_node_token(
    paths: &PrismPaths,
    endpoints: &PlatformEndpoints,
    creds: &StoredCredentials,
) -> Result<(String, Option<StoredCredentials>)> {
    if let Some(node_token) = paths.load_node_token() {
        tracing::debug!("using durable node token (does not rotate)");
        return Ok((node_token.key, None));
    }
    if let Some(key) = PlatformVar::API_KEY.get() {
        let key = key.trim().to_string();
        if !key.is_empty() {
            return Ok((key, None));
        }
    }
    if let Some(expires_at) = creds.expires_at
        && chrono::Utc::now() >= expires_at
    {
        tracing::info!("access token expired before node register, refreshing");
        let refreshed = refresh_access_token(paths, endpoints, creds).await?;
        // Thread the rotated creds out so the caller doesn't replay the
        // single-use refresh_token that `refresh_access_token` just consumed.
        return Ok((refreshed.access_token.clone(), Some(refreshed)));
    }
    Ok((creds.access_token.clone(), None))
}

// Old Ink/TypeScript TUI launcher removed — native Ratatui TUI is in crates/cli/src/tui/

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

/// Which Kafka brokers the mesh may connect to, if any.
///
/// Kafka is a TCP client started SEPARATELY from `start_mesh`, so gating the
/// mesh task does not cover it — and it never appears in a `reqwest` grep,
/// which is exactly how 9926eac0's "only two outbound sends" claim came to be
/// wrong. Returning None under hard offline disables the whole block at its
/// source rather than at each use.
///
/// `enabled()` rather than `check_url`: brokers are scheme-less `host:port`
/// and a list can name several, so there is no single URL to check.
///
/// Extracted so the decision is testable. It was inline in `main()` — the
/// higher-blast-radius half of the mesh work (`--kafka-brokers` is a free-form
/// flag naming an arbitrary REMOTE host, where mDNS is LAN-only) and the half
/// with no coverage. Best-covered path was not highest-risk path.
fn resolve_kafka_brokers(explicit: Option<&str>, with_kafka: bool) -> Option<String> {
    if prism_runtime::offline::enabled() {
        return None;
    }
    explicit
        .map(str::to_string)
        .or_else(|| with_kafka.then(|| "127.0.0.1:9092".to_string()))
}

/// The query body `query --federated` sends to every node, local and peer.
///
/// Mode is `"graph"`: the server's `execute_query` accepts only
/// graph/semantic/federated and 400s anything else. This function used to
/// send `"nl"`, a mode the server deleted with the Neo4j retirement — so
/// every federated CLI query was a guaranteed 400 rendered as "0 result(s)".
fn federated_query_body(query: &str) -> serde_json::Value {
    serde_json::json!({ "query": query, "mode": "graph" })
}

async fn handle_federated_query(
    query: &str,
    dashboard_url: &str,
    paths: &prism_runtime::PrismPaths,
) -> Result<()> {
    // Every send below is gated, and the gates are NOT allowed to be
    // swallowed. `create_dashboard_session*` already calls `check_url`, but
    // this function discarded its Result with `.ok()` and then built a
    // SEPARATE, ungated `reqwest::Client::new()` request and sent it anyway.
    // A guard that grep finds but the request never consults is worse than no
    // guard: it reads as covered.
    //
    // `query_federated` is an agent tool with `requires_approval: false`
    // (agent/src/command_tools.rs:320-326) and `dashboard_url` is in its
    // schema, so an injected prompt could name the host and POST the user's
    // literal query text to it with no human in the loop.
    prism_runtime::offline::check_url(dashboard_url).map_err(|r| anyhow!(r))?;

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
    let local_body = federated_query_body(query);
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
        // Second-order: these addresses come from the JSON the previous host
        // returned, so a hostile responder can name any peer it likes. Gate
        // each one rather than trusting step 1's check to cover them.
        if let Err(reason) = prism_runtime::offline::check_url(&peer_base) {
            println!(
                "[{}] skipped — {reason}",
                peer["name"].as_str().unwrap_or("unknown")
            );
            continue;
        }
        let peer_url = format!("{peer_base}/api/query");
        let body = federated_query_body(query);
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

fn validate_run_backend_target(
    backend: &str,
    ssh: Option<&str>,
    k8s_context: Option<&str>,
    slurm: Option<&str>,
) -> Result<()> {
    if backend == "byoc" && ssh.is_none() && k8s_context.is_none() && slurm.is_none() {
        anyhow::bail!("--backend byoc requires a target flag: --ssh, --k8s-context, or --slurm");
    }
    Ok(())
}

fn run_job_status_hint(resolved_backend: &str, job_id: uuid::Uuid) -> Option<String> {
    (resolved_backend != "local").then(|| format!("Check status:  prism job-status {job_id}"))
}

fn should_fetch_job_results(status: &prism_compute::JobStatus, is_slurm_array: bool) -> bool {
    matches!(status, prism_compute::JobStatus::Completed)
        || (is_slurm_array && matches!(status, prism_compute::JobStatus::Failed { .. }))
}

#[allow(clippy::too_many_arguments)]
async fn handle_run(
    data_dir: &Path,
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
    slurm_account: Option<&str>,
    slurm_time: Option<&str>,
    slurm_gres: Option<&str>,
    slurm_mem: Option<&str>,
    slurm_mem_per_cpu: Option<&str>,
    slurm_cpus_per_task: Option<u32>,
    slurm_nodes: Option<u32>,
    slurm_ntasks: Option<u32>,
    slurm_array: Option<&str>,
    slurm_dependency_afterok: Option<u64>,
    json: bool,
) -> Result<()> {
    use prism_compute::ExperimentPlan;
    use prism_compute::backend::ComputeRouter;
    use prism_compute::byoc::{ByocTarget, SlurmJobConfig};

    validate_run_backend_target(backend, ssh, k8s_context, slurm)?;

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
            ComputeRouter::local_only_persistent(data_dir)?.with_byoc(target),
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
            ComputeRouter::local_only_persistent(data_dir)?.with_byoc(target),
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
            config: Box::new(SlurmJobConfig {
                account: slurm_account.map(str::to_string),
                time: slurm_time.map(str::to_string),
                gres: slurm_gres.map(str::to_string),
                mem: slurm_mem.map(str::to_string),
                mem_per_cpu: slurm_mem_per_cpu.map(str::to_string),
                cpus_per_task: slurm_cpus_per_task,
                nodes: slurm_nodes,
                ntasks: slurm_ntasks,
                array: slurm_array.map(str::to_string),
                dependency_afterok: slurm_dependency_afterok,
                sif_path: image.to_string(),
                ..SlurmJobConfig::default()
            }),
        };
        (
            ComputeRouter::local_only_persistent(data_dir)?.with_byoc(target),
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
                // Resolve auth exactly like `prism compute` does: API key first
                // (MARC27_API_KEY → X-API-Key), else a Bearer session/token. The
                // old code read only MARC27_API_TOKEN and sent it as Bearer, which
                // 401'd for API-key agents.
                let (resolved_base, platform_auth) = resolve_agent_auth()?;
                let auth = marc27_auth_from(platform_auth);
                // Honor an explicit --platform-url override; otherwise use the
                // agent-resolved base (api.marc27.com/api/v1).
                let api_base = if platform_url == DEFAULT_RUN_PLATFORM_URL {
                    resolved_base
                } else {
                    platform_url.to_string()
                };
                (
                    ComputeRouter::with_marc27_persistent(&api_base, auth, data_dir)?,
                    "marc27",
                    serde_json::json!({
                        "kind": "marc27",
                        "platform_url": api_base,
                    }),
                )
            }
            _ => (
                ComputeRouter::local_only_persistent(data_dir)?,
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
    let submitted_record = router
        .tracker()
        .get(job_id)
        .await
        .context("submitted job is missing its tracking record")?;
    let slurm_job_id = submitted_record.slurm_job_id;

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
            if let Some(slurm_job_id) = slurm_job_id {
                object.insert("slurm_job_id".to_string(), slurm_job_id.into());
            }
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
        if let Some(slurm_job_id) = slurm_job_id {
            println!("SLURM job id: {slurm_job_id}");
        }
        if let Some(hint) = run_job_status_hint(resolved_backend, job_id) {
            println!("{hint}");
        }
        match status_result {
            Ok(status) => println!("Status: {:?}", status),
            Err(e) => println!("Status check: {e}"),
        }
    }

    Ok(())
}

async fn handle_job_status(paths: &PrismPaths, job_id_str: &str) -> Result<()> {
    use prism_compute::job::{JobTarget, JobTracker, TrackedStatus};

    let job_id: uuid::Uuid = job_id_str
        .parse()
        .with_context(|| format!("invalid job UUID: {job_id_str}"))?;
    let tracker = JobTracker::persistent(&paths.data_dir)?;
    let record = tracker.get(job_id).await.with_context(|| {
        format!(
            "job {job_id} is not in the local compute job registry at {}",
            paths.data_dir.display()
        )
    })?;

    let is_slurm_array = matches!(
        &record.target,
        JobTarget::Byoc(prism_compute::byoc::ByocTarget::Slurm { config, .. })
            if config.array.is_some()
    );
    let backend: Box<dyn prism_compute::ComputeBackend> = match record.target {
        JobTarget::Marc27 { api_base } => {
            let (_, platform_auth) = resolve_agent_auth()?;
            Box::new(prism_compute::Marc27Backend::new(
                &api_base,
                marc27_auth_from(platform_auth),
            ))
        }
        JobTarget::Byoc(target) => Box::new(prism_compute::byoc::ByocBackend::resume(
            target,
            job_id,
            record.slurm_job_id,
        )),
        JobTarget::Local => anyhow::bail!(
            "job {job_id} used local compute; cross-process local container status is not supported"
        ),
    };

    println!("Job: {job_id}");
    println!("Backend: {}", record.backend);
    let status = backend.status(job_id).await?;
    tracker
        .update_status(job_id, TrackedStatus::from(&status))
        .await?;
    println!("Status: {status:?}");
    if should_fetch_job_results(&status, is_slurm_array) {
        match backend.results(job_id).await {
            Ok(output) => println!("Output: {output}"),
            Err(e) => println!("Output: unavailable ({e})"),
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

    // 3. File GitHub issue (unless --no-github, or hard offline)
    //
    // This function already guarded its own platform POST (below), but not this
    // subprocess — `gh` carries the user's authenticated GitHub OAuth token to
    // api.github.com, so `PRISM_OFFLINE=1 prism report` published a bug report
    // and a credential anyway. A guard on the sockets a function opens says
    // nothing about the processes it spawns.
    //
    // Skipped rather than fatal: `--no-github` is already the supported way to
    // run this command without filing, so offline degrades to that and the user
    // still gets their report. Announced, never silent — a report that quietly
    // did not file is worse than one that says so.
    let blocked_offline = !no_github && prism_runtime::offline::enabled();
    if blocked_offline {
        println!("Filing GitHub issue... skipped (offline mode)");
    }
    let no_github = no_github || blocked_offline;
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

    // 4. Send to the hosted platform
    if let Some(c) = creds
        && !c.access_token.is_empty()
    {
        print!("Sending to the {}... ", crate::brand::brand().platform_name);
        let platform_body = serde_json::json!({
            "title": format!("bug report: {}", &description[..description.len().min(60)]),
            "description": format!(
                "{description}\n\nPRISM v{version}, Python {python_version}, {os_info}, {} cores, {} GB RAM",
                caps.cpu_cores, caps.ram_gb / 1024,
            ),
            "severity": "medium",
        });

        let url = format!("{}/support/tickets", endpoints.api_base);
        // `prism report` builds its own client rather than going through
        // PlatformClient, so it inherited none of that type's offline guard
        // and posted the session Bearer under PRISM_OFFLINE=1.
        prism_runtime::offline::check_url(&url).map_err(|reason| anyhow!(reason))?;
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

    println!("\nReport submitted. We'll follow up on GitHub and your platform dashboard.");
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
/// offline, no auth, network error → the local catalog cache at any age,
/// else empty. A fresh cache (within `CATALOG_TTL`) skips the network
/// entirely; a live fetch refreshes the cache. User-registered models
/// (`~/.prism/models.toml`) are always merged in FIRST so they override
/// catalog entries for resolution and limits. One fetch serves BOTH model
/// resolution (`resolve_catalog_model`) and limits (`model_limits`).
async fn fetch_model_catalog(paths: &PrismPaths) -> Vec<serde_json::Value> {
    let mut models = prism_agent::models::user_models_as_catalog_json();

    let cache = prism_agent::models::load_catalog_cache();
    let cache_is_fresh = cache
        .as_ref()
        .is_some_and(prism_agent::models::CatalogCache::is_fresh);
    let offline = prism_runtime::offline::enabled();
    if offline || cache_is_fresh {
        models.extend(cache.map(|c| c.models).unwrap_or_default());
        return models;
    }

    let fetched = fetch_platform_catalog_live_short_timeout(paths).await;
    match fetched {
        Some(live) => models.extend(live),
        // Live fetch failed → serve the stale cache rather than nothing.
        None => models.extend(cache.map(|c| c.models).unwrap_or_default()),
    }
    models
}

/// The startup-path live fetch: bounded by a short timeout so backend
/// startup is never held hostage, and every failure is `None` (fail-open).
/// On success the local cache is refreshed.
async fn fetch_platform_catalog_live_short_timeout(
    paths: &PrismPaths,
) -> Option<Vec<serde_json::Value>> {
    let (api_base, auth) = resolve_agent_auth().ok()?;
    let project_id = resolve_active_project_id(paths).ok()?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .ok()?;
    let response: serde_json::Value = auth
        .apply(client.get(format!("{api_base}/projects/{project_id}/llm/models")))
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .ok()?
        .json()
        .await
        .ok()?;
    let models = value_array(&response, &["models", "items", "data"])
        .cloned()
        .unwrap_or_default();
    if let Err(err) = prism_agent::models::save_catalog_cache(&models) {
        tracing::debug!("could not write model catalog cache: {err:#}");
    }
    Some(models)
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
/// The built-in `[llm].url` default (core config.rs `default_llm_url`). When
/// `fallback_url` still equals this, the user never configured an endpoint.
const DEFAULT_LLM_URL: &str = "http://localhost:8080";

/// Resolve the base URL for the MARC27 cloud chat target.
///
/// Order: explicit `LLM_BASE_URL` → the signed-in project's MARC27 endpoint →
/// an explicitly-configured `[llm].url`. It deliberately does NOT silently fall
/// back to the built-in localhost default: an unauthenticated user with no
/// configured endpoint used to run "cloud" chat against a local model while the
/// header showed the cloud model and credits never moved (#132). That case is
/// now an honest error (owner policy: explicit-only local mode).
fn marc27_llm_base_url(
    paths: &PrismPaths,
    api_base: &str,
    fallback_url: &str,
) -> anyhow::Result<String> {
    if let Ok(explicit) = std::env::var("LLM_BASE_URL") {
        return Ok(explicit);
    }
    if let Some(project_id) = paths
        .load_cli_state()
        .ok()
        .and_then(|s| s.credentials)
        .and_then(|c| c.project_id)
    {
        return Ok(marc27_llm_url_for_project(api_base, &project_id));
    }
    // Unauthenticated: honor only an explicitly-set url, refuse the default.
    resolve_unauth_llm_url(fallback_url)
}

/// Unauthenticated-case policy (owner: explicit-only local mode). An `[llm].url`
/// that differs from the built-in default is a deliberate local-mode choice and
/// is honored; the untouched default is refused with an honest message rather
/// than silently pointing "cloud" chat at localhost (#132).
fn resolve_unauth_llm_url(fallback_url: &str) -> anyhow::Result<String> {
    if fallback_url != DEFAULT_LLM_URL {
        return Ok(fallback_url.to_string());
    }
    anyhow::bail!(
        "Not signed in and no LLM endpoint configured. Sign in to use the hosted \
         platform (run sign-in from the palette), or set `[llm].url` in prism.toml \
         (or LLM_BASE_URL) to use a local model explicitly."
    )
}

#[cfg(test)]
mod tests {
    /// The credential must never leave the machine for a host the caller
    /// merely named. `--dashboard-url` is a free string on `mesh publish` and
    /// `query --federated`, and the agent tool schemas expose it to the model,
    /// so a prompt injection could point it anywhere.
    /// The guard existed, ran, returned Err — and the caller `.ok()`-swallowed
    /// it, then built a separate ungated client and sent anyway. `query_federated`
    /// is `requires_approval: false` with `dashboard_url` in its schema, so an
    /// injected prompt could POST the user's literal query text off-box.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn federated_query_is_refused_offline_before_any_send() {
        let _guard = boot_checks::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        // RAII, not a trailing remove_var: `.expect_err()` below can panic,
        // and an unwind past a manual cleanup leaves PRISM_OFFLINE set for the
        // rest of the binary — which then fails unrelated tests like
        // `a_signed_in_user_still_syncs_tools`, whose `should_sync_tools` gate
        // reads it. `clear_platform_env()` does NOT clear this var.
        struct OfflineEnvGuard(Option<String>);
        impl Drop for OfflineEnvGuard {
            fn drop(&mut self) {
                unsafe {
                    match self.0.take() {
                        Some(v) => std::env::set_var(prism_runtime::offline::ENV, v),
                        None => std::env::remove_var(prism_runtime::offline::ENV),
                    }
                }
            }
        }
        let _restore = OfflineEnvGuard(std::env::var(prism_runtime::offline::ENV).ok());
        unsafe { std::env::set_var(prism_runtime::offline::ENV, "1") };

        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let paths = prism_runtime::PrismPaths {
            config_dir: root.clone(),
            cache_dir: root.clone(),
            data_dir: root.clone(),
            state_dir: root,
        };
        // TEST-NET-3: a full connect attempt would cost seconds. Refusing by
        // policy is immediate, so a fast failure is itself part of the proof.
        let err = handle_federated_query("secret query text", "http://203.0.113.9:7327", &paths)
            .await
            .expect_err("offline must refuse a remote dashboard");

        let msg = format!("{err:#}");
        assert!(msg.contains("offline mode"), "{msg}");
        assert!(
            !msg.contains("secret query text"),
            "query text surfaced in the error path: {msg}"
        );
    }

    /// Relationships the pipeline dropped (referential containment) must
    /// reach the user's summary — count AND per-relationship reason. A drop
    /// only visible in `--json` is silent for everyone else.
    #[test]
    fn dropped_relationships_reach_the_ingest_summary() {
        // The local-tabular result shape `print_ingest_summary` reads.
        let result = serde_json::json!({
            "entities": {
                "entities": [{"type": "Alloy", "name": "Zorblatt-9", "properties": {}}],
                "relationships": [
                    {"from": "Zorblatt-9", "rel": "GLUED_TO", "to": "Phantomium"},
                    {"from": "Ghostium", "rel": "GLUED_TO", "to": "Phantomium"},
                    {"from": "Zorblatt-9", "rel": "HAS_PROPERTY", "to": "squishiness"}
                ]
            },
            "dropped_relationships": [
                "Zorblatt-9-[GLUED_TO]->Phantomium: undeclared endpoint(s): Phantomium",
                "Ghostium-[GLUED_TO]->Phantomium: undeclared endpoint(s): Ghostium, Phantomium"
            ],
            "graph": {"nodes_created": 2, "edges_created": 1}
        });
        let report = dropped_relationships_report(&result)
            .expect("a non-empty drop list must produce a report");
        assert!(report.contains("2 of 3"), "{report}");
        assert!(report.contains("Phantomium"), "{report}");
        assert!(report.contains("Ghostium"), "{report}");
        assert!(report.contains("NOT stored"), "{report}");

        // Nothing dropped (or a shape without the field) ⇒ no report line.
        assert_eq!(
            dropped_relationships_report(&serde_json::json!({
                "dropped_relationships": []
            })),
            None
        );
        assert_eq!(dropped_relationships_report(&serde_json::json!({})), None);
    }

    /// Entities the pipeline dropped for having a type the active ontology
    /// maps to no storage label must reach the user's summary the same way
    /// — count AND per-entity reason. Same contract as dropped
    /// relationships: a drop only visible in `--json` is silent.
    #[test]
    fn dropped_entities_reach_the_ingest_summary() {
        let result = serde_json::json!({
            "entities": {
                "entities": [
                    {"type": "Alloy", "name": "Bloopium", "properties": {}},
                    {"type": "Gadget", "name": "Sprocketium", "properties": {}}
                ],
                "relationships": []
            },
            "dropped_entities": [
                "entity 'Sprocketium': type 'Gadget' has no storage label in ontology 'emmo' \
                 (declared: Alloy, Element, Property, Process, Phase, Paper, Author, Dataset, Material)"
            ],
            "graph": {"nodes_created": 1, "edges_created": 0}
        });
        let report =
            dropped_entities_report(&result).expect("a non-empty drop list must produce a report");
        assert!(report.contains("1 of 2"), "{report}");
        assert!(report.contains("Sprocketium"), "{report}");
        assert!(report.contains("Gadget"), "{report}");
        assert!(report.contains("NOT stored"), "{report}");
        assert!(report.contains("never invented"), "{report}");

        // Nothing dropped (or a shape without the field) ⇒ no report line.
        assert_eq!(
            dropped_entities_report(&serde_json::json!({ "dropped_entities": [] })),
            None
        );
        assert_eq!(dropped_entities_report(&serde_json::json!({})), None);
    }

    /// The mode `query --federated` sends must be one the server still
    /// serves: `execute_query` accepts only graph/semantic/federated and
    /// 400s anything else. This function sent `"nl"` — a mode deleted with
    /// the Neo4j retirement — so every federated CLI query was a
    /// guaranteed 400 rendered as "0 result(s)".
    #[test]
    fn federated_query_sends_a_mode_the_server_still_accepts() {
        let body = federated_query_body("titanium alloys");
        assert_eq!(body["mode"], "graph", "got: {body}");
        assert_eq!(body["query"], "titanium alloys");
    }

    /// The higher-blast-radius half of the mesh offline work, and the half
    /// that had no test. `--kafka-brokers` is a free-form flag naming an
    /// arbitrary REMOTE host; mDNS is LAN-only. The mDNS gate got a
    /// mutation-tested test and this one got prose.
    #[test]
    fn kafka_brokers_resolve_to_none_under_hard_offline() {
        let _guard = boot_checks::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        struct OfflineEnvGuard(Option<String>);
        impl Drop for OfflineEnvGuard {
            fn drop(&mut self) {
                unsafe {
                    match self.0.take() {
                        Some(v) => std::env::set_var(prism_runtime::offline::ENV, v),
                        None => std::env::remove_var(prism_runtime::offline::ENV),
                    }
                }
            }
        }
        let _restore = OfflineEnvGuard(std::env::var(prism_runtime::offline::ENV).ok());

        unsafe { std::env::set_var(prism_runtime::offline::ENV, "1") };
        // An explicitly named REMOTE broker is the case that matters.
        assert_eq!(
            resolve_kafka_brokers(Some("broker.example:9092"), false),
            None
        );
        assert_eq!(
            resolve_kafka_brokers(None, true),
            None,
            "--with-kafka default"
        );
        assert_eq!(resolve_kafka_brokers(None, false), None);

        // Online: every input resolves as before. Without this the assertions
        // above would pass even if the function returned None unconditionally.
        unsafe { std::env::remove_var(prism_runtime::offline::ENV) };
        assert_eq!(
            resolve_kafka_brokers(Some("broker.example:9092"), false).as_deref(),
            Some("broker.example:9092")
        );
        assert_eq!(
            resolve_kafka_brokers(None, true).as_deref(),
            Some("127.0.0.1:9092"),
            "--with-kafka still defaults to loopback"
        );
        assert_eq!(
            resolve_kafka_brokers(None, false),
            None,
            "neither flag means no Kafka, offline or not"
        );
        // An explicit broker outranks the --with-kafka default.
        assert_eq!(
            resolve_kafka_brokers(Some("explicit:1234"), true).as_deref(),
            Some("explicit:1234")
        );
    }

    #[test]
    fn platform_token_only_goes_to_a_loopback_dashboard() {
        // Loopback, in the spellings is_loopback_url accepts.
        for ok in [
            "http://127.0.0.1:7327",
            "http://localhost:7327",
            "http://[::1]:7327",
        ] {
            assert_eq!(
                platform_token_for(ok, Some("live-token")),
                Some("live-token"),
                "{ok} is the user's own node"
            );
        }
        // Anything else, including a private LAN address, is withheld.
        for off_box in [
            "https://attacker.example/x",
            "http://203.0.113.9:7327",
            "http://10.0.0.4:7327",
            // A DOMAIN that merely looks loopback. This is the exact bug shape
            // 854a9345 fixed in offline::is_loopback_url, pinned HERE too so
            // the guard is regression-proof at its point of use and not only
            // by delegation.
            "http://127.evil.example/",
            "http://127.0.0.1.attacker.example/",
        ] {
            assert_eq!(
                platform_token_for(off_box, Some("live-token")),
                None,
                "{off_box} must not receive the platform credential"
            );
        }
        // No token in means no token out, loopback or not.
        assert_eq!(platform_token_for("http://127.0.0.1:7327", None), None);
    }

    /// Hard offline refuses outright — and still permits loopback, matching
    /// llm/embed/workflows rather than the blanket platform-client rule.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn dashboard_session_is_refused_offline_but_loopback_still_allowed() {
        let _guard = boot_checks::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        // RAII, not a trailing remove_var. The `.expect_err` calls below can
        // panic — and a panic there is EXACTLY the regression this test exists
        // to catch (the guard lets a request through and returns Ok). An
        // unwind past manual cleanup leaks PRISM_OFFLINE into every later test
        // in this binary. `federated_query_is_refused_offline_before_any_send`
        // 150 lines up carries a comment saying precisely this; this test did
        // not follow it.
        let _restore = prism_runtime::offline::test_support::OfflineEnvGuard::set("1");

        let remote = create_dashboard_session_for_user_with_platform_token(
            "http://203.0.113.9:7327",
            "user-1",
            None,
            None,
        )
        .await
        .expect_err("offline must refuse a remote dashboard");
        let remote_msg = format!("{remote:#}");

        // Loopback is NOT refused by the policy; it fails on connect instead.
        let local = create_dashboard_session_for_user_with_platform_token(
            "http://127.0.0.1:1",
            "user-1",
            None,
            None,
        )
        .await
        .expect_err("nothing is listening on port 1");
        let local_msg = format!("{local:#}");

        assert!(
            remote_msg.contains("offline mode"),
            "remote must be refused by policy: {remote_msg}"
        );
        assert!(
            !local_msg.contains("offline mode"),
            "loopback must not be refused by policy: {local_msg}"
        );
    }

    use super::*;

    #[test]
    fn campaign_rewards_include_text_evidence_tokens() {
        assert_eq!(
            format_classified_reward(0.8125, prism_campaign::EvidenceClass::Screening),
            "0.8125 [YELLOW screening]"
        );
        assert_eq!(
            format_classified_reward(3455.3, prism_campaign::EvidenceClass::Indeterminate),
            "3455.3000 [RED indeterminate]"
        );
    }

    #[test]
    fn unauth_llm_url_refuses_built_in_default() {
        // #132: unauthenticated + untouched default must NOT silently become
        // localhost — it errors instead.
        assert!(resolve_unauth_llm_url(DEFAULT_LLM_URL).is_err());
    }

    // ── `backend` / `ipc-serve` must not invent their own interpreter ──

    /// THE defect. `Backend` and `IpcServe` each declared their own
    /// `--python` defaulting to the literal `"python3"`, so their handlers
    /// received that sentinel instead of the resolved managed venv and ran
    /// whatever `python3` was first on `$PATH` — a system interpreter with
    /// none of PRISM's tools installed. `Commands::Tools` and the TUI used
    /// the resolved path. `ipc-serve` is the documented route for every
    /// external frontend (Desktop, IDE extension), so those frontends got a
    /// tool server that could not import `app`. Proven with a `python3`
    /// shim on `$PATH`.
    ///
    /// There is now exactly ONE `--python`: the global one, resolved once.
    #[test]
    fn only_one_python_flag_exists_and_it_is_the_global_one() {
        use clap::CommandFactory;

        let cmd = Cli::command();
        let global: Vec<_> = cmd
            .get_arguments()
            .filter(|a| a.get_id() == "python")
            .collect();
        assert_eq!(global.len(), 1, "the top level declares --python once");
        assert!(global[0].is_global_set(), "--python must be global");

        for sub in cmd.get_subcommands() {
            assert!(
                !sub.get_arguments().any(|a| a.get_id() == "python"),
                "`prism {}` declares its own --python; the global one is \
                 the only interpreter knob, and a second one is how these \
                 two subcommands ended up on a system python3",
                sub.get_name()
            );
        }
    }

    /// The global flag still carries a value given after the subcommand —
    /// which is how `prism ipc-serve` re-invokes `prism backend`, and how
    /// CI points at a pre-seeded interpreter.
    #[test]
    fn the_global_python_flag_works_in_subcommand_position() {
        for argv in [
            vec!["prism", "backend", "--python", "/opt/ci/python3.12"],
            vec!["prism", "ipc-serve", "--python", "/opt/ci/python3.12"],
        ] {
            let cli = Cli::try_parse_from(&argv).unwrap_or_else(|e| panic!("{argv:?}: {e}"));
            assert_eq!(cli.python, PathBuf::from("/opt/ci/python3.12"));
        }
        // Left off, it stays the sentinel that main() replaces with the venv.
        let cli = Cli::try_parse_from(["prism", "backend"]).unwrap();
        assert_eq!(cli.python, PathBuf::from("python3"));
    }

    /// …and the sentinel is only replaced for commands that are declared to
    /// need Python. Both of these are, so both get the managed venv.
    #[test]
    fn backend_and_ipc_serve_are_declared_to_need_the_venv() {
        for cmd in [
            Commands::Backend {
                project_root: PathBuf::from("."),
            },
            Commands::IpcServe {
                project_root: PathBuf::from("."),
            },
        ] {
            assert!(
                command_needs_python(Some(&cmd)),
                "{cmd:?} spawns the Python tool server"
            );
        }
    }

    /// `prism status` named a Python module that does not exist in the
    /// tree, so anyone who used it to find the worker was sent nowhere.
    /// The label is now the constant the spawner itself uses.
    #[test]
    fn status_names_the_module_the_tool_server_actually_runs() {
        assert_eq!(prism_python_bridge::TOOL_SERVER_MODULE, "app.tool_server");
        assert_ne!(prism_python_bridge::TOOL_SERVER_MODULE, "app.backend");
    }

    // ── F0 review-fix primitives ───────────────────────────────────────
    // These pin the contracts the node-up register-with-refresh logic depends
    // on. They would have caught the two review bugs:
    //  (1) storing the stale (401'd) client instead of the refreshed one
    //  (2) refreshing from the stale startup creds instead of the rotated ones

    #[test]
    fn platform_client_access_token_reflects_with_token() {
        // Bug (1) primitive: the value `node up` stores into the daemon state
        // is the `PlatformClient`; the daemon's REST heartbeat/role-sync runs
        // on whatever token it carries. `access_token()` MUST report the token
        // from the most recent `with_token`, so reassigning `platform` to a
        // refreshed client makes the stored client carry the LIVE token (not
        // the one that just 401'd).
        let stale = PlatformClient::new("https://api.marc27.com/api/v1").with_token("stale-dead");
        assert_eq!(stale.access_token(), Some("stale-dead"));
        let refreshed =
            PlatformClient::new("https://api.marc27.com/api/v1").with_token("fresh-live");
        assert_eq!(refreshed.access_token(), Some("fresh-live"));
        // Reassignment (the exact shape of the fix): a `mut` binding that is
        // overwritten by the refreshed client reports the refreshed token.
        let mut platform = stale;
        assert_eq!(platform.access_token(), Some("stale-dead"));
        platform = refreshed;
        assert_eq!(
            platform.access_token(),
            Some("fresh-live"),
            "reassigned client must carry the refreshed token"
        );
    }

    #[tokio::test]
    async fn resolve_node_token_fresh_creds_returns_no_rotation() {
        // Bug (2) primitive: when the creds are NOT expired (and no node-token
        // file / API key is set), `resolve_node_token` returns the access token
        // with `None` rotation. The 401-retry arm refreshes from the EFFECTIVE
        // creds; this contract ensures `None` ⇒ "use the original creds, no
        // rotation happened", so the retry doesn't replay an already-consumed
        // single-use refresh token.
        //
        // Uses a real temp-dir PrismPaths (all fields public) so the
        // durable-token branch finds no file; clears any inherited API key so
        // the API-key branch is skipped. Fresh creds → the refresh branch
        // (the only network call) is never taken.
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "prism-resolve-token-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let paths = PrismPaths {
            config_dir: dir.join("cfg"),
            cache_dir: dir.join("cache"),
            data_dir: dir.join("data"),
            state_dir: dir.join("state"),
        };
        std::fs::create_dir_all(dir.join("state")).unwrap();
        let creds = StoredCredentials {
            access_token: "fresh-untouched".to_string(),
            refresh_token: "unused-in-this-branch".to_string(),
            platform_url: "https://api.marc27.com".to_string(),
            user_id: None,
            display_name: None,
            org_id: None,
            org_name: None,
            project_id: None,
            project_name: None,
            expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
        };
        // env mutation is process-global + unsafe in edition 2024; avoid it.
        // If MARC27_API_KEY happens to be set in the test env, the function's
        // API-key branch short-circuits and this contract isn't exercisable —
        // skip gracefully rather than racing the global env.
        if PlatformVar::API_KEY.get().is_some() {
            eprintln!(
                "skipping resolve_node_token_fresh_creds_returns_no_rotation: \
                 MARC27_API_KEY is set in the env"
            );
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let endpoints = PlatformEndpoints {
            api_base: "https://api.marc27.com/api/v1".to_string(),
            node_ws: "wss://api.marc27.com/api/v1/nodes/connect".to_string(),
        };
        let (token, rotated) = resolve_node_token(&paths, &endpoints, &creds)
            .await
            .expect("fresh creds resolve without network");
        assert_eq!(token, "fresh-untouched");
        assert!(
            rotated.is_none(),
            "non-expired creds must NOT signal a rotation"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unauth_llm_url_honors_explicit_config() {
        // A user who explicitly configured a local endpoint keeps local mode.
        let explicit = "http://192.168.1.50:9000";
        assert_eq!(
            resolve_unauth_llm_url(explicit).unwrap(),
            explicit,
            "an explicitly-set [llm].url is a deliberate local-mode choice"
        );
    }

    // ── workflow llm-endpoint injection (bug c, CLI path) ──────────────
    // The Marc27 base URL is the outcome of the shared `marc27_llm_base_url`
    // resolver (tested above); these pin how the workflow injector consumes it.

    #[test]
    fn workflow_llm_marc27_signed_in_injects_project_endpoint() {
        // Signed-in cloud user, no [llm].model: must inject the resolved
        // project /llm endpoint and fall back to the platform `default` alias
        // — NOT error out (build_llm_config's Marc27 arm would) and NOT the
        // dead localhost default.
        let target = crate::chat_config::ChatTarget::Marc27 { model: None };
        let resolved = resolve_workflow_llm_endpoint(
            &test_registry(),
            &target,
            None,
            None,
            Some("https://api.marc27.com/projects/p1/llm".to_string()),
        );
        assert_eq!(
            resolved,
            Some((
                "https://api.marc27.com/projects/p1/llm".to_string(),
                "default".to_string()
            ))
        );
    }

    #[test]
    fn workflow_llm_marc27_honors_env_and_cfg_model() {
        let target = crate::chat_config::ChatTarget::Marc27 { model: None };
        // env LLM_MODEL wins.
        let with_env = resolve_workflow_llm_endpoint(
            &test_registry(),
            &target,
            Some("cfg-model"),
            Some("env-model".to_string()),
            Some("https://x/llm".to_string()),
        );
        assert_eq!(with_env.unwrap().1, "env-model");
        // No env ⇒ [llm].model is used.
        let with_cfg = resolve_workflow_llm_endpoint(
            &test_registry(),
            &target,
            Some("cfg-model"),
            None,
            Some("https://x/llm".to_string()),
        );
        assert_eq!(with_cfg.unwrap().1, "cfg-model");
    }

    #[test]
    fn workflow_llm_marc27_unresolved_base_injects_nothing() {
        // The anti-regression: when the Marc27 resolver refuses (unauthenticated
        // + no explicit endpoint → #132 error → None here), inject NOTHING so
        // the workflow falls back to its own env resolution instead of the dead
        // localhost default. Also proves a stray localhost `[llm].url` can't be
        // smuggled in via this path.
        let target = crate::chat_config::ChatTarget::Marc27 { model: None };
        assert_eq!(
            resolve_workflow_llm_endpoint(
                &test_registry(),
                &target,
                None,
                Some("m".to_string()),
                None
            ),
            None
        );
    }

    #[test]
    fn cli_workflow_run_preserves_config_resolved_llm_key_for_auth_endpoint() {
        let target = crate::chat_config::ChatTarget::Local {
            url: "http://127.0.0.1:9000/v1".to_string(),
            model: "auth-model".to_string(),
            api_key: Some("config-resolved-key".to_string()),
        };
        let key = resolve_workflow_llm_api_key_for_target(
            &target,
            &prism_core::config::LlmSection::default(),
            None,
        );
        assert_eq!(key.as_deref(), Some("config-resolved-key"));
    }

    #[test]
    fn workflow_llm_local_uses_configured_url_and_model() {
        let target = crate::chat_config::ChatTarget::Local {
            url: "http://127.0.0.1:11434/v1".to_string(),
            model: "llama3".to_string(),
            api_key: None,
        };
        assert_eq!(
            resolve_workflow_llm_endpoint(&test_registry(), &target, None, None, None),
            Some((
                "http://127.0.0.1:11434/v1".to_string(),
                "llama3".to_string()
            ))
        );
    }

    #[test]
    fn workflow_llm_local_empty_url_injects_nothing() {
        let target = crate::chat_config::ChatTarget::Local {
            url: String::new(),
            model: "llama3".to_string(),
            api_key: None,
        };
        assert_eq!(
            resolve_workflow_llm_endpoint(&test_registry(), &target, None, None, None),
            None
        );
    }

    /// The built-in registry, used by every endpoint-resolution test so
    /// they exercise the shipped data rather than a fixture that could
    /// drift from it.
    fn test_registry() -> crate::providers::Registry {
        crate::providers::Registry::builtin().expect("built-in registry must parse")
    }

    #[test]
    fn workflow_llm_provider_builds_openai_base() {
        let target = crate::chat_config::ChatTarget::Provider {
            provider: "Anthropic".to_string(),
            model: "claude-sonnet-5".to_string(),
            api_key_env: None,
        };
        assert_eq!(
            resolve_workflow_llm_endpoint(&test_registry(), &target, None, None, None),
            Some((
                "https://api.anthropic.com/v1".to_string(),
                "claude-sonnet-5".to_string()
            ))
        );
    }

    /// The bug the registry exists to kill. `https://api.{id}.com/v1`
    /// pointed Mistral at a host that does not exist, Groq at a path that
    /// 404s, Google at a host that serves no chat API, and OpenRouter at
    /// the wrong domain entirely. Every one of these came back wrong from
    /// all four chat surfaces; this pins the corrected values.
    #[test]
    fn workflow_llm_provider_uses_registry_not_the_dot_com_guess() {
        let reg = test_registry();
        let cases = [
            ("mistral", "https://api.mistral.ai/v1"),
            ("groq", "https://api.groq.com/openai/v1"),
            ("openrouter", "https://openrouter.ai/api/v1"),
            (
                "google",
                "https://generativelanguage.googleapis.com/v1beta/openai",
            ),
            ("cerebras", "https://api.cerebras.ai/v1"),
            ("zai", "https://api.z.ai/api/paas/v4"),
            ("xai", "https://api.x.ai/v1"),
            ("ollama", "http://localhost:11434/v1"),
        ];
        for (provider, expected) in cases {
            let target = crate::chat_config::ChatTarget::Provider {
                provider: provider.to_string(),
                model: "m".to_string(),
                api_key_env: None,
            };
            assert_eq!(
                resolve_workflow_llm_endpoint(&reg, &target, None, None, None),
                Some((expected.to_string(), "m".to_string())),
                "{provider} must resolve from the registry"
            );
        }
    }

    /// A slug nobody has declared keeps the historical guess rather than
    /// erroring — unchanged behaviour for unknown providers, so the
    /// registry is purely additive.
    #[test]
    fn workflow_llm_unknown_provider_keeps_legacy_guess() {
        let target = crate::chat_config::ChatTarget::Provider {
            provider: "some-new-vendor".to_string(),
            model: "m".to_string(),
            api_key_env: None,
        };
        assert_eq!(
            resolve_workflow_llm_endpoint(&test_registry(), &target, None, None, None),
            Some((
                "https://api.some-new-vendor.com/v1".to_string(),
                "m".to_string()
            ))
        );
    }

    /// MARC27 must NOT resolve through the direct-provider path: its
    /// endpoint is session-derived. If `base_url_for` ever started
    /// returning a static URL for it, cloud chat would silently bypass the
    /// signed-in project endpoint.
    #[test]
    fn marc27_is_never_a_direct_provider_endpoint() {
        let reg = test_registry();
        assert_eq!(crate::providers::base_url_for(&reg, "marc27"), None);
    }

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
        // Must hold the shared guard and clear BOTH spellings.
        //
        // This test took no lock and cleared only the historical name. That
        // was safe while nothing else wrote PROJECT_ID — but `cffcba9a`
        // migrated `env_project_override` onto `PlatformVar`, so it now reads
        // `PRISM_PROJECT_ID` too and PREFERS it. Any concurrent test setting
        // the neutral name made this one fail with a value it never set, on a
        // machine-dependent schedule. It flaked exactly that way.
        let _guard = boot_checks::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        unsafe {
            std::env::remove_var("PRISM_PROJECT_ID");
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
        // The neutral name outranks the historical one, same as everywhere.
        unsafe {
            std::env::set_var("PRISM_PROJECT_ID", "neutral-wins");
        }
        assert_eq!(env_project_override(), Some("neutral-wins".to_string()));
        unsafe {
            std::env::remove_var("PRISM_PROJECT_ID");
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

    /// Everything the unsupported-format message advertises must actually
    /// route. This is the falsifiable direction here: the message is built
    /// from `extensions()` + `PLATFORM_TEXT_EXTENSIONS` while the router
    /// reads `claims()` + the same const, so an `extensions()`/`claims()`
    /// incoherence, or anything added to the message without a route,
    /// fails. (The reverse direction — a runtime-registered connector must
    /// be ADVERTISED — is proven with novel data in `prism_ingest`'s
    /// connector tests; asserting it here against only built-ins would be
    /// a tautology a hardcoded list could satisfy, and implementing a
    /// novel `Connector` in this crate would need a polars dev-dependency.)
    #[test]
    fn every_advertised_ingest_format_actually_routes() {
        let supported = supported_ingest_formats();
        let advertised: Vec<&str> = supported.split(", ").collect();
        assert!(
            !advertised.is_empty(),
            "the message must advertise something"
        );
        for ext in &advertised {
            assert!(
                ingest_backend(Path::new(&format!("/tmp/x.{ext}"))).is_some(),
                "'{ext}' is advertised but does not route",
            );
        }
    }

    #[test]
    fn ingest_locality_and_discovery_agree_on_what_is_on_this_machine() {
        // Both surfaces read the same predicate; this pins that they still
        // do, so ingest can never call an endpoint "cloud" while discovery
        // is reporting it as a local server.
        for url in ["http://localhost:11434/v1", "http://127.0.0.1:8080"] {
            assert!(is_loopback_url(url));
            assert!(text_locality_for("auto", Some(url)).is_local());
        }
        assert!(!is_loopback_url("https://api.openai.com/v1"));
        assert!(!text_locality_for("auto", Some("https://api.openai.com/v1")).is_local());
    }

    #[test]
    fn text_locality_honors_explicit_config() {
        // An explicit choice is never second-guessed, whatever the LLM is.
        assert_eq!(
            text_locality_for("local", None),
            TextLocality::Local,
            "locality = \"local\" must stay local even with no LLM configured"
        );
        assert_eq!(
            text_locality_for("cloud", Some("http://localhost:8080")),
            TextLocality::Cloud
        );
    }

    #[test]
    fn text_locality_auto_follows_the_model() {
        assert_eq!(
            text_locality_for("auto", Some("http://localhost:11434/v1")),
            TextLocality::Local
        );
        assert_eq!(
            text_locality_for("auto", Some("https://api.openai.com/v1")),
            TextLocality::CloudNoLocalModel
        );
    }

    #[test]
    fn text_locality_treats_embedded_gguf_as_local() {
        // `prism use local --url gguf://local` is the MOST on-device
        // configuration possible — no server, no socket, weights loaded
        // in-process. `is_loopback_url` parses its host as Domain("local")
        // and says false, which routed documents to the platform (or, logged
        // out, printed "no on-device model to extract with" — a lie).
        for url in ["gguf://local", "gguf://local/"] {
            assert_eq!(
                text_locality_for("auto", Some(url)),
                TextLocality::Local,
                "{url} runs in-process and must classify as local"
            );
        }
        // The sentinel is exact — a lookalike remote URL must not ride in.
        assert_eq!(
            text_locality_for("auto", Some("gguf://local.example.com")),
            TextLocality::CloudNoLocalModel
        );
        // And an explicit cloud choice is still never second-guessed.
        assert_eq!(
            text_locality_for("cloud", Some("gguf://local")),
            TextLocality::Cloud
        );
    }

    #[test]
    fn text_locality_auto_without_a_model_is_not_a_cloud_choice() {
        // The fresh-install case: no `[llm] model`, so `build_llm_config`
        // fails and there is nothing on-device to extract with. This must
        // NOT read as "the user chose the cloud" — `handle_ingest` keys the
        // both-doors-shut error off this variant.
        assert_eq!(
            text_locality_for("auto", None),
            TextLocality::CloudNoLocalModel
        );
        assert!(!text_locality_for("auto", None).is_local());
    }

    #[test]
    fn no_ingest_backend_message_names_the_local_door_first() {
        let msg = no_ingest_backend_message(&[]);
        let local = msg.find("prism use local").expect("names local ingest");
        let login = msg.find("prism login").expect("names the platform");
        assert!(
            local < login,
            "local ingest must be offered before `prism login`:\n{msg}"
        );
        assert!(msg.contains("11434"), "names the Ollama endpoint:\n{msg}");
        assert!(msg.contains("8080"), "names the llama.cpp endpoint:\n{msg}");
    }

    fn discovered(
        provider_id: &str,
        name: &str,
        url: &str,
        models: &[&str],
    ) -> local_llm::LocalServer {
        local_llm::LocalServer {
            provider_id: provider_id.to_string(),
            name: name.to_string(),
            base_url: url.to_string(),
            models: models.iter().map(|m| m.to_string()).collect(),
            state: local_llm::ServerState::Ready,
        }
    }

    /// The whole point: one server, one model ⇒ the user gets a command
    /// they can run, not a template with a `<model>` hole in it.
    #[test]
    fn no_ingest_backend_message_prints_a_runnable_command_when_one_model_is_found() {
        let msg = no_ingest_backend_message(&[discovered(
            "ollama",
            "Ollama (local)",
            "http://localhost:11434/v1",
            &["qwen2.5:3b"],
        )]);
        assert!(
            msg.contains("prism use local --url http://localhost:11434/v1 --model qwen2.5:3b"),
            "expected a ready-to-run command:\n{msg}"
        );
        assert!(
            !msg.contains("<model>"),
            "a discovered model must replace the placeholder:\n{msg}"
        );
        // Local still comes before the account pitch.
        let local = msg.find("prism use local").unwrap();
        let login = msg.find("prism login").unwrap();
        assert!(local < login, "{msg}");
    }

    /// Several candidates ⇒ list them. Choosing where inference runs is
    /// the user's call, so a guess here would be the wrong kind of help.
    #[test]
    fn no_ingest_backend_message_lists_every_candidate_without_choosing() {
        let msg = no_ingest_backend_message(&[
            discovered(
                "ollama",
                "Ollama (local)",
                "http://localhost:11434/v1",
                &["qwen2.5:3b", "llama3.2:1b"],
            ),
            discovered(
                "llamacpp",
                "llama.cpp server (local)",
                "http://localhost:8081/v1",
                &["gemma-4-12B-it-qat-UD-Q4_K_XL.gguf"],
            ),
        ]);
        for model in [
            "qwen2.5:3b",
            "llama3.2:1b",
            "gemma-4-12B-it-qat-UD-Q4_K_XL.gguf",
        ] {
            assert!(msg.contains(model), "{model} missing from:\n{msg}");
        }
        assert!(msg.contains("http://localhost:8081/v1"), "{msg}");
        assert!(!msg.contains("<model>"), "{msg}");
    }

    /// A server still loading its weights is neither absent nor usable.
    /// Saying nothing about it sends the user off to start another one.
    #[test]
    fn no_ingest_backend_message_reports_a_loading_server() {
        let mut loading = discovered("vllm", "vLLM (local)", "http://localhost:8000/v1", &[]);
        loading.state = local_llm::ServerState::Loading;
        let msg = no_ingest_backend_message(&[loading]);
        assert!(msg.contains("still loading"), "{msg}");
        assert!(msg.contains("http://localhost:8000/v1"), "{msg}");
        // No model to offer, so the template stays.
        assert!(msg.contains("<model>"), "{msg}");
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
                semantic,
                limit,
                ..
            } => {
                assert_eq!(text, "NbMoTaW alloys");
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
    fn persisted_remote_runs_have_cross_process_status_hints() {
        assert!(run_job_status_hint("byoc", uuid::Uuid::nil()).is_some());
        assert!(run_job_status_hint("marc27", uuid::Uuid::nil()).is_some());
        assert_eq!(run_job_status_hint("local", uuid::Uuid::nil()), None);
    }

    #[test]
    fn failed_slurm_arrays_still_fetch_the_successful_task_results() {
        use prism_compute::JobStatus;

        assert!(should_fetch_job_results(
            &JobStatus::Failed {
                error: "task 7 failed".into(),
            },
            true,
        ));
        assert!(!should_fetch_job_results(
            &JobStatus::Failed {
                error: "single job failed".into(),
            },
            false,
        ));
    }

    #[test]
    fn byoc_backend_without_target_is_rejected() {
        let error = validate_run_backend_target("byoc", None, None, None).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("--backend byoc"), "{message}");
        for flag in ["--ssh", "--k8s-context", "--slurm"] {
            assert!(message.contains(flag), "missing {flag} in: {message}");
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
    fn cli_parses_slurm_resource_options() {
        let cli = Cli::try_parse_from([
            "prism",
            "run",
            "--slurm",
            "researcher@login.hpc",
            "--slurm-partition",
            "gpu",
            "--slurm-account",
            "esa-materials",
            "--slurm-time",
            "02:00:00",
            "--slurm-gres",
            "gpu:a100:1",
            "--slurm-mem",
            "64G",
            "--slurm-cpus-per-task",
            "8",
            "--slurm-nodes",
            "2",
            "--slurm-ntasks",
            "4",
            "--slurm-array",
            "0-15%4",
            "--slurm-dependency-afterok",
            "98765",
            "/shared/prism-worker.sif",
        ])
        .unwrap();

        match cli.command.unwrap() {
            Commands::Run {
                slurm_account,
                slurm_time,
                slurm_gres,
                slurm_mem,
                slurm_cpus_per_task,
                slurm_nodes,
                slurm_ntasks,
                slurm_array,
                slurm_dependency_afterok,
                ..
            } => {
                assert_eq!(slurm_account.as_deref(), Some("esa-materials"));
                assert_eq!(slurm_time.as_deref(), Some("02:00:00"));
                assert_eq!(slurm_gres.as_deref(), Some("gpu:a100:1"));
                assert_eq!(slurm_mem.as_deref(), Some("64G"));
                assert_eq!(slurm_cpus_per_task, Some(8));
                assert_eq!(slurm_nodes, Some(2));
                assert_eq!(slurm_ntasks, Some(4));
                assert_eq!(slurm_array.as_deref(), Some("0-15%4"));
                assert_eq!(slurm_dependency_afterok, Some(98765));
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
    fn parse_sse_json_events_accepts_blank_line_delimited_data_events() {
        // Real SSE framing: one `data:` line per event, each event
        // terminated by a blank line (this is how axum's `Sse` writer, and
        // every conformant SSE source, actually separates events on the
        // wire — see `SseDecoder`'s doc comment).
        let events = parse_sse_json_events(
            "data: {\"step\":\"started\",\"instance_id\":\"abc\"}\n\n\
data: {\"step\":\"agent_turn\",\"agent_id\":\"metallurgist\"}\n\n\
data: {\"step\":\"complete\",\"total_turns\":2}\n\n",
        )
        .unwrap();

        assert_eq!(events.len(), 3);
        assert_eq!(events[0]["step"], "started");
        assert_eq!(events[1]["agent_id"], "metallurgist");
        assert_eq!(events[2]["step"], "complete");
    }

    #[test]
    fn parse_sse_json_events_joins_multiline_data_fields() {
        // Per the SSE spec, several consecutive `data:` lines before a
        // blank line belong to ONE event and are joined with `\n`.
        let events = parse_sse_json_events(
            "data: {\"step\":\"complete\",\n\
data: \"summary\":\"line one\\nline two\"}\n\n",
        )
        .unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["step"], "complete");
        assert_eq!(events[0]["summary"], "line one\nline two");
    }

    /// Reproduces the real, live-observed mirofish/axum double `data:`
    /// encoding bug: `crates/core/src/mirofish/engine.rs` pre-formats each
    /// message as `format!("data: {json}\n\n")` and hands that *string* to
    /// `axum::response::sse::Event::data(msg)` in
    /// `crates/api/src/routes/discourse.rs`, which SSE-encodes it a second
    /// time — splitting on the embedded newlines and re-prefixing every
    /// resulting line with `data:`. This byte stream is what actually
    /// arrives over the wire for a single logical `{"step":"started",...}`
    /// event; a naive one-`data:`-line-per-event parser silently drops it
    /// (payload = the non-JSON string `data: {"step":...}`) and the whole
    /// discourse run comes back empty.
    #[test]
    fn parse_sse_json_events_reconstructs_double_data_prefixed_events() {
        let raw = "data:data: {\"step\":\"started\",\"instance_id\":\"abc\"}\n\
data:\n\
data:\n\
\n\
data:data: {\"step\":\"complete\",\"total_turns\":2}\n\
data:\n\
data:\n\
\n";

        let events = parse_sse_json_events(raw).unwrap();

        assert_eq!(events.len(), 2, "events: {events:?}");
        assert_eq!(events[0]["step"], "started");
        assert_eq!(events[0]["instance_id"], "abc");
        assert_eq!(events[1]["step"], "complete");
        assert_eq!(events[1]["total_turns"], 2);
    }

    /// Same double-encoded byte stream as above, but delivered as two
    /// separate network reads with the split landing *inside* the
    /// double-`data:`-prefixed line of the first event — the case a
    /// non-chunk-safe parser (one that only ever sees a fully-buffered
    /// `String`) can't represent at all. Feeding `SseDecoder` chunk by
    /// chunk directly (rather than through `parse_sse_json_events`, which
    /// only ever receives one fully-assembled body) proves the buffering
    /// survives a line — and an event's blank-line terminator — arriving in
    /// two pieces.
    #[test]
    fn sse_decoder_reassembles_event_split_across_two_chunks() {
        let raw = "data:data: {\"step\":\"started\",\"instance_id\":\"abc\"}\n\
data:\n\
data:\n\
\n\
data:data: {\"step\":\"complete\",\"total_turns\":2}\n\
data:\n\
data:\n\
\n";
        // Split mid-line, inside the JSON payload of the first event.
        let split_at = raw
            .find("\"instance_id\"")
            .expect("fixture contains marker");
        let (chunk_a, chunk_b) = raw.split_at(split_at);

        let mut decoder = SseDecoder::default();
        let mut events = decoder.push_chunk(chunk_a);
        events.extend(decoder.push_chunk(chunk_b));
        events.extend(decoder.finish());

        assert_eq!(events.len(), 2, "events: {events:?}");
        assert_eq!(events[0]["step"], "started");
        assert_eq!(events[0]["instance_id"], "abc");
        assert_eq!(events[1]["step"], "complete");
        assert_eq!(events[1]["total_turns"], 2);
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

    // ── WP4 one-call tool wrappers: CLI parsing ───────────────────────

    #[test]
    fn cli_parses_deploy_and_invoke_command() {
        let cli = Cli::try_parse_from([
            "prism",
            "deploy-and-invoke",
            "--name",
            "serve-demo",
            "--resource-slug",
            "mace-mh-1",
            "--input",
            r#"{"task":"single_point"}"#,
            "--ready-timeout-secs",
            "60",
        ])
        .unwrap();
        match cli.command.unwrap() {
            Commands::DeployAndInvoke {
                name,
                image,
                resource_slug,
                invoke_path,
                input,
                ready_timeout_secs,
                keep,
                ..
            } => {
                assert_eq!(name, "serve-demo");
                assert!(image.is_none());
                assert_eq!(resource_slug.as_deref(), Some("mace-mh-1"));
                assert_eq!(invoke_path, "/predict");
                assert_eq!(input, r#"{"task":"single_point"}"#);
                assert_eq!(ready_timeout_secs, 60);
                assert!(!keep);
            }
            _ => panic!("expected DeployAndInvoke command"),
        }
    }

    #[test]
    fn cli_parses_compute_run_command() {
        let cli = Cli::try_parse_from([
            "prism",
            "compute-run",
            "--image",
            "marc27/mace:latest",
            "--gpu",
            "A100-80GB",
            "--poll-timeout-secs",
            "120",
        ])
        .unwrap();
        match cli.command.unwrap() {
            Commands::ComputeRun {
                image,
                gpu,
                poll_timeout_secs,
                ..
            } => {
                assert_eq!(image, "marc27/mace:latest");
                assert_eq!(gpu.as_deref(), Some("A100-80GB"));
                assert_eq!(poll_timeout_secs, 120);
            }
            _ => panic!("expected ComputeRun command"),
        }
    }

    #[test]
    fn cli_parses_ingest_and_wait_command() {
        let cli = Cli::try_parse_from([
            "prism",
            "ingest-and-wait",
            "--url",
            "https://example.org/paper.pdf",
            "--mode",
            "graph",
        ])
        .unwrap();
        match cli.command.unwrap() {
            Commands::IngestAndWait {
                url, query, mode, ..
            } => {
                assert_eq!(url.as_deref(), Some("https://example.org/paper.pdf"));
                assert!(query.is_none());
                assert_eq!(mode, "graph");
            }
            _ => panic!("expected IngestAndWait command"),
        }
    }

    // ── WP4 one-call tool wrappers: mocked-transport behavior ─────────
    //
    // Each wrapper gets a happy path and a failure path. The failure path
    // is the load-bearing assertion: it proves a failed/errored job never
    // gets reported as a success, and (for deploy-and-invoke) that cleanup
    // still runs when the invoke call itself fails.

    #[tokio::test]
    async fn deploy_and_invoke_happy_path_creates_invokes_and_stops() {
        let mut server = mockito::Server::new_async().await;
        let api_base = server.url();
        let svc_url = format!("{api_base}/svc");

        let create_mock = server
            .mock("POST", "/compute/deployments")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"id":"dep-abc"}"#)
            .create_async()
            .await;
        let status_mock = server
            .mock("GET", "/compute/deployments/dep-abc")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(format!(
                r#"{{"status":"running","endpoint_url":"{svc_url}"}}"#
            ))
            .create_async()
            .await;
        let invoke_mock = server
            .mock("POST", "/svc/predict")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"answer":42}"#)
            .create_async()
            .await;
        let stop_mock = server
            .mock("DELETE", "/compute/deployments/dep-abc")
            .with_status(200)
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let auth = PlatformAuth::ApiKey("test-key".to_string());
        let result = run_deploy_and_invoke(
            &client,
            &api_base,
            &auth,
            "test-dep",
            Some("img:latest"),
            None,
            "local",
            None,
            None,
            None,
            &[],
            8080,
            "/health",
            "/predict",
            serde_json::json!({"task": "single_point"}),
            30,
            false,
            Duration::from_millis(1),
        )
        .await
        .expect("happy path must succeed");

        assert_eq!(result["deployment_id"], "dep-abc");
        assert_eq!(result["auto_stopped"], true);
        assert_eq!(result["kept_running"], false);
        assert_eq!(result["result"]["answer"], 42);

        create_mock.assert_async().await;
        status_mock.assert_async().await;
        invoke_mock.assert_async().await;
        stop_mock.assert_async().await;
    }

    #[tokio::test]
    async fn deploy_and_invoke_failure_path_still_stops_and_surfaces_real_error() {
        let mut server = mockito::Server::new_async().await;
        let api_base = server.url();
        let svc_url = format!("{api_base}/svc2");

        server
            .mock("POST", "/compute/deployments")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"id":"dep-fail"}"#)
            .create_async()
            .await;
        server
            .mock("GET", "/compute/deployments/dep-fail")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(format!(
                r#"{{"status":"running","endpoint_url":"{svc_url}"}}"#
            ))
            .create_async()
            .await;
        server
            .mock("POST", "/svc2/predict")
            .with_status(500)
            .with_body("internal error")
            .create_async()
            .await;
        // The load-bearing mock: cleanup MUST still fire even though the
        // invoke call above failed.
        let stop_mock = server
            .mock("DELETE", "/compute/deployments/dep-fail")
            .with_status(200)
            .expect(1)
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let auth = PlatformAuth::ApiKey("test-key".to_string());
        let result = run_deploy_and_invoke(
            &client,
            &api_base,
            &auth,
            "test-dep-fail",
            Some("img:latest"),
            None,
            "local",
            None,
            None,
            None,
            &[],
            8080,
            "/health",
            "/predict",
            serde_json::json!({"task": "single_point"}),
            30,
            false,
            Duration::from_millis(1),
        )
        .await;

        let err = result.expect_err("a failed invoke must never report success");
        let msg = err.to_string();
        assert!(msg.contains("dep-fail"), "got: {msg}");
        assert!(msg.contains("auto_stopped=true"), "got: {msg}");

        stop_mock.assert_async().await;
    }

    #[tokio::test]
    async fn compute_run_happy_path_returns_real_result() {
        let mut server = mockito::Server::new_async().await;
        let api_base = server.url();

        server
            .mock("POST", "/compute/submit")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"job_id":"job-1"}"#)
            .create_async()
            .await;
        server
            .mock("GET", "/compute/job-1")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status":"completed","result":{"score":0.9}}"#)
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let auth = PlatformAuth::ApiKey("test-key".to_string());
        let result = run_compute_job(
            &client,
            &api_base,
            &auth,
            "marc27/mace:latest",
            serde_json::json!({}),
            None,
            None,
            None,
            None,
            &[],
            30,
            Duration::from_millis(1),
        )
        .await
        .expect("happy path must succeed");

        assert_eq!(result["job_id"], "job-1");
        assert_eq!(result["status"], "completed");
        assert_eq!(result["result"]["score"], 0.9);
    }

    #[tokio::test]
    async fn compute_run_failure_path_surfaces_real_error_not_success() {
        let mut server = mockito::Server::new_async().await;
        let api_base = server.url();

        server
            .mock("POST", "/compute/submit")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"job_id":"job-2"}"#)
            .create_async()
            .await;
        server
            .mock("GET", "/compute/job-2")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status":"failed","error":"OOM killed"}"#)
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let auth = PlatformAuth::ApiKey("test-key".to_string());
        let result = run_compute_job(
            &client,
            &api_base,
            &auth,
            "marc27/mace:latest",
            serde_json::json!({}),
            None,
            None,
            None,
            None,
            &[],
            30,
            Duration::from_millis(1),
        )
        .await;

        let err = result.expect_err("a failed job must never report success");
        let msg = err.to_string();
        assert!(msg.contains("job-2"), "got: {msg}");
        assert!(msg.contains("failed"), "got: {msg}");
        assert!(msg.contains("OOM killed"), "got: {msg}");
    }

    #[tokio::test]
    async fn ingest_and_wait_happy_path_returns_graph_refs() {
        let mut server = mockito::Server::new_async().await;
        let api_base = server.url();

        server
            .mock("POST", "/knowledge/ingest-job")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"job_id":"ing-1","status":"queued"}"#)
            .create_async()
            .await;
        server
            .mock("GET", "/knowledge/ingest-jobs")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"jobs":[{"job_id":"ing-1","status":"completed","graph_refs":["Entity:Ti-6Al-4V"]}]}"#,
            )
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let auth = PlatformAuth::ApiKey("test-key".to_string());
        let result = run_ingest_job(
            &client,
            &api_base,
            &auth,
            Some("https://example.org/paper.pdf"),
            None,
            "full",
            30,
            Duration::from_millis(1),
        )
        .await
        .expect("happy path must succeed");

        assert_eq!(result["job_id"], "ing-1");
        assert_eq!(result["status"], "completed");
        assert_eq!(result["graph_refs"][0], "Entity:Ti-6Al-4V");
    }

    #[tokio::test]
    async fn ingest_and_wait_failure_path_surfaces_real_error_not_success() {
        let mut server = mockito::Server::new_async().await;
        let api_base = server.url();

        server
            .mock("POST", "/knowledge/ingest-job")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"job_id":"ing-2","status":"queued"}"#)
            .create_async()
            .await;
        server
            .mock("GET", "/knowledge/ingest-jobs")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"jobs":[{"job_id":"ing-2","status":"failed","error":"bad pdf"}]}"#)
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let auth = PlatformAuth::ApiKey("test-key".to_string());
        let result = run_ingest_job(
            &client,
            &api_base,
            &auth,
            Some("https://example.org/broken.pdf"),
            None,
            "full",
            30,
            Duration::from_millis(1),
        )
        .await;

        let err = result.expect_err("a failed ingest job must never report success");
        let msg = err.to_string();
        assert!(msg.contains("ing-2"), "got: {msg}");
        assert!(msg.contains("failed"), "got: {msg}");
        assert!(msg.contains("bad pdf"), "got: {msg}");
    }

    // ── Local Turso ontology read (query fallback chain) ─────────────

    /// Tempfile-backed Turso DB, removed (with SQLite journal sidecars) on drop.
    struct TempProvenanceDb {
        path: PathBuf,
    }

    impl TempProvenanceDb {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("prism_cli_local_query_{}.db", uuid::Uuid::new_v4()));
            Self { path }
        }
    }

    impl Drop for TempProvenanceDb {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let mut p = self.path.clone().into_os_string();
                p.push(suffix);
                let _ = std::fs::remove_file(p);
            }
        }
    }

    fn test_node(name: &str, tenant: &str) -> prism_provenance::GraphNode {
        prism_provenance::GraphNode {
            name: name.into(),
            entity_type: "Alloy".into(),
            label: "Matter".into(),
            class_iri: Some("https://w3id.org/emmo#EMMO_example_alloy".into()),
            tenant: tenant.into(),
        }
    }

    fn test_recalled_fact(object: &str, tenant: &str) -> prism_provenance::RecalledMaterialFact {
        prism_provenance::RecalledMaterialFact {
            subject: "Ti-6Al-4V".into(),
            predicate: "hasProperty".into(),
            object: object.into(),
            value: None,
            unit: None,
            conditions: Vec::new(),
            evidence_class: prism_provenance::EvidenceClass::Research,
            confidence: 0.9,
            source: "doc:test".into(),
            agent: "prism-ingest".into(),
            tenant: tenant.into(),
        }
    }

    #[test]
    fn local_ontology_formatting_matches_neo4j_shape() {
        let results = LocalOntologyResults {
            nodes: vec![test_node("Ti-6Al-4V", "local")],
            edges: vec![prism_provenance::GraphEdge {
                source: "Ti-6Al-4V".into(),
                target: "alpha phase".into(),
                rel_type: "hasPart".into(),
                count: 1,
                tenant: "local".into(),
            }],
            facts: vec![test_recalled_fact("tensile strength", "local")],
        };
        let out = format_local_ontology(&results);
        // Entity lines display the declared extraction type, independently
        // of the compatibility storage label (`Matter`).
        assert!(out.contains("  [Alloy] Ti-6Al-4V\n"), "got: {out}");
        assert!(
            out.contains("Ti-6Al-4V -[hasPart]-> alpha phase\n"),
            "got: {out}"
        );
        // The fact line carries the evidence class — a claim must be
        // visibly classed, for local facts as much as for peer facts.
        assert!(
            out.contains(
                "Ti-6Al-4V -[hasProperty]-> tensile strength  \
                 (confidence 0.90, evidence research, source doc:test)"
            ),
            "got: {out}"
        );
        // An all-local result prints NO peer markers anywhere.
        assert!(!out.contains("[peer"), "got: {out}");
        assert!(out.contains("(local ontology)"), "got: {out}");
    }

    /// THE POINT of the union read: a peer row must be visibly a peer row.
    /// A local and a peer entity carrying the SAME name both appear, each
    /// attributed — neither shadows the other, and only the peer one is
    /// tagged.
    #[test]
    fn peer_rows_are_visibly_attributed_and_local_rows_are_not() {
        let results = LocalOntologyResults {
            nodes: vec![
                test_node("Ti-6Al-4V", "local"),
                test_node("Ti-6Al-4V", "mesh:node-a"),
            ],
            edges: vec![prism_provenance::GraphEdge {
                source: "Ti-6Al-4V".into(),
                target: "beta phase".into(),
                rel_type: "hasPart".into(),
                count: 1,
                tenant: "mesh:node-a".into(),
            }],
            facts: vec![
                test_recalled_fact("tensile strength", "local"),
                test_recalled_fact("elongation", "mesh:node-a"),
            ],
        };
        let out = format_local_ontology(&results);

        // Both same-named entities appear; exactly the peer one is tagged.
        assert!(out.contains("  [Alloy] Ti-6Al-4V\n"), "got: {out}");
        assert!(
            out.contains("  [Alloy] Ti-6Al-4V  [peer mesh:node-a]\n"),
            "got: {out}"
        );
        // The header separates local, reference, and peer counts.
        assert!(
            out.contains("(1 local, 0 reference, 1 from mesh peers)"),
            "got: {out}"
        );
        // Peer relationship and fact lines carry the marker; local fact
        // lines do not.
        assert!(
            out.contains("Ti-6Al-4V -[hasPart]-> beta phase  [peer mesh:node-a]"),
            "got: {out}"
        );
        assert!(
            out.contains(
                "Ti-6Al-4V -[hasProperty]-> elongation  \
                 (confidence 0.90, evidence research, source doc:test)  [peer mesh:node-a]"
            ),
            "got: {out}"
        );
        assert!(
            out.contains(
                "Ti-6Al-4V -[hasProperty]-> tensile strength  \
                 (confidence 0.90, evidence research, source doc:test)\n"
            ),
            "got: {out}"
        );
    }

    /// Reference-ontology rows (`local@matkg`, …) are visibly attributed —
    /// but never as a PEER: labelling loaded reference data `[peer …]`
    /// would claim a mesh origin it does not have.
    #[test]
    fn reference_ontology_rows_are_labelled_but_never_as_peers() {
        let results = LocalOntologyResults {
            nodes: vec![
                test_node("LiFePO4", "local"),
                test_node("LiFePO4", "local@matkg"),
            ],
            edges: vec![],
            facts: vec![test_recalled_fact("Olivine", "local@matkg")],
        };
        let out = format_local_ontology(&results);
        assert!(out.contains("  [Alloy] LiFePO4  [matkg]\n"), "got: {out}");
        assert!(
            out.contains("(1 local, 1 reference, 0 from mesh peers)"),
            "got: {out}"
        );
        assert!(
            !out.contains("[peer"),
            "reference data must never be labelled as a peer: {out}"
        );
        assert!(out.contains("evidence research"), "got: {out}");
    }

    /// The documented `#[serde(default)]` path: a pre-attribution payload
    /// deserializes its tenant as `""`, which must render as LOCAL (no
    /// marker) — never as an invented peer.
    #[test]
    fn empty_tenant_renders_as_local_not_an_invented_peer() {
        assert_eq!(peer_tag(""), "");
        assert_eq!(peer_tag(LOCAL_ONTOLOGY_TENANT), "");
        assert_eq!(peer_tag("mesh:node-a"), "  [peer mesh:node-a]");
        assert_eq!(peer_tag("mesh"), "  [peer mesh]");
    }

    /// The CLI half of the laundering tripwire: facts that the mesh
    /// already asserts are flagged with their holding tenants; facts
    /// nobody synced are not. A future refactor that drops this block
    /// from the ingest path breaks this test, not just a stderr line.
    #[tokio::test]
    async fn collect_peer_echoes_flags_facts_the_mesh_already_asserts() {
        let db = TempProvenanceDb::new();
        let store = prism_provenance::ProvenanceStore::open(&db.path)
            .await
            .expect("open temp store");
        let now = chrono::Utc::now().to_rfc3339();
        let peer = prism_provenance::LocalProvenance {
            activity_id: "act_peer".into(),
            agent_id: "peer-sync".into(),
            agent_kind: "SoftwareAgent".into(),
            source_entity_id: "doc:peer".into(),
            source_kind: "Document".into(),
            tenant: "mesh:node-a".into(),
            started_at: now.clone(),
            ended_at: now,
            locality: "mesh".into(),
            origin_source_id: None,
        };
        store
            .write_fact(
                &prism_provenance::LocalFact {
                    subject: "Ti-6Al-4V".into(),
                    predicate: "has_phase".into(),
                    object: "beta".into(),
                    value: None,
                    unit: None,
                    confidence: Some(0.9),
                    kind: Some("phase".into()),
                },
                &peer,
            )
            .await
            .unwrap();

        let material_fact = |subject: &str, object: &str| prism_provenance::MaterialFact {
            subject: subject.into(),
            predicate: "has_phase".into(),
            object: object.into(),
            value: None,
            unit: None,
            conditions: Vec::new(),
            confidence: Some(0.9),
            kind: Some("phase".into()),
            evidence_class: prism_provenance::EvidenceClass::Research,
        };
        let facts = vec![
            material_fact("Ti-6Al-4V", "beta"),      // the peer's fact, echoed
            material_fact("Inconel 718", "gamma''"), // genuinely new
        ];

        let (echoes, errors) = collect_peer_echoes(&store, &facts).await;
        assert!(errors.is_empty(), "no check may fail here: {errors:?}");
        assert_eq!(echoes.len(), 1, "exactly the echoed fact is flagged");
        assert_eq!(echoes[0]["object"], "beta");
        assert_eq!(
            echoes[0]["peer_tenants"],
            serde_json::json!(["mesh:node-a"])
        );
    }

    #[tokio::test]
    async fn local_ontology_lookup_reads_ingested_facts_and_is_empty_safe() {
        let db = TempProvenanceDb::new();

        // Fresh (empty) store: clean miss, never an error.
        assert!(
            local_ontology_lookup(&db.path, "titanium", 10)
                .await
                .is_none(),
            "empty store must be a clean miss"
        );

        // Write one EMMO fact the way `prism ingest` does (tenant "local").
        let store = prism_provenance::ProvenanceStore::open(&db.path)
            .await
            .expect("open temp store");
        let now = chrono::Utc::now().to_rfc3339();
        let prov = prism_provenance::LocalProvenance {
            activity_id: "act_test".into(),
            agent_id: "prism-ingest".into(),
            agent_kind: "SoftwareAgent".into(),
            source_entity_id: "doc:test".into(),
            source_kind: "Document".into(),
            tenant: LOCAL_ONTOLOGY_TENANT.into(),
            started_at: now.clone(),
            ended_at: now,
            locality: "local".into(),
            origin_source_id: None,
        };
        store.record_activity(&prov).await.expect("record activity");
        store
            .write_fact(
                &prism_provenance::LocalFact {
                    subject: "Ti-6Al-4V".into(),
                    predicate: "hasPart".into(),
                    object: "alpha phase".into(),
                    value: None,
                    unit: None,
                    confidence: Some(0.9),
                    kind: Some("contains".into()),
                },
                &prov,
            )
            .await
            .expect("write fact");

        // Exact name → neighbor traversal (nodes + edge). A "contains"
        // fact is written as a CONTAINS_ELEMENT edge (see emmo write_fact).
        let hit = local_ontology_lookup(&db.path, "Ti-6Al-4V", 10)
            .await
            .expect("ingested entity must be queryable");
        assert!(hit.nodes.iter().any(|n| n.name == "Ti-6Al-4V"));
        assert!(
            hit.edges
                .iter()
                .any(|e| e.rel_type == "CONTAINS_ELEMENT" && e.target == "alpha phase")
        );

        // Substring → graph_search fallback still finds the node.
        let hit = local_ontology_lookup(&db.path, "6Al", 10)
            .await
            .expect("substring match must be queryable");
        assert!(hit.nodes.iter().any(|n| n.name == "Ti-6Al-4V"));

        // Unknown term → clean miss (caller renders "no matches").
        assert!(
            local_ontology_lookup(&db.path, "no-such-entity-xyz", 10)
                .await
                .is_none()
        );

        // Unopenable path (directory) → clean miss, never an error.
        assert!(
            local_ontology_lookup(&std::env::temp_dir(), "titanium", 10)
                .await
                .is_none(),
            "store open failure must degrade to a miss"
        );
    }

    /// Peer knowledge is read BY DEFAULT (the owner chose default-on over
    /// an opt-in flag), and a peer entity sharing the local entity's name
    /// must not shadow it — both rows return, each naming its tenant.
    /// This pins the CLI's default scope wiring: a read pinned back to
    /// `tenant = "local"` makes the peer row vanish and this test fail.
    #[tokio::test]
    async fn local_ontology_lookup_shows_peer_knowledge_by_default_attributed() {
        let db = TempProvenanceDb::new();
        let store = prism_provenance::ProvenanceStore::open(&db.path)
            .await
            .expect("open temp store");
        let now = chrono::Utc::now().to_rfc3339();
        let local = prism_provenance::LocalProvenance {
            activity_id: "act_local".into(),
            agent_id: "prism-ingest".into(),
            agent_kind: "SoftwareAgent".into(),
            source_entity_id: "doc:local".into(),
            source_kind: "Document".into(),
            tenant: LOCAL_ONTOLOGY_TENANT.into(),
            started_at: now.clone(),
            ended_at: now.clone(),
            locality: "local".into(),
            origin_source_id: None,
        };
        let peer = prism_provenance::LocalProvenance {
            activity_id: "act_peer".into(),
            source_entity_id: "doc:peer".into(),
            tenant: "mesh:node-a".into(),
            ..local.clone()
        };
        let fact = |object: &str| prism_provenance::LocalFact {
            subject: "Ti-6Al-4V".into(),
            predicate: "has_phase".into(),
            object: object.into(),
            value: None,
            unit: None,
            confidence: Some(0.9),
            kind: Some("phase".into()),
        };
        store.write_fact(&fact("alpha"), &local).await.unwrap();
        store.write_fact(&fact("beta"), &peer).await.unwrap();

        let hit = local_ontology_lookup(&db.path, "Ti-6Al-4V", 10)
            .await
            .expect("both tenants' knowledge must be readable");

        // The same-named entity appears once PER TENANT — the shadowing
        // trap: if either row disappears, a sync became invisible.
        assert!(
            hit.nodes
                .iter()
                .any(|n| n.name == "Ti-6Al-4V" && n.tenant == "local"),
            "local entity lost: {:?}",
            hit.nodes
        );
        assert!(
            hit.nodes
                .iter()
                .any(|n| n.name == "Ti-6Al-4V" && n.tenant == "mesh:node-a"),
            "peer entity invisible by default: {:?}",
            hit.nodes
        );

        // Facts arrive attributed, with the evidence class present.
        let alpha = hit
            .facts
            .iter()
            .find(|f| f.object == "alpha")
            .expect("local fact");
        assert_eq!(alpha.tenant, "local");
        let beta = hit
            .facts
            .iter()
            .find(|f| f.object == "beta")
            .expect("peer fact");
        assert_eq!(beta.tenant, "mesh:node-a");
    }

    // ── Lazy venv provisioning: who really needs the interpreter ───────
    //
    // Asserted through clap rather than by hand-building `Commands`, so a
    // renamed flag or a subcommand that stops parsing shows up here too —
    // and so the typo case exercises the real external-subcommand path.

    fn needs_python(argv: &[&str]) -> bool {
        let cli = Cli::try_parse_from(argv).unwrap_or_else(|e| panic!("{argv:?} must parse: {e}"));
        command_needs_python(cli.command.as_ref())
    }

    /// The one that mattered most: `prism login` is the FIRST command a new
    /// user runs. It is pure HTTP — `perform_full_login` only records the
    /// interpreter path as a string — so on a machine without Python 3.11+
    /// it used to die on "No Python 3.11+ found" before the device flow ever
    /// started, with no way forward.
    #[test]
    fn login_does_not_provision_a_venv() {
        assert!(
            !needs_python(&["prism", "login"]),
            "`prism login` must work on a machine with no Python at all"
        );
    }

    /// A mistyped command is about to be told it is unknown. Spending ~30 s
    /// building a virtualenv first is the most obviously wasted wait in the
    /// product.
    #[test]
    fn a_typo_does_not_provision_a_venv() {
        let cli = Cli::try_parse_from(["prism", "verison"]).expect("typos reach the catch-all");
        assert!(
            matches!(cli.command, Some(Commands::External(_))),
            "expected clap's external-subcommand catch-all, got {:?}",
            cli.command
        );
        assert!(!command_needs_python(cli.command.as_ref()));
    }

    /// One-shot commands that only talk to the platform over HTTP. None of
    /// their handlers accepts the interpreter path.
    #[test]
    fn one_shot_platform_commands_do_not_provision_a_venv() {
        for argv in [
            ["prism", "billing"].as_slice(),
            &["prism", "marketplace", "list"],
            &["prism", "mesh", "discover"],
            &["prism", "workflow", "list"],
            &["prism", "models", "list"],
            &["prism", "gpus"],
            // Already denied before this change — pinned so they stay denied.
            &["prism", "status"],
            &["prism", "doctor"],
            &["prism", "use", "list"],
        ] {
            assert!(!needs_python(argv), "{argv:?} must not build a venv");
        }
    }

    /// The deny-list must stay a deny-list. These genuinely spawn the Python
    /// tool server, and `node up` is the reason `Commands::Node` is NOT
    /// denied: this match cannot see which node subcommand was given.
    #[test]
    fn commands_that_spawn_the_tool_server_still_provision_a_venv() {
        for argv in [
            ["prism", "node", "up"].as_slice(),
            &["prism", "tools"],
            &["prism", "tui"],
        ] {
            assert!(needs_python(argv), "{argv:?} really does need Python");
        }
        // Bare `prism` is the TUI, which spawns the backend.
        assert!(command_needs_python(None), "bare `prism` launches the TUI");
    }

    // ── Background marketplace sync: no account, no phone-home ─────────
    //
    // These share `boot_checks::ENV_LOCK` with the boot_checks tests: both
    // clear the same three platform token vars, and two separate locks
    // would not serialize against each other.

    fn creds_with(token: &str) -> prism_runtime::StoredCredentials {
        prism_runtime::StoredCredentials {
            access_token: token.to_string(),
            ..Default::default()
        }
    }

    /// The defect: bare `prism` and `prism resume` on a fresh install with
    /// no account issued an unauthenticated GET to the hosted marketplace.
    #[test]
    fn no_account_means_no_marketplace_call() {
        let _guard = boot_checks::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        boot_checks::clear_platform_env();

        assert!(
            !should_sync_tools(None, None),
            "bare `prism` with no account must not contact the platform"
        );
        let resume = Cli::try_parse_from(["prism", "resume"]).unwrap();
        assert!(
            !should_sync_tools(resume.command.as_ref(), None),
            "`prism resume` with no account must not contact the platform"
        );
        // A credentials file left behind by a logout is not a credential.
        assert!(!should_sync_tools(None, Some(&creds_with("   "))));
    }

    #[test]
    fn hard_offline_mode_never_starts_marketplace_sync() {
        let _guard = boot_checks::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        boot_checks::clear_platform_env();
        unsafe {
            std::env::set_var(prism_runtime::offline::ENV, "1");
        }
        assert!(!should_sync_tools(None, Some(&creds_with("token-abc"))));
        unsafe {
            std::env::remove_var(prism_runtime::offline::ENV);
        }
    }

    /// …but a signed-in user still gets fresh tools. The fix must not have
    /// simply switched the sync off.
    #[test]
    fn a_signed_in_user_still_syncs_tools() {
        let _guard = boot_checks::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        boot_checks::clear_platform_env();

        assert!(should_sync_tools(None, Some(&creds_with("token-abc"))));
        let resume = Cli::try_parse_from(["prism", "resume"]).unwrap();
        assert!(should_sync_tools(
            resume.command.as_ref(),
            Some(&creds_with("token-abc"))
        ));
    }

    #[test]
    fn provision_commands_parse_the_agent_actionable_forms() {
        let extra = Cli::try_parse_from(["prism", "provision", "extra", "mace"]).unwrap();
        assert!(matches!(
            extra.command,
            Some(Commands::Provision {
                command: ProvisionCommands::Extra { name, wheelhouse: None }
            }) if name == "mace"
        ));

        let wheels = Cli::try_parse_from([
            "prism",
            "provision",
            "wheels",
            "--output",
            "/tmp/prism-wheels",
            "--extra",
            "mace,ml",
        ])
        .unwrap();
        assert!(matches!(
            wheels.command,
            Some(Commands::Provision {
                command: ProvisionCommands::Wheels { extras, .. }
            }) if extras == vec!["mace".to_string(), "ml".to_string()]
        ));
    }

    /// The command gate is unchanged: one-shot commands never synced and
    /// still must not, credential or no credential.
    #[test]
    fn one_shot_commands_never_sync_tools() {
        let _guard = boot_checks::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        boot_checks::clear_platform_env();

        let signed_in = creds_with("token-abc");
        for argv in [
            ["prism", "doctor"].as_slice(),
            &["prism", "billing"],
            &["prism", "status"],
        ] {
            let cli = Cli::try_parse_from(argv).unwrap();
            assert!(
                !should_sync_tools(cli.command.as_ref(), Some(&signed_in)),
                "{argv:?} is one-shot and must not sync"
            );
        }
    }

    // ── `[ontology]` config knob → local ingest ────────────────────────

    /// Write a project-scoped prism.toml; project config REPLACES the
    /// user's global one in `NodeConfig::load`, so these tests are
    /// deterministic regardless of ~/.prism/prism.toml.
    fn project_with_ontology_config(body: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".prism")).unwrap();
        std::fs::write(dir.path().join(".prism/prism.toml"), body).unwrap();
        dir
    }

    /// The `[ontology] id` knob ACTUALLY selects: the configured id travels
    /// from prism.toml through `run_local_ingest_file` into the pipeline's
    /// registry lookup. An unregistered id is refused loudly (naming what is
    /// registered), the default id ingests, and an unimplemented
    /// `[ontology] engine` — a knob nothing used to read — now refuses
    /// instead of silently meaning "llm".
    #[tokio::test]
    async fn ontology_config_knob_selects_the_ingest_vocabulary() {
        let dir = project_with_ontology_config("[ontology]\nid = \"zzz-unregistered\"\n");
        let root = dir.path();
        let csv = root.join("data.csv");
        std::fs::write(&csv, "a\n1\n").unwrap();

        let err = run_local_ingest_file(&csv, root, None, None, None, true, None)
            .await
            .expect_err("an unregistered configured ontology must refuse ingest");
        let msg = format!("{err:#}");
        assert!(msg.contains("zzz-unregistered"), "{msg}");
        assert!(msg.contains("emmo"), "{msg}");

        // The default id resolves and ingests (schema-only run).
        std::fs::write(
            root.join(".prism/prism.toml"),
            "[ontology]\nid = \"emmo\"\n",
        )
        .unwrap();
        let out = run_local_ingest_file(&csv, root, None, None, None, true, None)
            .await
            .expect("the default ontology must ingest");
        assert_eq!(out["backend"], "local_tabular");

        // An unimplemented engine is a loud refusal, not a silent "llm".
        std::fs::write(
            root.join(".prism/prism.toml"),
            "[ontology]\nengine = \"dmms\"\n",
        )
        .unwrap();
        let err = run_local_ingest_file(&csv, root, None, None, None, true, None)
            .await
            .expect_err("an unimplemented ontology engine must refuse");
        let msg = format!("{err:#}");
        assert!(msg.contains("dmms"), "{msg}");
        assert!(msg.contains("llm"), "{msg}");
    }

    /// Text-document ingest is EMMO-wired (EMMO prompt, QUDT-typed facts):
    /// under any other active ontology it must refuse honestly BEFORE any
    /// model or runtime is contacted, not extract with the wrong vocabulary.
    #[tokio::test]
    async fn text_ingest_refuses_a_non_default_ontology_honestly() {
        let dir = project_with_ontology_config("[ontology]\nid = \"chem\"\n");
        let root = dir.path();
        let md = root.join("notes.md");
        std::fs::write(&md, "# title\nbody text\n").unwrap();

        // TEST-NET-1 runtime URL: the guard fires before anything is
        // contacted, so an unreachable address is part of the proof.
        let err = run_local_text_ingest_file(
            &md,
            root,
            None,
            None,
            None,
            "http://192.0.2.1:1",
            true,
            None,
        )
        .await
        .expect_err("a non-default ontology must refuse text ingest");
        let msg = format!("{err:#}");
        assert!(msg.contains("EMMO"), "{msg}");
        assert!(msg.contains("chem"), "{msg}");
    }
}
