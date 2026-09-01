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
mod model_install;
mod notebook;
mod onboarding;
mod ontology_cmd;
mod papers;
mod plugins_cmd;
use prism_core::providers;
mod pyiron_cmd;
mod reverify_cmd;
mod tool_sync;
mod use_command;

use std::collections::BTreeMap;
use std::io::{self, IsTerminal, Write};
use std::num::{NonZeroU8, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
// std::process::Stdio removed — old Ink TUI launcher no longer needed
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use clap::{Parser, Subcommand};
use prism_client::DeviceFlowAuth;
use prism_client::PlatformResponseExt;
use prism_client::api::PlatformClient;
use prism_client::auth::{
    DeviceCodeResponse, IdentityProviderAdapter, MARC27_IDENTITY_PROVIDER,
    SUPABASE_IDENTITY_PROVIDER, TokenResponse, identity_provider_for,
};
use prism_client::supabase_auth::{SupabaseAuth, SupabaseAuthPolicy};
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

/// Project selected by the top-level `--project-root`. Platform auth helpers
/// are called from many command handlers; retaining the parsed root here keeps
/// every one on the same config instead of silently reloading the cwd.
static CLI_PROJECT_ROOT: OnceLock<PathBuf> = OnceLock::new();

// Deliberately no `Debug`: parsed values include login tokens and Supabase
// anon keys, and a derived formatter would print both verbatim.
#[derive(Parser)]
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

// Deliberately no `Debug`: `Login` owns secret-bearing command/env values.
/// Arguments for `prism run`. A named struct (rather than inline
/// variant fields) so the enum stays small next to its one-word variants.
#[derive(Debug, clap::Args)]
struct RunArgs {
    /// Container image to run, or a pre-staged .sif path for SLURM.
    image: String,
    /// Job name.
    #[arg(long, default_value = "experiment")]
    name: String,
    /// JSON inputs (key=value pairs merged into inputs object).
    #[arg(long, value_delimiter = ',')]
    input: Vec<String>,
    /// Backend: local, marc27, byoc, or hyperqueue.
    #[arg(long, default_value = "local")]
    backend: String,
    /// Platform API URL (for the `marc27` backend). When omitted, resolve
    /// PRISM_API_URL, config, or the endpoint stored by `prism login`.
    #[arg(long)]
    platform_url: Option<String>,
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
    /// HyperQueue task-set file: a JSON array of task objects
    /// `[{{"command": ["..."], "cwd": "...", "env": {{...}}}}, ...]`.
    /// Required for `--backend hyperqueue`; task sets do not fit
    /// `--input key=value` strings.
    #[arg(long)]
    hq_tasks: Option<String>,
    /// HyperQueue worker count for standalone mode (default 2).
    #[arg(long, default_value_t = 2)]
    hq_workers: u32,
    /// HyperQueue server directory (default <data_dir>/hyperqueue).
    #[arg(long)]
    hq_server_dir: Option<String>,
    /// HyperQueue automatic allocation scheduler: `slurm` or `pbs`.
    /// Switches from standalone local workers to HQ-managed allocations.
    #[arg(long)]
    hq_autoalloc: Option<String>,
    /// Walltime for HyperQueue automatic allocations, e.g. `1h`.
    #[arg(long, default_value = "1h")]
    hq_time_limit: String,
    /// Extra sbatch/qsub argument for HyperQueue allocations; repeatable
    /// (e.g. `--hq-extra --partition=main`).
    #[arg(long = "hq-extra", allow_hyphen_values = true)]
    hq_extra: Vec<String>,
    /// Emit machine-readable JSON instead of human-readable status lines.
    #[arg(long)]
    json: bool,
}

#[derive(Subcommand)]
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
    /// Use `--token <PAT>` for a platform token, `--provider supabase` for
    /// passwordless email PKCE, or `--provider marc27 --interactive-auth` for
    /// the retained device flow. PRISM never opens a browser automatically.
    Login {
        /// Use a pre-issued Personal Access Token from the platform website.
        /// This is non-interactive and suitable for headless runs.
        #[arg(
            long,
            value_name = "PAT",
            env = "PRISM_LOGIN_TOKEN",
            hide_env_values = true
        )]
        token: Option<String>,

        /// Identity provider used for interactive login (`marc27`,
        /// `supabase`, or `mirdyne`). Unknown values fail closed.
        ///
        /// `mirdyne` is a SEPARATE identity domain from `marc27` — its own
        /// issuer, its own accounts, its own principals. Signing in to one is
        /// not signing in to the other.
        #[arg(long, value_name = "PROVIDER", conflicts_with = "token")]
        provider: Option<String>,

        /// Enterprise SAML SSO by email domain, e.g. `--sso-domain acme.com`.
        ///
        /// The SAML exchange happens between your organisation's identity
        /// provider and PRISM's — PRISM never parses a SAML assertion. What
        /// returns here is the same signed token a passwordless login yields.
        #[arg(long, value_name = "DOMAIN", conflicts_with_all = ["token", "email"])]
        sso_domain: Option<String>,

        /// Enterprise SAML SSO by explicit connection id, for organisations
        /// with several connections or none registered against a domain.
        #[arg(
            long,
            value_name = "ID",
            conflicts_with_all = ["token", "email", "sso_domain"]
        )]
        sso_provider_id: Option<String>,

        /// Email address for Supabase's passwordless magic-link PKCE flow.
        #[arg(long, env = "PRISM_LOGIN_EMAIL", conflicts_with = "token")]
        email: Option<String>,

        /// Supabase project root. There is deliberately no hosted default.
        #[arg(
            long,
            env = "PRISM_SUPABASE_URL",
            value_name = "URL",
            conflicts_with = "token"
        )]
        supabase_url: Option<String>,

        /// Supabase anon/publishable key used only with Supabase Auth.
        #[arg(
            long,
            env = "PRISM_SUPABASE_ANON_KEY",
            value_name = "KEY",
            hide_env_values = true,
            conflicts_with = "token"
        )]
        supabase_anon_key: Option<String>,

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
    /// Induce, validate, and promote domain ontologies (corpus → TTL
    /// artifact). Ontologies are produced BY the LLM from source text —
    /// induction emits a versioned, provenance-stamped DRAFT artifact;
    /// promotion is the deliberate act that accepts it.
    Ontology {
        #[command(subcommand)]
        command: crate::ontology_cmd::OntologyCommands,
    },
    /// Re-read a stored assertion's exact cited source lines and ask the
    /// configured model whether they support it — the re-check that makes
    /// annotate-don't-refuse honest. Lists the span-unchecked population
    /// (`cited_by_reader`) and other re-checkable statuses; every verdict is
    /// recorded in the reverify ledger. Local + one LLM call per witness.
    Reverify {
        #[command(subcommand)]
        command: crate::reverify_cmd::ReverifyCommands,
    },
    /// List every extension plane's inventory — loaded and failed plugins,
    /// configured MCP servers, skills, workflows, policies, ontologies.
    /// ONE list surface for the standard plugin contract; the same view is
    /// reachable in the TUI (`/plugins list`) and by the agent (`plugins`
    /// tool). Local and offline.
    Plugins {
        #[command(subcommand)]
        command: crate::plugins_cmd::PluginsCommands,
    },
    /// Ingest a data file into the knowledge graph.
    Ingest {
        /// Path to a file or directory to ingest. Omit with `--status`.
        path: Option<PathBuf>,
        /// Corpus slug to associate with the ingested data.
        #[arg(long)]
        corpus: Option<String>,
        /// Model that READS PAGE IMAGES when the text layer cannot recover a
        /// page (scanned pages, figures, broken font encodings).
        ///
        /// Reading images and extracting facts are different capabilities. A
        /// text-only extraction model handed a page image returns HTTP 400,
        /// the page is reported unreadable, and the cause looks like a bad
        /// PDF when it is really a bad configuration. Defaults to --model,
        /// which is correct for a multimodal local model.
        #[arg(long, value_name = "MODEL", env = "LLM_VISION_MODEL")]
        vision_model: Option<String>,

        /// Base URL for the vision model, when it is not served by the same
        /// endpoint as --llm-url. Defaults to --llm-url.
        #[arg(long, value_name = "URL", env = "LLM_VISION_URL")]
        vision_url: Option<String>,

        /// Extract each document this many times and keep only facts that
        /// recur (see --agreement).
        ///
        /// A small extraction model does not fail deterministically — it
        /// invents, differently each run. Measured on one 36-page LPBF paper,
        /// five passes over identical text stored 4, 0, 2, 3 and 1 facts, and
        /// a third to two thirds of everything emitted was a number that
        /// appears nowhere in the document. A fabricated value changes
        /// between passes; a printed one does not. Cost is linear in this
        /// number. Default 1 — single pass, no filtering.
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=9))]
        samples: u8,
        /// Passes a fact must appear in before it is believed. Must not
        /// exceed --samples, or nothing could ever clear the bar.
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=9))]
        agreement: u8,
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
        /// Run the Phase-2 REPAIR pass instead of ingesting: drain the
        /// document's repair queue ONE ITEM AT A TIME through the model
        /// tier. `<PATH>` is the document as it was ingested (the queue is
        /// keyed by the path shown at ingest time). Requires the document
        /// to have been ingested locally — refusals the code tiers could
        /// not decide are the only items queued. Every item ends in an
        /// explicit accept or withdraw, ledgered; nothing is batched.
        #[arg(long)]
        repair: bool,
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
        /// Also show facts whose ingest checks did NOT verify them against
        /// their source (stored with a verification status such as
        /// `subject_not_verbatim`). The default shows only the trusted
        /// subset; unverified facts are present and findable, not promoted.
        #[arg(long)]
        include_unverified: bool,
        /// Dashboard URL for federated query peer discovery.
        #[arg(long, default_value = "http://127.0.0.1:7327")]
        dashboard_url: String,
    },
    /// Print available commands for AI agents. Pipe-friendly, grep-friendly.
    Agent,
    /// Submit a compute job (local Docker, the hosted platform, BYOC, or
    /// HyperQueue many-task sets). Boxed so the enum stays small next to
    /// its one-word variants — same pattern as ScheduleCreateArgs.
    Run(Box<RunArgs>),
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
    /// Research a materials-science goal with the local agent, its tools and
    /// its DAG. Runs entirely on this machine — same session the TUI drives.
    Research {
        /// Research goal or question that can trigger iterative search and synthesis.
        query: String,
        /// How hard to decompose. `0` answers directly; above it the goal is
        /// split into sub-questions run with `orchestrate_agents` up to this
        /// depth. The DAG is always available, so this shapes the ask rather
        /// than switching a backend.
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
        /// The DECLARED property the reward ranks on, keyed exactly as the
        /// evaluator reports it (e.g. "Tm_estimate_K"). Reward-property
        /// selection is by declaration, never by parsing the objective's
        /// English words.
        #[arg(long)]
        target_property: Option<String>,
        /// Which way `--target-property` is better: `maximize` or `minimize`.
        /// Required alongside `--target-property`. Declared, never inferred
        /// from the objective's words — "lower the density" used to rank as
        /// maximize.
        #[arg(long)]
        target_direction: Option<String>,
        /// Weighted reward over SEVERAL declared properties, repeatable:
        /// `--reward-weight Tm_estimate_K=1 --reward-weight delta_S_mix_J_per_molK=100`.
        ///
        /// A single `--target-property` optimises one number, and a
        /// single-number objective can have a degenerate optimum. Measured: a
        /// 25-iteration run maximising `Tm_estimate_K` — a rule-of-mixtures
        /// average, so maximised by 100% of the highest-melting element —
        /// climbed monotonically to `W0.995 Re0.005`, reaching 100.0% of pure
        /// tungsten's melting point while its mixing entropy fell from 11.36
        /// to 0.26 J/mol·K. It optimised away from "high-entropy alloy" for
        /// 25 straight iterations, keeping 0.5% Re only to satisfy the
        /// two-element rule. The loop was right; the objective was not.
        ///
        /// Weights are applied to the evaluator's own keys, so a trade-off is
        /// STATED rather than left to a constraint to police.
        #[arg(long = "reward-weight", value_name = "PROPERTY=WEIGHT")]
        reward_weight: Vec<String>,
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
    /// Render a running notebook IN THIS TERMINAL.
    ///
    /// PRISM starts Jupyter but had nowhere to show it without leaving for
    /// a browser, which is the same complaint as exiting to the CLI. This
    /// drives a headless browser and brings the page back here.
    View {
        /// Port of the notebook to view. Defaults to the only running one.
        #[arg(long)]
        port: Option<u16>,
        /// Capture a PNG instead of text. iTerm2 and kitty draw it inline.
        #[arg(long)]
        image: bool,
        /// Where to write the PNG (implies --image).
        #[arg(long)]
        out: Option<String>,
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
    /// Publish PRISM's own materials tools to the configured provider marketplace so
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
    /// Explicitly download and verify one pinned local model.
    Install {
        #[arg(value_enum)]
        model: model_install::InstallableModel,
        /// Output a machine-readable installation report.
        #[arg(long)]
        json: bool,
    },
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
fn preflight_command_auth(command: Option<&Commands>, project_root: &Path) -> Result<()> {
    if let Some(Commands::Node {
        command: NodeCommands::Up { offline, .. },
    }) = command
        && !(*offline || prism_runtime::offline::enabled())
    {
        // `node up` needs Python later, but missing auth must be reported
        // before venv provisioning can block or touch the network.
        let config = prism_core::config::NodeConfig::load(Some(project_root));
        let _ = resolve_agent_auth_with_url(config.platform.url.as_deref())?;
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
        // Ontology induction is Rust + the configured LLM HTTP endpoint;
        // no handler takes the interpreter path.
        Some(Commands::Ontology { .. }) => false,
        // The paper pipeline — search, sweep, full-text, claims — is the Rust
        // retrieval engine, the configured LLM endpoint over HTTP, and the
        // local provenance store; `papers::handle` takes no interpreter path
        // and papers.rs never names Python. Measured before this arm: a
        // `papers claims` run against loopback endpoints spent 63.5 s of its
        // 65 s building a venv (pip reaching PyPI) and 2.1 s doing the work.
        Some(Commands::Papers { .. }) => false,
        // Re-verification re-reads stored facts against their sources over
        // HTTP; `reverify_cmd::run` takes no interpreter path either.
        Some(Commands::Reverify { .. }) => false,
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
        .with_env_filter(prism_runtime::log_filter())
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
    let project_root = cli.project_root.clone();
    let _ = CLI_PROJECT_ROOT.set(project_root.clone());
    preflight_command_auth(cli.command.as_ref(), &project_root)?;
    let paths = PrismPaths::discover()?;
    let platform_config = prism_core::config::NodeConfig::load(Some(&project_root));
    let startup_state = paths.load_cli_state().ok();
    let endpoints = PlatformEndpoints::resolve_for_paths(
        platform_config.platform.url.as_deref(),
        platform_config.platform.provider.as_deref(),
        startup_state
            .as_ref()
            .and_then(|state| state.credentials.as_ref()),
        &paths,
    );

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
    // task that pulls tool updates from the configured provider marketplace. This is
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
        && let Some(endpoints) = endpoints.as_ref()
        && let Ok((_, credential)) = resolve_agent_auth_with_url(Some(endpoints.api_base.as_str()))
    {
        let platform =
            prism_client::api::PlatformClient::new(&endpoints.api_base).with_auth(credential);
        crate::tool_sync::spawn_background_sync_owned(platform);
    }

    match cli.command.unwrap_or(Commands::Tui {
        fake_backend: false,
        scenario: "basic_chat".to_string(),
    }) {
        Commands::Setup { interactive_auth } => {
            let endpoints = require_platform_endpoints(endpoints.as_ref())?;
            let mut state = paths.load_cli_state()?;
            state.preferred_python = Some(python.display().to_string());
            if state.credentials.is_none() {
                let credentials = run_device_login(endpoints, interactive_auth).await?;
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
                    platform_provider: credentials.platform_provider,
                    identity_provider_url: credentials.identity_provider_url,
                    identity_provider_key: credentials.identity_provider_key,
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
                let stored_auth = auth::stored_bearer_for_endpoints(endpoints, creds)?
                    .context("stored platform session has no access token")?;
                let platform = PlatformClient::new(&endpoints.api_base).with_auth(stored_auth);
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
            let mut proactive_refresh_failed = false;
            if let Some(creds) = state.credentials.as_ref()
                && !creds.refresh_token.is_empty()
                && creds
                    .expires_at
                    .is_some_and(|exp| chrono::Utc::now() + chrono::Duration::minutes(5) >= exp)
            {
                match refresh_access_token(&paths, endpoints, creds).await {
                    Ok(new_creds) => {
                        state.credentials = Some(new_creds);
                        tracing::info!("access token refreshed proactively");
                    }
                    Err(e) => {
                        proactive_refresh_failed = true;
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
                boot_checks::run_boot_checks(state.credentials.as_ref(), Some(endpoints)).await;
            let auth_rejected = boot_checks
                .iter()
                .any(|c| c.name == "Auth" && c.result.starts_with("token rejected"));
            if auth_rejected
                && !proactive_refresh_failed
                && let Some(creds) = state.credentials.as_ref()
                && !creds.refresh_token.is_empty()
            {
                match refresh_access_token(&paths, endpoints, creds).await {
                    Ok(new_creds) => {
                        tracing::info!("access token refreshed after server-side rejection");
                        state.credentials = Some(new_creds);
                        // Redo the boot checks with the new token.
                        boot_checks = boot_checks::run_boot_checks(
                            state.credentials.as_ref(),
                            Some(endpoints),
                        )
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
            let platform = tui_platform_auth(Some(endpoints), state.credentials.as_ref())?;
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
            provider,
            email,
            sso_domain,
            sso_provider_id,
            supabase_url,
            supabase_anon_key,
            no_browser,
            interactive_auth,
        } => {
            let login_endpoints = resolve_login_endpoints(
                endpoints.as_ref(),
                provider.as_deref(),
                supabase_url.as_deref(),
            )?;
            let mode = match token {
                Some(pat) => LoginMode::Token(pat),
                None => LoginMode::Provider {
                    provider: selected_identity_provider(
                        provider.as_deref(),
                        login_endpoints.provider.as_deref(),
                    )?,
                    interactive_auth,
                    no_browser,
                    email,
                    supabase_url,
                    supabase_anon_key,
                    // Clap's `conflicts_with_all` already guarantees at most
                    // one of these is set, so this cannot silently prefer one
                    // organisation's connection over another.
                    sso: sso_domain
                        .map(SsoChoice::Domain)
                        .or_else(|| sso_provider_id.map(SsoChoice::ProviderId)),
                },
            };
            perform_full_login(&paths, &login_endpoints, &python, mode).await?;
            println!("Login complete.");
        }
        Commands::Status => {
            let state = paths.load_cli_state()?;
            // Resolve the credential once so a historical alias that would
            // actually supply it emits its one-time migration notice.
            let env_credential_present = endpoints
                .as_ref()
                .and_then(PlatformEndpoints::environment_credential)
                .is_some();
            let stored_credential_present = state
                .credentials
                .as_ref()
                .zip(endpoints.as_ref())
                .and_then(|(credentials, endpoints)| {
                    auth::stored_bearer_for_endpoints(endpoints, credentials)
                        .ok()
                        .flatten()
                })
                .is_some();
            let node_credential_present = paths.load_node_token().is_some_and(|token| {
                endpoints.as_ref().is_some_and(|endpoints| {
                    auth::stored_node_bearer_for_endpoints(endpoints, &token)
                        .ok()
                        .flatten()
                        .is_some()
                })
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "paths": paths,
                    "platform": endpoints.as_ref(),
                    "platform_status": if endpoints.is_some() { "configured" } else { "not configured" },
                    "credentials_present": stored_credential_present || env_credential_present || node_credential_present,
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
                    target_property,
                    target_direction,
                    reward_weight,
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

                    // The direction is declared, never read off the objective's
                    // words: "lower the density" once ranked as maximize and a
                    // campaign spent its budget returning the worst candidates
                    // as best. Refused here, before anything is spent.
                    let target_direction = target_direction
                        .as_deref()
                        .map(str::parse::<prism_campaign::Direction>)
                        .transpose()?;
                    if target_property.is_some() && target_direction.is_none() {
                        bail!(
                            "--target-property requires --target-direction maximize|minimize: \
                             the reward's sign is declared, not inferred from the objective text"
                        );
                    }
                    let campaign_goal = CampaignGoal {
                        description: goal.clone(),
                        elements: elements_vec,
                        objective: objective.clone().unwrap_or_default(),
                        target_property: target_property.clone(),
                        target_direction,
                        constraints: Vec::new(),
                        seeds: Vec::new(),
                    };

                    // Parsed here rather than deeper: a malformed weight is a
                    // typo in the command, and the operator should hear about
                    // it before a campaign starts spending, not on iteration 1.
                    let mut reward_weights = std::collections::BTreeMap::new();
                    for pair in &reward_weight {
                        let (property, weight) = pair.split_once('=').ok_or_else(|| {
                            anyhow!(
                                "invalid --reward-weight {pair:?}. Expected PROPERTY=WEIGHT, \
                                 e.g. --reward-weight Tm_estimate_K=1"
                            )
                        })?;
                        let parsed: f64 = weight.trim().parse().with_context(|| {
                            format!("--reward-weight {pair:?}: {weight:?} is not a number")
                        })?;
                        reward_weights.insert(property.trim().to_string(), parsed);
                    }
                    if !reward_weights.is_empty() && target_property.is_some() {
                        // Both would silently pick one path; say which wins.
                        println!(
                            "  note: --reward-weight is set, so the weighted reward is used \
                             and --target-property is ignored"
                        );
                    }

                    let config = CampaignConfig {
                        max_iterations,
                        batch_size,
                        budget_usd: budget,
                        checkpoint_every,
                        approval_gate_at: gates_vec,
                        project_root: Some(project_root.clone()),
                        reward_weights,
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

                    // No --reward-weight and no --target-property: derive the
                    // objective (one LLM call, validated against the domain's
                    // reward registry) and SHOW it before any compute is
                    // spent, so the operator can argue with it. Explicit
                    // flags always win; a failed derivation falls back to the
                    // domain's documented default policy — an objective is
                    // never fabricated.
                    match campaign.derive_reward_spec().await {
                        Ok(Some(spec)) => {
                            println!(
                                "Objective (derived by {} — no --reward-weight/--target-property given):",
                                spec.derived_by.as_deref().unwrap_or("the configured model")
                            );
                            for line in spec.describe() {
                                println!("  {line}");
                            }
                            println!(
                                "  Not what you want? Restart with --reward-weight PROPERTY=WEIGHT or --target-property PROPERTY."
                            );
                            println!();
                        }
                        Ok(None) => {}
                        Err(error) => {
                            println!(
                                "  note: could not derive a reward objective ({error:#}); \
                                 falling back to the domain's default reward policy"
                            );
                        }
                    }

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
            NotebookCommands::View { port, image, out } => {
                let sessions = notebook::list()?;
                // Naming the alternatives beats "not found": the usual cause
                // is a second notebook running, not a missing one.
                let session = match (port, sessions.len()) {
                    (Some(p), _) => sessions.iter().find(|s| s.port == p).ok_or_else(|| {
                        anyhow::anyhow!(
                            "no notebook on port {p}. Running: {}",
                            if sessions.is_empty() {
                                "none — start one with `prism notebook start`".to_string()
                            } else {
                                sessions
                                    .iter()
                                    .map(|s| s.port.to_string())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            }
                        )
                    })?,
                    (None, 1) => &sessions[0],
                    (None, 0) => anyhow::bail!(
                        "no notebook is running — start one with `prism notebook start`"
                    ),
                    (None, _) => anyhow::bail!(
                        "{} notebooks are running; choose one with --port {}",
                        sessions.len(),
                        sessions
                            .iter()
                            .map(|s| s.port.to_string())
                            .collect::<Vec<_>>()
                            .join(" | --port ")
                    ),
                };
                let mode = if image || out.is_some() {
                    notebook::ViewMode::Image
                } else {
                    notebook::ViewMode::Text
                };
                let rendered = notebook::view(session, mode, out.as_deref())?;
                match mode {
                    notebook::ViewMode::Text => println!("{rendered}"),
                    notebook::ViewMode::Image => {
                        println!("Captured notebook on port {}: {rendered}", session.port);
                        println!(
                            "  iTerm2: imgcat {rendered}    kitty: kitty +kitten icat {rendered}"
                        );
                    }
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
            // `backend` has its own project root. Resolve the platform from
            // that project's config rather than the top-level default root.
            let backend_state = paths.load_cli_state().ok();
            let endpoints = PlatformEndpoints::resolve_for_paths(
                node_config.platform.url.as_deref(),
                node_config.platform.provider.as_deref(),
                backend_state
                    .as_ref()
                    .and_then(|state| state.credentials.as_ref()),
                &paths,
            );

            // Also load ~/.prism/config.toml [chat] — the user-visible
            // chat target set by `prism use local/provider/marc27`.
            // If the user configured a local or direct-provider target,
            // that takes precedence over prism.toml [llm] for the agent
            // backend's LLM endpoint. This unifies the two config worlds
            // so `prism use local` actually affects `prism backend`.
            let chat_target = crate::chat_config::load().unwrap_or_default().chat;

            // Generic key chain for the local/direct-provider targets.
            // Provider keys (ANTHROPIC/OPENAI) belong ONLY here — never on
            // the marc27 arm: now that the project `.env` is actually
            // loaded, an ANTHROPIC_API_KEY in it would otherwise shadow the
            // platform JWT and 401 every platform LLM call.
            let api_key = std::env::var("LLM_API_KEY")
                .or_else(|_| std::env::var("ANTHROPIC_API_KEY"))
                .or_else(|_| std::env::var("OPENAI_API_KEY"))
                .ok()
                .or_else(|| cfg_llm.resolve_api_key());

            // Platform model catalog, fetched ONCE (fail-open: empty when
            // offline). Serves both marc27 model resolution and the limits
            // lookup below.
            let catalog = fetch_model_catalog(&paths).await;

            // Resolve base_url, model, and api_key from the chat target
            // when it overrides the prism.toml [llm] defaults.
            let (base_url, model, api_key, credential_kind) = match &chat_target {
                crate::chat_config::ChatTarget::Local {
                    url,
                    model,
                    api_key: local_key,
                } => (
                    url.clone(),
                    model.clone(),
                    local_key.clone().or(api_key),
                    None,
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
                    let provider_key = std::env::var(&env_name).ok();
                    (
                        provider_endpoint(&registry, provider),
                        model.clone(),
                        provider_key.or(api_key),
                        None,
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
                    let endpoints = require_platform_endpoints(endpoints.as_ref())?;
                    let (base_url, platform_derived) =
                        marc27_llm_base_url_with_source(&paths, &endpoints.api_base, &cfg_llm.url)?;
                    let platform_token = if platform_derived {
                        backend_state
                            .as_ref()
                            .and_then(|state| state.credentials.as_ref())
                            .map(|credentials| {
                                auth::stored_bearer_for_endpoints(endpoints, credentials)
                            })
                            .transpose()?
                            .flatten()
                            .map(|credential| credential.secret().to_string())
                    } else {
                        None
                    };
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
                    // provider API key (PRISM_API_KEY — no login, no expiry)
                    // → PRISM_TOKEN → the logged-in session JWT.
                    // Provider keys are NOT platform credentials.
                    let (marc27_key, credential_kind) =
                        if let Ok(raw_override) = std::env::var("LLM_API_KEY") {
                            (Some(raw_override), None)
                        } else if !platform_derived {
                            (cfg_llm.resolve_api_key(), None)
                        } else if let Some(credential) = endpoints.environment_credential() {
                            match credential {
                                PlatformAuth::ApiKey(key) => (
                                    Some(key),
                                    Some(prism_ingest::llm::LlmCredentialKind::ApiKey),
                                ),
                                PlatformAuth::Bearer(token) => (
                                    Some(token),
                                    Some(prism_ingest::llm::LlmCredentialKind::Bearer),
                                ),
                            }
                        } else {
                            (
                                platform_token.clone(),
                                platform_token
                                    .as_ref()
                                    .map(|_| prism_ingest::llm::LlmCredentialKind::Bearer),
                            )
                        };
                    (base_url, model, marc27_key, credential_kind)
                }
            };

            // The model's real limits from the platform catalog. Drives
            // the agent's context budget: compaction fires on token
            // pressure against THIS window, not a guessed constant.
            // (None, None) for unknown models (local llama.cpp, offline)
            // → the agent falls back to turn-count compaction.
            let (context_window, max_output_tokens) = model_limits(&catalog, &model);
            tracing::info!(?context_window, ?max_output_tokens, model = %model, "model limits");

            // Same two properties the native frontend used to lose to
            // `..Default::default()`: a local server's REAL context window
            // (`model_limits` only knows the catalog, which does not know an
            // unregistered local model) and whether the endpoint can stream at
            // all. Both are endpoint facts, so both are resolved from the
            // endpoint rather than defaulted.
            let context_window = context_window.or_else(|| {
                Some(prism_agent::models::resolve_context_window(&base_url, &model) as u64)
            });
            let streaming = prism_core::providers::streams_for_url(
                &prism_core::providers::Registry::load(),
                &base_url,
            );
            let llm_config = LlmConfig {
                base_url,
                model,
                api_key,
                credential_kind,
                embedding_model: cfg_llm.embedding_model.clone(),
                context_window,
                max_output_tokens,
                streaming,
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
            if let Some(endpoints) = endpoints.as_ref() {
                tool_server_env.insert("PRISM_API_URL".to_string(), endpoints.api_base.clone());
                tool_server_env.insert("MARC27_API_URL".to_string(), endpoints.api_base.clone());
            }

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
                    let resolved_llm = match &chat_target {
                        crate::chat_config::ChatTarget::Local {
                            url,
                            model,
                            api_key: local_key,
                        } => Some((
                            url.clone(),
                            model.clone(),
                            local_key.clone().or_else(|| {
                                prism_core::config::NodeConfig::resolve_api_key(
                                    &node_config.indexer,
                                )
                            }),
                            None,
                        )),
                        crate::chat_config::ChatTarget::Provider {
                            provider,
                            model,
                            api_key_env,
                        } => {
                            let registry = crate::providers::Registry::load();
                            let env_name = api_key_env.clone().unwrap_or_else(|| {
                                crate::providers::default_api_key_env(&registry, provider)
                            });
                            Some((
                                provider_endpoint(&registry, provider),
                                model.clone(),
                                std::env::var(&env_name).ok().or_else(|| {
                                    prism_core::config::NodeConfig::resolve_api_key(
                                        &node_config.indexer,
                                    )
                                }),
                                None,
                            ))
                        }
                        crate::chat_config::ChatTarget::Marc27 { .. } => {
                            let configured_base_url = node_config.indexer.uri.clone();
                            let platform_derived = configured_base_url.is_none()
                                && matches!(
                                    node_config.indexer.mode.as_str(),
                                    "platform" | "marc27" | "external"
                                )
                                && endpoints.is_some();
                            let base_url = configured_base_url.or_else(|| {
                                match node_config.indexer.mode.as_str() {
                                    "platform" | "marc27" | "external" => endpoints
                                        .as_ref()
                                        .map(|endpoints| format!("{}/llm", endpoints.api_base)),
                                    _ => Some("http://localhost:8080".into()),
                                }
                            });
                            let stored_credentials = paths
                                .load_cli_state()
                                .ok()
                                .and_then(|state| state.credentials);
                            let bound_stored = if platform_derived {
                                endpoints
                                    .as_ref()
                                    .zip(stored_credentials.as_ref())
                                    .map(|(endpoints, credentials)| {
                                        auth::stored_bearer_for_endpoints(endpoints, credentials)
                                    })
                                    .transpose()?
                                    .flatten()
                            } else {
                                None
                            };
                            let (api_key, credential_kind) = if platform_derived
                                && let Some(credential) = endpoints
                                    .as_ref()
                                    .and_then(|value| value.environment_credential())
                            {
                                match credential {
                                    PlatformAuth::ApiKey(key) => (
                                        Some(key),
                                        Some(prism_ingest::llm::LlmCredentialKind::ApiKey),
                                    ),
                                    PlatformAuth::Bearer(token) => (
                                        Some(token),
                                        Some(prism_ingest::llm::LlmCredentialKind::Bearer),
                                    ),
                                }
                            } else if let Some(credential) = bound_stored {
                                (
                                    Some(credential.secret().to_string()),
                                    Some(if credential.is_api_key() {
                                        prism_ingest::llm::LlmCredentialKind::ApiKey
                                    } else {
                                        prism_ingest::llm::LlmCredentialKind::Bearer
                                    }),
                                )
                            } else {
                                (
                                    prism_core::config::NodeConfig::resolve_api_key(
                                        &node_config.indexer,
                                    ),
                                    None,
                                )
                            };
                            let model = node_config
                                .indexer
                                .model
                                .clone()
                                .unwrap_or_else(|| "gemma-3-27b".into());
                            base_url.map(|base_url| (base_url, model, api_key, credential_kind))
                        }
                    };
                    if let Some((base_url, model, api_key, credential_kind)) = resolved_llm {
                        server_node_state.llm = Some(prism_ingest::LlmConfig {
                            base_url,
                            model,
                            api_key,
                            credential_kind,
                            embedding_model: node_config.indexer.embedding_model.clone(),
                            ..Default::default()
                        });
                    } else {
                        tracing::info!(
                            "no platform configured; node chat service starts without a hosted LLM"
                        );
                    }
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
                    let endpoints = require_platform_endpoints(endpoints.as_ref())?;
                    let (resolved_api_base, resolved_auth) = resolved_platform_auth
                        .as_ref()
                        .expect("non-offline node auth was preflighted");
                    let creds = cli_state.credentials.as_ref();
                    if let Some(credentials) = creds {
                        auth::validate_stored_session_binding(endpoints, credentials).with_context(
                            || {
                                "stored identity cannot configure remote-session verification for the selected platform"
                            },
                        )?;
                    }
                    server_node_state.identity_verifier = identity_verifier_for(endpoints, creds)
                        .with_context(
                        || "failed to configure identity verification for remote PRISM sessions",
                    )?;
                    daemon_org_id = creds.and_then(|value| value.org_id.clone());
                    let (client_auth, maybe_refreshed) =
                        resolve_node_auth(&paths, endpoints, creds, resolved_auth).await?;
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
                    let client_uses_api_key = client_auth.is_api_key();
                    let mut platform =
                        PlatformClient::new(resolved_api_base).with_auth(client_auth);
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
                            Err(api_err) if api_err.is_token_expired() && !client_uses_api_key => {
                                tracing::info!(
                                    "node register rejected with token_expired — refreshing and retrying once"
                                );
                                // Refresh from the EFFECTIVE creds (rotated
                                // by resolve_node_token if it already
                                // refreshed), never the stale startup
                                // binding.
                                let effective_creds = effective_creds.as_ref().ok_or_else(|| {
                                        anyhow!("token expired without a stored session; re-authentication or PRISM_API_KEY is required")
                                    })?;
                                let refreshed = refresh_access_token(
                                    &paths,
                                    endpoints,
                                    effective_creds,
                                )
                                .await
                                .context(
                                    "token expired and refresh failed — re-authentication required",
                                )?;
                                // Reassign the OUTER client so the daemon
                                // state (stored below) carries the live token.
                                platform = PlatformClient::new(resolved_api_base).with_auth(
                                    PlatformAuth::Bearer(refreshed.access_token.clone()),
                                );
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
                    let chat_api_base = endpoints.as_ref().map(|value| value.api_base.clone());
                    tokio::spawn(async move {
                        let llm_config = chat_state
                            .llm
                            .clone()
                            .expect("checked is_some before spawn");
                        let mut tool_server_env =
                            prism_agent::service::default_tool_server_env(chat_api_base.as_deref());
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
                            let sync_config = Some(prism_mesh::sync::SyncConfig {
                                provenance_db: prism_provenance::store_path(),
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
                    prism_node::daemon::run_daemon(endpoints.as_ref(), &paths, daemon_options)
                        .await;

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
                print_node_status(&caps, endpoints.as_ref());
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
        Commands::Ontology { command } => {
            crate::ontology_cmd::handle(command, &cli.project_root).await?;
        }
        Commands::Reverify { command } => {
            crate::reverify_cmd::run(command, &cli.project_root).await?;
        }
        Commands::Plugins { command } => {
            crate::plugins_cmd::handle(command, &cli.python.to_string_lossy(), &cli.project_root)
                .await?;
        }
        Commands::Ingest {
            path,
            corpus,
            samples,
            agreement,
            vision_model,
            vision_url,
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
            repair,
        } => {
            // Refused BEFORE any work, and before the status/platform/watch
            // branch, so every route that ingests honours the same bar: an
            // agreement above the sample count filters out every fact and
            // would report a full document as empty.
            let sampling = prism_ingest::text_extract::SamplingPolicy::new(
                NonZeroUsize::from(NonZeroU8::new(samples).expect("clap enforces >= 1")),
                NonZeroUsize::from(NonZeroU8::new(agreement).expect("clap enforces >= 1")),
            )
            .ok_or_else(|| {
                anyhow!(
                    "--agreement {agreement} exceeds --samples {samples}: no fact can \
                     appear in more passes than are run, so every fact would be \
                     filtered and the document would look empty"
                )
            })?;

            if repair {
                if status || platform || watch || schema_only {
                    bail!(
                        "`--repair` runs the Phase-2 repair pass over a document's repair \
                         queue and cannot be combined with --status, --platform, --watch \
                         or --schema-only."
                    );
                }
                let path = path.as_deref().ok_or_else(|| {
                    anyhow!(
                        "`prism ingest --repair` requires the document whose repair queue \
                         should be drained — the same path it was ingested under."
                    )
                })?;
                let summary = run_local_repair_pass(
                    path,
                    &project_root,
                    model.as_deref(),
                    llm_url.as_deref(),
                    api_key.as_deref(),
                    &runtime_url,
                )
                .await?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&summary)?);
                } else {
                    print_repair_summary(&summary);
                }
                let error_count = summary["errors"].as_array().map_or(0, Vec::len);
                if error_count > 0 {
                    bail!("{error_count} repair step(s) failed — see errors above/in JSON");
                }
            } else if status {
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
                    sampling,
                    VisionModelChoice {
                        model: vision_model.as_deref(),
                        url: vision_url.as_deref(),
                    },
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
                    sampling,
                    VisionModelChoice {
                        model: vision_model.as_deref(),
                        url: vision_url.as_deref(),
                    },
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
            include_unverified,
            dashboard_url,
        } => {
            if platform {
                // Route through the configured provider API.
                handle_platform_query(&text, semantic, json_output, limit).await?;
            } else if federated {
                handle_federated_query(&text, &dashboard_url, &paths).await?;
            } else {
                handle_query(&text, semantic, limit, include_unverified).await?;
            }
        }
        Commands::Agent => {
            print_agent_guide();
        }
        Commands::Run(run) => {
            let RunArgs {
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
                hq_tasks,
                hq_workers,
                hq_server_dir,
                hq_autoalloc,
                hq_time_limit,
                hq_extra,
                json,
            } = *run;
            let hq = HqRunFlags {
                tasks: hq_tasks.as_deref(),
                workers: hq_workers,
                server_dir: hq_server_dir.as_deref(),
                autoalloc: hq_autoalloc.as_deref(),
                time_limit: hq_time_limit.as_str(),
                extra: &hq_extra,
            };
            handle_run(
                &paths.data_dir,
                &name,
                &image,
                &input,
                &backend,
                platform_url.as_deref(),
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
                &hq,
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
            handle_federation_command(command, &paths, endpoints.as_ref()).await?;
        }
        Commands::Report {
            description,
            log_file,
            no_github,
        } => {
            let endpoints = require_platform_endpoints(endpoints.as_ref())?;
            handle_report(
                &paths,
                endpoints,
                &description,
                log_file.as_deref(),
                no_github,
            )
            .await?;
        }
        Commands::Marketplace { command } => {
            use prism_client::marketplace::MarketplaceClient;

            let endpoints = require_platform_endpoints(endpoints.as_ref())?;
            // Marketplace listing can be public, but when any supported
            // credential exists it must retain its X-API-Key/Bearer kind.
            let platform = resolve_agent_auth_with_url(Some(&endpoints.api_base))
                .map(|(_, credential)| {
                    PlatformClient::new(&endpoints.api_base).with_auth(credential)
                })
                .unwrap_or_else(|_| PlatformClient::new(&endpoints.api_base));
            let marketplace_authenticated = platform.access_token().is_some();
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
                        if !marketplace_authenticated {
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
            // LOCAL. Research is not a separate system: it is this agent,
            // given a goal, with the DAG (`orchestrate_agents`), the whole
            // tool surface, and the notebook it always has.
            //
            // This command used to POST to `/agent-runs` and poll, so every
            // answer came from `marc27_core::research::engine` — a vendor the
            // project no longer uses. Nothing local was ever exercised, which
            // is why an expired token made research unavailable outright
            // rather than degraded. It now drives `prism backend`, the same
            // JSON-RPC session the TUI and PRISM Desktop already drive.
            let exe = std::env::current_exe()?;
            let mut child = tokio::process::Command::new(exe)
                .arg("backend")
                .arg("--project-root")
                .arg(&project_root)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                // Kept, not discarded: a backend that panics says why HERE,
                // and a research run that dies silently is indistinguishable
                // from one that found nothing.
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true)
                .spawn()?;
            let mut child_stdin = child.stdin.take().expect("stdin was piped");
            let child_stdout = child.stdout.take().expect("stdout was piped");
            let child_stderr = child.stderr.take().expect("stderr was piped");

            // FAN-OUT IS WIDTH, NOT DEPTH. An agent spawned by
            // `orchestrate_agents` carries `orchestration_forbidden`, so a
            // second level is refused by design — "a caller who wants more
            // parallel work asks for a WIDER batch". Asking for levels spends
            // turns on something the harness will not do.
            let prompt = if depth == 0 {
                query.clone()
            } else {
                format!(
                    "Research goal: {query}\n\nDecompose this into independent \
                     sub-questions and run them as ONE batch with \
                     orchestrate_agents, then synthesise a single answer. \
                     Cite the sources you actually read."
                )
            };

            use tokio::io::AsyncWriteExt as _;
            for request in [
                serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "init", "params": {}}),
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "input.message",
                    "params": {"text": prompt}
                }),
            ] {
                child_stdin
                    .write_all(format!("{request}\n").as_bytes())
                    .await?;
            }
            child_stdin.flush().await?;

            use tokio::io::AsyncBufReadExt as _;
            // Drained on its own task: a full stderr pipe blocks the child,
            // and a blocked child never reaches `ui.turn.complete`.
            let stderr_task = tokio::spawn(async move {
                let mut lines = tokio::io::BufReader::new(child_stderr).lines();
                let mut kept = Vec::new();
                while let Ok(Some(line)) = lines.next_line().await {
                    if kept.len() < 200 {
                        kept.push(line);
                    }
                }
                kept
            });

            let mut reader = tokio::io::BufReader::new(child_stdout).lines();
            let mut answer = String::new();
            let mut cost = serde_json::Value::Null;
            let mut backend_error: Option<String> = None;
            let mut turn_completed = false;
            let mut prompt_id = 100i64;
            while let Some(line) = reader.next_line().await? {
                let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) else {
                    continue;
                };
                let params = event.get("params");
                let text_of = |key: &str| {
                    params
                        .and_then(|p| p.get(key))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                };
                match event.get("method").and_then(serde_json::Value::as_str) {
                    Some("ui.text.delta") => {
                        if let Some(text) = text_of("text") {
                            answer.push_str(&text);
                            // Stream it: a research turn runs for minutes, and
                            // a silent pipe is indistinguishable from a hang.
                            if !json {
                                print!("{text}");
                                io::stdout().flush().ok();
                            }
                        }
                    }
                    // Progress goes to STDERR so stdout stays one clean
                    // document — the same contract the hosted version kept.
                    //
                    // `agent` is present only when the call belongs to a DAG
                    // lane, so printing it makes the decomposition visible:
                    // without it a fan-out looks identical to one agent
                    // working sequentially.
                    Some("ui.tool.start") => {
                        if let Some(tool) = text_of("tool_name") {
                            match text_of("agent") {
                                Some(agent) => eprintln!("  [{agent}] {tool}"),
                                None => eprintln!("  · {tool}"),
                            }
                        }
                    }
                    // AN UNANSWERED PROMPT IS A DEADLOCK. The backend emits
                    // `ui.prompt` for a tool that needs approval and then
                    // WAITS for `input.prompt_response`; a driver that only
                    // listens hangs forever. Measured: a `--depth 2` run sat
                    // for 85 minutes at 0.0% CPU with no network connection
                    // open, having printed its plan and nothing else.
                    //
                    // What to answer is NOT uniform. `orchestrate_agents` and
                    // `spawn_subagent` are `requires_approval: true`, and this
                    // command's own prompt ORDERS the model to decompose — so
                    // a blanket "n" denies the one tool the operator asked
                    // for by typing `--depth`, and the run silently collapses
                    // to a flat answer. That consent is given at invocation;
                    // it is granted here and SAID OUT LOUD, never assumed.
                    //
                    // Everything else is declined: this command is
                    // non-interactive, and granting `execute_bash` or `file`
                    // on an absent human's behalf is not a default anyone
                    // chose. Research's own tools (`prior_art_search`,
                    // `materials_search`) declare no approval, so they are
                    // never affected either way.
                    Some("ui.prompt") => {
                        let tool = text_of("tool_name").unwrap_or_else(|| "?".to_string());
                        let orchestration =
                            matches!(tool.as_str(), "orchestrate_agents" | "spawn_subagent");
                        let allow = orchestration && depth > 0;
                        if allow {
                            eprintln!("  approved (you asked for decomposition): {tool}");
                        } else {
                            eprintln!("  declined (needs approval, nobody is here): {tool}");
                        }
                        prompt_id += 1;
                        let reply = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": prompt_id,
                            "method": "input.prompt_response",
                            "params": {
                                // "y" approves THIS CALL. Never "a".
                                //
                                // "a" is `allow-session`, and it does what it
                                // says: the backend maps it to
                                // `ApprovalResponse::AllowAll`, which calls
                                // `PermissionOverrides::allow_all()` and
                                // inserts the `"*"` wildcard. Every later
                                // tool — `execute_bash`, `file`,
                                // `knowledge_write` — is then auto-approved
                                // and NO further prompt is ever emitted, so
                                // the decline branch below can never fire
                                // again. This code shipped for part of a day
                                // granting unrestricted autonomy while the
                                // comment above claimed it declined
                                // everything but orchestration.
                                "response": if allow { "y" } else { "n" },
                                "tool_name": tool
                            }
                        });
                        child_stdin
                            .write_all(format!("{reply}\n").as_bytes())
                            .await?;
                        child_stdin.flush().await?;
                    }
                    // A backend failure is a FAILURE. Left unhandled it
                    // arrives as an empty answer and exit 0, which is the
                    // expired-token case this rewrite exists to remove
                    // reappearing in a quieter form.
                    Some("ui.backend.error") => {
                        backend_error = Some(
                            text_of("message")
                                .or_else(|| text_of("error"))
                                .unwrap_or_else(|| line.clone()),
                        );
                    }
                    Some("ui.cost") => {
                        cost = params.cloned().unwrap_or(serde_json::Value::Null);
                    }
                    Some("ui.turn.complete") => {
                        turn_completed = true;
                        break;
                    }
                    _ => {}
                }
            }
            // Closing stdin is the backend's own shutdown signal — it drains
            // a pending turn and reaps the Python tool server through
            // `kill_on_drop`. SIGKILL would forfeit both and can strand that
            // grandchild. `kill_on_drop` on our own child remains the
            // backstop if we leave early.
            drop(child_stdin);
            let status = child.wait().await.ok();
            let stderr_lines = stderr_task.await.unwrap_or_default();

            // Say what went wrong, loudly, rather than presenting an empty
            // document as a finished answer.
            if let Some(error) = backend_error {
                for line in stderr_lines.iter().rev().take(10).rev() {
                    eprintln!("  {line}");
                }
                anyhow::bail!("research failed: {error}");
            }
            if !turn_completed {
                for line in stderr_lines.iter().rev().take(10).rev() {
                    eprintln!("  {line}");
                }
                let how = status
                    .map(|s| format!("backend exited with {s}"))
                    .unwrap_or_else(|| "backend exited".to_string());
                anyhow::bail!("research ended before the turn completed: {how}");
            }
            if answer.trim().is_empty() {
                anyhow::bail!("research produced no answer");
            }

            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "query": query,
                        "depth": depth,
                        "answer": answer,
                        "cost": cost,
                    }))?
                );
            } else {
                println!();
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
            if let Some(endpoints) = endpoints.as_ref() {
                onboarding::run_if_first_launch(&paths, endpoints, &python).await?;
            }

            // Auto-refresh + refresh-on-rejection. Mirrors the `prism
            // setup` path — see comments there for design notes.
            // The two triggers (proactive expiry + reactive 401) keep
            // users out of the "log in again every session" loop.
            let mut state = paths.load_cli_state().ok().unwrap_or_default();
            let mut proactive_refresh_failed = false;
            if let Some(creds) = state.credentials.as_ref()
                && let Some(endpoints) = endpoints.as_ref()
                && !creds.refresh_token.is_empty()
                && creds
                    .expires_at
                    .is_some_and(|exp| chrono::Utc::now() + chrono::Duration::minutes(5) >= exp)
            {
                match refresh_access_token(&paths, endpoints, creds).await {
                    Ok(new_creds) => {
                        state.credentials = Some(new_creds);
                        tracing::info!("access token refreshed proactively");
                    }
                    Err(e) => {
                        proactive_refresh_failed = true;
                        tracing::warn!(error = %e, "proactive token refresh failed");
                    }
                }
            }
            let mut boot_checks =
                boot_checks::run_boot_checks(state.credentials.as_ref(), endpoints.as_ref()).await;
            let auth_rejected = boot_checks
                .iter()
                .any(|c| c.name == "Auth" && c.result.starts_with("token rejected"));
            if auth_rejected
                && !proactive_refresh_failed
                && let Some(creds) = state.credentials.as_ref()
                && let Some(endpoints) = endpoints.as_ref()
                && !creds.refresh_token.is_empty()
                && let Ok(new_creds) = refresh_access_token(&paths, endpoints, creds).await
            {
                tracing::info!("access token refreshed after server-side rejection");
                state.credentials = Some(new_creds);
                boot_checks =
                    boot_checks::run_boot_checks(state.credentials.as_ref(), Some(endpoints)).await;
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
                let endpoints = require_platform_endpoints(endpoints.as_ref())?;
                eprintln!();
                eprintln!("\x1b[33mYour platform session has expired — re-authenticating…\x1b[0m");
                eprintln!();
                if let Err(e) = perform_full_login(
                    &paths,
                    endpoints,
                    &python,
                    provider_login_mode(endpoints, false, true)?,
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
                    boot_checks::run_boot_checks(state.credentials.as_ref(), Some(endpoints)).await;
            }
            boot::boot_sequence(&boot_checks);
            let _ = &python;
            // Launch the new Ratatui full-screen TUI. It spawns
            // `prism backend` as a subprocess and talks JSON-RPC.
            let prism_bin =
                std::env::current_exe().context("failed to locate current prism executable")?;
            // Give the TUI the platform bearer so it can poll the org credit
            // balance at turn boundaries (status bar). None → no credits shown.
            let platform = tui_platform_auth(endpoints.as_ref(), state.credentials.as_ref())?;
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
            let mut proactive_refresh_failed = false;
            if let Some(creds) = state.credentials.as_ref()
                && let Some(endpoints) = endpoints.as_ref()
                && !creds.refresh_token.is_empty()
                && creds
                    .expires_at
                    .is_some_and(|exp| chrono::Utc::now() + chrono::Duration::minutes(5) >= exp)
            {
                match refresh_access_token(&paths, endpoints, creds).await {
                    Ok(new_creds) => {
                        state.credentials = Some(new_creds);
                        tracing::info!("access token refreshed proactively");
                    }
                    Err(e) => {
                        proactive_refresh_failed = true;
                        tracing::warn!(error = %e, "proactive token refresh failed");
                    }
                }
            }
            let mut boot_checks =
                boot_checks::run_boot_checks(state.credentials.as_ref(), endpoints.as_ref()).await;
            let auth_rejected = boot_checks
                .iter()
                .any(|c| c.name == "Auth" && c.result.starts_with("token rejected"));
            if auth_rejected
                && !proactive_refresh_failed
                && let Some(creds) = state.credentials.as_ref()
                && let Some(endpoints) = endpoints.as_ref()
                && !creds.refresh_token.is_empty()
                && let Ok(new_creds) = refresh_access_token(&paths, endpoints, creds).await
            {
                tracing::info!("access token refreshed after server-side rejection");
                state.credentials = Some(new_creds);
                boot_checks =
                    boot_checks::run_boot_checks(state.credentials.as_ref(), Some(endpoints)).await;
            }
            // Same inline re-login as the Tui branch — see comment
            // there. Resuming on dead creds is even more confusing
            // because the user expects their old conversation to load.
            if boot_checks
                .iter()
                .any(|c| c.name == "Auth" && c.result.starts_with("token rejected"))
            {
                let endpoints = require_platform_endpoints(endpoints.as_ref())?;
                eprintln!();
                eprintln!("\x1b[33mYour platform session has expired — re-authenticating…\x1b[0m");
                eprintln!();
                if let Err(e) = perform_full_login(
                    &paths,
                    endpoints,
                    &python,
                    provider_login_mode(endpoints, false, true)?,
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
                    boot_checks::run_boot_checks(state.credentials.as_ref(), Some(endpoints)).await;
            }
            boot::boot_sequence(&boot_checks);
            let prism_bin =
                std::env::current_exe().context("failed to locate current prism executable")?;
            let platform = tui_platform_auth(endpoints.as_ref(), state.credentials.as_ref())?;
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
            inject_workflow_platform_endpoint(&mut values, project_root, paths);
            let options = resolve_workflow_llm_options(
                project_root,
                paths,
                caller_supplied_llm_base_url,
                node_token,
            )?;
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
    inject_workflow_platform_endpoint(&mut values, project_root, paths);
    let options = resolve_workflow_llm_options(
        project_root,
        paths,
        caller_supplied_llm_base_url,
        node_token,
    )?;
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
    endpoints: Option<&PlatformEndpoints>,
) -> Result<()> {
    match command {
        FederationCommands::Whoami { json } => {
            // This is local reporting, not an authenticated platform call.
            // API-key-only users and fresh installs therefore get a truthful
            // identity report instead of an invented login requirement.
            let state = paths.load_cli_state().ok().unwrap_or_default();
            let creds = state.credentials.as_ref();
            // Report the name that actually supplied the key, not a fixed
            // string -- an operator with both spellings set otherwise cannot
            // tell which one the process read.
            let env_credential = PlatformVar::get_with_source_preferred_then_alias(&[
                PlatformVar::API_KEY,
                PlatformVar::TOKEN,
                PlatformVar::API_TOKEN,
            ]);
            let credential_source = if let Some((_, name)) = env_credential {
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
                .or_else(|| endpoints.map(|value| value.api_base.as_str()))
                .unwrap_or("not configured");

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

            // A manually supplied URL is not cryptographic identity proof.
            // Never carry a stored provider credential to it; remote peers
            // must be vouched for through authenticated platform discovery.
            let sessions = prism_mesh::peer_session::PeerSessions::new(None);
            let sync_config = Some(prism_mesh::sync::SyncConfig {
                provenance_db: prism_provenance::store_path(),
            });

            println!("Pulling dataset '{dataset_name}' from {peer} (node {publisher})...");
            let client = prism_mesh::sync::sync_http_client();
            let synced = prism_mesh::sync::sync_dataset_from_peer(
                &client,
                // Operator-named destinations receive only tokenless mints.
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
pub(crate) fn build_llm_config(
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

    // Read before the move into the struct below.
    let model_for_limits = model.clone();
    let endpoint_for_limits = base_url.clone();
    Ok(prism_ingest::LlmConfig {
        base_url,
        model,
        api_key,
        embedding_model: llm.embedding_model.clone(),
        timeout_secs: llm.timeout_secs,
        // `[llm] max_output_tokens` — the knob the thinking-mode extraction
        // diagnostic tells users to raise. It existed in the message but not
        // in the config until 2026-08-10.
        max_output_tokens: llm.max_output_tokens,
        // THE MODEL'S REAL CONTEXT WINDOW, resolved from the registry that
        // already knows it rather than left `None`.
        //
        // PRISM deliberately does not cap output on the operator's behalf
        // (see `LlmClient::effective_max_tokens`): a fixed ceiling silently
        // breaks any model that reasons before it answers, which it did once
        // already. The bound that justifies that policy is the CONTEXT
        // WINDOW — and on this path there wasn't one, so nothing bounded
        // generation at all.
        //
        // Measured: a campaign proposal call against a local 12B reasoner ran
        // to 8,813 decoded tokens, roughly five minutes, and died on the
        // 300-second HTTP timeout with the connection dropped mid-message.
        // Not one iteration completed. The same `None` also left every HTTP
        // model on the ingest path sharing one hardcoded elision budget.
        //
        // `get_model_config` is the registry the agent loop already uses —
        // fuzzy id match, sanity clamps, user models ahead of the catalog
        // cache ahead of the static seed. Reused here rather than reimplemented,
        // because a second answer to "how big is this model's window" is how
        // the first one drifted.
        // ASK the endpoint before trusting the registry. `get_model_config`
        // answers for a model it does not know with the UNKNOWN fallback's
        // 128k, and `tool_catalog::tool_token_budget` then sizes the tool
        // block against a window that does not exist — measured 2026-08-19:
        // 171 definitions (12,289 tokens) shipped at a llama.cpp server
        // started with `-c 16384`, and the first tool result overflowed it.
        // A local server reports its real `n_ctx`; compaction cannot fix this
        // one because the tool block is not part of the history it shrinks.
        context_window: Some(prism_agent::models::resolve_context_window(
            &endpoint_for_limits,
            &model_for_limits,
        ) as u64),
        // Whether this endpoint can stream, per the provider registry. Unknown
        // endpoints stream; only a declared `streaming = false` turns it off.
        streaming: prism_core::providers::streams_for_url(
            &prism_core::providers::Registry::load(),
            &endpoint_for_limits,
        ),
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
        let state = paths.load_cli_state().ok();
        PlatformEndpoints::resolve_for_paths(
            node_config.platform.url.as_deref(),
            node_config.platform.provider.as_deref(),
            state.as_ref().and_then(|value| value.credentials.as_ref()),
            paths,
        )
        .and_then(|endpoints| marc27_llm_base_url(paths, &endpoints.api_base, &cfg_llm.url).ok())
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
) -> Result<WorkflowExecutionOptions> {
    let resolved = resolve_workflow_llm_pair(project_root, paths);
    let trusted_llm_credential = resolved
        .as_ref()
        .map(|(base_url, _)| resolve_workflow_llm_api_key(project_root, paths, base_url))
        .transpose()?
        .flatten();
    Ok(WorkflowExecutionOptions {
        trusted_llm_base_url: resolved.as_ref().map(|(base_url, _)| base_url.clone()),
        trusted_llm_api_key: trusted_llm_credential
            .as_ref()
            .map(|(value, _)| value.clone()),
        trusted_llm_credential_kind: trusted_llm_credential.and_then(|(_, kind)| kind),
        caller_supplied_llm_base_url,
        trusted_node_port: node_token.as_ref().map(|_| 7327),
        trusted_node_token: node_token,
    })
}

fn resolve_workflow_llm_api_key(
    project_root: &Path,
    paths: &PrismPaths,
    selected_base_url: &str,
) -> Result<Option<(String, Option<prism_ingest::llm::LlmCredentialKind>)>> {
    let node_config = prism_core::config::NodeConfig::load(Some(project_root));
    let chat_target = crate::chat_config::load().unwrap_or_default().chat;
    let stored_credentials = paths
        .load_cli_state()
        .ok()
        .and_then(|state| state.credentials);
    let endpoints = PlatformEndpoints::resolve_for_paths(
        node_config.platform.url.as_deref(),
        node_config.platform.provider.as_deref(),
        stored_credentials.as_ref(),
        paths,
    );
    let platform_derived = if matches!(chat_target, crate::chat_config::ChatTarget::Marc27 { .. }) {
        endpoints
            .as_ref()
            .and_then(|endpoints| {
                marc27_llm_base_url_with_source(paths, &endpoints.api_base, &node_config.llm.url)
                    .ok()
            })
            .is_some_and(|(base_url, derived)| {
                derived && base_url.trim_end_matches('/') == selected_base_url.trim_end_matches('/')
            })
    } else {
        false
    };
    let platform_token = if platform_derived {
        endpoints
            .as_ref()
            .zip(stored_credentials.as_ref())
            .map(|(endpoints, credentials)| {
                auth::stored_bearer_for_endpoints(endpoints, credentials)
            })
            .transpose()?
            .flatten()
            .map(|credential| credential.secret().to_string())
    } else {
        None
    };
    let credential_endpoints = if platform_derived {
        endpoints.as_ref()
    } else {
        None
    };
    Ok(resolve_workflow_llm_api_key_for_target(
        &chat_target,
        &node_config.llm,
        credential_endpoints,
        platform_token,
        platform_derived,
    ))
}

fn resolve_workflow_llm_api_key_for_target(
    chat_target: &crate::chat_config::ChatTarget,
    cfg_llm: &prism_core::config::LlmSection,
    endpoints: Option<&PlatformEndpoints>,
    platform_token: Option<String>,
    platform_derived: bool,
) -> Option<(String, Option<prism_ingest::llm::LlmCredentialKind>)> {
    let non_empty = |key: Option<String>| key.filter(|value| !value.trim().is_empty());

    match chat_target {
        crate::chat_config::ChatTarget::Local { api_key, .. } => non_empty(api_key.clone())
            .or_else(|| non_empty(cfg_llm.resolve_api_key()))
            .map(|value| (value, None)),
        crate::chat_config::ChatTarget::Provider {
            provider,
            api_key_env,
            ..
        } => {
            let registry = crate::providers::Registry::load();
            let env_name = api_key_env
                .clone()
                .unwrap_or_else(|| crate::providers::default_api_key_env(&registry, provider));
            non_empty(std::env::var(env_name).ok())
                .or_else(|| non_empty(cfg_llm.resolve_api_key()))
                .map(|value| (value, None))
        }
        crate::chat_config::ChatTarget::Marc27 { .. } => {
            if let Some(value) = non_empty(std::env::var("LLM_API_KEY").ok()) {
                return Some((value, None));
            }
            if !platform_derived {
                return non_empty(cfg_llm.resolve_api_key()).map(|value| (value, None));
            }
            if let Some(credential) = endpoints.and_then(PlatformEndpoints::environment_credential)
            {
                let (value, kind) = match credential {
                    PlatformAuth::ApiKey(value) => {
                        (value, prism_ingest::llm::LlmCredentialKind::ApiKey)
                    }
                    PlatformAuth::Bearer(value) => {
                        (value, prism_ingest::llm::LlmCredentialKind::Bearer)
                    }
                };
                return Some((value, Some(kind)));
            }
            non_empty(cfg_llm.resolve_api_key())
                .map(|value| (value, None))
                .or_else(|| {
                    non_empty(platform_token)
                        .map(|value| (value, Some(prism_ingest::llm::LlmCredentialKind::Bearer)))
                })
        }
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

/// Inject the already-resolved platform endpoint for built-in workflows.
/// Config and stored-login endpoints must work without duplicating them into
/// an environment variable; an explicit `--set platform_api_base=...` wins.
fn inject_workflow_platform_endpoint(
    values: &mut BTreeMap<String, String>,
    project_root: &Path,
    paths: &PrismPaths,
) {
    if values.contains_key("platform_api_base") {
        return;
    }
    let config = prism_core::config::NodeConfig::load(Some(project_root));
    let state = paths.load_cli_state().ok();
    if let Some(endpoints) = PlatformEndpoints::resolve_for_paths(
        config.platform.url.as_deref(),
        config.platform.provider.as_deref(),
        state.as_ref().and_then(|state| state.credentials.as_ref()),
        paths,
    ) {
        values.insert("platform_api_base".to_string(), endpoints.api_base);
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
// `owl` and `cif` were advertised here for a while, but no OWL or CIF parser
// exists anywhere in this workspace — both were read as raw text and fed to
// the materials-fact prompt, i.e. the product claimed a capability it did not
// have. They are refused (with this list in the error) until a real parser
// lands.
const PLATFORM_TEXT_EXTENSIONS: &[&str] = &["pdf", "json", "jsonl", "txt", "md"];

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
    /// The LOCAL pipeline driving a REMOTE model the caller named explicitly.
    /// Everything but inference happens here; document text goes to that
    /// endpoint and the facts land in the bundled store. Separate from
    /// [`Self::Local`] for one reason: the banner must not claim "nothing
    /// leaves your machine" when text is being sent somewhere.
    LocalPipelineRemoteModel,
}

impl TextLocality {
    /// Does this run use the LOCAL PIPELINE — extract here, ground here,
    /// write to the bundled store — as opposed to handing the document to the
    /// hosted platform? True for a remote model the caller named, because
    /// only inference is remote.
    fn is_local(self) -> bool {
        matches!(self, Self::Local | Self::LocalPipelineRemoteModel)
    }

    /// Does inference happen ON THIS MACHINE? Only this may be used to claim
    /// nothing leaves it.
    fn inference_is_on_device(self) -> bool {
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
        // `auto`: use the LOCAL PIPELINE whenever an extraction backend has
        // actually been chosen. Loopback and `gguf://local` are on-device. A
        // caller who passes `--llm-url` (or sets LLM_BASE_URL) with a key has
        // ALSO chosen where inference happens — an enterprise pointing PRISM
        // at its own vLLM, or a bring-your-own-key run against a hosted
        // model. Refusing those into the platform branch made "bring your own
        // endpoint" impossible: the flags existed and were then overruled.
        //
        // Local here means the local PIPELINE (extract, ground, write to the
        // bundled store), NOT local inference. Where inference happens is said
        // out loud in the banner, which stops claiming on-device for a remote
        // endpoint.
        _ => match llm_base_url {
            Some(url) if is_loopback_url(url) || prism_ingest::llm::is_local_gguf_url(url) => {
                TextLocality::Local
            }
            Some(url) if !url.trim().is_empty() => TextLocality::LocalPipelineRemoteModel,
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
/// (`[ontology] id`, default "emmo"), loading a project-local promoted
/// artifact into the process registry when needed. Unimplemented `[ontology]
/// engine` values remain loud refusals.
fn active_ontology_from_config(project_root: &Path) -> Result<String> {
    let node_config = prism_core::config::NodeConfig::load(Some(project_root));
    let engine = node_config.ontology.engine;
    if engine != "llm" {
        bail!(
            "[ontology] engine = \"{engine}\" is not implemented — only \"llm\" is. \
             Remove the setting or set engine = \"llm\"."
        );
    }
    let id = node_config.ontology.id;
    prism_ingest::ontologies::active_from_project(Some(&id), project_root)?;
    Ok(id)
}

/// The paper loop's policy, loaded from the project's `prism.toml`
/// (`[ingest]`). These are policy choices — the reading standard a first
/// `finish` must clear and the capability-verdict thresholds — so they live
/// in config and the loop receives values, never hardcoded numbers.
pub(crate) fn paper_agent_policy(
    project_root: &Path,
) -> prism_ingest::paper_agent::PaperAgentPolicy {
    let config = prism_core::config::NodeConfig::load(Some(project_root));
    prism_ingest::paper_agent::PaperAgentPolicy {
        finish_coverage_floor: config.ingest.finish_coverage_floor,
        finish_quantity_floor: config.ingest.finish_quantity_floor,
        model_acceptance_floor: config.ingest.model_acceptance_floor,
        model_degenerate_ceiling: config.ingest.model_degenerate_ceiling,
    }
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

    // `[ingest] batch_rows` is the operator's override; `None` lets the
    // pipeline derive the batch size from the model's context window. EVERY
    // row is processed either way — this used to be a `max_sample_rows: 10`
    // literal (in two places), which extracted ten rows of any dataset and
    // threw the rest away without a word.
    let batch_rows = prism_core::config::NodeConfig::load(Some(project_root))
        .ingest
        .batch_rows;

    let config = if schema_only {
        PipelineConfig {
            llm: None,
            batch_rows,
            mapping: None,
            provenance_db: None,
            ontology,
            semantic_validation:
                prism_ingest::semantic_validation::SemanticValidationPolicy::default(),
            on_progress: None,
        }
    } else {
        let llm_cfg = build_llm_config(project_root, llm_url, model, api_key)?;
        PipelineConfig {
            llm: Some(llm_cfg),
            batch_rows,
            mapping,
            provenance_db: None,
            ontology,
            semantic_validation:
                prism_ingest::semantic_validation::SemanticValidationPolicy::default(),
            // Progress goes to STDERR as it happens: these runs are minutes
            // per batch on a local 12B model, a silent terminal is a bug,
            // and `--json` stdout must stay parseable.
            on_progress: Some(std::sync::Arc::new(|line: &str| eprintln!("  {line}"))),
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

/// Describe the exact subject/object identities the text writer is about to
/// persist, so advisory semantic validation measures the proposal rather than
/// reconstructing it after the graph has changed.
fn semantic_entities_for_text_facts(
    facts: &[prism_provenance::MaterialFact],
    classes: &std::collections::HashMap<String, prism_ingest::classify::EntityClass>,
) -> Vec<prism_ingest::semantic_validation::SemanticEntityProposal> {
    use prism_ingest::semantic_validation::SemanticEntityProposal;

    let mut entities = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for fact in facts {
        let classified = classes.get(&fact.subject).zip(classes.get(&fact.object));
        match classified {
            Some((subject, object)) => {
                for (name, class) in [(&fact.subject, subject), (&fact.object, object)] {
                    let proposal = SemanticEntityProposal {
                        name: name.clone(),
                        entity_type: class.entity_type.clone(),
                        storage_label: class.storage_label.clone(),
                        class_iri: Some(class.class_iri.clone()),
                    };
                    let identity = (
                        proposal.name.clone(),
                        proposal.entity_type.clone(),
                        proposal.storage_label.clone(),
                        proposal.class_iri.clone(),
                    );
                    if seen.insert(identity) {
                        entities.push(proposal);
                    }
                }
            }
            None => {
                // The active ontology supplied no endpoint classification.
                // Record that absence generically; inferring a domain class
                // from a closed Rust `kind` list made this paper path specific
                // to one vocabulary and silently mistyped customer ontologies.
                for name in [&fact.subject, &fact.object] {
                    let proposal = SemanticEntityProposal {
                        name: name.clone(),
                        entity_type: "Entity".to_string(),
                        storage_label: "Entity".to_string(),
                        class_iri: None,
                    };
                    let identity = (
                        proposal.name.clone(),
                        proposal.entity_type.clone(),
                        proposal.storage_label.clone(),
                        proposal.class_iri.clone(),
                    );
                    if seen.insert(identity) {
                        entities.push(proposal);
                    }
                }
            }
        }
    }
    entities
}

/// Make the vision document reader available for this process, if a model is
/// configured at all. Since the composability seam took over the PDF path
/// (`ensure_vision_seam`), this direct registration is its FALLBACK: if the
/// seam ever fails to initialise, the reader is still installed this way, so
/// a seam bug can degrade supervision but never remove vision (audit F11).
///
/// Registered ONCE, alongside the built-in text layer, so escalation has
/// somewhere to go when a page comes back scanned or with a broken font
/// encoding. Failure here is deliberately not fatal: an already-registered
/// reader (a second ingest in one process) must not cost the user the ingest,
/// and the text layer still works without it. What it must never do is fail
/// SILENTLY — a missing renderer surfaces as the reason escalation reports
/// against the page it could not fix.
/// Build the config for the VISION reader.
///
/// Reading a page image and extracting facts from text are DIFFERENT
/// CAPABILITIES, and assuming one model does both is how a text-only
/// extraction model ends up being handed a PNG. Measured: running ingest with
/// `--model glm-5.2` made the vision reader glm-5.2 too, and every figure page
/// came back `HTTP 400 messages.content.type is invalid, allowed values:
/// ['text']`. The page was reported honestly as unreadable, but the cause was
/// our own configuration.
///
/// So vision takes its own model, and its own base URL when the vision model
/// lives somewhere else. Both fall back to the extraction model's — which is
/// correct for a genuinely multimodal local model (Gemma 4 reads both), and
/// which keeps the no-flags path exactly as it was.
/// Which model reads page images, when it differs from the extraction model.
/// A named pair rather than two more bare `Option<&str>` parameters, so a
/// caller cannot silently transpose the URL and the model.
#[derive(Debug, Clone, Copy, Default)]
pub struct VisionModelChoice<'a> {
    pub model: Option<&'a str>,
    pub url: Option<&'a str>,
}

fn build_vision_llm_config(
    project_root: &Path,
    llm_url: Option<&str>,
    model: Option<&str>,
    api_key: Option<&str>,
    vision_url: Option<&str>,
    vision_model: Option<&str>,
) -> Result<prism_ingest::LlmConfig> {
    let mut cfg = build_llm_config(
        project_root,
        vision_url.or(llm_url),
        vision_model.or(model),
        api_key,
    )?;
    // An explicit vision model must win even when the chat target supplied a
    // model — otherwise `prism use local` would silently override it and the
    // flag would look accepted while doing nothing.
    if let Some(vision_model) = vision_model.filter(|m| !m.trim().is_empty()) {
        cfg.model = vision_model.to_string();
    }
    if let Some(vision_url) = vision_url.filter(|u| !u.trim().is_empty()) {
        cfg.base_url = vision_url.to_string();
    }
    Ok(cfg)
}

fn register_vision_reader(cfg: prism_ingest::LlmConfig) {
    use std::sync::Arc;
    if cfg.base_url.trim().is_empty() {
        return;
    }
    let reader = Arc::new(prism_ingest::document::VisionUnderstanding::new(
        Arc::new(prism_ingest::document::CommandRasteriser::poppler()),
        Arc::new(prism_ingest::llm::LlmClient::new(cfg)),
    ));
    // First registration in this process simply registers. In a REUSED
    // process (watch mode, the TUI) the id is already taken — by run 1's
    // `retire` tombstone — and a register-only call here left that
    // tombstone standing: vision permanently off from run 2 onward,
    // announced nowhere (round two's silent muzzle through a different
    // door). Displace it deliberately.
    if prism_ingest::document::register_understanding(reader.clone()).is_err()
        && let Err(error) = prism_ingest::document::replace_understanding(reader)
    {
        // Both doors failed (the id vanished between the calls). This
        // fallback exists to preserve vision, so its own failure must be
        // loud, not a debug line.
        eprintln!("Warning: could not install the vision document reader: {error:#}");
    }
}

/// Wiring of the composability seam: the vision endpoint becomes a
/// supervised key, published ONCE for the whole ingest run — the caller owns
/// the slot and every file of the run shares it. Nothing is probed, ever:
/// no host is contacted and no credential leaves the machine until a page
/// actually needs vision, and from then on the READER's own reads are the
/// endpoint's health signal (`VisionSeam::observe`) — enough consecutive
/// failures park the reader once for the rest of the corpus, and a parked
/// endpoint earns real trial reads back on a bounded backoff
/// (`VisionSeam::retry_endpoint_if_due`, consulted here, once per file).
async fn ensure_vision_seam(
    vision_seam: &mut Option<prism_ingest::document::VisionSeam>,
    cfg: prism_ingest::LlmConfig,
) {
    if let Some(seam) = vision_seam {
        seam.retry_endpoint_if_due().await;
        return;
    }
    match prism_ingest::document::VisionSeam::start(cfg.clone()).await {
        Ok(seam) => *vision_seam = Some(seam),
        // Reachable when the reader component fails to activate — `settle`
        // swallows activation errors into fiber status, and `start` checks
        // the state actually reached (audit F11). The seam steps aside
        // LOUDLY and the reader is registered directly, as it was before
        // the seam existed: a seam bug may cost supervision, never vision.
        Err(error) => {
            eprintln!(
                "Warning: the vision seam could not activate its reader ({error:#}); \
                 the vision reader is registered directly instead"
            );
            register_vision_reader(cfg);
        }
    }
}

/// The `deferred` summary fragment for a PDF whose damaged pages need the
/// vision endpoint — withdrawn, or live but failing on the wire — or `None`
/// when nothing is deferred. The caller
/// records it and PROCEEDS with extraction (audit F2) — deferral is
/// bookkeeping about the blocked pages, never a reason to discard the sound
/// ones. Factored out of `run_local_text_ingest_file` so the decision is
/// testable without a probe, an endpoint, or an LLM.
fn deferred_vision_pages(
    vision_seam: Option<&prism_ingest::document::VisionSeam>,
    outcome: &prism_ingest::document::ReadOutcome,
) -> Option<serde_json::Value> {
    let seam = vision_seam?;
    let reason = prism_ingest::document::vision_deferral_reason(&seam.status(), outcome)?;
    eprintln!("Deferred: {reason}");
    let pages: Vec<u32> = outcome.unrecovered().map(|note| note.number).collect();
    Some(serde_json::json!({
        "waiting_on": prism_ingest::document::VISION_ENDPOINT_KEY.to_string(),
        "reason": reason,
        "pages": pages,
    }))
}

/// One operator-facing line for an adapter that contributed nothing —
/// worded by its typed `kind`, never by `Display` alone. A reader that RAN
/// and failed must not be called "unavailable": that word claims it never
/// ran, which is exactly the defect class `SkipNote`'s typed kind exists to
/// prevent (observed verbatim before this helper: "document reader
/// unavailable — vision: rendering page 1: pdftoppm failed").
fn skip_note_line(note: &prism_ingest::document::SkipNote) -> String {
    let what = match note.kind {
        prism_ingest::document::SkipKind::Unavailable => "document reader unavailable",
        prism_ingest::document::SkipKind::Failed(_) => "document reader failed",
    };
    format!("Note: {what} — {note}")
}

/// Read one document's text ON DEVICE — the SAME reader Phase 1 uses,
/// because the repair tier must look at exactly the text the facts were
/// judged against. PDFs go through the document-understanding plane (no
/// runtime sidecar, no network beyond the operator's configured model);
/// other text formats read the file directly.
async fn read_document_text(path: &Path, runtime_url: &str) -> Result<(String, Option<String>)> {
    if ingest_format(path) == "pdf" {
        let bytes = std::fs::read(path)
            .with_context(|| format!("failed to read PDF {}", path.display()))?;
        // Read through the document-understanding plane rather than calling
        // one extractor directly: the text layer is only one way to read a
        // PDF, and a page it cannot read (scanned, or a broken font encoding
        // that drops a whole table) escalates to whichever richer adapter is
        // registered and available. Which reader actually ran is reported,
        // never assumed.
        let label = path.display().to_string();
        let doc = prism_ingest::document::SourceDocument::whole(&bytes, "pdf", &label);
        let outcome =
            prism_ingest::document::read(&doc, &prism_ingest::document::Policy::default())
                .await
                .with_context(|| format!("reading PDF {} locally", path.display()))?;

        // Say what could not be read and what could not be tried. A page of a
        // paper that silently never reached the extractor is exactly the kind
        // of hole this plane exists to stop being invisible.
        for skipped in &outcome.skipped {
            eprintln!("{}", skip_note_line(skipped));
        }
        let unrecovered: Vec<String> = outcome
            .unrecovered()
            .map(|note| format!("p{} ({})", note.number, note.damage))
            .collect();
        let warning = (!unrecovered.is_empty()).then(|| {
            format!(
                "{} of {} pages could not be read cleanly: {}",
                unrecovered.len(),
                outcome.understanding.pages.len(),
                unrecovered.join(", "),
            )
        });
        if let Some(warning) = &warning {
            eprintln!("Warning: {warning}");
        }
        Ok((outcome.understanding.plain_text(), warning))
    } else {
        // Non-PDF text formats just read the file. Either way the runtime
        // sidecar is never contacted on the local path.
        let (text, _pages, warning) = extract_platform_ingest_text(path, runtime_url).await?;
        Ok((text, warning))
    }
}

/// Persist the exact UTF-8 representation whose line coordinates are stored
/// on assertions. A PDF cannot be reread by treating its binary bytes as
/// text, and a later document-reader upgrade might produce different lines;
/// the content-addressed snapshot keeps the original witness reopenable.
pub(crate) fn persist_source_text_snapshot(
    prism_home: &Path,
    source_text: &str,
) -> Result<PathBuf> {
    use sha2::{Digest as _, Sha256};

    let revision = Sha256::digest(source_text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let directory = prism_home.join("source-text");
    std::fs::create_dir_all(&directory).with_context(|| {
        format!(
            "creating source-text snapshot directory {}",
            directory.display()
        )
    })?;
    let path = directory.join(format!("{revision}.txt"));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => file
            .write_all(source_text.as_bytes())
            .with_context(|| format!("writing source-text snapshot {}", path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = std::fs::read_to_string(&path)
                .with_context(|| format!("reading source-text snapshot {}", path.display()))?;
            if existing != source_text {
                bail!(
                    "source-text snapshot {} does not match its SHA-256 address",
                    path.display()
                );
            }
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("creating source-text snapshot {}", path.display()));
        }
    }
    Ok(std::fs::canonicalize(&path).unwrap_or(path))
}

/// Ingest a text document entirely on-device with the active registered
/// ontology and write the resulting facts (with one PROV-O activity) into
/// the bundled Turso provenance store. Nothing leaves the machine.
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
    sampling: prism_ingest::text_extract::SamplingPolicy,
    vision: VisionModelChoice<'_>,
    // The RUN-scoped seam slot, owned by the caller: one seam for a whole
    // corpus, so a mid-corpus endpoint death parks the reader ONCE (running
    // the tombstone inverse on the live component — audit F3) instead of
    // each file rebuilding a runtime that forgot everything (the shape that
    // made the tombstone test-only and paid a dead endpoint per file).
    vision_seam: &mut Option<prism_ingest::document::VisionSeam>,
) -> Result<serde_json::Value> {
    let ontology_id = active_ontology_from_config(project_root)?;
    let ontology = prism_ingest::ontologies::active(Some(&ontology_id))?;
    // The UNION of loaded ontologies (active first) — what the paper reader
    // navigates and what class-IRI bindings resolve against. The `ontology`
    // handle above stays the PRIMARY: tenant, classification stamp, and
    // typed fact shapes remain the active ontology's.
    let ontology_set = prism_ingest::ontologies::loaded(Some(&ontology_id))?;

    if mapping_path.is_some() {
        eprintln!(
            "Warning: --mapping is only applied to the local tabular ingest pipeline and is ignored for {}.",
            path.display()
        );
    }

    // The vision reader is built from the configured model and has to exist
    // BEFORE the read begins, so a page the text layer cannot recover has
    // somewhere to escalate to. Best-effort on purpose: READING a document
    // must not start requiring a model just because escalation might want
    // one. With no model configured there is simply no vision reader, and
    // escalation reports that against any page it could not fix.
    //
    // Every configured vision endpoint — explicit (--vision-url /
    // --vision-model, the LLM_VISION_* env) or inherited from the extraction
    // model — goes through the composability seam on the default PDF path
    // (audit F10). This is safe to run by default because nothing gates the
    // reader up front: there is NO probe, no host is contacted until a page
    // actually needs vision, and a park DEFERS the blocked pages while the
    // rest of the document ingests (audit F2). What the seam adds over
    // direct registration is teardown and memory: a mid-corpus endpoint
    // death (measured by the reader's own reads) swaps in the tombstone
    // once, the corpus stops paying that endpoint's timeout per file, and
    // real trial reads on a bounded backoff win the endpoint back.
    if ingest_format(path) == "pdf"
        && let Ok(cfg) = build_vision_llm_config(
            project_root,
            llm_url,
            model,
            api_key,
            vision.url,
            vision.model,
        )
        && !cfg.base_url.trim().is_empty()
    {
        ensure_vision_seam(vision_seam, cfg).await;
    }

    // PDFs are read ON DEVICE through the document-understanding plane: no
    // runtime sidecar, and no network beyond whatever model the operator
    // configured. Each reader owns its own blocking, so a `pdf-extract` panic
    // on one malformed file is an honest per-file error rather than a dead
    // ingest run.
    // `page_ranges` carries the document's structural units (byte ranges of
    // pages in `text`) into segmentation; empty means "no page structure
    // known" and segmentation falls back to blank-line paragraphs.
    let (text, _page_ranges, warning, deferred) = if ingest_format(path) == "pdf" {
        let bytes = std::fs::read(path)
            .with_context(|| format!("failed to read PDF {}", path.display()))?;
        // Read through the document-understanding plane rather than calling
        // one extractor directly: the text layer is only one way to read a
        // PDF, and a page it cannot read (scanned, or a broken font encoding
        // that drops a whole table) escalates to whichever richer adapter is
        // registered and available. Which reader actually ran is reported,
        // never assumed.
        let label = path.display().to_string();
        let doc = prism_ingest::document::SourceDocument::whole(&bytes, "pdf", &label);
        let outcome =
            prism_ingest::document::read(&doc, &prism_ingest::document::Policy::default())
                .await
                .with_context(|| format!("reading PDF {} locally", path.display()))?;

        // Say what could not be read and what could not be tried. A page of a
        // paper that silently never reached the extractor is exactly the kind
        // of hole this plane exists to stop being invisible.
        for skipped in &outcome.skipped {
            eprintln!("{}", skip_note_line(skipped));
        }
        let unrecovered: Vec<String> = outcome
            .unrecovered()
            .map(|note| format!("p{} ({})", note.number, note.damage))
            .collect();
        let warning = (!unrecovered.is_empty()).then(|| {
            format!(
                "{} of {} pages could not be read cleanly: {}",
                unrecovered.len(),
                outcome.understanding.pages.len(),
                unrecovered.join(", "),
            )
        });
        if let Some(warning) = &warning {
            eprintln!("Warning: {warning}");
        }

        // Mid-corpus endpoint death (audit F3): the LIVE reader's own reads
        // are the health signal — no probe exists to disagree with them.
        // After enough consecutive ENDPOINT-side failures the key is
        // withdrawn: the seam's inverse swaps in the tombstone and the REST
        // of the corpus skips the dead host via readiness instead of
        // re-paying its timeout per file; bounded trial reads later win it
        // back. A single failure, any successful vision read, or any number
        // of LOCAL failures (poppler dying on an encrypted file says
        // nothing about the host) withdraws nothing.
        if let Some(seam) = vision_seam.as_mut()
            && let Some(reason) = seam.observe(&outcome).await
        {
            eprintln!("Note: vision reader parked — {reason}");
        }

        // The seam's consumer-side decision (audit F2): pages that needed
        // vision while the endpoint key is withdrawn — or whose live read
        // failed on the wire — are DEFERRED — recorded
        // in the summary with the reason, readable by a re-run once the
        // endpoint answers — and everything the text layer read soundly is
        // extracted and stored NOW. The old code parked the whole document
        // here (39 sound pages discarded over one micrograph) and the run
        // still exited 0; a tool that silently ingests nothing and reports
        // success is far worse than one that ingests degraded text.
        let deferred = deferred_vision_pages(vision_seam.as_ref(), &outcome);

        let (text, ranges) = outcome.understanding.plain_text_with_page_ranges();
        (text, ranges, warning, deferred)
    } else {
        // Non-PDF text formats just read the file. Either way the runtime
        // sidecar is never contacted on the local path.
        let (text, _pages, warning) = extract_platform_ingest_text(path, runtime_url).await?;
        (text, Vec::new(), warning, None)
    };
    let chars = text.chars().count();
    if text.trim().is_empty() {
        match deferred {
            // The fully-scanned PDF: no readable text NOW, but every page is
            // deferred behind the vision endpoint — so this is a real
            // summary (zero facts, the deferred pages machine-readable), not
            // an error. Bailing here made the document that most needs
            // vision the only one excluded from vision recovery: the bail
            // aborted the whole batch (`handle_ingest` stops on a file
            // error), the file's mtime was already recorded as seen, and the
            // error string carried no retry marker — permanently lost, and
            // invisible to the run-level deferral verdict (audit F9).
            Some(deferred) => {
                eprintln!(
                    "Note: no ingestable text in {} yet — every readable page \
                     is deferred behind the vision endpoint",
                    path.display()
                );
                return Ok(serde_json::json!({
                    "backend": "local_text",
                    "path": path.display().to_string(),
                    "format": ingest_format(path),
                    "chars": 0,
                    "warning": warning,
                    "facts_written": 0,
                    "deferred": deferred,
                }));
            }
            // With nothing deferred there is nothing a re-run could recover:
            // the document itself has no ingestable text, and that is still
            // an honest per-file error.
            None => bail!("No ingestable text found in {}", path.display()),
        }
    }

    if schema_only {
        let mut summary = serde_json::json!({
            "backend": "local_text",
            "path": path.display().to_string(),
            "format": ingest_format(path),
            "schema_only": true,
            "chars": chars,
            "warning": warning,
        });
        if let Some(deferred) = deferred {
            summary["deferred"] = deferred;
        }
        return Ok(summary);
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

    // CONTRACT CHANGE (agentic paper reading): prompt context size no longer
    // partitions the source. One bounded loop owns the complete document and
    // pulls only the ontology entries and numbered paper lines it needs.
    // Repeating that loop once per old prompt window would multiply cost and
    // produce duplicate ontology-extension proposals without exposing any
    // additional data.
    let windows = [(0usize, text.len())];
    let chunks_total = windows.len();
    // The agentic reader receives the complete RAW document below. Raw line
    // boundaries are the stable coordinates carried into provenance and must
    // never be defined by PDF-wrap heuristics.
    // Cost, said BEFORE the run starts: input at the client's ~4-bytes/token
    // estimate. Output is metered and billed per token — counting is the
    // control, and the actual usage is reported at the end.
    eprintln!(
        "  extraction plan: {} bytes and {} raw line(s) in one tool-driven paper workspace; \
         the model fetches bounded ranges on demand; output is metered and billed per token",
        text.len(),
        text.lines().count().max(1),
    );

    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let prism_home = PathBuf::from(home).join(".prism");
    // ONE store-selection rule, shared with every other plane:
    // `$PRISM_PROVENANCE_DB` then `~/.prism/provenance.db`. Hardcoding the
    // home path here made the override work on some commands and silently
    // not others — which is how a benchmark writes into the LIVE graph.
    let db_path = prism_provenance::store_path();
    let store = prism_provenance::ProvenanceStore::open(&db_path).await?;

    let now = chrono::Utc::now().to_rfc3339();
    // Keep the original document as the independence/origin identity, while
    // retrieval reopens the exact text representation whose hash and lines
    // the population agent cited. This is essential for PDFs (binary source,
    // extracted-text coordinates) and also freezes evidence across reader
    // upgrades or later edits to a text file.
    let source_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let document_id = source_path.display().to_string();
    let source_text_snapshot = persist_source_text_snapshot(&prism_home, &text)?;
    let prov = prism_provenance::LocalProvenance {
        activity_id: uuid::Uuid::new_v4().to_string(),
        agent_id: agent_id.clone(),
        agent_kind: "SoftwareAgent".into(),
        // ONE provenance source for every window of this document — the
        // file itself. The store keys evidence independence on the origin
        // source (`origin_source_key`), so two windows of the SAME document
        // asserting one fact contribute ONE evidence row: chunking cannot
        // fabricate corroboration or inflate confidence.
        source_entity_id: source_text_snapshot.display().to_string(),
        source_kind: "Document".into(),
        // CONTRACT CHANGE: the text path used to write every ontology into
        // the bare "local" tenant, blending a promoted ontology's facts
        // into EMMO's keyspace while the tabular path isolated them.
        // Compose the per-ontology tenant exactly as the tabular path does;
        // the default ontology keeps the bare "local" every existing store
        // was written with.
        tenant: prism_ingest::ontologies::storage_tenant(
            prism_provenance::LOCAL_TENANT,
            ontology.id(),
        ),
        started_at: now.clone(),
        ended_at: now,
        locality: "local".into(),
        // Corroboration still keys on the original paper, not on a derived
        // text snapshot or a particular extraction run.
        origin_source_id: Some(document_id.clone()),
        // The agent tool call that launched this CLI run, when one did.
        // `None` when a person ran the command directly.
        origin_action_id: prism_provenance::action_id_from_env(),
    };
    store.record_activity(&prov).await?;

    let classification = prism_provenance::OntologyClassification {
        version_iri: ontology.version_iri().as_str(),
        artifact_sha256: ontology.artifact_sha256(),
    };

    // Chunks are extracted and written ONE AT A TIME, so a failure at chunk
    // 7 of 20 costs chunk 7: everything already written stays written, the
    // failure lands on the errors spine (non-zero exit), and an interrupted
    // run keeps the chunks it finished.
    let mut written_facts: Vec<prism_provenance::MaterialFact> = Vec::new();
    // Property names contributed by facts that were actually STORED,
    // accumulated across chunks so the resolution ladder below runs once per
    // document rather than once per window.
    let mut property_terms: Vec<prism_ingest::property_resolution::PropertyTerm> = Vec::new();
    let mut seen_facts: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut dropped_facts: Vec<String> = Vec::new();
    let mut parse_errors: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut peer_echoes: Vec<serde_json::Value> = Vec::new();
    let mut peer_echo_check_errors: Vec<String> = Vec::new();
    let mut semantic_validation = Vec::new();
    let mut agent_traces: Vec<prism_ingest::paper_agent::PaperAgentTrace> = Vec::new();
    // Samples that lost their agreement vote because they never had a fair
    // chance to read the document, and chunks where EVERY sample showed the
    // routed model was not capable — both reported, never silent.
    let mut agreement_exclusions: Vec<prism_ingest::text_extract::SampleExclusion> = Vec::new();
    // Summed across chunks so the reported rate describes the whole document.
    let mut agreement: Option<prism_ingest::text_extract::AgreementSummary> = None;
    let mut model_insufficient: Vec<prism_ingest::text_extract::ModelInsufficiency> = Vec::new();
    let mut agent_turns = 0usize;
    let mut agent_tool_calls = 0usize;
    let mut proposed_classes: Vec<prism_ingest::paper_agent::OntologyClassProposal> = Vec::new();
    let mut proposed_relations: Vec<prism_ingest::paper_agent::OntologyRelationProposal> =
        Vec::new();
    let mut chunks_processed = 0usize;
    let mut llm_usage: Option<prism_ingest::llm::UsageInfo> = None;
    let mut semantic_policy =
        prism_ingest::semantic_validation::SemanticValidationPolicy::default();
    // The numeric prior's eligible fact kinds come from the active
    // ontology's declaration, not a materials-shaped Rust default.
    semantic_policy.resolve_eligible_fact_kinds(ontology.as_ref());

    // ── The extraction funnel, counted where each fact leaves it ──
    //
    // "58 extracted, 4 stored" is an anecdote until every proposed fact is
    // attributed to where it ended up. Under annotate-not-refuse a failed
    // check no longer drops the fact — it is STORED carrying a weak
    // verification status — so the funnel partitions by destination:
    // facts_proposed = stored_trusted + stored_unverified +
    // dropped_malformed + deduped + store_failed, asserted by test.
    // `dropped_malformed` is the one refusal left (shapes the store cannot
    // represent); `store_failed` covers facts stranded by a mid-chunk
    // store-write failure (also on the errors spine) so the identity holds
    // even on a partial run.
    let mut facts_proposed = 0usize;
    let mut stored_trusted = 0usize;
    let mut stored_unverified = 0usize;
    // Per-status counts of the unverified facts — the review surface's
    // work queue, visible in the summary rather than buried in the store.
    let mut unverified_by_status: std::collections::BTreeMap<&'static str, usize> =
        std::collections::BTreeMap::new();
    let mut dropped_malformed = 0usize;
    let mut deduped = 0usize;
    let mut store_failed = 0usize;

    let paper_policy = crate::paper_agent_policy(project_root);
    for (index, (start, end)) in windows.iter().enumerate() {
        let chunk_no = index + 1;
        eprintln!(
            "  chunk {chunk_no}/{chunks_total}: extracting bytes {start}-{end} \
             (a local model can take minutes per chunk — this is work, not a hang)…"
        );
        let extraction = match prism_ingest::text_extract::extract_facts_from_chunk_sampled(
            &llm,
            title,
            &text[*start..*end],
            prism_ingest::text_extract::DocumentContext {
                document: &text,
                chunk_start_byte: *start,
                ontologies: &ontology_set,
            },
            prism_ingest::text_extract::GroundingPolicy::default(),
            sampling,
            paper_policy,
        )
        .await
        {
            Ok(extraction) => extraction,
            Err(e) => {
                errors.push(format!(
                    "chunk {chunk_no}/{chunks_total} (bytes {start}-{end}): LLM extraction \
                     failed: {e:#}"
                ));
                eprintln!(
                    "  chunk {chunk_no}/{chunks_total}: FAILED — facts from completed \
                     chunks are already stored; continuing with the remaining chunks"
                );
                continue;
            }
        };
        let prism_ingest::text_extract::TextExtraction {
            write_ups: _,
            facts,
            citations,
            ontology_bindings,
            parse_error,
            dropped_facts: chunk_dropped_facts,
            rejections: chunk_rejections,
            usage,
            agent_traces: chunk_agent_traces,
            agreement_exclusions: chunk_agreement_exclusions,
            model_insufficient: chunk_model_insufficient,
            agreement: chunk_agreement,
            proposed_classes: chunk_proposed_classes,
            proposed_relations: chunk_proposed_relations,
        } = extraction;
        if let Some(chunk_agreement) = chunk_agreement {
            agreement
                .get_or_insert_with(prism_ingest::text_extract::AgreementSummary::default)
                .absorb(&chunk_agreement);
        }
        if let Some(usage) = usage {
            let total = llm_usage.get_or_insert(prism_ingest::llm::UsageInfo {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
            });
            total.prompt_tokens += usage.prompt_tokens;
            total.completion_tokens += usage.completion_tokens;
            total.total_tokens += usage.total_tokens;
        }
        if let Some(parse_error) = parse_error {
            parse_errors.push(format!("chunk {chunk_no}/{chunks_total}: {parse_error}"));
        }
        agent_turns += chunk_agent_traces
            .iter()
            .map(|trace| trace.turns)
            .sum::<usize>();
        agent_tool_calls += chunk_agent_traces
            .iter()
            .flat_map(|trace| &trace.samples)
            .map(|turn| turn.tool_calls.len())
            .sum::<usize>();
        agent_traces.extend(chunk_agent_traces);
        agreement_exclusions.extend(chunk_agreement_exclusions);
        model_insufficient.extend(chunk_model_insufficient);
        proposed_classes.extend(chunk_proposed_classes);
        proposed_relations.extend(chunk_proposed_relations);
        // Funnel: everything the model proposed for this chunk either
        // survived into `extraction.facts` (annotated with a verification
        // status) or is in `extraction.rejections` (shapes the store cannot
        // represent — the one refusal left).
        facts_proposed += facts.len() + chunk_rejections.len();
        dropped_malformed += chunk_rejections.len();
        dropped_facts.extend(chunk_dropped_facts);

        if facts.len() != citations.len() || facts.len() != ontology_bindings.len() {
            // This is an internal contract breach, not a grounding verdict.
            // A fact without its exact witness cannot enter the cited write
            // path, so surface the whole chunk as failed rather than pairing
            // unrelated entries by position.
            store_failed += facts.len();
            errors.push(format!(
                "chunk {chunk_no}/{chunks_total}: extractor returned {} facts, {} citations, and {} ontology bindings",
                facts.len(),
                citations.len(),
                ontology_bindings.len()
            ));
            continue;
        }

        // De-duplicate across windows: the overlap re-reads boundary text by
        // design, so both neighbours may extract the same fact — it is ONE
        // fact. (The store would refuse the duplicate evidence anyway; this
        // keeps the stored counts honest and skips redundant writes.) The
        // dedup key is the FACT identity with the verification stamp
        // cleared: the same fact judged differently at a window seam is
        // still one fact, and the store's best-wins aggregation is the
        // right place to reconcile the two verdicts — but only the first
        // sighting is written here, so the seam duplicate is simply skipped.
        let annotated_count = facts.len();
        let new_facts: Vec<(
            prism_provenance::MaterialFact,
            prism_provenance::SourceCitation,
            prism_ingest::paper_agent::FactOntologyBinding,
        )> = facts
            .into_iter()
            .zip(citations)
            .zip(ontology_bindings)
            .map(|((fact, citation), binding)| (fact, citation, binding))
            .filter(|(fact, _, _)| {
                let mut identity = fact.clone();
                identity.verification = None;
                identity.verification_reason = None;
                serde_json::to_string(&identity)
                    .map(|key| seen_facts.insert(key))
                    .unwrap_or(true)
            })
            .collect();
        deduped += annotated_count - new_facts.len();
        let new_fact_values: Vec<prism_provenance::MaterialFact> =
            new_facts.iter().map(|(fact, _, _)| fact.clone()).collect();

        // Peer-echo tripwire, BEFORE this chunk's writes: an agent that read
        // a peer fact out of `prism query` and fed it back through `prism
        // ingest` re-asserts it under "local" — laundering peer knowledge
        // into local, which corroboration then counts as independent
        // evidence. The store cannot block that write (it is
        // indistinguishable from a genuinely independent source stating the
        // same fact), so the collision is reported LOUDLY instead of
        // absorbed silently. DETECTION, not prevention: the write proceeds.
        let (echoes, check_errors) = collect_peer_echoes(&store, &new_fact_values).await;
        if !echoes.is_empty() {
            eprintln!(
                "  WARNING: {} extracted fact(s) already exist under mesh peer tenant(s).",
                echoes.len()
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
        if !check_errors.is_empty() {
            eprintln!(
                "  WARNING: {} fact(s) could not be checked against mesh tenants — their \
                 laundering status is UNKNOWN, not clean (see `peer_echo_check_errors`).",
                check_errors.len()
            );
        }
        peer_echoes.extend(echoes);
        peer_echo_check_errors.extend(check_errors);

        // Ontology navigation and semantic judgement belong to the bounded
        // paper loop above. Do not follow it with a second one-shot
        // classifier or a fixed corpus prior. Until a proposed ontology
        // extension is governed and applied, the storage adapter uses its
        // generic node shape while the assertion and ontology-version stamp
        // remain complete.
        let classes: std::collections::HashMap<String, prism_ingest::classify::EntityClass> =
            std::collections::HashMap::new();

        // The model proposes; geometry measures. This runs once for the
        // chunk's whole batch BEFORE its first fact write. Its report cannot
        // alter, merge, drop, or block a proposal, and unavailable geometry
        // remains an explicit status in the returned ingest summary.
        use prism_provenance::FactPayload as _;
        let local_facts: Vec<_> = new_fact_values
            .iter()
            .map(|fact| fact.to_local_fact())
            .collect();
        let semantic_entities = semantic_entities_for_text_facts(&new_fact_values, &classes);
        let semantic = prism_ingest::semantic_validation::validate_write_best_effort(
            &store,
            &semantic_entities,
            &local_facts,
            &prov.tenant,
            &semantic_policy,
        )
        .await;
        semantic_validation.push(semantic.report.clone());

        let mut chunk_written = 0usize;
        let mut write_error = None;
        let planned_writes = new_facts.len();
        for (fact, citation, binding) in new_facts {
            let subject = binding
                .subject_class_iri
                .as_deref()
                .map(|iri| prism_ingest::paper_agent::resolve_class_binding(&ontology_set, iri))
                .transpose()
                .map_err(anyhow::Error::msg)?;
            let object = binding
                .object_class_iri
                .as_deref()
                .map(|iri| prism_ingest::paper_agent::resolve_class_binding(&ontology_set, iri))
                .transpose()
                .map_err(anyhow::Error::msg)?;
            let nodes = prism_provenance::OntologyBoundFactNodes {
                subject: subject
                    .as_ref()
                    .map(|node| prism_provenance::ClassifiedNode {
                        entity_type: &node.entity_type,
                        storage_label: &node.storage_label,
                        class_iri: &node.class_iri,
                    }),
                object: object
                    .as_ref()
                    .map(|node| prism_provenance::ClassifiedNode {
                        entity_type: &node.entity_type,
                        storage_label: &node.storage_label,
                        class_iri: &node.class_iri,
                    }),
            };
            let write = store
                .write_ontology_bound_fact_with_citation(
                    &fact,
                    &prov,
                    fact.evidence_class,
                    nodes,
                    classification,
                    &citation,
                )
                .await;
            match write {
                Ok(()) => {
                    match fact.verification {
                        Some(status) if !status.is_trusted() => {
                            stored_unverified += 1;
                            *unverified_by_status.entry(status.as_str()).or_insert(0) += 1;
                        }
                        _ => stored_trusted += 1,
                    }
                    prism_ingest::property_resolution::property_terms_for_fact(
                        &ontology_set,
                        &fact,
                        binding.predicate_iri.as_deref(),
                        binding.object_class_iri.as_deref(),
                        usize::try_from(citation.line_start())
                            .ok()
                            .zip(usize::try_from(citation.line_end()).ok())
                            .filter(|(from_line, to_line)| *from_line >= 1 && to_line >= from_line)
                            .map(
                                |(from_line, to_line)| prism_ingest::paper_agent::PaperCitation {
                                    source_revision_id: citation.source_revision_id().to_string(),
                                    from_line,
                                    to_line,
                                    quoted_text: citation.evidence_span().to_string(),
                                },
                            ),
                        &mut property_terms,
                    );
                    written_facts.push(fact);
                    chunk_written += 1;
                }
                Err(e) => {
                    write_error = Some(format!(
                        "chunk {chunk_no}/{chunks_total}: store write failed after {chunk_written} \
                         fact(s) ('{} {} {}'): {e:#}",
                        fact.subject, fact.predicate, fact.object
                    ));
                    break;
                }
            }
        }

        // Reuse the validation batch's one model call after the unchanged
        // graph writes. The storage query joins through existing entities,
        // so endpoints from a refused/unwritten fact cannot be minted here.
        if let Some(model) = semantic.embedding_model()
            && let Err(error) = store
                .store_precomputed_name_embeddings(
                    semantic.embedding_names(),
                    semantic.embedding_vectors(),
                    &prov.tenant,
                    model,
                )
                .await
        {
            tracing::warn!(
                %error,
                "storing semantic validation vectors failed — graph write unaffected"
            );
        }
        match write_error {
            Some(message) => {
                // Facts stranded by the failed write: neither written nor
                // dropped, so the funnel needs its own bucket for them.
                store_failed += planned_writes - chunk_written;
                errors.push(message);
                eprintln!(
                    "  chunk {chunk_no}/{chunks_total}: STORE WRITE FAILED — earlier \
                     facts are already stored; continuing with the remaining chunks"
                );
            }
            None => {
                chunks_processed += 1;
                eprintln!("  chunk {chunk_no}/{chunks_total}: {chunk_written} fact(s) stored");
            }
        }
    }

    eprintln!(
        "  extraction funnel: {facts_proposed} proposed → {} stored \
         ({stored_trusted} verified, {stored_unverified} stored-unverified \
         [excluded from default reads, awaiting review], \
         {dropped_malformed} malformed, {deduped} duplicate, \
         {store_failed} store-failed)",
        written_facts.len(),
    );
    if !unverified_by_status.is_empty() {
        let breakdown: Vec<String> = unverified_by_status
            .iter()
            .map(|(status, count)| format!("{count} {status}"))
            .collect();
        eprintln!("  unverified by status: {}", breakdown.join(", "));
    }

    // ── Persist the ontology-extension proposals ───────────────────────
    //
    // Until now these were PRINT-ONLY: `ontology_extensions` in the summary
    // JSON was the only place a proposal ever landed, and a 91-paper corpus
    // run lost every one of its 3,947 citation-backed class proposals that
    // way. The loop can read an ontology; it cannot grow one without this.
    // Proposals go to the durable governance queue with their citations;
    // identities already dispositioned (accepted or rejected) are suppressed
    // and COUNTED, never silently dropped. A store failure is on the errors
    // spine — a proposal that cannot be stored must say so, not vanish.
    let mut proposals_enqueued = 0usize;
    let mut proposals_suppressed = 0usize;
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0);
    {
        use prism_ingest::paper_agent::{class_proposal_queue_item, relation_proposal_queue_item};
        let document_label = path.display().to_string();
        let mut queued = Vec::new();
        for proposal in &proposed_classes {
            queued.push(class_proposal_queue_item(
                proposal,
                &document_label,
                &prov.tenant,
                now_secs,
            ));
        }
        for proposal in &proposed_relations {
            queued.push(relation_proposal_queue_item(
                proposal,
                &document_label,
                &prov.tenant,
                now_secs,
            ));
        }
        for (item, citation_json) in queued {
            match store
                .enqueue_ontology_proposal(&item, &citation_json, now_secs)
                .await
            {
                Ok(prism_provenance::OntologyProposalEnqueue::SupersededByDisposition) => {
                    proposals_suppressed += 1;
                }
                Ok(_) => {
                    proposals_enqueued += 1;
                }
                Err(error) => {
                    errors.push(format!(
                        "persisting ontology proposal '{}' failed: {error:#}",
                        item.label
                    ));
                }
            }
        }
    }
    if proposals_enqueued > 0 || proposals_suppressed > 0 {
        eprintln!(
            "  ontology proposals: {proposals_enqueued} queued for governance \
             (with citations), {proposals_suppressed} suppressed (already \
             accepted or rejected) — review with `prism ontology proposals list`"
        );
    }

    if let Some(usage) = &llm_usage {
        eprintln!(
            "  LLM usage (billed): {} prompt + {} completion = {} tokens",
            usage.prompt_tokens, usage.completion_tokens, usage.total_tokens
        );
    }

    // Zero facts because the model returned garbage is a different outcome
    // from zero facts because the document held none. Only `parse_error`
    // tells them apart on the user's side; with windows it aggregates one
    // reason per garbled chunk.
    let parse_error = if parse_errors.is_empty() {
        None
    } else {
        Some(parse_errors.join("; "))
    };

    // LOUD capability refusal (§D.5): a document that EVERY sample showed
    // the routed model could not read must say so on stderr as well as in
    // the machine-readable summary — a thin result must not pass quietly
    // as a quiet paper.
    for verdict in &model_insufficient {
        eprintln!("  WARNING: {}", verdict.detail);
    }

    // The ontology resolution ladder, AFTER every write: bind each stored
    // measurement's free-text property name against the UNION of loaded
    // ontologies (exact → normalised → semantic → proposal). Facts are
    // already in the store, so no rung can discard one; a resolution
    // failure is reported in the summary and never fails the ingest that
    // produced it. Terms that bind to nothing become citation-carrying
    // proposals and stay on `unbound_term_bindings` for re-resolution — no
    // paper is re-read when a richer ontology later arrives.
    let property_resolution = if property_terms.is_empty() {
        None
    } else {
        let backend = tokio::task::spawn_blocking(prism_embed::from_config)
            .await
            .ok()
            .flatten();
        match prism_ingest::property_resolution::resolve_property_terms(
            &store,
            &ontology_set,
            backend.as_deref(),
            &prov.tenant,
            &document_id,
            &property_terms,
            prism_ingest::property_resolution::DEFAULT_SEMANTIC_BIND_THRESHOLD,
        )
        .await
        {
            Ok(bindings) => Some(prism_ingest::property_resolution::binding_report(&bindings)),
            Err(error) => Some(serde_json::json!({
                "error": format!(
                    "property resolution failed after the writes (facts are unaffected): {error:#}"
                ),
            })),
        }
    };

    let mut summary = serde_json::json!({
        "backend": "local_text",
        "path": path.display().to_string(),
        "format": ingest_format(path),
        "schema_only": false,
        "chars": chars,
        "facts_written": written_facts.len(),
        "property_resolution": property_resolution,
        "model": agent_id,
        "store": db_path.display().to_string(),
        "source_text_snapshot": source_text_snapshot.display().to_string(),
        "warning": warning,
        // Coverage, reported where a log line cannot be lost: the subscriber
        // is built with `EnvFilter::from_default_env()`, whose default
        // directive is ERROR, so `tracing::warn!` reaches nobody unless
        // RUST_LOG is set. `chunks_processed < chunks_total` is ALWAYS
        // accompanied by entries in `errors` (→ FAILED STEPS, non-zero exit).
        "chunks_total": chunks_total,
        "chunks_processed": chunks_processed,
        // The extraction funnel: every proposed fact attributed to where it
        // ended up. The counters PARTITION facts_proposed —
        // stored_trusted + stored_unverified + dropped_malformed + deduped
        // + store_failed = facts_proposed — so "58 extracted, 4 stored"
        // stops being an anecdote and becomes a diagnosis. Under
        // annotate-not-refuse a failed check stores the fact with a weak
        // verification status (`stored_unverified`, broken down by status)
        // instead of dropping it; only unstorable shapes are dropped.
        "funnel": {
            "facts_proposed": facts_proposed,
            "stored_trusted": stored_trusted,
            "stored_unverified": stored_unverified,
            "unverified_by_status": unverified_by_status,
            "dropped_malformed": dropped_malformed,
            "deduped": deduped,
            "store_failed": store_failed,
            "facts_written": written_facts.len(),
        },
        "parse_error": parse_error,
        // Facts dropped ONE BY ONE during extraction — only shapes the
        // store cannot represent (for example, undeserializable fact JSON).
        // Non-empty unit terms are preserved; absent or empty numeric units
        // are stored under `unit_unresolved`, not dropped. A
        // failed CHECK no longer drops a fact: it stores it under a weak
        // verification status (see `funnel.stored_unverified`). Same
        // contract as the tabular pipeline's `dropped_relationships`: a
        // PARTIAL result the summary must surface, never a silent drop.
        "dropped_facts": dropped_facts,
        // Compatibility counters for clients that consumed the old summary.
        // Fresh paper-agent proposals never enter the legacy repair queue:
        // malformed tool calls receive retryable errors inside the bounded
        // loop, and any still-unrepresentable result is reported above with
        // its complete tool trace rather than handed to a second domain prompt.
        "repairs": {
            "code_accepted": 0,
            "code_withdrawn": 0,
            "enqueued_for_model": 0,
            "dispositions": [],
        },
        // Chunk-level step failures: extraction or store-write failures for
        // individual chunks. The OTHER chunks' facts are already stored — a
        // mid-run failure costs the failed chunk, never the run.
        "errors": errors,
        // What the run actually cost, when the backend reports usage.
        "llm_usage": llm_usage,
        // The complete population-loop audit surface. `traces` retains every
        // tool name, argument and result; the totals make budget behaviour
        // easy to inspect without first traversing the nested trace.
        "paper_agent": {
            "loops": agent_traces.len(),
            "turns": agent_turns,
            "tool_calls": agent_tool_calls,
            // Samples that got no agreement vote because they never had a
            // fair chance to read the document, each with the measured
            // reason.
            "agreement_exclusions": agreement_exclusions,
            // What cross-sample agreement ACHIEVED. Absent for a single-pass
            // run, which never attempted it. A run where nothing agreed must
            // not be indistinguishable from a clean one.
            "agreement": agreement,
            "traces": agent_traces,
        },
        // Non-empty means EVERY sample of those chunks showed the routed
        // model was not capable of reading the document. The annotated facts
        // are retained; the verdict names the model, the numbers, and what
        // to change — a thin result must not masquerade as a quiet paper.
        "model_insufficient": model_insufficient,
        // Ontology evolution is a separate governed product. Population only
        // records what the reader proposed against the selected ontology; it
        // never mutates that ontology during extraction.
        "ontology_extensions": {
            "classes": proposed_classes,
            "relations": proposed_relations,
        },
        // The durable half of the same record: every proposal above is also
        // queued (with its citations) in the governance store, so review —
        // not stdout — is where a proposal lives or dies. `suppressed`
        // counts identities already accepted or rejected, which are never
        // re-queued.
        "ontology_proposal_queue": {
            "enqueued": proposals_enqueued,
            "suppressed": proposals_suppressed,
        },
        // Facts that already exist under a mesh peer tenant — the loud
        // half of the laundering tripwire (see the WARNING above).
        "peer_echoes": peer_echoes,
        // Facts whose echo check FAILED: unknown status, not clean.
        "peer_echo_check_errors": peer_echo_check_errors,
        // One report per write-bearing extraction chunk. A status of
        // `unavailable` or `failed` has `passed: null`; an unchecked write
        // can therefore never pose as validated in machine-readable output.
        "semantic_validation": semantic_validation,
    });
    // Pages waiting on the vision endpoint (audit F2): everything above was
    // extracted from what COULD be read; this names what could not, and
    // why, so a re-run can finish the job.
    if let Some(deferred) = deferred {
        summary["deferred"] = deferred;
    }
    Ok(summary)
}

/// Drain a legacy document repair queue one item at a time.
///
/// Fresh paper-agent population never writes this queue: invalid tool calls
/// receive retryable errors inside the bounded loop, and valid cited facts
/// are stored directly. This command remains solely so installations with
/// pre-agent queue rows can finish or withdraw that already-persisted work.
async fn run_local_repair_pass(
    path: &Path,
    project_root: &Path,
    model: Option<&str>,
    llm_url: Option<&str>,
    api_key: Option<&str>,
    runtime_url: &str,
) -> Result<serde_json::Value> {
    // Same honesty as the ingest path: repair runs against the built-in
    // EMMO extraction contract only.
    let ontology_id = active_ontology_from_config(project_root)?;
    if ontology_id != prism_ingest::ontologies::DEFAULT_ONTOLOGY_ID {
        bail!(
            "text-document repair currently runs against the built-in EMMO ontology only; \
             the active ontology '{ontology_id}' has no repair tier. Set [ontology] id = \"emmo\"."
        );
    }
    let ontology = prism_ingest::ontologies::active(Some(&ontology_id))?;

    let (text, warning) = read_document_text(path, runtime_url)
        .await
        .with_context(|| format!("reading {} for the repair pass", path.display()))?;
    if text.trim().is_empty() {
        bail!("No readable text found in {}", path.display());
    }

    // Phase 1 records a canonical absolute path so retrieval can re-open the
    // cited source. Resolve the same key here when draining its repair queue.
    let document_id = std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string();

    let llm_cfg = build_llm_config(project_root, llm_url, model, api_key)?;
    let agent_id = if llm_cfg.model.is_empty() {
        "prism-repair".to_string()
    } else {
        llm_cfg.model.clone()
    };
    let llm = prism_ingest::llm::LlmClient::new(llm_cfg);

    let db_path = prism_provenance::store_path();
    let store = prism_provenance::ProvenanceStore::open(&db_path).await?;

    // Say "nothing to do" honestly instead of spending a model on an empty
    // queue (or mis-reporting it as a success that repaired something).
    if store.pending_repairs(&document_id, 1).await?.is_empty() {
        eprintln!(
            "repair queue: no pending items for {} — nothing to do.",
            path.display()
        );
        return Ok(serde_json::json!({
            "backend": "local_repair",
            "document": document_id,
            "warning": warning,
            "items_seen": 0,
            "accepted": 0,
            "withdrawn": 0,
            "requeued": 0,
            "model_calls": 0,
            "dispositions": [],
            "errors": [],
        }));
    }

    // The repair run is its OWN activity: a distinct agent row so the
    // ledger can tell Phase 1 writes from Phase 2 repairs.
    let now = chrono::Utc::now().to_rfc3339();
    let prov = prism_provenance::LocalProvenance {
        activity_id: uuid::Uuid::new_v4().to_string(),
        agent_id: agent_id.clone(),
        agent_kind: "SoftwareAgent".into(),
        source_entity_id: document_id.clone(),
        source_kind: "Document".into(),
        // Same composed tenancy as Phase 1 — one isolation contract for
        // every path. Today the repair pass refuses non-default ontologies,
        // so this still composes to the bare "local"; the shape is here so
        // the contract does not regress the day that gate opens.
        tenant: prism_ingest::ontologies::storage_tenant(
            prism_provenance::LOCAL_TENANT,
            ontology.id(),
        ),
        started_at: now.clone(),
        ended_at: now,
        locality: "local".into(),
        origin_source_id: None,
        // The agent tool call that launched this CLI run, when one did.
        // `None` when a person ran the command directly.
        origin_action_id: prism_provenance::action_id_from_env(),
    };
    store.record_activity(&prov).await?;
    let classification = prism_provenance::OntologyClassification {
        version_iri: ontology.version_iri().as_str(),
        artifact_sha256: ontology.artifact_sha256(),
    };

    let policy = prism_ingest::repair_worker::RepairWorkerPolicy::default();
    let decided_at = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
    eprintln!(
        "repair queue: working at most {} item(s) for {} — one model call per item, every \
         item ends in an explicit accept or withdraw (a local model can take minutes per \
         item — this is work, not a hang)…",
        policy.max_items_per_run,
        path.display()
    );
    let report = prism_ingest::repair_worker::run_repair_pass(
        &store,
        &llm,
        &document_id,
        &text,
        &prov,
        classification,
        &policy,
        decided_at,
        ontology.as_ref(),
    )
    .await?;

    for disposition in &report.dispositions {
        eprintln!(
            "  {} [{}] {}: {}",
            disposition.item_id, disposition.dispositioner, disposition.outcome, disposition.reason
        );
    }
    for error in &report.errors {
        eprintln!("  ERROR: {error}");
    }
    eprintln!(
        "repair queue: {} item(s) seen — {} accepted, {} withdrawn, {} requeued for a \
         later run; {} model call(s)",
        report.items_seen, report.accepted, report.withdrawn, report.requeued, report.model_calls
    );
    if let Some(usage) = &report.usage {
        eprintln!(
            "  LLM usage (billed): {} prompt + {} completion = {} tokens",
            usage.prompt_tokens, usage.completion_tokens, usage.total_tokens
        );
    }

    Ok(serde_json::json!({
        "backend": "local_repair",
        "document": document_id,
        "model": agent_id,
        "store": db_path.display().to_string(),
        "warning": warning,
        "items_seen": report.items_seen,
        "accepted": report.accepted,
        "withdrawn": report.withdrawn,
        "requeued": report.requeued,
        "model_calls": report.model_calls,
        "dispositions": report
            .dispositions
            .iter()
            .map(|d| serde_json::json!({
                "item_id": d.item_id,
                "attempt": d.attempt,
                "class": d.class,
                "outcome": d.outcome,
                "reason": d.reason,
                "dispositioner": d.dispositioner,
                "evidence": d.evidence,
            }))
            .collect::<Vec<_>>(),
        "errors": report.errors,
        "llm_usage": report.usage,
    }))
}

/// Human-readable rendering of a `local_repair` summary (the `--json` flag
/// prints the raw value instead).
fn print_repair_summary(summary: &serde_json::Value) {
    let document = summary["document"].as_str().unwrap_or("?");
    println!("Repairing: {document}");
    println!(
        "  Items: {} seen — {} accepted, {} withdrawn, {} requeued; {} model call(s)",
        summary["items_seen"].as_u64().unwrap_or(0),
        summary["accepted"].as_u64().unwrap_or(0),
        summary["withdrawn"].as_u64().unwrap_or(0),
        summary["requeued"].as_u64().unwrap_or(0),
        summary["model_calls"].as_u64().unwrap_or(0),
    );
    if let Some(dispositions) = summary["dispositions"].as_array() {
        for row in dispositions {
            println!(
                "  - [{}] {} {}: {}",
                row["dispositioner"].as_str().unwrap_or("?"),
                row["item_id"].as_str().unwrap_or("?"),
                row["outcome"].as_str().unwrap_or("?"),
                row["reason"].as_str().unwrap_or(""),
            );
        }
    }
    if let Some(errors) = summary["errors"].as_array() {
        for error in errors {
            println!("  ERROR: {}", error.as_str().unwrap_or("?"));
        }
    }
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

    // Pages deferred behind the vision endpoint (audit F2): named
    // FIRST, then the normal extraction report follows — the sound pages
    // WERE ingested and their counts must print. The old code parked the
    // whole document and returned here, which hid a stored-nothing run
    // behind a calm sentence.
    if let Some(deferred) = summary.get("deferred") {
        let reason = deferred
            .get("reason")
            .and_then(|value| value.as_str())
            .unwrap_or("waiting for a withdrawn provider");
        println!("  Deferred: {reason}");
    }

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
            if let Some(report) = row_coverage_report(result) {
                println!("{report}");
            }
            if let Some(report) = llm_usage_report(result) {
                println!("{report}");
            }
            if let Some(report) = extraction_decoding_report(result) {
                println!("{report}");
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
                    println!("  Warning: unparseable model output \u{2014} {parse_error}");
                }
                if let Some(report) = chunk_coverage_report(summary) {
                    println!("{report}");
                }
                if let Some(report) = agreement_report(summary) {
                    println!("{report}");
                }
                if let Some(report) = llm_usage_report(summary) {
                    println!("{report}");
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
                    // The extraction funnel: where every proposed fact went.
                    // This line is what turns "58 extracted, 4 stored" from
                    // an anecdote into a diagnosis.
                    if let Some(funnel) = summary.get("funnel") {
                        let count = |key: &str| {
                            funnel
                                .get(key)
                                .and_then(|value| value.as_u64())
                                .unwrap_or(0)
                        };
                        println!(
                            "  Funnel: {} proposed -> {} stored ({} verified, {} \
                             stored-unverified, {} malformed, {} duplicate, {} store-failed)",
                            count("facts_proposed"),
                            count("facts_written"),
                            count("stored_trusted"),
                            count("stored_unverified"),
                            count("dropped_malformed"),
                            count("deduped"),
                            count("store_failed"),
                        );
                        // The unverified facts are excluded from default
                        // reads and awaiting review — say so, by status,
                        // where the user can see it.
                        if let Some(by_status) = funnel
                            .get("unverified_by_status")
                            .and_then(|value| value.as_object())
                            .filter(|map| !map.is_empty())
                        {
                            let breakdown: Vec<String> = by_status
                                .iter()
                                .map(|(status, count)| format!("{count} {status}"))
                                .collect();
                            println!(
                                "  Unverified (excluded from default reads, findable \
                                 with `prism query --include-unverified`): {}",
                                breakdown.join(", ")
                            );
                        }
                    }
                    if let Some(alias) = summary.get("alias") {
                        let written = alias
                            .get("written")
                            .and_then(|value| value.as_u64())
                            .unwrap_or(0);
                        let rejected = alias
                            .get("rejected")
                            .and_then(|value| value.as_array())
                            .map(Vec::len)
                            .unwrap_or(0);
                        if written > 0 || rejected > 0 {
                            println!(
                                "  Aliases: {written} verified same_as edge(s) written, \
                                 {rejected} proposal(s) rejected"
                            );
                        }
                        if let Some(error) = alias.get("error").and_then(|value| value.as_str()) {
                            println!("  Warning: alias pass unavailable — {error}");
                        }
                    }
                    // Per-fact drops are the text path's analogue of the
                    // tabular `dropped_relationships` report: a PARTIAL
                    // result the user must see — these lines are the only
                    // window on it outside --json.
                    if let Some(report) = dropped_facts_report(summary) {
                        println!("{report}");
                    }
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

/// The decoding line for one local-tabular ingest result: whether the LLM
/// output was grammar-constrained to the active ontology's vocabulary, and
/// with which determinism knobs. A DEGRADED constraint (the endpoint
/// rejected `response_format: json_schema`, or the backend has none) is a
/// warning the user must see — a capability that silently isn't applied is
/// the exact defect class this pipeline keeps removing. `None` only when
/// extraction never ran (schema-only mode, refusal).
fn extraction_decoding_report(result: &serde_json::Value) -> Option<String> {
    let trace = result.get("extraction_decoding")?;
    let mode = trace.get("mode").and_then(|v| v.as_str()).unwrap_or("?");
    if let Some(reason) = trace.get("degraded").and_then(|v| v.as_str()) {
        return Some(format!(
            "  Warning: extraction was NOT schema-constrained (ran as {mode}): {reason}"
        ));
    }
    let seed = trace
        .get("seed")
        .and_then(|v| v.as_i64())
        .map_or("unset".to_string(), |s| s.to_string());
    let temperature = trace
        .get("temperature")
        .and_then(|v| v.as_f64())
        .map_or("unset".to_string(), |t| t.to_string());
    Some(format!(
        "  Extraction: schema-constrained to the active ontology's vocabulary \
         (mode {mode}, seed {seed}, temperature {temperature})"
    ))
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

/// The per-fact drop report for one local TEXT ingest, same contract as
/// [`dropped_relationships_report`]: extracted facts that were NOT written
/// because their shape cannot be represented — one entry per dropped fact,
/// with the reason. Unit vocabulary is not a drop gate: non-empty terms are
/// preserved exactly, while missing or empty numeric units are stored with a
/// `unit_unresolved` annotation. A drop is a PARTIAL result (the valid
/// remainder was stored, exit stays 0), never a silent one. `None` when
/// nothing was dropped.
fn dropped_facts_report(summary: &serde_json::Value) -> Option<String> {
    let dropped = summary.get("dropped_facts")?.as_array()?;
    if dropped.is_empty() {
        return None;
    }
    let mut out = format!(
        "  Dropped: {} extracted fact(s) NOT stored because their shape could not be represented:",
        dropped.len(),
    );
    for reason in dropped.iter().filter_map(|value| value.as_str()) {
        out.push_str(&format!("\n    ! {reason}"));
    }
    Some(out)
}

/// Row coverage for one local-tabular ingest: rows processed of rows held,
/// and the batch count. This line is the user's window on the run's
/// COMPLETENESS — the old pipeline extracted exactly 10 rows of any dataset
/// and reported the discard nowhere. `None` when extraction did not run
/// (schema-only). A shortfall is always accompanied by per-batch entries on
/// the errors spine (→ FAILED STEPS, non-zero exit).
fn row_coverage_report(result: &serde_json::Value) -> Option<String> {
    let batches = result.get("batches")?.as_u64()?;
    if batches == 0 {
        return None;
    }
    let processed = result.get("rows_processed")?.as_u64()?;
    let total = result.get("row_count")?.as_u64()?;
    let failed = result
        .get("batches_failed")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let mut out =
        format!("  Extraction: {processed} of {total} row(s) processed in {batches} batch(es)");
    if processed < total || failed > 0 {
        out.push_str(&format!(
            " — {} row(s) NOT processed ({failed} batch(es) failed; see FAILED STEPS)",
            total.saturating_sub(processed),
        ));
    }
    Some(out)
}

/// Cross-sample agreement for one paper ingest, and what it cost.
///
/// `None` for a single-pass run, which never attempted agreement — a rate
/// would imply a comparison that never happened.
///
/// This line exists because its absence hid a real failure. A corpus of 86
/// papers run at `--samples 3 --agreement 2` stamped `sample_disagreement` on
/// 21,109 of 21,218 facts. Every summary reported "facts written" and exited
/// clean, so the operator had no way to see that the default read filter was
/// hiding 99.5% of what had just been ingested. Corroboration failing is a
/// result about the MODEL, and it belongs next to the fact count it qualifies.
fn agreement_report(summary: &serde_json::Value) -> Option<String> {
    let agreement = summary.get("paper_agent")?.get("agreement")?;
    let claims = agreement.get("claims")?.as_u64()?;
    let corroborated = agreement.get("corroborated")?.as_u64()?;
    let required = agreement.get("required")?.as_u64()?;
    let comparable = agreement.get("comparable_samples")?.as_u64()?;
    if claims == 0 || required <= 1 {
        return None;
    }
    let percent = 100.0 * corroborated as f64 / claims as f64;
    let mut out = format!(
        "  Agreement: {corroborated} of {claims} claim(s) corroborated ({percent:.0}%) \
         by {required} of {comparable} comparable sample(s)"
    );
    let stamped = claims.saturating_sub(corroborated);
    if stamped > 0 {
        out.push_str(&format!(
            " — {stamped} stamped sample_disagreement and HIDDEN from `prism query` \
             unless --include-unverified"
        ));
    }
    Some(out)
}

/// Chunk coverage for one local TEXT ingest: windows processed of windows
/// planned. The whole document is windowed — nothing is truncated — so a
/// shortfall here means failed chunks, each with an entry on the errors
/// spine. `None` when the summary carries no chunk fields (schema-only).
fn chunk_coverage_report(summary: &serde_json::Value) -> Option<String> {
    let total = summary.get("chunks_total")?.as_u64()?;
    let processed = summary.get("chunks_processed")?.as_u64()?;
    let mut out = format!("  Chunks: {processed} of {total} processed (whole document windowed)");
    if processed < total {
        out.push_str(&format!(
            " — {} chunk(s) FAILED; their text was not extracted (see FAILED STEPS); \
             facts from completed chunks are stored",
            total - processed
        ));
    }
    Some(out)
}

/// What the run actually cost, when the backend reported usage: output is
/// metered and billed per token — counting is the control, not truncation.
/// `None` when the backend reported nothing (absence, never a made-up zero).
fn llm_usage_report(summary: &serde_json::Value) -> Option<String> {
    let usage = summary.get("llm_usage")?;
    let prompt = usage.get("prompt_tokens")?.as_u64()?;
    let completion = usage.get("completion_tokens")?.as_u64()?;
    Some(format!(
        "  LLM usage (billed): {prompt} prompt + {completion} completion = {} tokens",
        prompt + completion
    ))
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

/// How many documents in this run carry pages deferred behind the vision
/// endpoint. Surfaced as a visible count and used by watch mode to know
/// which files to re-ingest when the endpoint returns (audit F8/F9).
fn ingest_summary_deferrals(summaries: &[serde_json::Value]) -> usize {
    summaries
        .iter()
        .filter(|summary| summary.get("deferred").is_some())
        .count()
}

/// The starved fraction of a run's PDF corpus at which deferral stops being
/// a "partial" outcome and becomes the run's verdict. Inclusive: half the
/// corpus starving IS the vision outage costing the run its PDFs. Below it,
/// the deferral stays a visible count and exit 0 — making a genuinely
/// partial deferral an error would re-muzzle the run over pages it never
/// needed; above it, "399 of 400 starved plus one junk fact" must not read
/// as success (the round-three gate demanded ALL PDFs starved, so exactly
/// that run exited 0).
const STARVED_RUN_ERROR_PERCENT: usize = 50;

/// Audit F9: the run-level exit verdict for deferrals. The error-worthy
/// unit is a PDF-backed DOCUMENT that deferred pages and stored zero facts
/// (a "starved" document); the run fails when at least
/// [`STARVED_RUN_ERROR_PERCENT`] of its PDF documents starved. Counting
/// documents at that granularity (never a whole-run fact sum) keeps a
/// `.csv` in the directory from vouching for 399 empty PDFs, and keeps one
/// junk fact from vouching for the whole run — a deferred document that
/// stored facts is a live document, and never counts as starved. Starved is
/// counted over PDF-backed summaries ONLY, so a non-PDF that one day
/// carries `deferred` cannot push the count past the denominator and
/// silently disable the gate.
fn deferred_and_nothing_stored(summaries: &[serde_json::Value]) -> Option<String> {
    let is_pdf = |summary: &&serde_json::Value| {
        summary.get("format").and_then(|f| f.as_str()) == Some("pdf")
    };
    let pdf_documents = summaries.iter().filter(is_pdf).count();
    let starved = summaries
        .iter()
        .filter(is_pdf)
        .filter(|summary| {
            summary.get("deferred").is_some()
                && summary.get("facts_written").and_then(|v| v.as_u64()) == Some(0)
        })
        .count();
    (pdf_documents > 0 && starved * 100 >= pdf_documents * STARVED_RUN_ERROR_PERCENT).then(|| {
        format!(
            "{starved} of {pdf_documents} PDF document(s) have \
             {DEFERRED_NOTHING_STORED_MARKER} and stored nothing — re-run \
             once the endpoint answers (see the Deferred lines above)"
        )
    })
}

/// The stable phrase inside [`deferred_and_nothing_stored`]'s message, so
/// watch mode can recognise that error and keep the file eligible for the
/// endpoint-recovery retry (audit F8) — a fully-deferred file must not lose
/// its retry exactly because it also stored nothing (audit F9). Deliberately
/// does NOT say "withdrawn": the below-breaker window defers pages behind an
/// endpoint that is still live, and this message must stay true there.
const DEFERRED_NOTHING_STORED_MARKER: &str = "pages deferred behind the vision endpoint";

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

/// Upload a document to the configured provider's holistic ingest pipeline.
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
        // The agent tool call that launched this CLI run, when one did.
        // `None` when a person ran the command directly.
        origin_action_id: prism_provenance::action_id_from_env(),
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

/// What one `handle_ingest` run did, beyond its exit status. Watch mode
/// reads `deferred_documents` to know the file must be re-ingested once the
/// vision endpoint answers (audit F8).
#[derive(Debug)]
struct IngestRunReport {
    deferred_documents: usize,
}

/// The `--json` payload for one ingest run. A single file keeps its summary
/// as the whole payload (the shape scripts already parse); several files
/// ride under `documents`. Either way `deferred_documents` is a TOP-LEVEL
/// field: the run-level deferral count used to print in the human branch
/// alone, so an agent or script keying off `--json` could not see the
/// run-level verdict at all.
fn ingest_json_payload(
    summaries: Vec<serde_json::Value>,
    deferred_documents: usize,
) -> serde_json::Value {
    let mut payload = if summaries.len() == 1 {
        summaries.into_iter().next().unwrap_or_default()
    } else {
        serde_json::json!({ "documents": summaries })
    };
    if let Some(object) = payload.as_object_mut() {
        object.insert(
            "deferred_documents".to_string(),
            serde_json::json!(deferred_documents),
        );
    }
    payload
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
    sampling: prism_ingest::text_extract::SamplingPolicy,
    vision: VisionModelChoice<'_>,
) -> Result<IngestRunReport> {
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
        if schema_only {
            // `--schema-only` skips LLM extraction entirely, so NO document
            // text is sent anywhere regardless of where the model lives. The
            // remote banner below was printed here too, telling the operator
            // their document had been transmitted when the run made no model
            // call at all — verified: `--schema-only` on a 3795-char PDF exits
            // 0 having contacted nothing.
            //
            // That is the mirror image of the lie the comment below guards
            // against, and it is the more corrosive direction: a warning that
            // fires when it does not apply teaches the reader to ignore it,
            // and they will then ignore it on the run where it IS true. For a
            // brief that must not leave the machine, this banner is the one
            // signal that matters.
            eprintln!(
                "⚑ SCHEMA ONLY — measuring structure on-device; no model is called \
                 and no document text leaves your machine"
            );
        } else if locality.inference_is_on_device() {
            eprintln!("⚑ LOCAL — extracting on-device, nothing leaves your machine");
        } else if locality.is_local() {
            // Local pipeline, remote model. Say so plainly: document text IS
            // sent to the endpoint the caller named. Printing the on-device
            // banner here would be a lie the user cannot detect.
            eprintln!(
                "⚑ LOCAL PIPELINE, REMOTE MODEL — document text is sent to the \
                 endpoint you configured; facts are written to your local store"
            );
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

    // The vision seam lives for the WHOLE run (audit F3): one runtime, one
    // reader activation, one tombstone inverse — so a mid-corpus endpoint
    // death parks the reader once for every remaining file, instead of each
    // file rebuilding a runtime that forgot the last file's discovery.
    let mut vision_seam: Option<prism_ingest::document::VisionSeam> = None;

    let mut summaries = Vec::new();
    // Held, not `?`'d: a per-file error must still reach the seam
    // retirement below before it leaves this function.
    // One file's failure is that file's failure. This was `break` on the first
    // `Err`, and the run then returned before printing a single summary: file 7
    // of 400 unreadable meant files 8-400 never attempted AND files 1-6 —
    // already written to the graph — never reported, never counted, absent
    // from `--json`. The only production `Err => break` over a collection in
    // the workspace, and `prism ingest` is an agent tool, so it was the
    // model's path too.
    let mut file_errors: Vec<(PathBuf, anyhow::Error)> = Vec::new();
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
                .await
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
                    sampling,
                    vision,
                    &mut vision_seam,
                )
                .await
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
                .await
            }
            None => Err(anyhow::anyhow!(
                "Unsupported ingest format for {}. Supported: {}",
                target.display(),
                supported_ingest_formats()
            )),
        };
        match summary {
            Ok(summary) => summaries.push(summary),
            Err(error) => file_errors.push((target.clone(), error)),
        }
    }

    // Retire the run's component NOW, explicitly: `Runtime` has no `Drop`
    // by design, so dropping the slot with the fiber Active would leave the
    // tombstone inverse unrun and a stale reader registered process-wide
    // (watch mode, the TUI, a test binary) — in a module whose thesis is
    // that teardown must not be forgotten by hand.
    if let Some(seam) = vision_seam.take() {
        seam.retire().await;
    }
    let ingested = summaries.len();
    let total_step_errors: usize = summaries.iter().map(ingest_summary_errors).sum();
    let deferred_documents = ingest_summary_deferrals(&summaries);
    let deferred_nothing_stored = if schema_only {
        // Schema-only stores nothing BY DESIGN; a deferral there proves
        // nothing about the endpoint costing the run its facts.
        None
    } else {
        deferred_and_nothing_stored(&summaries)
    };

    if json_output {
        let mut payload = ingest_json_payload(summaries, deferred_documents);
        if let Some(object) = payload.as_object_mut() {
            // Every failed file, by name and reason — a script must be able
            // to tell "12 documents" from "12 of 40, 28 failed".
            object.insert("ingested".to_string(), serde_json::json!(ingested));
            object.insert(
                "file_errors".to_string(),
                serde_json::json!(
                    file_errors
                        .iter()
                        .map(|(path, error)| {
                            serde_json::json!({
                                "path": path.display().to_string(),
                                "error": format!("{error:#}"),
                            })
                        })
                        .collect::<Vec<_>>()
                ),
            );
        }
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        for (index, summary) in summaries.iter().enumerate() {
            if index > 0 {
                println!();
            }
            print_ingest_summary(summary);
        }
        for (path, error) in &file_errors {
            eprintln!("Failed: {} — {error:#}", path.display());
        }
        // A visible run-level count (audit F9): a partial deferral is NOT an
        // error — the sound pages' facts are stored — but it must never be
        // invisible either.
        if deferred_documents > 0 {
            println!(
                "\n{deferred_documents} document(s) have pages deferred behind the \
                 vision endpoint; re-run them once it answers"
            );
        }
    }

    // A failed file exits non-zero AFTER every sound file has been reported
    // and stored — the exit code says "not everything", the output says what.
    if !file_errors.is_empty() {
        let (first_path, first_error) = &file_errors[0];
        bail!(
            "{} of {} file(s) failed ({ingested} ingested) — first: {}: {first_error:#}",
            file_errors.len(),
            file_errors.len() + ingested,
            first_path.display()
        );
    }
    // Exit non-zero when configured steps failed — agents and scripts key
    // off the exit code, and the old exit-0-having-stored-nothing was the
    // audit's #2 critical.
    if total_step_errors > 0 {
        bail!("{total_step_errors} ingest step(s) failed — see errors above/in JSON");
    }
    // Audit F9: a fully-deferred run that stored NOTHING exits non-zero —
    // the parked summaries carry no `errors`, so without this the
    // exit-0-having-stored-nothing came straight back.
    if let Some(message) = deferred_nothing_stored {
        bail!("{message}");
    }

    Ok(IngestRunReport { deferred_documents })
}

/// Watch-mode pacing for endpoint-recovery retries (audit F8). A retry is a
/// REAL re-ingest — same auth, same timeouts, same retry policy as any read;
/// there is no probe to consult, because a check cheaper than the reader was
/// wrong about it twice — so it is paced like real work: first chance after
/// a minute, doubling to a fifteen-minute ceiling while the endpoint stays
/// dead, reset by any recovery. Quick enough to catch an endpoint that
/// blipped; bounded enough that a dead weekend costs dozens of single-file
/// trials, not thousands.
const WATCH_RETRY_MIN_DELAY: Duration = Duration::from_secs(60);
const WATCH_RETRY_MAX_DELAY: Duration = Duration::from_secs(900);

fn watch_retry_backoff(previous: Duration) -> Duration {
    (previous * 2).min(WATCH_RETRY_MAX_DELAY)
}

/// One watched file's ingest, with the deferred-set bookkeeping every call
/// site needs identically: a run that deferred documents — or error-exited
/// with the fully-deferred marker (audit F9), which must keep its retry
/// eligibility exactly like a partial deferral (audit F8) — stays in the
/// set; a clean run leaves it. Returns `true` when the file fully ingested
/// with nothing deferred.
#[allow(clippy::too_many_arguments)]
async fn watch_ingest_once(
    path: &Path,
    deferred: &mut std::collections::HashSet<PathBuf>,
    project_root: &Path,
    model: Option<&str>,
    llm_url: Option<&str>,
    api_key: Option<&str>,
    schema_only: bool,
    runtime_url: &str,
    corpus: Option<&str>,
    json_output: bool,
    mapping: Option<&Path>,
    sampling: prism_ingest::text_extract::SamplingPolicy,
    vision: VisionModelChoice<'_>,
) -> bool {
    match handle_ingest(
        path,
        project_root,
        model,
        llm_url,
        api_key,
        schema_only,
        runtime_url,
        corpus,
        json_output,
        mapping,
        sampling,
        vision,
    )
    .await
    {
        Ok(report) if report.deferred_documents > 0 => {
            deferred.insert(path.to_path_buf());
            false
        }
        Ok(_) => {
            deferred.remove(path);
            true
        }
        Err(error) => {
            if format!("{error:#}").contains(DEFERRED_NOTHING_STORED_MARKER) {
                deferred.insert(path.to_path_buf());
            }
            eprintln!("  Error: {error}");
            false
        }
    }
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
    sampling: prism_ingest::text_extract::SamplingPolicy,
    vision: VisionModelChoice<'_>,
) -> Result<()> {
    use std::collections::{HashMap, HashSet};
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
    // Files whose last ingest deferred pages behind the vision
    // endpoint (audit F8). Their mtimes are in `seen` like everyone else's —
    // this set is the promise "It will ingest fully once the endpoint is
    // back" actually being kept: each poll tick, if anything is deferred and
    // the endpoint answers a bounded probe, the deferred files re-ingest.
    let mut deferred: HashSet<PathBuf> = HashSet::new();

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
        watch_ingest_once(
            &path,
            &mut deferred,
            project_root,
            model,
            llm_url,
            api_key,
            schema_only,
            runtime_url,
            corpus,
            json_output,
            mapping,
            sampling,
            vision,
        )
        .await;
    }

    // Poll loop — check for new/modified files every 5 seconds
    let poll_interval = Duration::from_secs(5);
    let mut retry_delay = WATCH_RETRY_MIN_DELAY;
    let mut next_retry = std::time::Instant::now() + retry_delay;
    loop {
        tokio::time::sleep(poll_interval).await;

        // Audit F8: deferred files re-ingest once the endpoint recovers,
        // discovered by REAL reads on a bounded backoff. ONE trial file
        // pays for the discovery; only its recovery opens the gate for the
        // rest — so a dead endpoint costs one file's re-ingest per backoff
        // step, never the whole set per tick.
        if !deferred.is_empty()
            && std::time::Instant::now() >= next_retry
            && let Some(trial) = deferred.iter().next().cloned()
        {
            deferred.remove(&trial);
            println!(
                "Retrying deferred file {} (the vision endpoint may be back)",
                trial.display()
            );
            let recovered = watch_ingest_once(
                &trial,
                &mut deferred,
                project_root,
                model,
                llm_url,
                api_key,
                schema_only,
                runtime_url,
                corpus,
                json_output,
                mapping,
                sampling,
                vision,
            )
            .await;
            if recovered {
                retry_delay = WATCH_RETRY_MIN_DELAY;
                for path in deferred.drain().collect::<Vec<_>>() {
                    println!(
                        "Vision endpoint is answering again — re-ingesting {}",
                        path.display()
                    );
                    watch_ingest_once(
                        &path,
                        &mut deferred,
                        project_root,
                        model,
                        llm_url,
                        api_key,
                        schema_only,
                        runtime_url,
                        corpus,
                        json_output,
                        mapping,
                        sampling,
                        vision,
                    )
                    .await;
                }
            } else {
                retry_delay = watch_retry_backoff(retry_delay);
            }
            next_retry = std::time::Instant::now() + retry_delay;
        }

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
                watch_ingest_once(
                    &path,
                    &mut deferred,
                    project_root,
                    model,
                    llm_url,
                    api_key,
                    schema_only,
                    runtime_url,
                    corpus,
                    json_output,
                    mapping,
                    sampling,
                    vision,
                )
                .await;
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
  export PRISM_API_URL=https://provider.example/api/v1 # choose a platform provider
  export PRISM_API_KEY=your_provider_key               # provider-defined, non-expiring key
  prism query --platform "titanium"

OUTPUT:
  Default: human-readable, one result per line (grep-friendly)
  --json:  JSON array (pipe to jq/python)
  --platform: route through the hosted API instead of the local graph

AGENT SETUP:
  export PRISM_API_URL=https://provider.example/api/v1
  export PRISM_API_KEY=your_provider_key               # no login, refresh, or expiry

EXAMPLES:
  prism query --platform --semantic "creep resistant superalloy" --json | jq '.[].content'
  prism query --platform "Ti-6Al-4V" | grep MAT
  prism query --platform --json "fatigue" | python3 -c "import sys,json;[print(e['name']) for e in json.load(sys.stdin)]"
  prism status | grep nodes
"#
    );
}

/// Map the shared auth seam onto the compute crate's decoupled `Marc27Auth`.
fn marc27_auth_from(auth: PlatformAuth) -> prism_compute::Marc27Auth {
    match auth {
        PlatformAuth::ApiKey(key) => prism_compute::Marc27Auth::ApiKey(key),
        PlatformAuth::Bearer(token) => prism_compute::Marc27Auth::Bearer(token),
    }
}

/// Resolve platform auth for every CLI command that reaches a hosted provider. This is
/// the single CLI auth chokepoint: it accepts API-key-only users, stored
/// sessions, and legacy credentials, and never starts interactive auth.
fn resolve_agent_auth() -> Result<(String, PlatformAuth)> {
    resolve_agent_auth_with_url(None)
}

fn resolve_agent_auth_with_url(configured_url: Option<&str>) -> Result<(String, PlatformAuth)> {
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

    let paths = PrismPaths::discover().ok();
    let config = prism_core::config::NodeConfig::load(CLI_PROJECT_ROOT.get().map(PathBuf::as_path));
    let configured_url = configured_url.or(config.platform.url.as_deref());
    let resolved = auth::resolve_from_environment_with_provider(
        paths.as_ref(),
        configured_url,
        config.platform.provider.as_deref(),
    )?;
    Ok((resolved.api_base, resolved.credential))
}

fn require_platform_endpoints(endpoints: Option<&PlatformEndpoints>) -> Result<&PlatformEndpoints> {
    endpoints.ok_or_else(|| anyhow!(prism_runtime::auth::PLATFORM_NOT_CONFIGURED))
}

/// Build the TUI's platform-credit client only from a stored session that is
/// still bound to the exact provider and URL recorded at login.
fn tui_platform_auth(
    endpoints: Option<&PlatformEndpoints>,
    credentials: Option<&StoredCredentials>,
) -> Result<Option<prism_tui::PlatformAuth>> {
    let (Some(endpoints), Some(credentials)) = (endpoints, credentials) else {
        return Ok(None);
    };
    let Some(credential) = auth::stored_bearer_for_endpoints(endpoints, credentials)? else {
        return Ok(None);
    };
    Ok(Some(prism_tui::PlatformAuth {
        base_url: endpoints.api_base.clone(),
        token: credential.secret().to_string(),
    }))
}

/// Resolve the transport endpoint for login without inventing a Supabase
/// project. Selecting MARC27 explicitly retains its compatibility endpoint;
/// selecting Supabase requires a URL from the CLI, environment, config, or a
/// stored login.
fn resolve_login_endpoints(
    endpoints: Option<&PlatformEndpoints>,
    provider: Option<&str>,
    supabase_url: Option<&str>,
) -> Result<PlatformEndpoints> {
    if let Some(provider) = provider {
        identity_provider_for(Some(provider))
            .with_context(|| format!("unknown identity provider `{provider}`"))?;
    }

    if provider == Some(SUPABASE_IDENTITY_PROVIDER) {
        let environment_url = std::env::var("PRISM_SUPABASE_URL")
            .ok()
            .and_then(|value| non_blank(&value).map(str::to_string));
        let provider_neutral_url = std::env::var(PlatformVar::API_URL.preferred)
            .ok()
            .and_then(|value| non_blank(&value).map(str::to_string));
        let already_supabase = endpoints
            .filter(|endpoints| endpoints.provider.as_deref() == Some(SUPABASE_IDENTITY_PROVIDER))
            .map(|endpoints| endpoints.api_base.as_str());
        if let Some(url) = supabase_url
            .and_then(non_blank)
            .or(environment_url.as_deref())
            .or(provider_neutral_url.as_deref())
            .or(already_supabase)
        {
            return Ok(PlatformEndpoints::from_url_with_provider(
                url,
                Some(SUPABASE_IDENTITY_PROVIDER.to_string()),
            ));
        }
    }

    if provider == Some(MARC27_IDENTITY_PROVIDER) {
        if let Some(endpoints) = endpoints.filter(|endpoints| {
            endpoints.provider.is_none()
                || endpoints.provider.as_deref() == Some(MARC27_IDENTITY_PROVIDER)
        }) {
            return Ok(PlatformEndpoints::from_url_with_provider(
                &endpoints.api_base,
                Some(MARC27_IDENTITY_PROVIDER.to_string()),
            ));
        }
        return Ok(PlatformEndpoints::marc27());
    }

    if let Some(endpoints) = endpoints {
        return Ok(endpoints.clone());
    }

    match provider {
        Some(SUPABASE_IDENTITY_PROVIDER) => bail!(
            "Supabase is not configured. Set PRISM_SUPABASE_URL and \
             PRISM_SUPABASE_ANON_KEY (or PRISM_API_URL and PRISM_API_KEY)."
        ),
        Some(_) | None => bail!(prism_runtime::auth::PLATFORM_NOT_CONFIGURED),
    }
}

fn selected_identity_provider(
    explicit_provider: Option<&str>,
    configured_provider: Option<&str>,
) -> Result<IdentityProviderAdapter> {
    let provider = explicit_provider.or(configured_provider);
    identity_provider_for(provider).with_context(|| match provider {
        Some(provider) => format!("unknown identity provider `{provider}`"),
        None => "identity provider is not configured; pass --provider marc27, \
                 --provider supabase, or --provider mirdyne"
            .to_string(),
    })
}

fn provider_login_mode(
    endpoints: &PlatformEndpoints,
    interactive_auth: bool,
    no_browser: bool,
) -> Result<LoginMode> {
    Ok(LoginMode::Provider {
        provider: selected_identity_provider(None, endpoints.provider.as_deref())?,
        interactive_auth,
        no_browser,
        email: None,
        supabase_url: None,
        supabase_anon_key: None,
        // This helper re-logs in with whatever the platform is configured
        // for; it has no user-supplied SSO connection to pass on.
        sso: None,
    })
}

fn identity_verifier_for(
    endpoints: &PlatformEndpoints,
    credentials: Option<&StoredCredentials>,
) -> Result<Option<prism_client::auth::IdentityVerifierConfig>> {
    let Some(provider) = identity_provider_for(endpoints.provider.as_deref()) else {
        // A provider-neutral or unknown platform may still support its own
        // API-key registration, but it must not inherit another provider's
        // remote-session verification protocol.
        return Ok(None);
    };
    let verifier = match provider {
        IdentityProviderAdapter::Marc27 => prism_client::auth::IdentityVerifierConfig::new(
            Some(MARC27_IDENTITY_PROVIDER),
            &endpoints.api_base,
            None,
        )?,
        // Shared arm, but the verifier is built with `provider.as_str()` — the
        // adapter's OWN id — never a hard-coded one. Hard-coding Supabase here
        // would silently verify a Mirdyne token against Supabase's issuer,
        // which is precisely the identity-domain collapse this design exists
        // to prevent.
        IdentityProviderAdapter::Supabase | IdentityProviderAdapter::Mirdyne => {
            let credentials = credentials.with_context(|| {
                format!(
                    "{} identity verification is not configured: no stored login exists",
                    provider.as_str()
                )
            })?;
            let project_url = credentials
                .identity_provider_url
                .as_deref()
                .with_context(|| {
                    format!(
                        "{} identity verification is missing its project URL",
                        provider.as_str()
                    )
                })?;
            prism_client::auth::IdentityVerifierConfig::new(
                Some(provider.as_str()),
                project_url,
                credentials.identity_provider_key.as_deref(),
            )?
        }
    };
    Ok(Some(verifier))
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
        .ok_or_else(|| anyhow!("No active project selected; authenticate or set PRISM_PROJECT_ID."))
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
    let state = paths.load_cli_state().unwrap_or_default();
    let config = prism_core::config::NodeConfig::load(CLI_PROJECT_ROOT.get().map(PathBuf::as_path));
    let mint_endpoints = PlatformEndpoints::resolve_for_paths(
        config.platform.url.as_deref(),
        config.platform.provider.as_deref(),
        state.credentials.as_ref(),
        paths,
    )
    .context("platform endpoint disappeared while minting the node credential")?;
    if mint_endpoints.api_base != api_base {
        bail!("platform endpoint changed while minting the node credential; retry the command");
    }
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
        platform_url: api_base.clone(),
        platform_provider: mint_endpoints.provider,
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
    // Install is the only explicit local-model acquisition path. Register is
    // purely local. Neither needs platform auth or the hosted catalog.
    let command = match command {
        ModelsCommands::Install { model, json } => {
            let report = model_install::install(model).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "{} model: {}",
                    match report.status {
                        model_install::InstallDisposition::Installed => "Installed",
                        model_install::InstallDisposition::AlreadyInstalled => {
                            "Already installed"
                        }
                    },
                    report.model
                );
                println!("  path: {}", report.path.display());
                println!("  source: {}@{}", report.repository, report.revision);
                println!("  pinned artifact view: {}", report.artifact_revision_url);
                println!(
                    "  verified: {} file(s), {} bytes",
                    report.files_verified, report.bytes_verified
                );
                if let Some(sha256) = report.sha256 {
                    println!("  SHA-256: {sha256}");
                }
                match (
                    report.artifact_license,
                    report.artifact_license_evidence_url,
                ) {
                    (Some(license), Some(evidence)) => {
                        println!("  artifact license: {license} (evidence: {evidence})")
                    }
                    (Some(license), None) => println!("  artifact license: {license}"),
                    (None, _) => {
                        println!("  artifact license: not declared by artifact repository")
                    }
                }
                if let (Some(repository), Some(revision), Some(url)) = (
                    report.source_model_repository,
                    report.source_model_revision,
                    report.source_model_revision_url.as_deref(),
                ) {
                    println!("  source model: {repository}@{revision}");
                    println!("  pinned source view: {url}");
                }
                if let Some(license) = report.source_model_license {
                    match report.source_model_license_evidence_url {
                        Some(evidence) => {
                            println!("  source model license: {license} (evidence: {evidence})")
                        }
                        None => println!("  source model license: {license}"),
                    }
                }
            }
            return Ok(());
        }
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
        ModelsCommands::Install { .. } => unreachable!("install returns early"),
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
    ensure_dashboard_role(&rbac_engine, creds, user_id)?;

    create_dashboard_session_for_user_with_platform_token(
        dashboard_url,
        user_id,
        creds.display_name.as_deref(),
        Some(creds.access_token.as_str()),
    )
    .await
}

/// Bootstrap a purely local/login-neutral operator, while requiring a
/// recognized verified provider role for Supabase principals. An unknown
/// Supabase claim deliberately leaves no external row and must never turn
/// into local `NodeAdmin` merely because the dashboard is opened.
fn ensure_dashboard_role(
    rbac_engine: &prism_core::rbac::RbacEngine,
    credentials: &StoredCredentials,
    user_id: &str,
) -> Result<()> {
    if rbac_engine.get_local_role(user_id)?.is_some() {
        return Ok(());
    }
    if credentials.platform_provider.as_deref() == Some(SUPABASE_IDENTITY_PROVIDER)
        && rbac_engine
            .get_external_role(prism_node::provider_roles::SUPABASE_ROLE_PROVIDER, user_id)?
            .is_none()
    {
        bail!(
            "verified Supabase identity has no recognized PRISM role; refusing local administrator bootstrap"
        );
    }
    if rbac_engine.get_role(user_id)?.is_none() {
        rbac_engine.assign_role(user_id, prism_core::rbac::LocalRole::NodeAdmin)?;
    }
    Ok(())
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
        // A compromised local dashboard can echo the platform token from the
        // request body. Never reflect an untrusted response body into CLI/TUI
        // output or logs.
        bail!("Dashboard session creation failed: {status}");
    }

    let session: DashboardSessionResponse = resp.json().await?;
    Ok(session.session_id)
}

/// Open the shared provenance store (`~/.prism/provenance.db`) so a campaign
/// loop persists every progress transition to Turso. Failure degrades to
/// checkpoint-only persistence with a loud warning — a locked or corrupt
/// store must not brick a discovery run.
async fn open_campaign_provenance() -> Option<prism_provenance::ProvenanceStore> {
    let db_path = prism_provenance::store_path();
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
                target_property: None,
                target_direction: None,
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

/// Query the configured provider API (graph search or semantic search).
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
    // The directory is created by `ProvenanceStore::open` for every caller.
    let db_path = prism_provenance::store_path();
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
    /// Matching facts the verification filter kept back, by stored status.
    /// Printed, so a narrowed read never looks like the whole store.
    withheld: std::collections::BTreeMap<String, usize>,
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
    verification: prism_provenance::VerificationFilter,
) -> Result<Option<LocalOntologyResults>> {
    // A store we COULD NOT READ is not a store with nothing in it. This
    // returned `Option` and logged the open failure at `debug!`, so a locked
    // or missing database printed "No direct matches" — the caller, and the
    // agent shelling out to it, then reported that the corpus does not contain
    // what it was asked about. Measured: with the TUI holding the file, a
    // search that finds 9 entities on a free store answered "no matches" and
    // exit 0.
    let store = prism_provenance::ProvenanceStore::open(db_path)
        .await
        .with_context(|| {
            format!(
                "could not open the local knowledge graph at {}. Another PRISM \
                 process (the TUI, an ingest) may be holding it — close it and \
                 retry, or point PRISM_PROVENANCE_DB at a different store",
                db_path.display()
            )
        })?;
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
        // Degrading to "no neighbours" here is the same lie one level down: a
        // failed read is not an absent relationship.
        Err(e) => {
            return Err(e).with_context(|| {
                format!(
                    "could not read the local knowledge graph at {}",
                    db_path.display()
                )
            });
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
    // complete shape, so evidence class, verification status, and owning
    // tenant reach the printer instead of being fetched and thrown away.
    // The default filter is the TRUSTED subset; `--include-unverified`
    // widens it to everything, statuses shown.
    let prism_provenance::RecallReport { facts, withheld } = match store
        .recall_with_context_report(text, &tenants, limit, verification)
        .await
    {
        Ok(report) => report,
        // Same rule as the neighbour read: a failed recall is not an absence
        // of facts, and must not be reported as one.
        Err(e) => {
            return Err(e).with_context(|| {
                format!("could not read stored facts from {}", db_path.display())
            });
        }
    };

    if nodes.is_empty() && edges.is_empty() && facts.is_empty() {
        // A genuine miss: the store was read, and it holds nothing matching.
        return Ok(None);
    }
    Ok(Some(LocalOntologyResults {
        nodes,
        edges,
        facts,
        withheld,
    }))
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
/// Attribution for semantic hits: which SUBJECT each matched property belongs to.
///
/// Semantic search ranks entity vectors and returns bare entity names. For a
/// property node that name is the property STRING — "415 MPa√m crack-initiation
/// fracture toughness (KJIc)" — with no indication of which material it was
/// measured on. Two alloys from the same paper then land adjacent in one result
/// list, indistinguishable:
///
///   1. 235 MPa√m crack-initiation fracture toughness (KJIc)  (score: 0.8756)
///   2. 415 MPa√m crack-initiation fracture toughness (KJIc)  (score: 0.8718)
///
/// Measured consequence: asked about CrCoNi, the agent reported BOTH as CrCoNi
/// (235 belongs to CrMnFeCoNi); asked again it "corrected" itself and assigned
/// BOTH to CrMnFeCoNi (415 belongs to CrCoNi). Half wrong each time, stated
/// confidently, in a product whose whole claim is provenance. The model was not
/// hallucinating — retrieval handed it unattributed numbers.
///
/// The plain-text path already prints the owning triple, so the data is present;
/// only this renderer dropped it. Returns the facts in which the hit appears as
/// the OBJECT — an exact match, so a hit that is a subject in its own right
/// gets no invented owner.
async fn semantic_hit_owners(
    db_path: &Path,
    hits: &[prism_provenance::SemanticEntityHit],
) -> Vec<Vec<prism_provenance::RecalledFact>> {
    let Ok(store) = prism_provenance::ProvenanceStore::open(db_path).await else {
        // Attribution is an enrichment: if the store cannot be reopened we
        // print what we always printed rather than failing the search.
        return vec![Vec::new(); hits.len()];
    };
    let mut owners = Vec::with_capacity(hits.len());
    for hit in hits {
        let facts = store
            .recall(&hit.name, &hit.tenant, 4)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|fact| fact.object == hit.name)
            .take(2)
            .collect();
        owners.push(facts);
    }
    owners
}

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

    // Native snapshot verification and ONNX initialization are blocking;
    // acquisition is a separate explicit command and never occurs here.
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
            // An unverified fact is NEVER printed bare: its status (and the
            // check's reason, when recorded) rides the line, so nothing
            // weak can pose as verified in the output.
            let verification = match fact.verification_status {
                Some(status) if !status.is_trusted() => match &fact.verification_reason {
                    Some(reason) => format!(", UNVERIFIED {} — {reason}", status.as_str()),
                    None => format!(", UNVERIFIED {}", status.as_str()),
                },
                _ => String::new(),
            };
            let _ = writeln!(
                out,
                "  {} -[{}]-> {}  (confidence {:.2}, evidence {}, source {}{verification}){}",
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
    if !results.withheld.is_empty() {
        let total: usize = results.withheld.values().sum();
        let by_status = results
            .withheld
            .iter()
            .map(|(status, n)| {
                format!(
                    "{n} {}",
                    if status.is_empty() {
                        "unrecorded"
                    } else {
                        status
                    }
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            out,
            "\n{total} matching fact(s) withheld by verification status ({by_status}); \
             `--include-unverified` shows them."
        );
    }
    out
}

async fn handle_query(
    text: &str,
    semantic: bool,
    limit: usize,
    include_unverified: bool,
) -> Result<()> {
    // Resolve through the SHARED helper, which honours `$PRISM_PROVENANCE_DB`
    // — "the platform's documented override for this database".
    //
    // This line used to open-code `$HOME/.prism/provenance.db`, which is
    // precisely what that helper's doc comment warns against, and the
    // consequence was not a test-isolation slip but a wrong ANSWER: the
    // agent's `query_local` shells out to this command, so pointing PRISM at
    // a different store (a merged corpus, a scratch graph) moved the agent's
    // ingest and its writes but NOT its reads. The agent searched the empty
    // default store, found nothing, and truthfully reported that the corpus
    // contained no papers on the subject — while 21k assertions sat in the
    // store the operator had actually selected. A retrieval layer that reads
    // the wrong database turns an honest agent into a confidently wrong one.
    let turso_db = prism_agent::hooks::provenance_db_path();

    if semantic {
        // Bundled Turso entity vectors written by local ingest, ranked by
        // the native `vector_distance_cos()` (offline prism-embed query
        // embedding — no services needed). An unusable index errors out
        // here rather than printing an empty, reassuring list.
        let results = local_semantic_lookup(&turso_db, text, limit).await?;
        let owners = semantic_hit_owners(&turso_db, &results).await;
        println!("\nSemantic search results ({} matches):\n", results.len());
        for (i, hit) in results.iter().enumerate() {
            println!(
                "  {}. {}  (score: {:.4}){}",
                i + 1,
                hit.name,
                hit.similarity,
                peer_tag(&hit.tenant)
            );
            // Name the SUBJECT this value was measured on. Without it, two
            // alloys' toughness values sit adjacent and unattributable.
            for fact in owners.get(i).into_iter().flatten() {
                // SUBJECT is the whole point of this line — without it two
                // alloys' values sit adjacent and unattributable. Everything
                // else is kept short on purpose: the first version printed the
                // full predicate IRI and the absolute source path on every hit,
                // and measured at 2394 of 3546 output characters (67%), with
                // 1110 of those (31%) being the SAME 90-character path repeated
                // ten times. That is retrieval spending the agent's turn budget
                // on punctuation. A 12-hex prefix identifies the snapshot
                // uniquely and `query <hash>` still finds it.
                let src = std::path::Path::new(&fact.source)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map_or_else(|| fact.source.clone(), |s| s.chars().take(12).collect());
                println!(
                    "       of: {}  (conf {:.2}, src {})",
                    fact.subject, fact.confidence, src
                );
            }
        }
        if results.is_empty() {
            // Observed live in a TUI transcript: "(the local semantic index is
            // empty — ingest data first with: prism ingest <path>)". This
            // stdout is piped verbatim into whatever surface invoked the CLI,
            // so "ingest data first with: prism ingest" told a TUI user to
            // exit and type a command — the exact defect no_exit_to_cli.rs
            // exists to ban (it escaped because the literal's continuation
            // split "with:" from "prism"). State the fact; each surface owns
            // its own ingest remedy.
            println!("  (the local semantic index is empty — nothing has been ingested yet)");
        }
    } else {
        // Graph traversal over the bundled Turso provenance store
        // (~/.prism/provenance.db, tenant "local") — the sole graph
        // backend; no running services required.
        println!("Querying knowledge graph: \"{text}\"\n");

        let filter = if include_unverified {
            prism_provenance::VerificationFilter::Any
        } else {
            prism_provenance::VerificationFilter::Trusted
        };
        if let Some(local) = local_ontology_lookup(&turso_db, text, limit, filter).await? {
            print!("{}", format_local_ontology(&local));
        } else if include_unverified {
            println!("  No direct matches. Try --semantic for vector search.");
        } else {
            println!(
                "  No direct matches. Try --semantic for vector search, or \
                 --include-unverified to also search facts whose ingest checks \
                 did not verify them."
            );
        }
    }

    Ok(())
}

/// Mode flag for [`perform_full_login`] — picks the credential source without
/// committing callers to the structure of [`Commands::Login`]'s arguments.
/// Owned form of `SsoSelector` so `LoginMode` holds no borrows.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SsoChoice {
    Domain(String),
    ProviderId(String),
}

impl SsoChoice {
    fn as_selector(&self) -> prism_client::supabase_auth::SsoSelector<'_> {
        match self {
            Self::Domain(d) => prism_client::supabase_auth::SsoSelector::Domain(d),
            Self::ProviderId(id) => prism_client::supabase_auth::SsoSelector::ProviderId(id),
        }
    }
}

enum LoginMode {
    /// Personal Access Token — non-interactive, suitable for headless
    /// scripts and CI. Skips the device-flow polling step.
    Token(String),
    /// Interactive provider flow. MARC27 uses retained device authorization;
    /// Supabase uses passwordless email magic-link PKCE.
    Provider {
        provider: IdentityProviderAdapter,
        interactive_auth: bool,
        no_browser: bool,
        email: Option<String>,
        supabase_url: Option<String>,
        supabase_anon_key: Option<String>,
        /// Enterprise SAML SSO connection, when the user named one. Clap
        /// already guarantees at most one of domain/provider-id/email/token.
        sso: Option<SsoChoice>,
    },
}

/// Run the full login recipe used by `prism login` AND by the inline
/// relogin path in `prism tui` / `prism resume` when both refreshes
/// fail.
///
/// Steps:
/// 1. Mint fresh credentials through the selected provider.
/// 2. Fetch the provider-platform profile when that protocol supports it.
/// 3. Pick org + project for a PRISM-compatible platform (auto-selects when
///    only one exists — see [`select_project`]).
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
    let (mut credentials, interactive_auth, load_platform_context) = match mode {
        LoginMode::Token(_)
            if endpoints.provider.as_deref() == Some(SUPABASE_IDENTITY_PROVIDER) =>
        {
            bail!(
                "pre-issued token login is not supported for Supabase; use its passwordless PKCE login"
            )
        }
        LoginMode::Token(pat) => (run_token_login(endpoints, &pat).await?, false, true),
        LoginMode::Provider {
            provider: IdentityProviderAdapter::Marc27,
            interactive_auth,
            no_browser,
            ..
        } => (
            run_device_login_with_opts(endpoints, interactive_auth, no_browser).await?,
            true,
            true,
        ),
        // Mirdyne signs in through the same passwordless PKCE flow, pointed at
        // MIRDYNE's issuer. `run_supabase_login` takes the project URL and key
        // as arguments, so nothing about it is Supabase-specific except the
        // name; a Mirdyne login therefore stores Mirdyne's issuer and produces
        // a Mirdyne-scoped principal.
        LoginMode::Provider {
            provider: IdentityProviderAdapter::Supabase | IdentityProviderAdapter::Mirdyne,
            email,
            supabase_url,
            supabase_anon_key,
            sso,
            no_browser: _,
            interactive_auth: _,
        } => (
            run_supabase_login(
                paths,
                endpoints,
                email.as_deref(),
                supabase_url.as_deref(),
                supabase_anon_key.as_deref(),
                sso.as_ref().map(SsoChoice::as_selector),
            )
            .await?,
            false,
            false,
        ),
    };
    if load_platform_context {
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
        credentials.user_id = profile.as_ref().map(|profile| profile.id.clone());
        credentials.display_name = profile.and_then(|profile| profile.display_name);
        credentials.org_id = selected.org_id;
        credentials.org_name = selected.org_name;
        credentials.project_id = selected.project_id;
        credentials.project_name = selected.project_name;
    }

    // Store the non-secret interpreter preference before beginning the
    // coordinated credential update. A later failure must never report login
    // failure after the new token pair has already been committed.
    let mut state = paths.load_cli_state()?;
    state.preferred_python = Some(python.display().to_string());
    paths.save_cli_state(&state)?;
    // The shared writer mirrors the restricted cli-state record into
    // ~/.prism/credentials.json and rolls the CLI record back if the mirror
    // cannot be replaced.
    paths.persist_credentials(&credentials)?;

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
    match identity_provider_for(endpoints.provider.as_deref()) {
        Some(IdentityProviderAdapter::Marc27) => {}
        Some(IdentityProviderAdapter::Supabase) => bail!(
            "Supabase does not implement device authorization; this provider uses passwordless email PKCE"
        ),
        Some(IdentityProviderAdapter::Mirdyne) => bail!(
            "Mirdyne does not implement device authorization; this provider uses passwordless email PKCE"
        ),
        None => bail!("device login requires an explicitly configured MARC27 identity provider"),
    }
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
        u64::try_from(start.interval).unwrap_or_default(),
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
        platform_provider: endpoints.provider.clone(),
        identity_provider_url: None,
        identity_provider_key: None,
        user_id: None,
        display_name: None,
        org_id: None,
        org_name: None,
        project_id: None,
        project_name: None,
        expires_at,
    })
}

#[derive(Clone)]
struct SupabaseLoginConfig {
    project_url: String,
    anon_key: String,
}

impl std::fmt::Debug for SupabaseLoginConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SupabaseLoginConfig")
            .field("project_url", &self.project_url)
            .field("anon_key", &"[REDACTED]")
            .finish()
    }
}

async fn run_supabase_login(
    paths: &PrismPaths,
    endpoints: &PlatformEndpoints,
    email: Option<&str>,
    configured_url: Option<&str>,
    configured_anon_key: Option<&str>,
    sso: Option<prism_client::supabase_auth::SsoSelector<'_>>,
) -> Result<StoredCredentials> {
    let config = resolve_supabase_login_config(paths, configured_url, configured_anon_key)?;
    let auth = SupabaseAuth::new(
        reqwest::Client::new(),
        &config.project_url,
        &config.anon_key,
        SupabaseAuthPolicy::default(),
    )?;

    // SSO and email login converge on ONE completion path, so there is no
    // second token-handling route to keep correct.
    let attempt = match sso {
        Some(selector) => {
            let (attempt, url) = auth.begin_sso_login(selector).await?;
            println!();
            println!("Sign in with your organisation's identity provider:");
            println!();
            println!("    {url}");
            println!();
            println!(
                "PRISM is waiting on 127.0.0.1:{}. It does not open a browser for you.",
                attempt
                    .redirect_uri()
                    .port()
                    .context("SSO callback URL is missing its ephemeral port")?
            );
            attempt
        }
        None => {
            let email = resolve_supabase_login_email(email)?;
            let attempt = auth.begin_email_login(&email).await?;
            println!();
            println!("Supabase sent a passwordless login link to your email.");
            println!(
                "Open it in a browser on this machine; PRISM is waiting on 127.0.0.1:{}.",
                attempt
                    .redirect_uri()
                    .port()
                    .context("Supabase callback URL is missing its ephemeral port")?
            );
            attempt
        }
    };
    io::stdout()
        .flush()
        .context("failed to flush login instructions")?;

    let session = auth.complete_email_login(attempt).await?;
    let role_claim = session.claims.role.as_deref().unwrap_or("");
    std::fs::create_dir_all(&paths.state_dir).with_context(|| {
        format!(
            "failed to create PRISM state directory {}",
            paths.state_dir.display()
        )
    })?;
    let engine = prism_core::rbac::RbacEngine::new(&paths.state_dir.join("rbac.db"))?;
    let role_sync = prism_node::provider_roles::sync_supabase_login_role(
        &engine,
        &session.claims.iss,
        &session.claims.sub,
        role_claim,
    )?;
    let expires_at = session.tokens.expires_in.and_then(|seconds| {
        chrono::Utc::now().checked_add_signed(chrono::Duration::seconds(seconds as i64))
    });

    println!("Supabase identity verified.");
    Ok(StoredCredentials {
        access_token: session.tokens.access_token,
        refresh_token: session.tokens.refresh_token,
        platform_url: endpoints.api_base.trim_end_matches("/api/v1").to_string(),
        platform_provider: Some(SUPABASE_IDENTITY_PROVIDER.to_string()),
        identity_provider_url: Some(config.project_url),
        identity_provider_key: Some(config.anon_key),
        user_id: Some(role_sync.principal_id),
        display_name: session.claims.email,
        org_id: None,
        org_name: None,
        project_id: None,
        project_name: None,
        expires_at,
    })
}

fn resolve_supabase_login_config(
    paths: &PrismPaths,
    configured_url: Option<&str>,
    configured_anon_key: Option<&str>,
) -> Result<SupabaseLoginConfig> {
    let stored = paths
        .load_cli_state()
        .ok()
        .and_then(|state| state.credentials);
    let stored_supabase = stored.as_ref().filter(|credentials| {
        credentials.platform_provider.as_deref() == Some(SUPABASE_IDENTITY_PROVIDER)
    });
    let environment_provider = std::env::var(PlatformVar::PROVIDER.preferred)
        .ok()
        .and_then(|value| non_blank(&value).map(|value| value.to_ascii_lowercase()))
        .or_else(|| {
            std::env::var(PlatformVar::PROVIDER.alias)
                .ok()
                .and_then(|value| non_blank(&value).map(|value| value.to_ascii_lowercase()))
        });
    let provider_neutral_environment_url = environment_provider
        .as_deref()
        .is_none_or(|provider| provider == SUPABASE_IDENTITY_PROVIDER)
        .then(|| {
            std::env::var(PlatformVar::API_URL.preferred)
                .ok()
                .and_then(|value| {
                    non_blank(&value).map(|value| {
                        value
                            .trim_end_matches('/')
                            .strip_suffix("/api/v1")
                            .unwrap_or(value.trim_end_matches('/'))
                            .to_string()
                    })
                })
        })
        .flatten();
    let project_url = configured_url
        .and_then(non_blank)
        .map(str::to_string)
        .or_else(|| {
            std::env::var("PRISM_SUPABASE_URL")
                .ok()
                .and_then(|value| non_blank(&value).map(str::to_string))
        })
        // `PRISM_API_URL` is accepted only as an explicit, provider-neutral
        // input for this attempt. Never recover it through PlatformEndpoints:
        // that value may instead have come from a MARC27 project config or a
        // stale MARC27 login and must not be relabelled as Supabase Auth.
        .or(provider_neutral_environment_url)
        .or_else(|| {
            stored_supabase.and_then(|credentials| credentials.identity_provider_url.clone())
        });
    let anon_key = configured_anon_key
        .and_then(non_blank)
        .map(str::to_string)
        .or_else(|| {
            std::env::var("PRISM_SUPABASE_ANON_KEY")
                .ok()
                .and_then(|value| non_blank(&value).map(str::to_string))
        })
        // PRISM_API_KEY remains the provider-neutral configured key surface.
        // Never fall back to MARC27_API_KEY for a Supabase provider.
        .or_else(|| {
            std::env::var(PlatformVar::API_KEY.preferred)
                .ok()
                .and_then(|value| non_blank(&value).map(str::to_string))
        })
        .or_else(|| {
            stored_supabase.and_then(|credentials| credentials.identity_provider_key.clone())
        });

    match (project_url, anon_key) {
        (Some(project_url), Some(anon_key)) => Ok(SupabaseLoginConfig {
            project_url,
            anon_key,
        }),
        _ => bail!(
            "Supabase is not configured. Set PRISM_SUPABASE_URL and \
             PRISM_SUPABASE_ANON_KEY (or configure the same project through \
             PRISM_API_URL and PRISM_API_KEY)."
        ),
    }
}

fn resolve_supabase_login_email(email: Option<&str>) -> Result<String> {
    if let Some(email) = email.and_then(non_blank) {
        return Ok(email.to_string());
    }
    ensure_interactive_email_prompt()?;
    print!("Supabase email: ");
    io::stdout()
        .flush()
        .context("failed to flush the email prompt")?;
    let mut email = String::new();
    io::stdin()
        .read_line(&mut email)
        .context("failed to read the Supabase email")?;
    non_blank(&email)
        .map(str::to_string)
        .context("Supabase email must not be empty")
}

fn ensure_interactive_email_prompt() -> Result<()> {
    if io::stdin().is_terminal() && io::stdout().is_terminal() {
        Ok(())
    } else {
        bail!("Supabase login needs an email. Pass --email <address> or set PRISM_LOGIN_EMAIL.")
    }
}

fn non_blank(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

/// Headless / CI / SSH-only login path. Skips the device flow and
/// validates a Personal Access Token (PAT) the user issued with the
/// configured provider. The token is the only credential needed; org +
/// project selection is deferred to the post-login interactive step
/// (Commands::Login handler), which prompts only if multiple orgs/
/// projects are visible — and falls back to env var `PRISM_PROJECT_ID`
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
    // never-expiring locally; on a provider 401 we fall through
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
        platform_provider: endpoints.provider.clone(),
        identity_provider_url: None,
        identity_provider_key: None,
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
                    "Using project from PRISM_PROJECT_ID: {} ({})",
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
                    "warning: PRISM_PROJECT_ID={} could not be resolved: {err}",
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
            "multiple organizations require a selection; set PRISM_PROJECT_ID=<project_id>, then re-authenticate"
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
            "multiple projects require a selection; set PRISM_PROJECT_ID=<project_id>, then re-authenticate"
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
    auth::validate_stored_session_binding(endpoints, creds)?;
    let platform = PlatformClient::new(&endpoints.api_base);
    let provider =
        identity_provider_for(creds.platform_provider.as_deref()).with_context(|| {
            match creds.platform_provider.as_deref() {
                Some(provider) => {
                    format!("unknown identity provider `{provider}`; refusing refresh")
                }
                None => {
                    "identity provider is missing; refusing to send the refresh token".to_string()
                }
            }
        })?;
    let (provider_url, provider_key) = match provider {
        IdentityProviderAdapter::Marc27 => (marc27_refresh_url(endpoints, creds)?, None),
        IdentityProviderAdapter::Supabase | IdentityProviderAdapter::Mirdyne => {
            let url = creds.identity_provider_url.as_deref().with_context(|| {
                format!(
                    "{} is not configured: missing identity provider URL",
                    provider.as_str()
                )
            })?;
            let key = creds.identity_provider_key.as_deref().with_context(|| {
                format!(
                    "{} is not configured: missing identity provider key",
                    provider.as_str()
                )
            })?;
            (url.to_string(), Some(key))
        }
    };
    let refreshed = provider
        .refresh_token(
            platform.inner(),
            &provider_url,
            provider_key,
            &creds.refresh_token,
        )
        .await?;

    let mut new_creds = creds.clone();
    new_creds.access_token = refreshed.tokens.access_token;
    new_creds.refresh_token = refreshed.tokens.refresh_token;
    new_creds.expires_at = refreshed.tokens.expires_in.and_then(|secs| {
        chrono::Utc::now().checked_add_signed(chrono::Duration::seconds(secs as i64))
    });
    if let Some(claims) = refreshed.supabase_claims {
        ensure_supabase_refresh_principal(creds, &claims.iss, &claims.sub)?;
        std::fs::create_dir_all(&paths.state_dir).with_context(|| {
            format!(
                "failed to create PRISM state directory {}",
                paths.state_dir.display()
            )
        })?;
        let engine = prism_core::rbac::RbacEngine::new(&paths.state_dir.join("rbac.db"))?;
        let role_claim = claims.role.as_deref().unwrap_or("");
        let role_sync = prism_node::provider_roles::sync_supabase_login_role(
            &engine,
            &claims.iss,
            &claims.sub,
            role_claim,
        )?;
        new_creds.user_id = Some(role_sync.principal_id);
        new_creds.display_name = claims.email;
    }

    auth::validate_stored_session_binding(endpoints, &new_creds)?;

    // Persist rotated tokens to BOTH stores as one coordinated update: the authoritative
    // `cli-state.json` AND the `~/.prism/credentials.json` SDK mirror that the
    // Python platform tools read. Writing only one store left the other holding
    // a refresh token that single-use rotation had since REVOKED — replaying the
    // stale token tripped the server's token-family invalidation, forced a
    // device-flow re-login (the "re-login every ~24h" drift), and (for the SDK
    // mirror specifically) left node-up reading an expired access token that
    // 401'd on `POST /nodes/register` and dropped to silent offline mode.
    // `persist_credentials` is the single well-tested both-store writer; it
    // rolls the CLI record back if the mirror cannot be replaced.
    paths.persist_credentials(&new_creds).context(
        "failed to commit the rotated credential pair; do not retry this refresh token, sign in again",
    )?;

    Ok(new_creds)
}

fn ensure_supabase_refresh_principal(
    credentials: &StoredCredentials,
    issuer: &str,
    subject: &str,
) -> Result<String> {
    let principal_id = prism_node::provider_roles::map_supabase_principal(issuer, subject)
        .context("verified Supabase token has no canonical PRISM principal")?;
    let stored_principal = credentials
        .user_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("stored Supabase identity is missing its canonical principal; refusing refresh")?;
    if stored_principal != principal_id {
        bail!("Supabase refresh returned a different verified subject; refusing refresh");
    }
    Ok(principal_id)
}

/// Select the only MARC27 URL allowed to receive a stored refresh token.
///
/// Process and project endpoint overrides are useful for ordinary requests,
/// but a refresh token is bound to the provider and URL recorded at login.
/// Requiring the normalized URLs to match prevents an unrelated endpoint,
/// even one labelled `marc27`, from receiving that long-lived secret.
fn marc27_refresh_url(endpoints: &PlatformEndpoints, creds: &StoredCredentials) -> Result<String> {
    if endpoints.provider.as_deref() != Some(MARC27_IDENTITY_PROVIDER) {
        bail!(
            "configured platform provider does not match stored identity provider `marc27`; \
             refusing refresh"
        );
    }
    let stored_url = creds.platform_url.trim();
    if stored_url.is_empty() {
        bail!("stored MARC27 identity is missing its platform URL; refusing refresh");
    }
    let stored_api_base = PlatformEndpoints::from_url(stored_url).api_base;
    let selected_api_base = PlatformEndpoints::from_url(&endpoints.api_base).api_base;
    if stored_api_base != selected_api_base {
        bail!(
            "configured platform URL does not match the stored MARC27 identity; refusing refresh"
        );
    }
    Ok(stored_api_base)
}

/// Resolve the typed platform credential for the node-up register call.
///
/// 1. durable node token (`prism node token mint`) — non-rotating, no expiry;
/// 2. the already-resolved native-first environment/stored credential;
/// 3. refresh only when that selected credential is the expiring stored
///    session.
///
/// Returns `(credential, refreshed_creds)`:
/// - `credential` retains its API-key-versus-bearer wire semantics;
/// - `refreshed_creds` is `Some(...)` ONLY when this call refreshed (rotating
///   the single-use refresh token). The caller MUST use these refreshed creds
///   for any later refresh (e.g. the 401-retry) — never the stale startup
///   binding — or it will replay a now-REVOKED refresh token and trip the
///   server's token-family invalidation (forcing re-login + potentially
///   revoking the good tokens just issued).
async fn resolve_node_auth(
    paths: &PrismPaths,
    endpoints: &PlatformEndpoints,
    creds: Option<&StoredCredentials>,
    resolved: &PlatformAuth,
) -> Result<(PlatformAuth, Option<StoredCredentials>)> {
    if let Some(node_token) = paths.load_node_token() {
        tracing::debug!("using durable node token (does not rotate)");
        let credential = auth::stored_node_bearer_for_endpoints(endpoints, &node_token)?
            .context("stored node credential is empty")?;
        return Ok((credential, None));
    }
    if let Some(creds) = creds
        && matches!(resolved, PlatformAuth::Bearer(value) if value == &creds.access_token)
        && let Some(expires_at) = creds.expires_at
        && chrono::Utc::now() >= expires_at
    {
        tracing::info!("access token expired before node register, refreshing");
        let refreshed = refresh_access_token(paths, endpoints, creds).await?;
        // Thread the rotated creds out so the caller doesn't replay the
        // single-use refresh_token that `refresh_access_token` just consumed.
        return Ok((
            PlatformAuth::Bearer(refreshed.access_token.clone()),
            Some(refreshed),
        ));
    }
    Ok((resolved.clone(), None))
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

fn print_node_status(caps: &NodeCapabilities, endpoints: Option<&PlatformEndpoints>) {
    let hostname = sysinfo::System::host_name().unwrap_or_else(|| "unknown".to_string());
    println!("Node: {hostname}");
    println!("Visibility: {}", caps.visibility);
    match endpoints {
        Some(endpoints) => println!("Platform: {}", endpoints.node_ws),
        None => println!("Platform: not configured (set PRISM_API_URL)"),
    }
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

/// HyperQueue flags for `prism run`, bundled so `handle_run`'s signature does
/// not grow five more positional parameters.
struct HqRunFlags<'a> {
    tasks: Option<&'a str>,
    workers: u32,
    server_dir: Option<&'a str>,
    autoalloc: Option<&'a str>,
    time_limit: &'a str,
    extra: &'a [String],
}

impl HqRunFlags<'_> {
    fn any_set(&self) -> bool {
        self.tasks.is_some()
            || self.server_dir.is_some()
            || self.autoalloc.is_some()
            || !self.extra.is_empty()
    }
}

/// Validate the HyperQueue flag set against the chosen backend. HyperQueue
/// is many independent tasks in one HQ job — a task-set file is mandatory,
/// and mixing BYOC target flags in is a misconfiguration, not a fallback.
fn validate_run_hyperqueue(
    backend: &str,
    hq: &HqRunFlags<'_>,
    ssh: Option<&str>,
    k8s_context: Option<&str>,
    slurm: Option<&str>,
) -> Result<()> {
    let byoc_target_given = ssh.is_some() || k8s_context.is_some() || slurm.is_some();
    if backend == "hyperqueue" || backend == "hq" {
        if byoc_target_given {
            // `handle_run` dispatches on --ssh/--k8s-context/--slurm BEFORE
            // the backend string, so this combination would silently run on
            // BYOC and drop the task set on the floor. Refuse it instead.
            anyhow::bail!(
                "--backend hyperqueue cannot be combined with --ssh, --k8s-context, \
                 or --slurm (those select a BYOC target); a task set is one HQ job, \
                 not a Slurm submission"
            );
        }
        if hq.tasks.is_none() {
            anyhow::bail!(
                "--backend hyperqueue requires --hq-tasks <FILE>: a JSON array of \
                 task objects such as [{{\"command\":[\"python3\",\"ingest.py\",\"paper.pdf\"]}}] \
                 — a task set does not fit --input key=value strings"
            );
        }
        if hq.workers == 0 {
            anyhow::bail!("--hq-workers must be at least 1");
        }
        if let Some(scheduler) = hq.autoalloc
            && scheduler != "slurm"
            && scheduler != "pbs"
        {
            anyhow::bail!("--hq-autoalloc must be `slurm` or `pbs`, got {scheduler:?}");
        }
        return Ok(());
    }
    if hq.any_set() || hq.workers != 2 {
        anyhow::bail!(
            "--hq-* flags belong to --backend hyperqueue; this run uses backend {backend:?}"
        );
    }
    Ok(())
}

/// Read and parse a HyperQueue task-set file. Fails with the file path and
/// the serde context so a malformed task is fixable from the error alone.
fn load_hq_tasks(path: &str) -> Result<Vec<prism_compute::HqTask>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read the HyperQueue task file {path:?}"))?;
    serde_json::from_str(&raw).with_context(|| {
        format!(
            "{path:?} is not a HyperQueue task set: expected a JSON array of \
             {{\"command\": [\"...\"], \"cwd\"?: \"...\", \"env\"?: {{...}}, \"stdin\"?: \"...\"}} objects"
        )
    })
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
    platform_url: Option<&str>,
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
    hq: &HqRunFlags<'_>,
    json: bool,
) -> Result<()> {
    use prism_compute::ExperimentPlan;
    use prism_compute::backend::ComputeRouter;
    use prism_compute::byoc::{ByocTarget, SlurmJobConfig};

    validate_run_backend_target(backend, ssh, k8s_context, slurm)?;
    validate_run_hyperqueue(backend, hq, ssh, k8s_context, slurm)?;

    // Parse key=value inputs into JSON
    let mut input_map = serde_json::Map::new();
    for kv in inputs {
        if let Some((k, v)) = kv.split_once('=') {
            input_map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
        }
    }

    // A HyperQueue run carries its task set as a real JSON array, not a
    // string — `--input key=value` can only produce strings, so task sets
    // arrive via --hq-tasks and are spliced in here.
    if let Some(tasks_path) = hq.tasks {
        let tasks = load_hq_tasks(tasks_path)?;
        input_map.insert(
            "tasks".to_string(),
            serde_json::to_value(&tasks).context("failed to serialise the HyperQueue task set")?,
        );
    }

    let inputs_json = serde_json::Value::Object(input_map);
    let plan = ExperimentPlan {
        name: name.to_string(),
        image: image.to_string(),
        inputs: inputs_json.clone(),
        // `prism run`'s SLURM flags (--slurm-gres, --slurm-time, ...) already
        // reach the scheduler through `SlurmJobConfig` below. Routing the same
        // flags through `ResourceSpec` as well would apply them twice, once as
        // the cluster default and once as the per-job overlay. The agent-facing
        // path (`compute_submit`) is where a per-job GPU request is expressed.
        resources: Default::default(),
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
                // (PRISM_API_KEY → X-API-Key), else a Bearer session/token. The
                // old code read only MARC27_API_TOKEN and sent it as Bearer, which
                // 401'd for API-key agents.
                let (resolved_base, platform_auth) = resolve_agent_auth_with_url(platform_url)?;
                let auth = marc27_auth_from(platform_auth);
                let api_base = platform_url
                    .map(|url| PlatformEndpoints::from_url(url).api_base)
                    .unwrap_or(resolved_base);
                (
                    ComputeRouter::with_marc27_persistent(&api_base, auth, data_dir)?,
                    "marc27",
                    serde_json::json!({
                        "kind": "marc27",
                        "platform_url": api_base,
                    }),
                )
            }
            "hyperqueue" | "hq" => {
                use prism_compute::hyperqueue::{HqMode, HqScheduler, HyperQueueConfig};

                let server_dir = hq
                    .server_dir
                    .map(PathBuf::from)
                    .unwrap_or_else(|| data_dir.join("hyperqueue"));
                let mode = match hq.autoalloc {
                    Some(scheduler) => HqMode::AutoAlloc {
                        scheduler: match scheduler {
                            "slurm" => HqScheduler::Slurm,
                            "pbs" => HqScheduler::Pbs,
                            other => anyhow::bail!(
                                "--hq-autoalloc must be `slurm` or `pbs`, got {other:?}"
                            ),
                        },
                        time_limit: hq.time_limit.to_string(),
                        extra_args: hq.extra.to_vec(),
                    },
                    None => HqMode::Standalone {
                        workers: hq.workers,
                    },
                };
                let config = HyperQueueConfig {
                    binary: "hq".into(),
                    server_dir: server_dir.clone(),
                    mode,
                    startup_timeout: prism_compute::hyperqueue::DEFAULT_STARTUP_TIMEOUT,
                };
                (
                    ComputeRouter::local_only_persistent(data_dir)?.with_hyperqueue(config),
                    "hyperqueue",
                    serde_json::json!({
                        "kind": "hyperqueue",
                        "mode": hq.autoalloc.unwrap_or("standalone"),
                        "server_dir": server_dir.display().to_string(),
                        "workers": hq.workers,
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
    let hyperqueue_job_id = submitted_record.hyperqueue_job_id;

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
            if let Some(hq_job_id) = hyperqueue_job_id {
                object.insert("hyperqueue_job_id".to_string(), hq_job_id.into());
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
        if let Some(hq_job_id) = hyperqueue_job_id {
            println!("HyperQueue job id: {hq_job_id}");
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
            let (resolved_base, platform_auth) = resolve_agent_auth_with_url(Some(&api_base))?;
            if PlatformEndpoints::from_url(&resolved_base).api_base
                != PlatformEndpoints::from_url(&api_base).api_base
            {
                bail!("current platform credential is bound to a different endpoint than this job");
            }
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
        JobTarget::HyperQueue { server_dir } => {
            // The tracker persisted the HQ job id at submit time; resuming
            // the backend with it is all cross-process status needs. If the
            // `hq` binary or the server is gone, `status` says so honestly.
            Box::new(prism_compute::HyperQueueBackend::resume(
                prism_compute::HyperQueueConfig::standalone(server_dir, 1),
                job_id,
                record.hyperqueue_job_id,
            ))
        }
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
        && let Some(platform_auth) = auth::stored_bearer_for_endpoints(endpoints, c)?
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
        let resp = platform_auth
            .apply(reqwest::Client::new().post(&url))
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
    marc27_llm_base_url_with_source(paths, api_base, fallback_url).map(|(url, _)| url)
}

/// Return the selected LLM destination and whether it was derived from the
/// bound platform endpoint. Stored platform bearers may accompany only the
/// platform-derived destination.
fn marc27_llm_base_url_with_source(
    paths: &PrismPaths,
    api_base: &str,
    fallback_url: &str,
) -> anyhow::Result<(String, bool)> {
    if let Ok(explicit) = std::env::var("LLM_BASE_URL") {
        return Ok((explicit, false));
    }
    if let Some(project_id) = paths
        .load_cli_state()
        .ok()
        .and_then(|s| s.credentials)
        .and_then(|c| c.project_id)
    {
        return Ok((marc27_llm_url_for_project(api_base, &project_id), true));
    }
    // Unauthenticated: honor only an explicitly-set url, refuse the default.
    resolve_unauth_llm_url(fallback_url).map(|url| (url, false))
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
    /// One home for every CLI test that touches the process-wide
    /// document-understanding registry (the vision seam installs and parks
    /// its reader there). `cargo test` runs this binary's tests on
    /// concurrent threads, and per-test locks serialise nothing. Other
    /// crates' test binaries are separate processes with their own registry
    /// — the ingest crate's `GLOBAL_REGISTRY_TEST_LOCK` covers those and
    /// could not race this binary anyway. Async-aware because guards are
    /// deliberately held across `.await`s.
    static VISION_REGISTRY_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// Removes the "vision" adapter a seam test installed, so parallel
    /// tests that expect "vision" absent are not poisoned.
    struct DeregisterVision;
    impl Drop for DeregisterVision {
        fn drop(&mut self) {
            let _ = prism_ingest::document::deregister_understanding("vision");
        }
    }

    /// A loopback port that was bound and then released: nothing answers
    /// there, and nothing in these tests should ever contact it.
    fn unanswered_llm_config() -> prism_ingest::LlmConfig {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);
        prism_ingest::LlmConfig {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            model: "stub-vlm".into(),
            ..Default::default()
        }
    }

    /// The production deferral branch (`deferred_vision_pages`, the same
    /// function `run_local_text_ingest_file` records into its summary): a
    /// damaged PDF whose vision endpoint key is withdrawn DEFERS the blocked
    /// pages — a fragment naming the key, the reason and the page numbers —
    /// while a document with no seam running gets no fragment at all. It is
    /// a fragment, not a summary (audit F2): the caller attaches it and
    /// proceeds with extraction; deferral must never displace the stored
    /// facts. No endpoint is ever contacted.
    #[tokio::test]
    async fn a_damaged_pdf_defers_its_pages_when_the_vision_endpoint_is_withdrawn() {
        use prism_ingest::document::{
            Damage, Modality, PageNote, PageText, ReadOutcome, Understanding, VisionSeam,
        };
        let _registry = VISION_REGISTRY_TEST_LOCK.lock().await;
        let _cleanup = DeregisterVision;

        let outcome = ReadOutcome {
            understanding: Understanding {
                adapter_id: "text-layer".into(),
                modality: Modality::TextLayer,
                pages: vec![PageText {
                    number: 2,
                    text: String::new(),
                }],
            },
            notes: vec![PageNote {
                number: 2,
                damage: Damage::new("empty", "no text recovered"),
                recovered_by: None,
            }],
            skipped: Vec::new(),
        };

        // A run's seam after its breaker tripped: started, then withdrawn
        // with the reader's own last error — the exact state
        // `VisionSeam::observe` leaves behind after consecutive failures.
        let mut seam = VisionSeam::start(unanswered_llm_config())
            .await
            .expect("the seam starts");
        seam.withdraw("the vision reader failed 3 consecutive read(s); last: connection refused")
            .await;

        let deferred = deferred_vision_pages(Some(&seam), &outcome)
            .expect("a damaged document with the key withdrawn must defer its pages");
        assert_eq!(deferred["waiting_on"], "llm.vision.endpoint");
        assert_eq!(
            deferred["pages"],
            serde_json::json!([2]),
            "the deferred pages are machine-readable for the re-run",
        );
        let reason = deferred["reason"]
            .as_str()
            .expect("the deferral reason is a sentence");
        assert!(reason.contains("connection refused"), "{reason}");
        assert!(
            reason.contains("p2"),
            "the deferred page is named: {reason}"
        );
        assert!(
            reason.contains("ingested now"),
            "the reason must promise the rest of the document is stored, \
             not discarded (audit F2): {reason}",
        );

        // No seam (no vision configured): today's behaviour — proceed,
        // degraded but reported upstream, with nothing to defer against.
        assert!(
            deferred_vision_pages(None, &outcome).is_none(),
            "without a seam the document must proceed exactly as before",
        );
    }

    /// The run-level seam init (audit F11): one call initialises the slot,
    /// activates the reader component, and installs the vision adapter in
    /// the process-wide registry — no probe, no network. In a module whose
    /// premise is that silent degradation is the enemy, an init that
    /// silently produced a fiberless runtime (no park, no reader, flat text
    /// forever) was the worst exit; this pins the wiring that makes it
    /// impossible.
    #[tokio::test]
    async fn ensure_vision_seam_activates_the_reader_for_the_run() {
        let _registry = VISION_REGISTRY_TEST_LOCK.lock().await;
        let _cleanup = DeregisterVision;
        let cfg = unanswered_llm_config();

        let mut slot: Option<prism_ingest::document::VisionSeam> = None;
        ensure_vision_seam(&mut slot, cfg.clone()).await;

        let seam = slot.as_ref().expect("the run slot must be initialised");
        let status = seam.status();
        assert_eq!(status.len(), 1, "one supervised component");
        assert_eq!(
            status[0].state,
            prism_runtime::seam::FiberState::Active,
            "the reader component must be ACTIVE for the whole run, so a \
             mid-corpus withdrawal actually runs its tombstone inverse \
             (audit F3): {:?}",
            status[0],
        );
        assert!(
            prism_ingest::document::registry().get("vision").is_some(),
            "activation must install the vision adapter",
        );

        // Second call in the same run: the slot is reused, not rebuilt.
        ensure_vision_seam(&mut slot, cfg).await;
        assert_eq!(
            slot.as_ref().map(|seam| seam.status().len()),
            Some(1),
            "a later file must reuse the run's seam, never stack a second one",
        );
    }

    /// Round four's R2: in a reused process (watch mode, the TUI), run 1's
    /// `retire` leaves the tombstone under the id "vision" — so run 2's
    /// direct-registration fallback finds the id taken. A register-only
    /// fallback swallowed that at `tracing::debug!` and the tombstone stood:
    /// vision permanently off from run 2 onward, announced nowhere. The
    /// fallback must DISPLACE whatever holds the id.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn register_vision_reader_displaces_a_previous_runs_tombstone() {
        let _guard = boot_checks::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _registry = VISION_REGISTRY_TEST_LOCK.lock().await;
        let _cleanup = DeregisterVision;
        // A fake renderer on PATH so the LIVE reader reports Ready
        // deterministically — the assertion below distinguishes live from
        // tombstone by readiness, which must not depend on the host having
        // poppler.
        let fake_bin = tempfile::tempdir().expect("fake bin dir");
        let fake = fake_bin.path().join("pdftoppm");
        std::fs::write(&fake, "#!/bin/sh\nexit 7\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let _path = PathGuard::prepend(fake_bin.path());

        // Run 1 ends: retire installs the tombstone.
        let seam = prism_ingest::document::VisionSeam::start(unanswered_llm_config())
            .await
            .expect("the seam starts");
        seam.retire().await;
        assert!(
            !prism_ingest::document::registry()
                .get("vision")
                .expect("the tombstone stands after retirement")
                .readiness()
                .is_ready(),
            "precondition: run 1 left the tombstone",
        );

        // Run 2's fallback must install a LIVE reader over it.
        register_vision_reader(unanswered_llm_config());
        assert!(
            prism_ingest::document::registry()
                .get("vision")
                .expect("a reader is installed")
                .readiness()
                .is_ready(),
            "the fallback must displace run 1's tombstone with a live reader \
             — a register-only fallback leaves vision permanently off",
        );
    }

    /// Round four's R7: the operator-facing skip line is worded by the
    /// note's typed `kind`. An adapter that RAN and failed was available —
    /// calling it "unavailable" is the exact defect the typed kind exists
    /// to prevent.
    #[test]
    fn a_failed_reader_is_not_reported_as_unavailable() {
        use prism_ingest::document::{FailureOrigin, SkipKind, SkipNote};

        let failed = SkipNote {
            adapter_id: "vision".into(),
            reason: "rendering page 1: pdftoppm failed".into(),
            kind: SkipKind::Failed(FailureOrigin::Local),
        };
        let line = skip_note_line(&failed);
        assert!(line.contains("document reader failed"), "{line}");
        assert!(
            !line.contains("unavailable"),
            "an adapter that ran must not be called unavailable: {line}",
        );
        assert!(
            line.contains("vision: rendering page 1: pdftoppm failed"),
            "the id and the adapter's own reason survive: {line}",
        );

        let unavailable = SkipNote {
            adapter_id: "vision".into(),
            reason: "'pdftoppm' is not on PATH".into(),
            kind: SkipKind::Unavailable,
        };
        let line = skip_note_line(&unavailable);
        assert!(line.contains("document reader unavailable"), "{line}");
        assert!(!line.contains("failed —"), "{line}");
    }

    /// Audit F9 + round four's R3, the exit verdict at DOCUMENT
    /// granularity: the run fails when at least
    /// [`STARVED_RUN_ERROR_PERCENT`] of its PDF-backed documents deferred
    /// pages and stored zero facts. The round-three gate demanded ALL PDFs
    /// starved, so "399 of 400 starved plus one junk fact" exited 0. A
    /// `.csv` in the directory must not vouch for the PDFs, one junk fact
    /// must not vouch for the run, a deferred document that stored its
    /// sound pages' facts is never starved, and a genuinely partial
    /// deferral (below the threshold) stays exit 0 with a visible count.
    #[test]
    fn a_starved_run_is_an_error_and_a_genuinely_partial_one_is_not() {
        let deferred_empty = serde_json::json!({
            "backend": "local_text",
            "format": "pdf",
            "facts_written": 0,
            "deferred": {"waiting_on": "llm.vision.endpoint", "reason": "r", "pages": [2]},
        });
        let deferred_stored = serde_json::json!({
            "backend": "local_text",
            "format": "pdf",
            "facts_written": 4,
            "deferred": {"waiting_on": "llm.vision.endpoint", "reason": "r", "pages": [7]},
        });
        let clean_pdf = serde_json::json!({
            "backend": "local_text",
            "format": "pdf",
            "facts_written": 0,
        });
        let csv = serde_json::json!({"backend": "local_tabular", "format": "csv"});
        // A non-PDF that one day carries `deferred` — the gate must count
        // starvation over PDFs only, or this pushes `starved` past the
        // denominator and disarms it.
        let deferred_txt = serde_json::json!({
            "backend": "local_text",
            "format": "txt",
            "facts_written": 0,
            "deferred": {"waiting_on": "llm.vision.endpoint", "reason": "r", "pages": [1]},
        });

        // Every PDF starved: the muzzle chain's end state, and it must be
        // loud, with exact counts.
        let batch = vec![deferred_empty.clone(), deferred_empty.clone()];
        assert_eq!(ingest_summary_deferrals(&batch), 2);
        let message = deferred_and_nothing_stored(&batch)
            .expect("a run whose every PDF starved must exit non-zero");
        assert!(message.contains("stored nothing"), "{message}");
        assert!(message.contains("2 of 2"), "exact counts: {message}");
        assert!(
            message.contains(DEFERRED_NOTHING_STORED_MARKER),
            "watch mode recognises the error by this marker: {message}",
        );

        // Round four's R3: 399 of 400 starved is not "partial" in any
        // ordinary sense. The round-three `==` gate exited 0 here.
        let mut nearly_all = vec![deferred_stored.clone()];
        nearly_all.resize(400, deferred_empty.clone());
        let message = deferred_and_nothing_stored(&nearly_all)
            .expect("399 of 400 starved PDFs must exit non-zero");
        assert!(message.contains("399 of 400"), "exact counts: {message}");

        // A tabular file beside the starved PDF must NOT vouch for it —
        // round two's gate compared deferrals against ALL summaries and
        // exited 0 here.
        assert!(
            deferred_and_nothing_stored(&[deferred_empty.clone(), csv.clone()]).is_some(),
            "a csv in the directory must not make a starved PDF run green",
        );

        // At the (inclusive) threshold: half the corpus starved IS the
        // outage costing the run its PDFs, even when the other half's
        // deferred document stored facts.
        assert!(
            deferred_and_nothing_stored(&[deferred_empty.clone(), deferred_stored.clone()])
                .is_some(),
            "half the PDF corpus starving is the run's verdict, not a footnote",
        );

        // Below the threshold the deferral stays a visible count, exit 0:
        // a deferred document that stored facts is alive (never starved),
        // and an undeferred PDF's zero facts are its own honest outcome.
        assert!(
            deferred_and_nothing_stored(&[
                deferred_empty.clone(),
                deferred_stored,
                clean_pdf.clone()
            ])
            .is_none(),
            "one starved PDF in three is a visible count, never an error",
        );
        assert!(
            deferred_and_nothing_stored(&[
                deferred_empty.clone(),
                clean_pdf.clone(),
                clean_pdf.clone()
            ])
            .is_none()
        );

        // The starved count is filtered to PDF-backed summaries: a deferred
        // non-PDF neither trips the gate on its own…
        assert!(
            deferred_and_nothing_stored(&[
                deferred_txt.clone(),
                clean_pdf.clone(),
                clean_pdf.clone()
            ])
            .is_none(),
            "a deferred non-PDF must not count as a starved PDF",
        );
        // …nor disarms it when a PDF genuinely starved.
        assert!(
            deferred_and_nothing_stored(&[deferred_txt, deferred_empty]).is_some(),
            "a deferred non-PDF must not push starved past the denominator \
             and silently disable the gate",
        );

        // No PDFs at all: this gate has no opinion.
        assert!(deferred_and_nothing_stored(&[csv]).is_none());
        assert!(deferred_and_nothing_stored(&[clean_pdf]).is_none());
    }

    /// Round four's R3: the run-level deferral count must be visible to
    /// `--json` consumers, not only in the human branch — as a TOP-LEVEL
    /// `deferred_documents` field on both payload shapes.
    #[test]
    fn the_json_payload_carries_the_run_level_deferral_count() {
        let summary = serde_json::json!({
            "backend": "local_text",
            "format": "pdf",
            "facts_written": 3,
        });

        let single = ingest_json_payload(vec![summary.clone()], 1);
        assert_eq!(
            single["deferred_documents"], 1,
            "a single-file payload carries the run-level count: {single}",
        );
        assert_eq!(
            single["backend"], "local_text",
            "the single summary is still the payload itself: {single}",
        );

        let multi = ingest_json_payload(vec![summary.clone(), summary], 2);
        assert_eq!(
            multi["deferred_documents"], 2,
            "a multi-file payload carries the run-level count: {multi}",
        );
        assert_eq!(
            multi["documents"]
                .as_array()
                .expect("summaries ride under `documents`")
                .len(),
            2,
        );
    }

    /// Audit F8: watch-mode recovery retries are REAL re-ingests, so they
    /// are paced — doubling from a minute to a capped ceiling while the
    /// endpoint stays dead — never fired free by a probe that could be
    /// wrong about the reader.
    #[test]
    fn the_watch_retry_schedule_doubles_and_caps() {
        assert_eq!(
            watch_retry_backoff(WATCH_RETRY_MIN_DELAY),
            WATCH_RETRY_MIN_DELAY * 2,
        );
        let mut delay = WATCH_RETRY_MIN_DELAY;
        for _ in 0..16 {
            delay = watch_retry_backoff(delay);
        }
        assert_eq!(
            delay, WATCH_RETRY_MAX_DELAY,
            "the schedule must cap, not grow unboundedly",
        );
        assert_eq!(
            watch_retry_backoff(WATCH_RETRY_MAX_DELAY),
            WATCH_RETRY_MAX_DELAY
        );
    }

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

    /// EVERY MODEL GETS ITS REAL WINDOW, not `None`.
    ///
    /// `build_llm_config` left `context_window` unset, and that single `None`
    /// removed the only bound on generation. PRISM does not cap output on the
    /// operator's behalf by design — a fixed ceiling once made a reasoning
    /// model spend its whole budget thinking and return no JSON — so the
    /// context window IS the bound. Without it a campaign proposal call ran to
    /// 8,813 decoded tokens and died on the HTTP timeout, and every HTTP model
    /// on the ingest path shared one hardcoded elision budget.
    #[test]
    fn build_llm_config_resolves_a_real_context_window() {
        let root = tempfile::tempdir().expect("temp project root");
        let config = build_llm_config(
            root.path(),
            Some("http://127.0.0.1:8081/v1"),
            Some("gpt-5.5"),
            None,
        )
        .expect("config builds");
        let window = config
            .context_window
            .expect("a model without a context window has nothing bounding its output");
        assert!(
            window >= 8_192,
            "a real window, not a placeholder: got {window}"
        );
        // And it must agree with the agent loop — a second answer to this
        // question is how the first one drifted.
        //
        // Compare against `resolve_context_window`, the function production
        // calls, NOT `get_model_config`. The two differ on purpose: the
        // registry answers for the model NAME, while resolution asks the
        // ENDPOINT first and only falls back to the registry. This assertion
        // named the registry and passed for the wrong reason — the `/props`
        // probe was building `{base}/v1/props`, 404ing, and falling back — so
        // it agreed with the registry precisely while the probe was broken.
        // Fixing the probe made it fail here against a developer machine with
        // a llama-server on 8081: `n_ctx` 16384 measured, versus 1,050,000
        // claimed by the catalog for hosted `gpt-5.5`, which is not what is
        // listening on that port. The measured number is the correct one, and
        // an assertion that only holds while a probe is broken is worse than
        // no assertion.
        assert_eq!(
            window,
            prism_agent::models::resolve_context_window("http://127.0.0.1:8081/v1", "gpt-5.5")
                as u64
        );
    }

    /// Row coverage must reach the user's summary: processed of total, the
    /// batch count, and — on a shortfall — how many rows were NOT processed.
    /// This line is the tabular run's honesty about COMPLETENESS (the old
    /// pipeline extracted 10 rows of any dataset and said nothing); dropping
    /// it re-silences the discard.
    #[test]
    fn row_coverage_reaches_the_ingest_summary() {
        let clean = serde_json::json!({
            "row_count": 40, "rows_processed": 40, "batches": 4, "batches_failed": 0
        });
        let report = row_coverage_report(&clean).expect("extraction ran");
        assert!(report.contains("40 of 40 row(s)"), "{report}");
        assert!(report.contains("4 batch(es)"), "{report}");
        assert!(!report.contains("NOT processed"), "{report}");

        let partial = serde_json::json!({
            "row_count": 40, "rows_processed": 30, "batches": 4, "batches_failed": 1
        });
        let report = row_coverage_report(&partial).expect("extraction ran");
        assert!(report.contains("30 of 40"), "{report}");
        assert!(
            report.contains("10 row(s) NOT processed") && report.contains("FAILED STEPS"),
            "{report}"
        );

        // Schema-only runs (no batches) print no coverage line.
        assert_eq!(
            row_coverage_report(&serde_json::json!({"row_count": 40, "batches": 0})),
            None
        );
        assert_eq!(row_coverage_report(&serde_json::json!({})), None);
    }

    /// Chunk coverage, same contract for the text path: processed of total,
    /// and failed chunks named as failures with stored-progress spelled out.
    #[test]
    fn chunk_coverage_reaches_the_ingest_summary() {
        let clean = serde_json::json!({"chunks_total": 6, "chunks_processed": 6});
        let report = chunk_coverage_report(&clean).expect("chunk fields present");
        assert!(report.contains("6 of 6"), "{report}");
        assert!(!report.contains("FAILED"), "{report}");

        let partial = serde_json::json!({"chunks_total": 20, "chunks_processed": 6});
        let report = chunk_coverage_report(&partial).expect("chunk fields present");
        assert!(report.contains("6 of 20"), "{report}");
        assert!(report.contains("14 chunk(s) FAILED"), "{report}");
        assert!(
            report.contains("facts from completed chunks are stored"),
            "{report}"
        );

        assert_eq!(chunk_coverage_report(&serde_json::json!({})), None);
    }

    /// A run where corroboration collapsed must SAY so in the summary.
    ///
    /// Regression for a silent failure: 86 papers at `--samples 3
    /// --agreement 2` stamped `sample_disagreement` on 21,109 of 21,218 facts
    /// while every printed line said the ingest was clean. The rate and its
    /// consequence — hidden from the default read — both have to appear.
    #[test]
    fn collapsed_agreement_reaches_the_ingest_summary() {
        let collapsed = serde_json::json!({"paper_agent": {"agreement": {
            "claims": 104, "corroborated": 0, "comparable_samples": 3, "required": 2,
        }}});
        let report = agreement_report(&collapsed).expect("a sampled run reports");
        assert!(
            report.contains("0 of 104 claim(s) corroborated (0%)"),
            "the collapse must be stated, got: {report}"
        );
        assert!(
            report.contains("104 stamped sample_disagreement")
                && report.contains("HIDDEN from `prism query`"),
            "the consequence is the point, not just the rate: {report}"
        );

        // Full agreement reports the rate and adds no scare text.
        let clean = serde_json::json!({"paper_agent": {"agreement": {
            "claims": 12, "corroborated": 12, "comparable_samples": 3, "required": 2,
        }}});
        let report = agreement_report(&clean).expect("a sampled run reports");
        assert!(
            report.contains("12 of 12 claim(s) corroborated (100%)"),
            "{report}"
        );
        assert!(!report.contains("HIDDEN"), "nothing was hidden: {report}");

        // A single-pass run never compared anything, so it must not print a
        // rate — "0 of N corroborated" would libel a perfectly good ingest.
        let single = serde_json::json!({"paper_agent": {"agreement": {
            "claims": 40, "corroborated": 0, "comparable_samples": 1, "required": 1,
        }}});
        assert_eq!(agreement_report(&single), None);
        assert_eq!(agreement_report(&serde_json::json!({})), None);
    }

    /// Reported usage is what the run COST — output is metered and billed
    /// per token; counting is the control, not truncation. Absent usage
    /// prints nothing rather than a fabricated zero.
    #[test]
    fn llm_usage_reaches_the_ingest_summary() {
        let summary = serde_json::json!({
            "llm_usage": {"prompt_tokens": 1200, "completion_tokens": 400, "total_tokens": 1600}
        });
        let report = llm_usage_report(&summary).expect("usage present");
        assert!(report.contains("1200 prompt"), "{report}");
        assert!(report.contains("400 completion"), "{report}");
        assert!(report.contains("1600 tokens"), "{report}");

        assert_eq!(llm_usage_report(&serde_json::json!({})), None);
        assert_eq!(
            llm_usage_report(&serde_json::json!({"llm_usage": null})),
            None
        );
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

    /// The decoding trace must reach the user's summary in BOTH directions:
    /// a degraded constraint is a Warning carrying the endpoint's reason
    /// (never silent), and an enforced one states the recorded determinism
    /// knobs. A shape without the field (schema-only run, refusal) prints
    /// nothing.
    #[test]
    fn extraction_decoding_reaches_the_ingest_summary() {
        // Degraded: the endpoint rejected json_schema — the summary must
        // say so and carry the reason.
        let degraded = serde_json::json!({
            "extraction_decoding": {
                "mode": "json_object",
                "degraded": "the endpoint rejected schema-constrained decoding \
                             (response_format json_schema): HTTP 400",
                "seed": 42,
                "temperature": 0.0
            }
        });
        let report = extraction_decoding_report(&degraded)
            .expect("a degraded constraint must produce a warning line");
        assert!(report.contains("Warning"), "{report}");
        assert!(report.contains("NOT schema-constrained"), "{report}");
        assert!(report.contains("json_object"), "{report}");
        assert!(report.contains("rejected"), "{report}");

        // Enforced: the summary states mode, seed, and temperature — the
        // reproducibility knobs the provenance activity records.
        let enforced = serde_json::json!({
            "extraction_decoding": {
                "mode": "json_schema",
                "seed": 42,
                "temperature": 0.0
            }
        });
        let report = extraction_decoding_report(&enforced)
            .expect("an enforced constraint must still be stated");
        assert!(!report.contains("Warning"), "{report}");
        assert!(report.contains("json_schema"), "{report}");
        assert!(report.contains("seed 42"), "{report}");
        assert!(report.contains("temperature 0"), "{report}");

        // No trace ⇒ no line (extraction never ran).
        assert_eq!(extraction_decoding_report(&serde_json::json!({})), None);
    }

    /// Facts whose malformed shape the TEXT extractor cannot represent must
    /// reach the user's summary — count AND per-fact reason, same contract as
    /// dropped relationships/entities on the tabular path. Unit vocabulary
    /// does not enter this bucket: non-empty terms are preserved and absent
    /// terms are annotated on stored facts.
    #[test]
    fn dropped_facts_reach_the_ingest_summary() {
        // CONTRACT CHANGE (agentic paper reading): this fixture used to call
        // an arbitrary unit spelling a drop. The only drop contract left is
        // an actually unrepresentable proposal shape.
        let summary = serde_json::json!({
            "backend": "local_text",
            "facts_written": 2,
            "dropped_facts": [
                "'sample has_property ?': malformed fact: missing field `object`"
            ],
        });
        let report =
            dropped_facts_report(&summary).expect("a non-empty drop list must produce a report");
        assert!(report.contains("1 extracted fact(s)"), "{report}");
        assert!(report.contains("missing field `object`"), "{report}");
        assert!(report.contains("NOT stored"), "{report}");
        assert!(
            report.contains("shape could not be represented"),
            "{report}"
        );

        // Nothing dropped (or a shape without the field) ⇒ no report line.
        assert_eq!(
            dropped_facts_report(&serde_json::json!({ "dropped_facts": [] })),
            None
        );
        assert_eq!(dropped_facts_report(&serde_json::json!({})), None);
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

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn dashboard_error_body_cannot_reflect_the_platform_token() {
        let _guard = boot_checks::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _offline = prism_runtime::offline::test_support::OfflineEnvGuard::clear();
        let dashboard = wiremock::MockServer::start().await;
        let marker = "dashboard-platform-secret-marker";
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/api/sessions"))
            .respond_with(
                wiremock::ResponseTemplate::new(401)
                    .set_body_json(serde_json::json!({ "error": marker })),
            )
            .mount(&dashboard)
            .await;

        let error = create_dashboard_session_for_user_with_platform_token(
            &dashboard.uri(),
            "test-user",
            None,
            Some(marker),
        )
        .await
        .expect_err("a 401 must be reported")
        .to_string();

        assert!(error.contains("401"), "{error}");
        assert!(!error.contains(marker), "reflected secret leaked: {error}");
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
    fn exact_source_text_snapshot_is_hash_addressed_and_reopenable() {
        let home = tempfile::tempdir().unwrap();
        let text = "first line\nsecond line\n";

        let first = persist_source_text_snapshot(home.path(), text).unwrap();
        let second = persist_source_text_snapshot(home.path(), text).unwrap();

        assert_eq!(first, second);
        assert_eq!(std::fs::read_to_string(&first).unwrap(), text);
        assert_eq!(
            first.file_name().and_then(|name| name.to_str()),
            Some("c2097f55f01fc297fc7f4acf21438123e06e4d409a818524428534e850642f4f.txt")
        );
    }

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
                "the command must spawn the Python tool server"
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
            platform_provider: Some("marc27".to_string()),
            identity_provider_url: None,
            identity_provider_key: None,
            user_id: None,
            display_name: None,
            org_id: None,
            org_name: None,
            project_id: None,
            project_name: None,
            expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
        };
        let endpoints = PlatformEndpoints {
            api_base: "https://api.marc27.com/api/v1".to_string(),
            node_ws: "wss://api.marc27.com/api/v1/nodes/connect".to_string(),
            provider: Some("marc27".to_string()),
        };
        let selected = PlatformAuth::Bearer(creds.access_token.clone());
        let (credential, rotated) = resolve_node_auth(&paths, &endpoints, Some(&creds), &selected)
            .await
            .expect("fresh creds resolve without network");
        assert_eq!(credential, selected);
        assert!(
            rotated.is_none(),
            "non-expired creds must NOT signal a rotation"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn marc27_refresh_refuses_an_unrelated_endpoint_before_any_request() {
        let server = wiremock::MockServer::start().await;
        let directory = tempfile::tempdir().expect("isolated refresh state");
        let paths = PrismPaths {
            config_dir: directory.path().join("config"),
            cache_dir: directory.path().join("cache"),
            data_dir: directory.path().join("data"),
            state_dir: directory.path().join("state"),
        };
        let credentials = StoredCredentials {
            access_token: "expired-access-token".into(),
            refresh_token: "refresh-token-must-not-leak".into(),
            platform_url: "https://stored.marc27.example".into(),
            platform_provider: Some(MARC27_IDENTITY_PROVIDER.into()),
            ..Default::default()
        };
        let endpoints = PlatformEndpoints::from_url_with_provider(
            &server.uri(),
            Some(MARC27_IDENTITY_PROVIDER.into()),
        );

        let error = refresh_access_token(&paths, &endpoints, &credentials)
            .await
            .expect_err("an endpoint override must not receive the stored refresh token")
            .to_string();

        assert!(error.contains("does not match"), "{error}");
        assert!(!error.contains("refresh-token-must-not-leak"), "{error}");
        assert_eq!(
            server
                .received_requests()
                .await
                .expect("wiremock request recording")
                .len(),
            0,
            "URL binding must fail before any request is sent"
        );
    }

    #[tokio::test]
    async fn supabase_refresh_refuses_an_unrelated_platform_before_identity_request() {
        let identity = wiremock::MockServer::start().await;
        let directory = tempfile::tempdir().expect("isolated refresh state");
        let paths = PrismPaths {
            config_dir: directory.path().join("config"),
            cache_dir: directory.path().join("cache"),
            data_dir: directory.path().join("data"),
            state_dir: directory.path().join("state"),
        };
        let credentials = StoredCredentials {
            access_token: "expired-access-token".into(),
            refresh_token: "supabase-refresh-token-must-not-leak".into(),
            platform_url: "https://trusted-platform.example".into(),
            platform_provider: Some(SUPABASE_IDENTITY_PROVIDER.into()),
            identity_provider_url: Some(identity.uri()),
            identity_provider_key: Some("public-anon-key".into()),
            user_id: Some("supabase:bound-user".into()),
            ..Default::default()
        };
        let endpoints = PlatformEndpoints::from_url_with_provider(
            "https://unrelated-platform.example",
            Some(SUPABASE_IDENTITY_PROVIDER.into()),
        );

        let error = refresh_access_token(&paths, &endpoints, &credentials)
            .await
            .expect_err("platform mismatch must fail before Supabase refresh")
            .to_string();

        assert!(error.contains("does not match"), "{error}");
        assert!(!error.contains("supabase-refresh-token-must-not-leak"));
        assert_eq!(
            identity.received_requests().await.unwrap().len(),
            0,
            "binding must fail before any identity-provider request"
        );
    }

    #[test]
    fn supabase_refresh_requires_a_stored_canonical_principal() {
        let issuer = "https://project.supabase.co/auth/v1";
        let mut credentials = StoredCredentials {
            platform_provider: Some(SUPABASE_IDENTITY_PROVIDER.to_string()),
            ..Default::default()
        };

        for missing in [None, Some("   ".to_string())] {
            credentials.user_id = missing;
            let error = ensure_supabase_refresh_principal(&credentials, issuer, "user-123")
                .expect_err("refresh must not establish a previously unbound identity")
                .to_string();
            assert!(error.contains("missing its canonical principal"), "{error}");
        }
    }

    #[test]
    fn unknown_supabase_role_cannot_bootstrap_local_node_admin() {
        let engine = prism_core::rbac::RbacEngine::in_memory().unwrap();
        let credentials = StoredCredentials {
            platform_provider: Some(SUPABASE_IDENTITY_PROVIDER.to_string()),
            user_id: Some("supabase:scoped-user".into()),
            ..Default::default()
        };

        let error = ensure_dashboard_role(&engine, &credentials, "supabase:scoped-user")
            .expect_err("missing recognized Supabase role must fail closed")
            .to_string();
        assert!(error.contains("no recognized PRISM role"), "{error}");
        assert_eq!(engine.get_local_role("supabase:scoped-user").unwrap(), None);
    }

    #[test]
    fn recognized_supabase_viewer_never_becomes_local_admin() {
        let engine = prism_core::rbac::RbacEngine::in_memory().unwrap();
        let principal = "supabase:scoped-user";
        engine
            .assign_external_role(
                prism_node::provider_roles::SUPABASE_ROLE_PROVIDER,
                principal,
                principal,
                prism_core::rbac::LocalRole::Viewer,
            )
            .unwrap();
        let credentials = StoredCredentials {
            platform_provider: Some(SUPABASE_IDENTITY_PROVIDER.to_string()),
            user_id: Some(principal.into()),
            ..Default::default()
        };

        ensure_dashboard_role(&engine, &credentials, principal).unwrap();
        assert_eq!(
            engine.get_role(principal).unwrap(),
            Some(prism_core::rbac::LocalRole::Viewer)
        );
        assert_eq!(engine.get_local_role(principal).unwrap(), None);
    }

    #[tokio::test]
    async fn node_register_keeps_the_native_first_typed_credential() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "prism-node-auth-precedence-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let paths = PrismPaths {
            config_dir: dir.join("cfg"),
            cache_dir: dir.join("cache"),
            data_dir: dir.join("data"),
            state_dir: dir.join("state"),
        };
        std::fs::create_dir_all(&paths.state_dir).unwrap();
        let endpoints = PlatformEndpoints {
            api_base: "https://provider.example/api/v1".to_string(),
            node_ws: "wss://provider.example/api/v1/nodes/connect".to_string(),
            provider: None,
        };
        let selected = PlatformAuth::Bearer("native-session".to_string());

        let (credential, rotated) = resolve_node_auth(&paths, &endpoints, None, &selected)
            .await
            .expect("typed environment credential resolves without rereading aliases");

        assert_eq!(credential, selected);
        assert!(rotated.is_none());
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
            None,
            false,
        );
        assert_eq!(key, Some(("config-resolved-key".to_string(), None)));
    }

    #[test]
    fn workflow_platform_credential_prefers_every_native_name_over_legacy_aliases() {
        let _guard = boot_checks::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        boot_checks::clear_platform_env();
        unsafe {
            std::env::remove_var("LLM_API_KEY");
            std::env::set_var("PRISM_TOKEN", "native-session");
            std::env::set_var("MARC27_API_KEY", "m27_shadowed-key");
        }

        let resolved = resolve_workflow_llm_api_key_for_target(
            &crate::chat_config::ChatTarget::Marc27 { model: None },
            &prism_core::config::LlmSection::default(),
            Some(&PlatformEndpoints::marc27()),
            Some("stored-session".into()),
            true,
        );

        assert_eq!(
            resolved,
            Some((
                "native-session".to_string(),
                Some(prism_ingest::llm::LlmCredentialKind::Bearer)
            ))
        );
        boot_checks::clear_platform_env();
    }

    #[test]
    fn workflow_supabase_endpoint_does_not_treat_anon_key_as_user_auth() {
        let _guard = boot_checks::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        boot_checks::clear_platform_env();
        unsafe {
            std::env::remove_var("LLM_API_KEY");
            std::env::set_var("PRISM_API_KEY", "supabase-public-anon-key");
        }
        let endpoints = PlatformEndpoints::from_url_with_provider(
            "https://project.supabase.co",
            Some("supabase".to_string()),
        );

        let resolved = resolve_workflow_llm_api_key_for_target(
            &crate::chat_config::ChatTarget::Marc27 { model: None },
            &prism_core::config::LlmSection::default(),
            Some(&endpoints),
            Some("verified-user-session".into()),
            true,
        );

        assert_eq!(
            resolved,
            Some((
                "verified-user-session".to_string(),
                Some(prism_ingest::llm::LlmCredentialKind::Bearer)
            ))
        );
        boot_checks::clear_platform_env();
    }

    #[test]
    fn explicit_workflow_llm_url_never_inherits_platform_credentials() {
        let _guard = boot_checks::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        boot_checks::clear_platform_env();
        let previous_llm_key = std::env::var_os("LLM_API_KEY");
        unsafe { std::env::remove_var("LLM_API_KEY") };

        let resolved = resolve_workflow_llm_api_key_for_target(
            &crate::chat_config::ChatTarget::Marc27 { model: None },
            &prism_core::config::LlmSection::default(),
            Some(&PlatformEndpoints::marc27()),
            Some("stored-platform-secret-marker".into()),
            false,
        );

        unsafe {
            match previous_llm_key {
                Some(value) => std::env::set_var("LLM_API_KEY", value),
                None => std::env::remove_var("LLM_API_KEY"),
            }
        }
        assert_eq!(resolved, None);
        boot_checks::clear_platform_env();
    }

    #[test]
    fn tui_platform_auth_refuses_a_stored_bearer_at_an_unrelated_endpoint() {
        let endpoints = PlatformEndpoints::from_url_with_provider(
            "https://unrelated.example",
            Some(SUPABASE_IDENTITY_PROVIDER.to_string()),
        );
        let credentials = StoredCredentials {
            access_token: "stored-tui-token-must-not-leak".to_string(),
            platform_url: "https://trusted.example".to_string(),
            platform_provider: Some(SUPABASE_IDENTITY_PROVIDER.to_string()),
            ..Default::default()
        };

        let error = tui_platform_auth(Some(&endpoints), Some(&credentials))
            .expect_err("the TUI must not receive a bearer bound to another endpoint")
            .to_string();

        assert!(error.contains("stored session binding refused"), "{error}");
        assert!(error.contains("does not match"), "{error}");
        assert!(!error.contains("stored-tui-token-must-not-leak"));
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
        // No OWL or CIF parser exists anywhere in this workspace; while these
        // were advertised they were read as raw text and fed to a materials-
        // fact prompt. Refusing is the honest answer until a parser lands.
        assert_eq!(ingest_backend(Path::new("/tmp/onto.owl")), None);
        assert_eq!(ingest_backend(Path::new("/tmp/structure.cif")), None);
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
        // A remote endpoint now uses the local PIPELINE (that is the point of
        // bring-your-own-endpoint), so `is_local` no longer distinguishes it.
        // The agreement this test exists to pin is about ON-DEVICE inference,
        // which is the predicate discovery reports and the banner claims.
        assert!(!is_loopback_url("https://api.openai.com/v1"));
        assert!(
            !text_locality_for("auto", Some("https://api.openai.com/v1")).inference_is_on_device()
        );
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

        // CHANGED DELIBERATELY. A remote endpoint the caller NAMED used to
        // land in `CloudNoLocalModel`, which then demanded a platform
        // account — so `--llm-url` at a third-party OpenAI-compatible
        // endpoint was accepted as a flag and then overruled, making
        // bring-your-own-endpoint impossible. It now uses the local PIPELINE
        // with a remote model.
        assert_eq!(
            text_locality_for("auto", Some("https://api.openai.com/v1")),
            TextLocality::LocalPipelineRemoteModel
        );

        // What the old assertion was really protecting SURVIVES, and is what
        // the banner keys on: a remote endpoint is never mistaken for
        // on-device, so PRISM cannot claim "nothing leaves your machine"
        // while sending document text away.
        assert!(!TextLocality::LocalPipelineRemoteModel.inference_is_on_device());
        assert!(TextLocality::Local.inference_is_on_device());
        // …while both still use the local pipeline and the bundled store.
        assert!(TextLocality::LocalPipelineRemoteModel.is_local());

        // No endpoint at all is still a dead end, not a routing decision.
        assert_eq!(
            text_locality_for("auto", None),
            TextLocality::CloudNoLocalModel
        );
        assert_eq!(
            text_locality_for("auto", Some("   ")),
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
        // The sentinel is exact — a lookalike must not ride in as in-process.
        // It is now treated as a named remote endpoint (any non-empty URL is),
        // but the property that matters holds: it is NOT on-device, so PRISM
        // will not claim in-process execution for it.
        assert_eq!(
            text_locality_for("auto", Some("gguf://local.example.com")),
            TextLocality::LocalPipelineRemoteModel
        );
        assert!(
            !text_locality_for("auto", Some("gguf://local.example.com")).inference_is_on_device()
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

    const NO_HQ_EXTRA: &[String] = &[];

    fn hq_flags(
        tasks: Option<&'static str>,
        autoalloc: Option<&'static str>,
    ) -> HqRunFlags<'static> {
        HqRunFlags {
            tasks,
            workers: 2,
            server_dir: None,
            autoalloc,
            time_limit: "1h",
            extra: NO_HQ_EXTRA,
        }
    }

    #[test]
    fn hyperqueue_backend_requires_a_tasks_file() {
        let hq = hq_flags(None, None);
        let error = validate_run_hyperqueue("hyperqueue", &hq, None, None, None).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("--hq-tasks"), "{message}");
    }

    #[test]
    fn hyperqueue_backend_rejects_byoc_target_flags() {
        // --slurm would silently win dispatch over --backend hyperqueue and
        // drop the task set; validation must refuse the combination.
        let hq = hq_flags(Some("tasks.json"), None);
        let error =
            validate_run_hyperqueue("hyperqueue", &hq, None, None, Some("u@h")).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("cannot be combined"), "{message}");
    }

    #[test]
    fn hyperqueue_backend_rejects_unknown_autoalloc_scheduler() {
        let hq = hq_flags(Some("tasks.json"), Some("lsf"));
        let error = validate_run_hyperqueue("hyperqueue", &hq, None, None, None).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("slurm"), "{message}");
        assert!(message.contains("pbs"), "{message}");
    }

    #[test]
    fn hyperqueue_flags_are_rejected_on_other_backends() {
        let hq = hq_flags(Some("tasks.json"), None);
        let error = validate_run_hyperqueue("local", &hq, None, None, None).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("--hq-* flags belong to"), "{message}");
    }

    #[test]
    fn hyperqueue_flags_absent_is_fine_on_other_backends() {
        let hq = hq_flags(None, None);
        assert!(validate_run_hyperqueue("byoc", &hq, Some("u@h"), None, None).is_ok());
    }

    #[test]
    fn hyperqueue_task_file_parses_and_reports_its_path() {
        let dir = std::env::temp_dir().join(format!("prism-cli-hq-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let tasks_path = dir.join("tasks.json");
        std::fs::write(
            &tasks_path,
            r#"[{"command":["python3","ingest.py","paper-47.pdf"],"cwd":"/work/corpus",
               "env":{"VENV":"/work/venv"}}]"#,
        )
        .unwrap();

        let tasks = load_hq_tasks(tasks_path.to_str().unwrap()).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].command, ["python3", "ingest.py", "paper-47.pdf"]);
        assert_eq!(tasks[0].cwd.as_deref(), Some("/work/corpus"));
        assert_eq!(
            tasks[0].env.get("VENV").map(String::as_str),
            Some("/work/venv")
        );

        std::fs::write(&tasks_path, "[{\"command\": \"not-an-array\"}]").unwrap();
        let error = load_hq_tasks(tasks_path.to_str().unwrap()).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("tasks.json"), "{message}");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cli_parses_hyperqueue_options() {
        let cli = Cli::try_parse_from([
            "prism",
            "run",
            "--backend",
            "hyperqueue",
            "--hq-tasks",
            "/work/corpus/tasks.json",
            "--hq-workers",
            "2",
            "--hq-server-dir",
            "/work/hq",
            "--hq-autoalloc",
            "slurm",
            "--hq-time-limit",
            "4h",
            "--hq-extra",
            "--partition=main",
            "--hq-extra",
            "--account=alloc",
            "unused-by-hyperqueue",
        ])
        .unwrap();
        match cli.command.unwrap() {
            Commands::Run(run) => {
                let RunArgs {
                    hq_tasks,
                    hq_workers,
                    hq_server_dir,
                    hq_autoalloc,
                    hq_time_limit,
                    hq_extra,
                    backend,
                    ..
                } = *run;
                assert_eq!(backend, "hyperqueue");
                assert_eq!(hq_tasks.as_deref(), Some("/work/corpus/tasks.json"));
                assert_eq!(hq_workers, 2);
                assert_eq!(hq_server_dir.as_deref(), Some("/work/hq"));
                assert_eq!(hq_autoalloc.as_deref(), Some("slurm"));
                assert_eq!(hq_time_limit, "4h");
                assert_eq!(
                    hq_extra,
                    [
                        "--partition=main".to_string(),
                        "--account=alloc".to_string()
                    ]
                );
            }
            _ => panic!("expected Run command"),
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
            Commands::Run(run) => {
                let RunArgs {
                    image,
                    name,
                    backend,
                    json,
                    ..
                } = *run;
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
            "research-alloc",
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
            Commands::Run(run) => {
                let RunArgs {
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
                } = *run;
                assert_eq!(slurm_account.as_deref(), Some("research-alloc"));
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
    fn cli_parses_both_explicit_model_install_commands() {
        for (name, expected) in [
            (
                "bge-small-en-v1.5",
                model_install::InstallableModel::BgeSmallEnV15,
            ),
            (
                "gemma-4-12b-it-qat-q4_0",
                model_install::InstallableModel::Gemma4_12bItQatQ40,
            ),
        ] {
            let cli = Cli::try_parse_from(["prism", "models", "install", name]).unwrap();
            match cli.command.unwrap() {
                Commands::Models {
                    command: ModelsCommands::Install { model, json },
                } => {
                    assert_eq!(model, expected);
                    assert!(!json);
                }
                _ => panic!("expected Models::Install command for {name}"),
            }
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
            verification_status: None,
            verification_reason: None,
        }
    }

    #[test]
    fn a_narrowed_query_says_how_many_facts_it_withheld() {
        let mut results = LocalOntologyResults {
            nodes: vec![test_node("Ti-6Al-4V", "local")],
            edges: vec![],
            facts: vec![test_recalled_fact("tensile strength", "local")],
            withheld: [("subject_not_verbatim".to_string(), 2usize)]
                .into_iter()
                .collect(),
        };
        let out = format_local_ontology(&results);
        assert!(
            out.contains("2 matching fact(s) withheld") && out.contains("2 subject_not_verbatim"),
            "the count and its status must be printed:\n{out}"
        );
        assert!(
            out.contains("--include-unverified"),
            "must say how to see them:\n{out}"
        );
        results.withheld.clear();
        let out = format_local_ontology(&results);
        assert!(
            !out.contains("withheld"),
            "nothing withheld, nothing claimed:\n{out}"
        );
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
                props_json: None,
                confidence: None,
            }],
            facts: vec![test_recalled_fact("tensile strength", "local")],
            withheld: Default::default(),
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
                props_json: None,
                confidence: None,
            }],
            facts: vec![
                test_recalled_fact("tensile strength", "local"),
                test_recalled_fact("elongation", "mesh:node-a"),
            ],
            withheld: Default::default(),
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
            withheld: Default::default(),
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
            origin_action_id: None,
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
            verification: None,
            verification_reason: None,
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

    /// Two alloys, one shared property NAME shape — the semantic renderer must
    /// say which alloy each measured value belongs to.
    ///
    /// Regression for a measured wrong answer: semantic search returned
    /// "235 MPa√m …" and "415 MPa√m …" adjacent with no subject, and the agent
    /// attributed both to whichever alloy the user had asked about — wrong on
    /// one of them, twice, in opposite directions, stated as a correction.
    #[tokio::test]
    async fn semantic_hits_carry_the_subject_they_were_measured_on() {
        let db = TempProvenanceDb::new();
        let store = prism_provenance::ProvenanceStore::open(&db.path)
            .await
            .expect("open temp store");
        let now = chrono::Utc::now().to_rfc3339();
        let prov = prism_provenance::LocalProvenance {
            activity_id: "act_attr".into(),
            agent_id: "prism-ingest".into(),
            agent_kind: "SoftwareAgent".into(),
            source_entity_id: "doc:toughness".into(),
            source_kind: "Document".into(),
            tenant: LOCAL_ONTOLOGY_TENANT.into(),
            started_at: now.clone(),
            ended_at: now,
            locality: "local".into(),
            origin_source_id: None,
            origin_action_id: None,
        };
        store.record_activity(&prov).await.expect("record activity");

        // The real pair from the ingested corpus: same property phrasing,
        // different alloys, same source document.
        for (subject, object) in [
            (
                "CrCoNi medium-entropy alloy",
                "415 MPa√m crack-initiation fracture toughness (KJIc)",
            ),
            (
                "CrMnFeCoNi high-entropy alloy",
                "235 MPa√m crack-initiation fracture toughness (KJIc)",
            ),
        ] {
            store
                .write_fact_with_classification(
                    &prism_provenance::LocalFact {
                        subject: subject.into(),
                        predicate: "hasProperty".into(),
                        object: object.into(),
                        value: None,
                        unit: None,
                        confidence: Some(1.0),
                        kind: Some("contains".into()),
                    },
                    &prov,
                    prism_provenance::OntologyClassification {
                        version_iri: "urn:test:ontology:semantic-attr",
                        artifact_sha256:
                            "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
                    },
                    prism_provenance::FactGraphShape::emmo("contains"),
                )
                .await
                .expect("write fact");
        }

        // Stand in for the vector hits (the embedding backend is not exercised
        // here — the defect was the missing JOIN, not the ranking).
        let hits: Vec<prism_provenance::SemanticEntityHit> = [
            "415 MPa√m crack-initiation fracture toughness (KJIc)",
            "235 MPa√m crack-initiation fracture toughness (KJIc)",
        ]
        .iter()
        .map(|name| prism_provenance::SemanticEntityHit {
            name: (*name).to_string(),
            tenant: LOCAL_ONTOLOGY_TENANT.to_string(),
            similarity: 0.87,
        })
        .collect();

        let owners = semantic_hit_owners(&db.path, &hits).await;
        assert_eq!(owners.len(), 2);

        let subject_of = |i: usize| -> String {
            owners[i]
                .first()
                .unwrap_or_else(|| panic!("hit {i} must name the subject it was measured on"))
                .subject
                .clone()
        };
        assert_eq!(subject_of(0), "CrCoNi medium-entropy alloy");
        assert_eq!(
            subject_of(1),
            "CrMnFeCoNi high-entropy alloy",
            "the two values must NOT collapse onto one alloy"
        );
    }

    #[tokio::test]
    async fn local_ontology_lookup_reads_ingested_facts_and_is_empty_safe() {
        let db = TempProvenanceDb::new();

        // Fresh (empty) store: clean miss, never an error.
        assert!(
            local_ontology_lookup(
                &db.path,
                "titanium",
                10,
                prism_provenance::VerificationFilter::Trusted
            )
            .await
            .expect("the store is readable")
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
            origin_action_id: None,
        };
        store.record_activity(&prov).await.expect("record activity");
        // CONTRACT CHANGE: `write_fact` no longer resolves typed graph shapes
        // from the kind string — the shape is the ontology's declaration.
        // Write the way the ingest pipeline now does: with the EMMO-declared
        // shape for the "contains" kind.
        store
            .write_fact_with_classification(
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
                prism_provenance::OntologyClassification {
                    version_iri: "urn:test:ontology:local-lookup",
                    artifact_sha256: "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
                },
                prism_provenance::FactGraphShape::emmo("contains"),
            )
            .await
            .expect("write fact");

        // Exact name → neighbor traversal (nodes + edge). A "contains"
        // fact is written as a CONTAINS_ELEMENT edge (see emmo write_fact).
        let hit = local_ontology_lookup(
            &db.path,
            "Ti-6Al-4V",
            10,
            prism_provenance::VerificationFilter::Trusted,
        )
        .await
        .expect("the store is readable")
        .expect("ingested entity must be queryable");
        assert!(hit.nodes.iter().any(|n| n.name == "Ti-6Al-4V"));
        assert!(
            hit.edges
                .iter()
                .any(|e| e.rel_type == "CONTAINS_ELEMENT" && e.target == "alpha phase")
        );

        // Substring → graph_search fallback still finds the node.
        let hit = local_ontology_lookup(
            &db.path,
            "6Al",
            10,
            prism_provenance::VerificationFilter::Trusted,
        )
        .await
        .expect("the store is readable")
        .expect("substring match must be queryable");
        assert!(hit.nodes.iter().any(|n| n.name == "Ti-6Al-4V"));

        // Unknown term → clean miss (caller renders "no matches").
        assert!(
            local_ontology_lookup(
                &db.path,
                "no-such-entity-xyz",
                10,
                prism_provenance::VerificationFilter::Trusted
            )
            .await
            .expect("the store is readable")
            .is_none()
        );

        // CONTRACT CHANGE: an unreadable store is an ERROR, not a miss.
        //
        // This previously asserted the opposite — "store open failure must
        // degrade to a miss" — which is the defect written down as a
        // requirement. A store PRISM cannot open is not a store with nothing
        // in it, and reporting one as the other is how a locked database
        // became "No direct matches" and, through the agent that shells out to
        // this command, "the corpus does not contain that".
        let unreadable = local_ontology_lookup(
            &std::env::temp_dir(),
            "titanium",
            10,
            prism_provenance::VerificationFilter::Trusted,
        )
        .await;
        let error = match unreadable {
            Ok(_) => panic!("an unopenable store must not read as empty"),
            Err(error) => format!("{error:#}"),
        };
        assert!(
            error.contains("could not open the local knowledge graph"),
            "name what failed: {error}"
        );
        assert!(
            error.contains("holding it") || error.contains("PRISM_PROVENANCE_DB"),
            "an error must say how to get out of it: {error}"
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
            origin_action_id: None,
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

        let hit = local_ontology_lookup(
            &db.path,
            "Ti-6Al-4V",
            10,
            prism_provenance::VerificationFilter::Trusted,
        )
        .await
        .expect("the store is readable")
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
            "expected clap's external-subcommand catch-all"
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

    /// The paper pipeline is Rust end to end — retrieval engine, LLM over
    /// HTTP, provenance store. Measured before the exemption: `papers claims`
    /// against loopback endpoints spent 63.5 s of 65 s building a venv it never
    /// used (pip reaching PyPI from inside `cargo test`), then 2.1 s working.
    #[test]
    fn papers_commands_do_not_provision_a_venv() {
        for argv in [
            ["prism", "papers", "search", "--query", "pfas"].as_slice(),
            &["prism", "papers", "sweep", "--query", "pfas"],
            &["prism", "papers", "full-text", "--pmc", "PMC1"],
            &[
                "prism",
                "papers",
                "claims",
                "--url",
                "http://127.0.0.1:1/p.xml",
            ],
            &[
                "prism", "papers", "corpus", "--query", "pfas", "--out", "corpus",
            ],
            &["prism", "reverify", "list", "--status", "stale"],
            &["prism", "reverify", "history", "--assertion", "a1"],
        ] {
            assert!(!needs_python(argv), "{argv:?} is Rust end to end; no venv");
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

    /// CONTRACT CHANGE (agentic paper reading): text ingest now discovers a
    /// promoted project artifact in a fresh CLI process instead of requiring
    /// an in-memory registration from the promotion process. A schema-only
    /// run proves selection without contacting a model/runtime; the reader's
    /// tool loop receives this same adapter on real runs.
    #[tokio::test]
    async fn text_ingest_loads_a_promoted_non_default_ontology_from_project() {
        // CONTRACT CHANGE: the old test expected an early non-EMMO refusal.
        // Promotion now installs an artifact that a fresh ingest process can
        // resolve through the same registry used by the paper tools.
        let dir = project_with_ontology_config("[ontology]\nid = \"text-custom\"\n");
        let root = dir.path();
        let candidate = root.join("text-custom-candidate.ttl");
        let draft = prism_ingest::induction::InducedOntology {
            domain: "text-custom".to_string(),
            status: prism_ingest::induction::OntologyStatus::Draft,
            classes: vec![prism_ingest::induction::InducedClass {
                label: "Compound".to_string(),
                definition: "A concept supplied by the promoted ontology.".to_string(),
                parent: None,
                aligned_iri: None,
                declared_by_reference: false,
                sign_domain: None,
            }],
            relations: Vec::new(),
            provenance: prism_ingest::induction::InductionProvenance {
                corpus_hash: "sha256:text-project-load-test".to_string(),
                prompt_version: "test".to_string(),
                ..Default::default()
            },
        };
        prism_ingest::induction::ttl::write_artifact(&candidate, &draft)
            .expect("write draft ontology artifact");
        let promoted = prism_ingest::induction::ttl::promote_artifact(&candidate)
            .expect("promote ontology artifact");
        crate::ontology_cmd::install_promoted_artifact(root, &promoted)
            .expect("install promoted ontology for future processes");

        let md = root.join("notes.md");
        std::fs::write(&md, "# title\nbody text\n").unwrap();

        // TEST-NET-1 runtime URL: schema-only returns before anything is
        // contacted, so an unreachable address proves selection is local.
        let out = run_local_text_ingest_file(
            &md,
            root,
            None,
            None,
            None,
            "http://192.0.2.1:1",
            true,
            None,
            prism_ingest::text_extract::SamplingPolicy::default(),
            VisionModelChoice::default(),
            &mut None,
        )
        .await
        .expect("a project-installed non-default ontology must reach text ingest");
        assert_eq!(out["backend"], "local_text");
        assert_eq!(out["schema_only"], true);
        assert!(
            prism_ingest::ontologies::active(Some("text-custom")).is_ok(),
            "config resolution must register the project artifact for the paper reader"
        );
    }

    /// Build a minimal one-page PDF whose content stream draws `text` —
    /// enough for pdf-extract to find extractable text. Cross-reference
    /// offsets are computed, not hardcoded, so the fixture is valid byte-
    /// for-byte without a binary blob in the repo.
    fn minimal_pdf(text: &str) -> Vec<u8> {
        let stream = format!("BT /F1 12 Tf 72 720 Td ({text}) Tj ET");
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R >> >> >>"
                .to_string(),
            format!(
                "<< /Length {} >>\nstream\n{stream}\nendstream",
                stream.len()
            ),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (index, object) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
        }
        let xref_pos = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for offset in offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_pos}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    /// The local text path parses PDFs ON DEVICE: production dispatch
    /// (`run_local_text_ingest_file`, the same function `handle_ingest`
    /// calls) must extract text from a real PDF and report its size — never
    /// skip, never contact a runtime. The TEST-NET-1 runtime URL proves the
    /// second half: any contact would fail the test.
    ///
    /// Mutation check: restoring the old "local PDF parsing isn't available
    /// yet" refusal branch makes this die on `skipped`.
    #[tokio::test]
    async fn local_text_ingest_parses_a_pdf_on_device() {
        // This drives PRODUCTION dispatch, which starts a real `VisionSeam`
        // and registers a live "vision" reader in the process-wide registry
        // — proven with `--nocapture`. Unlocked and uncleaned, that live
        // reader can flip the tombstone assertion in
        // `handle_ingest_fails_loudly_when_every_pdf_starves_behind_vision`
        // running on a parallel thread.
        let _registry = VISION_REGISTRY_TEST_LOCK.lock().await;
        let _cleanup = DeregisterVision;
        let dir = project_with_ontology_config("[ontology]\nid = \"emmo\"\n");
        let root = dir.path();
        let pdf = root.join("paper.pdf");
        std::fs::write(&pdf, minimal_pdf("Ti-6Al-4V tensile strength 1100 MPa")).unwrap();

        let out = run_local_text_ingest_file(
            &pdf,
            root,
            None,
            None,
            None,
            "http://192.0.2.1:1",
            true, // schema-only: text extraction without an LLM or a store
            None,
            prism_ingest::text_extract::SamplingPolicy::default(),
            VisionModelChoice::default(),
            &mut None,
        )
        .await
        .expect("a parseable PDF must ingest locally");
        assert_eq!(out["backend"], "local_text");
        assert_eq!(out["format"], "pdf");
        assert!(
            out.get("skipped").is_none(),
            "the local PDF branch must parse, not skip: {out}"
        );
        let chars = out["chars"].as_u64().expect("chars must be reported");
        assert!(chars > 0, "no text extracted: {out}");
    }

    /// A malformed PDF is an honest per-file error naming the file — not a
    /// skip, not a silent empty success, and not a crash of the whole run.
    #[tokio::test]
    async fn local_text_ingest_reports_an_unparseable_pdf_honestly() {
        // Same registry hygiene as `local_text_ingest_parses_a_pdf_on_device`:
        // production dispatch touches the process-wide vision registry.
        let _registry = VISION_REGISTRY_TEST_LOCK.lock().await;
        let _cleanup = DeregisterVision;
        let dir = project_with_ontology_config("[ontology]\nid = \"emmo\"\n");
        let root = dir.path();
        let pdf = root.join("broken.pdf");
        std::fs::write(&pdf, b"%PDF-1.4 garbage, not really a pdf").unwrap();

        let err = run_local_text_ingest_file(
            &pdf,
            root,
            None,
            None,
            None,
            "http://192.0.2.1:1",
            true,
            None,
            prism_ingest::text_extract::SamplingPolicy::default(),
            VisionModelChoice::default(),
            &mut None,
        )
        .await
        .expect_err("a malformed PDF must be an error, not a silent skip");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("broken.pdf"),
            "error must name the file: {msg}"
        );
    }

    /// End-to-end through the PRODUCTION text-ingest path
    /// (`run_local_text_ingest_file` against a mocked OpenAI-shaped LLM):
    ///
    /// 1. non-empty unit terms selected by the reading model, including a
    ///    customer's non-QUDT term, are STORED exactly instead of translated
    ///    through a Rust vocabulary;
    /// 2. an explicitly blank selected term is still stored, explicitly
    ///    annotated `unit_unresolved` and excluded from trusted reads;
    /// 3. neither case becomes a per-fact drop or a second repair prompt.
    ///
    /// HOME is overridden under the shared env lock because the production
    /// path derives the store location from `$HOME/.prism` — the point is
    /// precisely NOT to bypass that derivation with an injected path.
    // Same contract as the other ENV_LOCK tests here: the guard must span
    // the awaits so no parallel test observes the overridden HOME.
    /// Restores the prior `$HOME` and embedding-backend choice on drop. Only
    /// construct while holding `boot_checks::ENV_LOCK` — both are
    /// process-global. Text-ingest tests force embeddings off so their mocked
    /// LLM is the only endpoint they can contact.
    struct HomeGuard {
        home: Option<std::ffi::OsString>,
        embed_backend: Option<std::ffi::OsString>,
    }

    impl HomeGuard {
        fn isolated(home: &Path) -> Self {
            let guard = Self {
                home: std::env::var_os("HOME"),
                embed_backend: std::env::var_os("PRISM_EMBED_BACKEND"),
            };
            unsafe {
                std::env::set_var("HOME", home);
                std::env::set_var("PRISM_EMBED_BACKEND", "off");
            }
            guard
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            unsafe {
                match self.home.take() {
                    Some(v) => std::env::set_var("HOME", v),
                    None => std::env::remove_var("HOME"),
                }
                match self.embed_backend.take() {
                    Some(v) => std::env::set_var("PRISM_EMBED_BACKEND", v),
                    None => std::env::remove_var("PRISM_EMBED_BACKEND"),
                }
            }
        }
    }

    /// `[llm] max_output_tokens` must reach the extraction client.
    /// gemma-4-12B (a reasoning model) spent the entire default 4096-token
    /// budget on reasoning_content and produced zero JSON — and the
    /// client's error message pointed at exactly this knob, which
    /// `build_llm_config` silently dropped until now.
    #[test]
    fn llm_max_output_tokens_reaches_the_extraction_client() {
        let _guard = boot_checks::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _restore_home = HomeGuard::isolated(home.path());

        let dir = project_with_ontology_config("[llm]\nmax_output_tokens = 8192\n");
        let cfg = build_llm_config(dir.path(), Some("http://127.0.0.1:9"), Some("m"), None)
            .expect("config must build");
        assert_eq!(cfg.max_output_tokens, Some(8192));

        // Unset → None: the client's conservative default stays binding.
        let dir = project_with_ontology_config("");
        let cfg = build_llm_config(dir.path(), Some("http://127.0.0.1:9"), Some("m"), None)
            .expect("config must build");
        assert_eq!(cfg.max_output_tokens, None);
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn text_ingest_preserves_selected_unit_terms_and_annotates_a_blank_one() {
        // CONTRACT CHANGE (vocabulary-neutral units): this test formerly
        // expected Rust to recognise two QUDT identifiers and reject an
        // arbitrary spelling. It now pins exact preservation of a customer
        // term and reserves `unit_unresolved` for an explicitly blank term;
        // absence alone is a semantic choice Rust cannot judge.
        let _guard = boot_checks::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let mut server = mockito::Server::new_async().await;
        let extraction = r#"{"facts":[
            {"subject":"Ti-6Al-4V","predicate":"has_measurement","object":"UTS","value":880.0,"unit":"QUDT:MegaPA","conditions":[],"confidence":0.9,"kind":"measurement","evidence_class":"research"},
            {"subject":"Ti-6Al-4V","predicate":"has_measurement","object":"density","value":4.43,"unit":"customer:U-42","conditions":[],"confidence":0.9,"kind":"measurement","evidence_class":"research"},
            {"subject":"Ti-6Al-4V","predicate":"has_measurement","object":"hardness","value":349.0,"unit":"   ","conditions":[],"confidence":0.9,"kind":"measurement","evidence_class":"research"}
        ]}"#;
        // Unit terms come from the ontology-reading model. Rust neither
        // translates them through a glossary nor rejects an unfamiliar
        // non-empty vocabulary term.
        let _mock = server
            .mock("POST", "/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body_from_request(agentic_extraction_responder(extraction))
            .create_async()
            .await;

        let home = tempfile::tempdir().expect("home tempdir");
        std::fs::create_dir_all(home.path().join(".prism")).unwrap();
        let _restore_home = HomeGuard::isolated(home.path());

        let project = project_with_ontology_config("[ontology]\nid = \"emmo\"\n");
        let root = project.path();
        let md = root.join("alloy-datasheet.md");
        std::fs::write(
            &md,
            "Ti-6Al-4V: UTS 880 QUDT:MegaPA, density 4.43 customer:U-42, hardness 349.",
        )
        .unwrap();

        let summary = run_local_text_ingest_file(
            &md,
            root,
            Some("test-extractor"),
            Some(&server.url()),
            None,
            "http://192.0.2.1:1",
            false,
            None,
            prism_ingest::text_extract::SamplingPolicy::default(),
            VisionModelChoice::default(),
            &mut None,
        )
        .await
        .expect("a document with one bad fact must still ingest the good ones");

        // All three facts landed; the envelope parsed, so the old "could
        // not be parsed as JSON" misreport must be gone.
        //
        // The blank-term fact is stored under `unit_unresolved` — excluded
        // from default reads, findable by filter — so the funnel counts it as
        // stored-unverified instead of silently losing it.
        assert_eq!(summary["facts_written"], 3, "summary: {summary}");
        assert_eq!(summary["parse_error"], serde_json::Value::Null);
        let funnel = &summary["funnel"];
        assert_eq!(funnel["facts_proposed"], 3, "summary: {summary}");
        assert_eq!(funnel["stored_trusted"], 2, "summary: {summary}");
        assert_eq!(funnel["stored_unverified"], 1, "summary: {summary}");
        assert_eq!(
            funnel["unverified_by_status"]["unit_unresolved"], 1,
            "summary: {summary}"
        );
        assert_funnel_partitions(funnel);
        let semantic = summary["semantic_validation"]
            .as_array()
            .expect("semantic validation reports must be machine-readable");
        assert_eq!(semantic.len(), 1, "one write-bearing chunk: {summary}");
        for check in ["near_duplicates", "typing", "triple_plausibility"] {
            assert_eq!(
                semantic[0][check]["status"], "unavailable",
                "an absent embedding backend cannot pose as an applied check: {summary}"
            );
            assert_eq!(
                semantic[0][check]["passed"],
                serde_json::Value::Null,
                "an unavailable check cannot pose as passed: {summary}"
            );
        }
        // Nothing was dropped — the defect travels ON the stored fact.
        let dropped = summary["dropped_facts"]
            .as_array()
            .expect("dropped_facts must be in the summary");
        assert!(dropped.is_empty(), "summary: {summary}");

        // The store the production path wrote is under the overridden HOME.
        let db_path = home.path().join(".prism/provenance.db");
        assert_eq!(
            summary["store"].as_str().unwrap(),
            db_path.display().to_string(),
            "the store must be the HOME-derived one"
        );
        let store = prism_provenance::ProvenanceStore::open(&db_path)
            .await
            .expect("the store the ingest wrote must open");
        let uts = store.recall_with_context("UTS", "local", 10).await.unwrap();
        assert_eq!(uts.len(), 1, "{uts:?}");
        assert_eq!(uts[0].value, Some(880.0));
        assert_eq!(
            uts[0].unit.as_deref(),
            Some("QUDT:MegaPA"),
            "the selected term must survive byte-for-byte"
        );
        let density = store
            .recall_with_context("density", "local", 10)
            .await
            .unwrap();
        assert_eq!(density.len(), 1, "{density:?}");
        assert_eq!(density[0].value, Some(4.43));
        assert_eq!(density[0].unit.as_deref(), Some("customer:U-42"));
        // A structurally blank term is invisible to the default (trusted)
        // read, while its cited fact remains available for review.
        let hardness = store
            .recall_with_context("hardness", "local", 10)
            .await
            .unwrap();
        assert!(
            hardness.is_empty(),
            "a fact carrying an explicitly blank term must not appear in the \
             default read: {hardness:?}"
        );
        // …but it is PRESENT and findable, with no unit and the status +
        // reason that say exactly what happened.
        let quarantined = store
            .recall_with_context_filtered(
                "hardness",
                &["local"],
                10,
                prism_provenance::VerificationFilter::Status(
                    prism_provenance::VerificationStatus::UnitUnresolved,
                ),
            )
            .await
            .unwrap();
        assert_eq!(quarantined.len(), 1, "{quarantined:?}");
        assert_eq!(quarantined[0].value, Some(349.0));
        assert_eq!(
            quarantined[0].unit, None,
            "a blank term becomes absent; no unit may be guessed"
        );
        assert!(
            quarantined[0]
                .verification_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("is empty")),
            "{:?}",
            quarantined[0].verification_reason
        );

        // Nothing reached the repair machinery: no refusal, no ledger row,
        // no queue item — the record to review lives in the store itself.
        assert_eq!(
            summary["repairs"]["code_withdrawn"], 0,
            "summary: {summary}"
        );
        assert_eq!(summary["repairs"]["code_accepted"], 0, "summary: {summary}");
        assert_eq!(
            summary["repairs"]["enqueued_for_model"], 0,
            "summary: {summary}"
        );
        let document_id = std::fs::canonicalize(&md).unwrap().display().to_string();
        assert!(
            store
                .repair_dispositions(&document_id)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .pending_repairs(&document_id, 10)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// Audit F2, end to end through PRODUCTION dispatch
    /// (`run_local_text_ingest_file` against a mocked extraction LLM): a PDF
    /// with a damaged page, ingested while the vision endpoint key is
    /// withdrawn, still EXTRACTS AND STORES — and its summary carries the
    /// `deferred` record naming the blocked pages. The old code returned a
    /// parked summary before extraction, discarding every sound page of a
    /// 40-page paper over one micrograph, and the run exited 0 having
    /// stored nothing.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn a_deferred_pdf_still_extracts_and_stores_its_readable_text() {
        use prism_ingest::document::VisionSeam;

        let _guard = boot_checks::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _registry = VISION_REGISTRY_TEST_LOCK.lock().await;
        let _cleanup = DeregisterVision;

        let mut server = mockito::Server::new_async().await;
        let extraction = r#"{"facts":[
            {"subject":"alloy","predicate":"has_measurement","object":"note","value":3.0,"unit":"customer:U-1","conditions":[],"confidence":0.9,"kind":"measurement","evidence_class":"research"}
        ]}"#;
        let _mock = server
            .mock("POST", "/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            // Cites line 3: pdf-extract renders this fixture with two
            // leading blank lines, and a citation quoting an empty line is
            // dropped as malformed.
            .with_body_from_request(agentic_extraction_responder_citing(extraction, 3))
            .create_async()
            .await;

        let home = tempfile::tempdir().expect("home tempdir");
        std::fs::create_dir_all(home.path().join(".prism")).unwrap();
        let _restore_home = HomeGuard::isolated(home.path());

        let project = project_with_ontology_config("[ontology]\nid = \"emmo\"\n");
        let root = project.path();
        let pdf = root.join("micrograph-paper.pdf");
        // Sparse on purpose: under the 120-char floor, so the page is
        // damaged and — with no vision reader active — stays unrecovered.
        std::fs::write(&pdf, minimal_pdf("Figure 3: cross-section micrograph")).unwrap();

        // The run-scoped seam after its breaker tripped: started, then
        // withdrawn with the reader's own last error — the tombstone stands
        // in the registry (restored by the guard above); no network.
        let mut seam = VisionSeam::start(unanswered_llm_config())
            .await
            .expect("the seam starts");
        seam.withdraw("the vision reader failed 3 consecutive read(s); last: connection refused")
            .await;
        let mut vision_seam = Some(seam);

        let summary = run_local_text_ingest_file(
            &pdf,
            root,
            Some("test-extractor"),
            Some(&server.url()),
            None,
            "http://192.0.2.1:1",
            false,
            None,
            prism_ingest::text_extract::SamplingPolicy::default(),
            VisionModelChoice::default(),
            &mut vision_seam,
        )
        .await
        .expect("a deferred document must still ingest its readable text");

        // The muzzle check: extraction RAN and stored. A parked-and-empty
        // summary here is the corpus-killer coming back.
        assert!(
            summary["facts_written"].as_u64().is_some_and(|n| n > 0),
            "the readable text must be extracted and stored despite the \
             deferral (audit F2): {summary}",
        );
        // And the deferral is recorded, machine-readably, for the re-run.
        let deferred = summary
            .get("deferred")
            .expect("the blocked pages must be recorded on the summary");
        assert_eq!(deferred["waiting_on"], "llm.vision.endpoint");
        assert_eq!(deferred["pages"], serde_json::json!([1]));
        assert!(
            deferred["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("connection refused")),
            "{deferred}",
        );
    }

    /// A PATH override that puts a directory FIRST, so a fake renderer
    /// shadows any real one. Restored on drop; callers hold
    /// `boot_checks::ENV_LOCK` for the duration.
    struct PathGuard {
        previous: Option<std::ffi::OsString>,
    }
    impl PathGuard {
        fn prepend(dir: &Path) -> Self {
            let previous = std::env::var_os("PATH");
            let mut paths: Vec<std::path::PathBuf> = vec![dir.to_path_buf()];
            if let Some(previous) = &previous {
                paths.extend(std::env::split_paths(previous));
            }
            let joined = std::env::join_paths(paths).expect("PATH joins");
            unsafe { std::env::set_var("PATH", joined) };
            Self { previous }
        }
    }
    impl Drop for PathGuard {
        fn drop(&mut self) {
            unsafe {
                match self.previous.take() {
                    Some(v) => std::env::set_var("PATH", v),
                    None => std::env::remove_var("PATH"),
                }
            }
        }
    }

    /// A fake `pdftoppm` on PATH that "renders" every page by emitting a
    /// small REAL PNG — so the vision reader's rasterise and crop steps
    /// succeed and the only thing left to fail is the endpoint call itself.
    /// Callers hold `boot_checks::ENV_LOCK` for the PATH override.
    fn fake_renderer_emitting_a_real_png() -> (tempfile::TempDir, PathGuard) {
        use base64::Engine as _;
        // A 64×48 flat-grey RGB PNG, 121 bytes.
        let png = base64::engine::general_purpose::STANDARD
            .decode(concat!(
                "iVBORw0KGgoAAAANSUhEUgAAAEAAAAAwCAIAAAAuKetIAAAAQElEQVR42u3PMQ0AAAwDoPpXVlm1",
                "sHcJOCB9LgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICVwP3MCGlAxJ6DgAAAABJ",
                "RU5ErkJggg==",
            ))
            .expect("embedded PNG decodes");
        let fake_bin = tempfile::tempdir().expect("fake bin dir");
        std::fs::write(fake_bin.path().join("page.png"), png).unwrap();
        let fake = fake_bin.path().join("pdftoppm");
        std::fs::write(&fake, "#!/bin/sh\ncat \"$(dirname \"$0\")/page.png\"\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = PathGuard::prepend(fake_bin.path());
        (fake_bin, path)
    }

    /// An LLM endpoint that ANSWERS and fails every call — HTTP 400, as a
    /// text-only model handed a PNG answers. Failing on the wire is what
    /// makes the failure REMOTE (round four's R1): a fake renderer that
    /// merely exits non-zero is a LOCAL failure now, and local failures
    /// defer nothing and trip no breaker.
    async fn endpoint_refusing_every_read() -> mockito::ServerGuard {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/chat/completions")
            .with_status(400)
            .with_body("no vision-capable model is loaded")
            .create_async()
            .await;
        server
    }

    /// Audits F9, B2 and B6 through `handle_ingest` ITSELF — the function
    /// the watch loop calls, which previously had zero test callers (a
    /// verifier replaced the exit gate's call site with a no-op and the
    /// whole suite stayed green). A corpus whose only PDF is fully scanned,
    /// read while the live vision reader's ENDPOINT call fails (the render
    /// itself succeeds — a local render failure is not a deferral, R1),
    /// must:
    /// 1. NOT abort or discard the file (B2): it yields a summary with zero
    ///    facts and machine-readable deferred pages;
    /// 2. exit non-zero carrying the deferral marker (F9), because every
    ///    PDF-backed document starved;
    /// 3. retire its seam on the way out (B6): the registry holds the
    ///    tombstone, never a stale live reader;
    /// and the watch loop's bookkeeping, driven by that same real error,
    /// must keep the file retry-eligible (F8).

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn handle_ingest_fails_loudly_when_every_pdf_starves_behind_vision() {
        let _guard = boot_checks::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _registry = VISION_REGISTRY_TEST_LOCK.lock().await;
        let _cleanup = DeregisterVision;

        let (_fake_bin, _path) = fake_renderer_emitting_a_real_png();
        let server = endpoint_refusing_every_read().await;

        let home = tempfile::tempdir().expect("home tempdir");
        std::fs::create_dir_all(home.path().join(".prism")).unwrap();
        let _restore_home = HomeGuard::isolated(home.path());
        let project = project_with_ontology_config("[ontology]\nid = \"emmo\"\n");
        let root = project.path();
        let pdf = root.join("scanned.pdf");
        // No readable text at all: the whole document depends on vision.
        std::fs::write(&pdf, minimal_pdf(" ")).unwrap();

        // The vision reader inherits this endpoint; extraction never runs
        // (no text survives to extract), so the 400s are all it answers.
        let llm_url = server.url();
        let err = handle_ingest(
            &pdf,
            root,
            Some("test-extractor"),
            Some(&llm_url),
            None,
            false,
            "http://192.0.2.1:1",
            None,
            false,
            None,
            prism_ingest::text_extract::SamplingPolicy::default(),
            VisionModelChoice::default(),
        )
        .await
        .expect_err("a run whose every PDF starved behind vision must exit non-zero");
        let message = format!("{err:#}");
        assert!(
            message.contains(DEFERRED_NOTHING_STORED_MARKER),
            "watch mode keys its retry off this marker: {message}",
        );
        assert!(message.contains("stored nothing"), "{message}");

        // B6: the run retired its component on the way out — the registry
        // holds the tombstone with the run-ended reason, never a stale live
        // reader for the next surface in this process to trust.
        match prism_ingest::document::registry()
            .get("vision")
            .expect("the tombstone stands after the run")
            .readiness()
        {
            prism_ingest::document::Readiness::Unavailable(reason) => {
                assert!(reason.contains("run ended"), "{reason}");
            }
            prism_ingest::document::Readiness::Ready => panic!(
                "handle_ingest must retire the seam: a stale live reader \
                 survived the run"
            ),
        }

        // F8: the watch loop's bookkeeping, driven by the SAME real error —
        // the fully-starved file keeps its retry eligibility.
        let mut deferred = std::collections::HashSet::new();
        let fully_ingested = watch_ingest_once(
            &pdf,
            &mut deferred,
            root,
            Some("test-extractor"),
            Some(&llm_url),
            None,
            false,
            "http://192.0.2.1:1",
            None,
            false,
            None,
            prism_ingest::text_extract::SamplingPolicy::default(),
            VisionModelChoice::default(),
        )
        .await;
        assert!(!fully_ingested);
        assert!(
            deferred.contains(&pdf),
            "a fully-starved file must keep its retry eligibility",
        );
    }

    /// One unreadable file must not end the corpus, and must not erase the
    /// files already ingested. This loop was `Err => break`, then returned
    /// before printing a summary: file 2 of 3 bad meant file 3 never
    /// attempted and file 1 — already in the graph — never reported. The run
    /// still exits non-zero, AFTER every sound file is reported, with a
    /// message that says what was lost and what was kept.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn one_bad_file_does_not_end_the_corpus_or_hide_the_good_ones() {
        let _guard = boot_checks::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        std::fs::create_dir_all(home.path().join(".prism")).unwrap();
        let _restore_home = HomeGuard::isolated(home.path());

        let corpus = tempfile::tempdir().expect("corpus tempdir");
        let csv = "material,uts_mpa\nTi-6Al-4V,950\nInconel 718,1240\n";
        std::fs::write(corpus.path().join("a-good.csv"), csv).unwrap();
        std::fs::write(
            corpus.path().join("b-bad.pdf"),
            b"%PDF-1.4 garbage not a pdf",
        )
        .unwrap();
        std::fs::write(corpus.path().join("c-good.csv"), csv).unwrap();

        // An explicit model + unroutable URL: resolution must not probe this
        // machine's live ports (it lists them in its refusal otherwise), and
        // schema-only never calls the model for a CSV.
        let err = handle_ingest(
            corpus.path(),
            corpus.path(),
            Some("test-extractor"),
            Some("http://127.0.0.1:1"),
            None,
            true, // schema_only: the sound files produce a summary with no LLM
            "http://192.0.2.1:1",
            None,
            false,
            None,
            prism_ingest::text_extract::SamplingPolicy::default(),
            VisionModelChoice::default(),
        )
        .await
        .expect_err("a run with a failed file must still exit non-zero");
        let msg = format!("{err:#}");
        assert!(msg.contains("1 of 3 file(s) failed"), "{msg}");
        assert!(
            msg.contains("2 ingested"),
            "the sound files must be counted, not lost: {msg}"
        );
        assert!(
            msg.contains("b-bad.pdf"),
            "the failed file must be named: {msg}"
        );
    }

    /// Round four's R5, restoring coverage round three deleted with
    /// `deferred_watch_files_retry_only_when_the_endpoint_answers`: the
    /// watch loop's retry set is driven by `watch_ingest_once`'s Ok arms,
    /// and a verifier deleted BOTH `deferred.insert(...)` and
    /// `deferred.remove(path)` from them with the suite staying green — a
    /// cleanly-ingested file could sit in the retry set forever, and a
    /// deferred one could silently lose its retry. Schema-only keeps the
    /// starved-run error gate out of the way, so the deferral arrives as
    /// `Ok(report.deferred_documents > 0)` — the exact arm the Err-path
    /// test above cannot reach.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn deferred_watch_files_leave_the_retry_set_only_when_clean() {
        let _guard = boot_checks::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _registry = VISION_REGISTRY_TEST_LOCK.lock().await;
        let _cleanup = DeregisterVision;

        let (_fake_bin, _path) = fake_renderer_emitting_a_real_png();
        let server = endpoint_refusing_every_read().await;
        let llm_url = server.url();

        let project = project_with_ontology_config("[ontology]\nid = \"emmo\"\n");
        let root = project.path();
        let scanned = root.join("scanned.pdf");
        std::fs::write(&scanned, minimal_pdf(" ")).unwrap();
        let sound = root.join("sound.pdf");
        // Comfortably above the sparse floor, so no page escalates and no
        // vision read happens: a clean ingest.
        std::fs::write(
            &sound,
            minimal_pdf(
                "A genuine paragraph of body text about nickel superalloy \
                 disks, powder metallurgy processing, and thermal cracking \
                 behaviour in alloys with high refractory content such as \
                 molybdenum, niobium and tungsten.",
            ),
        )
        .unwrap();

        let mut deferred = std::collections::HashSet::new();

        // A deferral through the REAL run report: the file must ENTER the
        // retry set and the run must not report full ingestion.
        let fully_ingested = watch_ingest_once(
            &scanned,
            &mut deferred,
            root,
            Some("test-extractor"),
            Some(&llm_url),
            None,
            true, // schema-only: no store, no extraction model
            "http://192.0.2.1:1",
            None,
            false,
            None,
            prism_ingest::text_extract::SamplingPolicy::default(),
            VisionModelChoice::default(),
        )
        .await;
        assert!(!fully_ingested, "a deferred run is not a full ingest");
        assert!(
            deferred.contains(&scanned),
            "a file whose run deferred documents must enter the retry set",
        );

        // A clean ingest must LEAVE the set — pre-seeded, then re-ingested
        // cleanly. Without the `deferred.remove`, this file retries forever
        // with nothing going red.
        deferred.insert(sound.clone());
        let fully_ingested = watch_ingest_once(
            &sound,
            &mut deferred,
            root,
            Some("test-extractor"),
            Some(&llm_url),
            None,
            true,
            "http://192.0.2.1:1",
            None,
            false,
            None,
            prism_ingest::text_extract::SamplingPolicy::default(),
            VisionModelChoice::default(),
        )
        .await;
        assert!(fully_ingested, "a clean run reports full ingestion");
        assert!(
            !deferred.contains(&sound),
            "a cleanly-ingested file must leave the retry set",
        );
        // The genuinely deferred file keeps its eligibility — another
        // file's success must not drain it.
        assert!(deferred.contains(&scanned));
    }

    /// THE FULL LOOP through production dispatch: Phase 1 enqueues a
    /// CONTRACT CHANGE: a value-less legacy `measurement` hint is no longer
    /// a population refusal. The generic relation stores immediately, so a
    /// later repair run sees no debt and makes no model call.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn agentic_population_does_not_enqueue_a_representable_relation() {
        // CONTRACT CHANGE: a closed `measurement` dispatch used to create a
        // repair item for this value-less relation. The generic cited fact is
        // representable and fresh agent output never enters the legacy queue.
        let _guard = boot_checks::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let mut ingest_server = mockito::Server::new_async().await;
        let extraction = r#"{"facts":[
            {"subject":"steel","predicate":"has_measurement","object":"elongation","value":null,"unit":null,"conditions":[],"confidence":0.9,"kind":"measurement","evidence_class":"research"}
        ]}"#;
        // The two-turn responder forces a read before the cited proposal.
        let _ingest_mock = ingest_server
            .mock("POST", "/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body_from_request(agentic_extraction_responder(extraction))
            .expect(2)
            .create_async()
            .await;

        let home = tempfile::tempdir().expect("home tempdir");
        std::fs::create_dir_all(home.path().join(".prism")).unwrap();
        let _restore_home = HomeGuard::isolated(home.path());

        let project = project_with_ontology_config("[ontology]\nid = \"emmo\"\n");
        let root = project.path();
        let md = root.join("steel-report.md");
        std::fs::write(
            &md,
            "The steel samples reached an elongation of 4.5 % in tension. For the\n\
             same steel the transverse elongation of 4.5 mm was recorded instead.",
        )
        .unwrap();

        let summary = run_local_text_ingest_file(
            &md,
            root,
            Some("test-extractor"),
            Some(&ingest_server.url()),
            None,
            "http://192.0.2.1:1",
            false,
            None,
            prism_ingest::text_extract::SamplingPolicy::default(),
            VisionModelChoice::default(),
            &mut None,
        )
        .await
        .expect("the representable relation ingests");
        assert_eq!(summary["facts_written"], 1, "{summary}");
        assert_eq!(summary["repairs"]["code_accepted"], 0, "{summary}");
        assert_eq!(summary["repairs"]["code_withdrawn"], 0, "{summary}");
        assert_eq!(summary["repairs"]["enqueued_for_model"], 0, "{summary}");

        let document_id = std::fs::canonicalize(&md).unwrap().display().to_string();
        let db_path = home.path().join(".prism/provenance.db");
        let store = prism_provenance::ProvenanceStore::open(&db_path)
            .await
            .expect("the store Phase 1 wrote must open");
        assert_eq!(
            store.pending_repairs(&document_id, 10).await.unwrap().len(),
            0,
            "a representable relation must not become repair debt"
        );

        let repair_summary = run_local_repair_pass(
            &md,
            root,
            Some("test-repairer"),
            Some(&ingest_server.url()),
            None,
            "http://192.0.2.1:1",
        )
        .await
        .expect("an empty repair queue is not an error");
        assert_eq!(repair_summary["backend"], "local_repair");
        assert_eq!(repair_summary["items_seen"], 0, "{repair_summary}");
        assert_eq!(repair_summary["accepted"], 0, "{repair_summary}");
        assert_eq!(repair_summary["withdrawn"], 0, "{repair_summary}");
        assert_eq!(repair_summary["model_calls"], 0, "{repair_summary}");
        assert!(
            repair_summary["errors"].as_array().unwrap().is_empty(),
            "{repair_summary}"
        );
    }

    /// `--repair` parses as an ingest flag carrying the document, and it
    /// refuses the combinations that make no sense — naming them, not
    /// silently picking one meaning.
    #[test]
    fn repair_flag_parses_and_conflicts_are_named() {
        let cli = Cli::try_parse_from(["prism", "ingest", "--repair", "/tmp/paper.pdf"]).unwrap();
        match cli.command {
            Some(Commands::Ingest { path, repair, .. }) => {
                assert!(repair);
                assert_eq!(path.unwrap(), PathBuf::from("/tmp/paper.pdf"));
            }
            _ => panic!("expected Ingest command"),
        }
        // --repair parses without a path (the runtime handler names the
        // missing document); the flag itself must not require it.
        let cli = Cli::try_parse_from(["prism", "ingest", "--repair"]).unwrap();
        match cli.command {
            Some(Commands::Ingest { path, repair, .. }) => {
                assert!(repair && path.is_none());
            }
            _ => panic!("expected Ingest command"),
        }
    }

    /// CONTRACT CHANGE: `kind` is no longer a closed paper-fact switch. A
    /// value-less relation is representable and is stored as the generic edge
    /// the model proposed, with its exact citation, rather than disappearing.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn a_valueless_legacy_kind_hint_is_stored_as_a_generic_cited_fact() {
        // CONTRACT CHANGE: the old store silently discarded this shape after
        // extraction counted it. Paper `kind` is no longer a Rust dispatch
        // enum, so the cited relation is stored generically.
        let _guard = boot_checks::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let mut server = mockito::Server::new_async().await;
        // Exactly the shape that used to pass every convert_fact rule: kind
        // "measurement", value null, unit null.
        let extraction = r#"{"facts":[
            {"subject":"Ti-6Al-4V","predicate":"has_measurement","object":"UTS","value":null,"unit":null,"conditions":[],"confidence":0.9,"kind":"measurement","evidence_class":"research"}
        ]}"#;
        // The legacy `kind`/`evidence_class` fields are deliberately present:
        // the generic propose_fact tool normalizes them away.
        let _mock = server
            .mock("POST", "/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body_from_request(agentic_extraction_responder(extraction))
            .create_async()
            .await;

        let home = tempfile::tempdir().expect("home tempdir");
        std::fs::create_dir_all(home.path().join(".prism")).unwrap();
        let _restore_home = HomeGuard::isolated(home.path());

        let project = project_with_ontology_config("[ontology]\nid = \"emmo\"\n");
        let root = project.path();
        let md = root.join("vague-datasheet.md");
        std::fs::write(&md, "Ti-6Al-4V has an ultimate tensile strength.").unwrap();

        let summary = run_local_text_ingest_file(
            &md,
            root,
            Some("test-extractor"),
            Some(&server.url()),
            None,
            "http://192.0.2.1:1",
            false,
            None,
            prism_ingest::text_extract::SamplingPolicy::default(),
            VisionModelChoice::default(),
            &mut None,
        )
        .await
        .expect("a representable value-less relation must ingest cleanly");

        assert_eq!(summary["facts_written"], 1, "summary: {summary}");
        assert_eq!(summary["parse_error"], serde_json::Value::Null);
        let dropped = summary["dropped_facts"]
            .as_array()
            .expect("dropped_facts must be in the summary");
        assert!(dropped.is_empty(), "summary: {summary}");
        assert_eq!(summary["repairs"]["enqueued_for_model"], 0, "{summary}");

        let db_path = home.path().join(".prism/provenance.db");
        let store = prism_provenance::ProvenanceStore::open(&db_path)
            .await
            .expect("the store the ingest opened must open");
        let facts = store
            .recall_with_context("Ti-6Al-4V", "local", 10)
            .await
            .unwrap();
        assert_eq!(facts.len(), 1, "generic fact was not stored: {facts:?}");
        assert_eq!(facts[0].predicate, "has_measurement");
        let evidence = store
            .assertion_evidence("local", "Ti-6Al-4V", "has_measurement", "UTS")
            .await
            .unwrap();
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].line_start, Some(1));
    }

    /// THE funnel invariant: the counters PARTITION `facts_proposed`.
    /// Any fact leaving the pipeline without incrementing exactly one bucket
    /// breaks this — which is the point: an unattributed loss is how "58
    /// extracted, 4 stored" stayed an anecdote. (Annotate-not-refuse folded
    /// the old per-check drop buckets into `stored_unverified` — a failed
    /// check stores the fact under a weak status — so the destinations are
    /// now: trusted, unverified, malformed, duplicate, store-failed. The
    /// stored halves must also sum to `facts_written`, and the per-status
    /// breakdown must sum to `stored_unverified`.)
    fn assert_funnel_partitions(funnel: &serde_json::Value) {
        let count = |key: &str| {
            funnel[key]
                .as_u64()
                .unwrap_or_else(|| panic!("funnel[{key}] must be a count: {funnel}"))
        };
        assert_eq!(
            count("facts_proposed"),
            count("stored_trusted")
                + count("stored_unverified")
                + count("dropped_malformed")
                + count("deduped")
                + count("store_failed"),
            "the funnel must partition every proposed fact: {funnel}"
        );
        assert_eq!(
            count("facts_written"),
            count("stored_trusted") + count("stored_unverified"),
            "every written fact is either trusted or explicitly unverified: {funnel}"
        );
        let by_status: u64 = funnel["unverified_by_status"]
            .as_object()
            .expect("unverified_by_status must be a map")
            .values()
            .map(|value| value.as_u64().expect("status counts are counts"))
            .sum();
        assert_eq!(
            by_status,
            count("stored_unverified"),
            "the per-status breakdown must account for every unverified fact: {funnel}"
        );
    }

    /// CONTRACT CHANGE: paper population no longer follows the reading loop
    /// with an orphan one-shot alias prompt. Names remain exactly as proposed;
    /// an alias must come through ontology navigation or an explicit fact.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn paper_population_does_not_run_a_second_alias_prompt() {
        // CONTRACT CHANGE: the reader's proposed identities go directly to
        // cited storage. This test now proves that no deleted alias-model
        // pass is invoked after the tool loop.
        let _guard = boot_checks::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let mut server = mockito::Server::new_async().await;
        let extraction = r#"{"facts":[
            {"subject":"Ti-6Al-4V","predicate":"has_measurement","object":"UTS","value":880.0,"unit":"MPa","conditions":[],"confidence":0.9,"kind":"measurement","evidence_class":"research"},
            {"subject":"Ti64","predicate":"has_measurement","object":"density","value":4.43,"unit":"g/cm3","conditions":[],"confidence":0.9,"kind":"measurement","evidence_class":"research"},
            {"subject":"IN718","predicate":"has_measurement","object":"UTS","value":1100.0,"unit":"MPa","conditions":[],"confidence":0.9,"kind":"measurement","evidence_class":"research"},
            {"subject":"IN625","predicate":"has_measurement","object":"UTS","value":900.0,"unit":"MPa","conditions":[],"confidence":0.9,"kind":"measurement","evidence_class":"research"}
        ]}"#;
        // CONTRACT CHANGE (agentic paper reading): this mock returns the
        // reader's tool calls, including exact citation coordinates.
        let extraction_mock = server
            .mock("POST", "/chat/completions")
            .match_body(mockito::Matcher::Regex(
                "Use the tools to read the paper".into(),
            ))
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body_from_request(agentic_extraction_responder(extraction))
            .expect(2)
            .create_async()
            .await;

        let home = tempfile::tempdir().expect("home tempdir");
        std::fs::create_dir_all(home.path().join(".prism")).unwrap();
        let _restore_home = HomeGuard::isolated(home.path());

        let project = project_with_ontology_config(
            "[ontology]\nid = \"emmo\"\n\n[ingest]\nchunk_bytes = 4096\n",
        );
        let root = project.path();
        let md = root.join("alias-paper.md");
        std::fs::write(
            &md,
            "Ti-6Al-4V (Ti64): UTS 880 MPa. Ti64 density 4.43 g/cm3. \
             IN718 UTS 1100 MPa while IN625 UTS 900 MPa.",
        )
        .unwrap();

        let summary = run_local_text_ingest_file(
            &md,
            root,
            Some("test-extractor"),
            Some(&server.url()),
            None,
            "http://192.0.2.1:1",
            false,
            None,
            prism_ingest::text_extract::SamplingPolicy::default(),
            VisionModelChoice::default(),
            &mut None,
        )
        .await
        .expect("the alias-bearing document must ingest");

        extraction_mock.assert_async().await;

        // All four extraction facts landed, and the funnel partitions them.
        assert_eq!(summary["facts_written"], 4, "summary: {summary}");
        let funnel = &summary["funnel"];
        assert_eq!(funnel["facts_proposed"], 4, "summary: {summary}");
        assert_eq!(funnel["facts_written"], 4, "summary: {summary}");
        assert_funnel_partitions(funnel);

        assert!(
            summary.get("alias").is_none(),
            "orphan alias pass ran: {summary}"
        );
        assert_eq!(ingest_summary_errors(&summary), 0, "summary: {summary}");

        let db_path = home.path().join(".prism/provenance.db");
        let store = prism_provenance::ProvenanceStore::open(&db_path)
            .await
            .unwrap();
        for name in ["Ti-6Al-4V", "Ti64", "IN718", "IN625"] {
            let facts = store.recall_with_context(name, "local", 10).await.unwrap();
            assert!(
                facts.iter().all(|fact| fact.predicate != "same_as"),
                "a second prompt invented an alias edge: {facts:?}"
            );
        }
    }

    // ── Whole-document chunking through the PRODUCTION text path ───────
    //
    // The defect this replaces: `MAX_PROMPT_TEXT_BYTES = 60_000` truncated
    // every document with NO chunking — a 4.1 MB NASA deck (~362K chars of
    // text) was read to its first ~60K and the rest never seen.

    /// A document text laid out so `[ingest] chunk_bytes = 2000` windows it
    /// into exactly 3 chunks (0-2000, 1488-3488, 2976-end), with ONE unique
    /// marker per window — so each extraction/review mock can be keyed to a
    /// chunk and prompt role without overlapping another request.
    /// A document that spans three windows AND actually states the facts the
    /// stub extractor returns.
    ///
    /// The subjects are named in the text on purpose: extraction now drops
    /// facts the source does not support (a model that invents a material
    /// must not be able to write it into the graph), so a fixture whose
    /// document never mentions `EarlyFactium` would exercise the drop path
    /// instead of the windowing and merge behaviour these tests are about.
    ///
    /// `EarlyFactium` is stated ONLY in the first window. The last window's
    /// stub reply still re-asserts it (the realistic overlap shape), which
    /// survives precisely because grounding consults the DOCUMENT, not the
    /// chunk — narrowing grounding back to the chunk drops it and fails the
    /// merge test's "no drops" assertion. It also keeps each marker's
    /// sentence free of the other windows' subjects, so the review-request
    /// mocks can be keyed on markers without cross-matching document-wide
    /// evidence spans.
    fn three_chunk_text() -> String {
        let filler = |n: usize| "filler sentence about processing. ".repeat(n);
        let mut text =
            String::from("AAAMARKER EarlyFactium and Survivium exhibit the alpha phase. ");
        text.push_str(&filler(70)); // MIDMARKER lands ~byte 2390: window 2 only
        text.push_str("MIDMARKER ");
        text.push_str(&filler(50)); // ZZZMARKER lands ~byte 4030: window 3 only
        text.push_str("ZZZMARKER LateFactium exhibits the omega phase.");
        assert!(text.len() > 4_000, "fixture must span three windows");
        text
    }

    /// One OpenAI-shaped chat body whose content is `facts_json`.
    fn chat_body(facts_json: &str) -> String {
        serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": facts_json}}]
        })
        .to_string()
    }

    /// Dynamic OpenAI SSE responder for a two-turn paper-agent fixture. The
    /// first request reads line 1; only the next request proposes facts from
    /// that returned line and finishes. This pins the real agent contract: a
    /// citation cannot be manufactured beside an unread sibling tool call.
    fn agentic_extraction_responder(
        facts_json: &str,
    ) -> impl Fn(&mockito::Request) -> Vec<u8> + Send + Sync + 'static {
        agentic_extraction_responder_citing(facts_json, 1)
    }

    /// [`agentic_extraction_responder`] with the cited line as a parameter,
    /// for fixtures whose evidence does not sit on line 1 — a PDF's
    /// extracted text starts wherever `pdf-extract` puts it, and a citation
    /// quoting an empty line is (correctly) dropped as malformed.
    fn agentic_extraction_responder_citing(
        facts_json: &str,
        cited_line: u32,
    ) -> impl Fn(&mockito::Request) -> Vec<u8> + Send + Sync + 'static {
        let envelope: serde_json::Value = serde_json::from_str(facts_json).expect("facts fixture");
        let facts = envelope["facts"]
            .as_array()
            .expect("facts fixture array")
            .clone();
        let read_event = serde_json::json!({
            "choices": [{"delta": {"tool_calls": [{
                "index": 0,
                "id": "read-1",
                "type": "function",
                "function": {
                    "name": "read_paper",
                    "arguments": serde_json::json!({"from_line": cited_line, "to_line": cited_line}).to_string(),
                }
            }]}}],
            "usage": {"prompt_tokens": 5, "completion_tokens": 5, "total_tokens": 10},
        });
        let read_body = format!("data: {read_event}\n\ndata: [DONE]\n\n");
        let mut calls = Vec::new();
        for (offset, fact) in facts.iter().enumerate() {
            calls.push(serde_json::json!({
                "index": offset,
                "id": format!("fact-{offset}"),
                "type": "function",
                "function": {
                    "name": "propose_fact",
                    "arguments": serde_json::json!({
                        "fact": fact,
                        "from_line": cited_line,
                        "to_line": cited_line,
                    }).to_string(),
                }
            }));
        }
        calls.push(serde_json::json!({
            "index": calls.len(),
            "id": "finish-1",
            "type": "function",
            "function": {"name": "finish", "arguments": "{}"},
        }));
        let proposal_event = serde_json::json!({
            "choices": [{"delta": {"tool_calls": calls}}],
            "usage": {"prompt_tokens": 5, "completion_tokens": 5, "total_tokens": 10},
        });
        let proposal_body = format!("data: {proposal_event}\n\ndata: [DONE]\n\n");
        move |request: &mockito::Request| {
            let has_tool_result = request
                .body()
                .ok()
                .and_then(|body| serde_json::from_slice::<serde_json::Value>(body).ok())
                .and_then(|body| {
                    body.get("messages")
                        .and_then(serde_json::Value::as_array)
                        .cloned()
                })
                .is_some_and(|messages| {
                    messages
                        .iter()
                        .any(|message| message["role"].as_str() == Some("tool"))
                });
            if has_tool_result {
                proposal_body.as_bytes().to_vec()
            } else {
                read_body.as_bytes().to_vec()
            }
        }
    }

    /// Paper text is no longer copied into the initial prompt. Chunk mocks
    /// therefore key on the source-revision hash advertised in metadata.
    fn agent_request_for_document(document: &str) -> mockito::Matcher {
        use sha2::{Digest as _, Sha256};
        let revision = Sha256::digest(document.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        mockito::Matcher::AllOf(vec![
            mockito::Matcher::Regex("Use the tools to read the paper".into()),
            mockito::Matcher::Regex(revision),
        ])
    }

    /// CONTRACT CHANGE: the text path used to put every ontology's facts in
    /// the bare `local` tenant. A promoted ontology must instead use the same
    /// composed tenant as tabular ingest, or two vocabularies blend in one
    /// keyspace. Restoring the old literal makes the scoped read below empty
    /// and the bare-tenant read non-empty.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn text_ingest_scopes_facts_to_the_promoted_ontology_tenant() {
        let _guard = boot_checks::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let ontology_id = "tenant-custom";
        let project =
            project_with_ontology_config(&format!("[ontology]\nid = \"{ontology_id}\"\n"));
        let root = project.path();
        let candidate = root.join("tenant-custom-candidate.ttl");
        let draft = prism_ingest::induction::InducedOntology {
            domain: ontology_id.to_string(),
            status: prism_ingest::induction::OntologyStatus::Draft,
            classes: vec![prism_ingest::induction::InducedClass {
                label: "NeutralEntity".to_string(),
                definition: "A neutral test concept.".to_string(),
                parent: None,
                aligned_iri: None,
                declared_by_reference: false,
                sign_domain: None,
            }],
            relations: Vec::new(),
            provenance: prism_ingest::induction::InductionProvenance {
                corpus_hash: "sha256:text-tenant-isolation-test".to_string(),
                prompt_version: "test".to_string(),
                ..Default::default()
            },
        };
        prism_ingest::induction::ttl::write_artifact(&candidate, &draft)
            .expect("write tenant-isolation ontology artifact");
        let promoted = prism_ingest::induction::ttl::promote_artifact(&candidate)
            .expect("promote tenant-isolation ontology artifact");
        crate::ontology_cmd::install_promoted_artifact(root, &promoted)
            .expect("install tenant-isolation ontology artifact");

        let text = "ScopeAlpha links ScopeBeta.";
        let document = root.join("tenant-scope.md");
        std::fs::write(&document, text).unwrap();
        let proposals = r#"{"facts":[
            {"subject":"ScopeAlpha","predicate":"links","object":"ScopeBeta","conditions":[],"confidence":0.9}
        ]}"#;
        let mut server = mockito::Server::new_async().await;
        let extraction = server
            .mock("POST", "/chat/completions")
            .match_body(agent_request_for_document(text))
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body_from_request(agentic_extraction_responder(proposals))
            .expect(2)
            .create_async()
            .await;

        let home = tempfile::tempdir().expect("home tempdir");
        std::fs::create_dir_all(home.path().join(".prism")).unwrap();
        let _restore_home = HomeGuard::isolated(home.path());
        let summary = run_local_text_ingest_file(
            &document,
            root,
            Some("test-extractor"),
            Some(&server.url()),
            None,
            "http://192.0.2.1:1",
            false,
            None,
            prism_ingest::text_extract::SamplingPolicy::default(),
            VisionModelChoice::default(),
            &mut None,
        )
        .await
        .expect("the promoted ontology's text fact must ingest");
        extraction.assert_async().await;
        assert_eq!(summary["facts_written"], 1, "{summary}");

        let db_path = home.path().join(".prism/provenance.db");
        let store = prism_provenance::ProvenanceStore::open(&db_path)
            .await
            .unwrap();
        let scoped_tenant =
            prism_ingest::ontologies::storage_tenant(prism_provenance::LOCAL_TENANT, ontology_id);
        assert_eq!(scoped_tenant, "local@tenant-custom");
        let scoped = store
            .recall_with_context("ScopeAlpha", &scoped_tenant, 10)
            .await
            .unwrap();
        assert_eq!(scoped.len(), 1, "scoped tenant missed its fact: {scoped:?}");
        let bare = store
            .recall_with_context("ScopeAlpha", prism_provenance::LOCAL_TENANT, 10)
            .await
            .unwrap();
        assert!(
            bare.is_empty(),
            "custom ontology leaked into local: {bare:?}"
        );
    }

    /// Dynamic OpenAI SSE responder for a two-turn paper-agent fixture whose
    /// SECOND turn proposes an ontology CLASS and RELATION extension (with
    /// valid parents/endpoints against the active ontology) and finishes.
    /// Cites the line the first turn read, exactly as the citation gate
    /// requires.
    fn agentic_extension_responder(
        parent_iri: &str,
    ) -> impl Fn(&mockito::Request) -> Vec<u8> + Send + Sync + 'static {
        let read_event = serde_json::json!({
            "choices": [{"delta": {"tool_calls": [{
                "index": 0,
                "id": "read-1",
                "type": "function",
                "function": {
                    "name": "read_paper",
                    "arguments": serde_json::json!({"from_line": 1, "to_line": 1}).to_string(),
                }
            }]}}],
            "usage": {"prompt_tokens": 5, "completion_tokens": 5, "total_tokens": 10},
        });
        let read_body = format!("data: {read_event}\n\ndata: [DONE]\n\n");
        let calls = vec![
            serde_json::json!({
                "index": 0,
                "id": "class-1",
                "type": "function",
                "function": {
                    "name": "propose_class",
                    "arguments": serde_json::json!({
                        "label": "Feedstock Powder",
                        "parent_iris": [parent_iri],
                        "description": "Powder fed into a process.",
                        "from_line": 1,
                        "to_line": 1,
                    }).to_string(),
                }
            }),
            serde_json::json!({
                "index": 1,
                "id": "relation-1",
                "type": "function",
                "function": {
                    "name": "propose_relation",
                    "arguments": serde_json::json!({
                        "label": "processed from powder",
                        "source_class_iri": parent_iri,
                        "target_class_iri": parent_iri,
                        "from_line": 1,
                        "to_line": 1,
                    }).to_string(),
                }
            }),
            serde_json::json!({
                "index": 2,
                "id": "finish-1",
                "type": "function",
                "function": {"name": "finish", "arguments": "{}"},
            }),
        ];
        let proposal_event = serde_json::json!({
            "choices": [{"delta": {"tool_calls": calls}}],
            "usage": {"prompt_tokens": 5, "completion_tokens": 5, "total_tokens": 10},
        });
        let proposal_body = format!("data: {proposal_event}\n\ndata: [DONE]\n\n");
        move |request: &mockito::Request| {
            let has_tool_result = request
                .body()
                .ok()
                .and_then(|body| serde_json::from_slice::<serde_json::Value>(body).ok())
                .and_then(|body| {
                    body.get("messages")
                        .and_then(serde_json::Value::as_array)
                        .cloned()
                })
                .is_some_and(|messages| {
                    messages
                        .iter()
                        .any(|message| message["role"].as_str() == Some("tool"))
                });
            if has_tool_result {
                proposal_body.as_bytes().to_vec()
            } else {
                read_body.as_bytes().to_vec()
            }
        }
    }

    /// THE deliverable of the proposal-persistence patch, end-to-end through
    /// the PRODUCTION text-ingest path: a reader that proposes ontology
    /// extensions leaves them — WITH their citations — in the governance
    /// store, not only in the printed summary. Before this, a 91-paper
    /// corpus run lost every one of its 3,947 citation-backed class
    /// proposals to stdout.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn text_ingest_persists_ontology_proposals_with_citations() {
        let _guard = boot_checks::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // Same setup as the tenant-isolation test: a promoted induced
        // ontology so the proposal's parent IRI resolves against something
        // real through the production config path.
        let ontology_id = "proposal-custom";
        let project =
            project_with_ontology_config(&format!("[ontology]\nid = \"{ontology_id}\"\n"));
        let root = project.path();
        let candidate = root.join("proposal-custom-candidate.ttl");
        let draft = prism_ingest::induction::InducedOntology {
            domain: ontology_id.to_string(),
            status: prism_ingest::induction::OntologyStatus::Draft,
            classes: vec![prism_ingest::induction::InducedClass {
                label: "NeutralEntity".to_string(),
                definition: "A neutral test concept.".to_string(),
                parent: None,
                aligned_iri: None,
                declared_by_reference: false,
                sign_domain: None,
            }],
            relations: Vec::new(),
            provenance: prism_ingest::induction::InductionProvenance {
                corpus_hash: "sha256:proposal-persistence-test".to_string(),
                prompt_version: "test".to_string(),
                ..Default::default()
            },
        };
        prism_ingest::induction::ttl::write_artifact(&candidate, &draft)
            .expect("write proposal-persistence ontology artifact");
        let promoted = prism_ingest::induction::ttl::promote_artifact(&candidate)
            .expect("promote proposal-persistence ontology artifact");
        crate::ontology_cmd::install_promoted_artifact(root, &promoted)
            .expect("install proposal-persistence ontology artifact");

        let text = "Feedstock powders were sieved before use.";
        let document = root.join("proposal-source.md");
        std::fs::write(&document, text).unwrap();
        let parent_iri = format!("https://prism.mirdyne.com/ontology/{ontology_id}#NeutralEntity");
        let mut server = mockito::Server::new_async().await;
        let extraction = server
            .mock("POST", "/chat/completions")
            .match_body(agent_request_for_document(text))
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body_from_request(agentic_extension_responder(&parent_iri))
            .expect(2)
            .create_async()
            .await;

        let home = tempfile::tempdir().expect("home tempdir");
        std::fs::create_dir_all(home.path().join(".prism")).unwrap();
        let _restore_home = HomeGuard::isolated(home.path());

        let summary = run_local_text_ingest_file(
            &document,
            root,
            Some("test-extractor"),
            Some(&server.url()),
            None,
            "http://192.0.2.1:1",
            false,
            None,
            prism_ingest::text_extract::SamplingPolicy::default(),
            VisionModelChoice::default(),
            &mut None,
        )
        .await
        .expect("an extension-proposing reader must ingest");
        extraction.assert_async().await;

        // The printed surface still reports the proposals …
        assert_eq!(
            summary["ontology_extensions"]["classes"]
                .as_array()
                .unwrap()
                .len(),
            1,
            "{summary}"
        );
        // … and the durable surface now holds them, with citations.
        assert_eq!(
            summary["ontology_proposal_queue"]["enqueued"], 2,
            "one class + one relation must be counted: {summary}"
        );
        let db_path = home.path().join(".prism/provenance.db");
        let store = prism_provenance::ProvenanceStore::open(&db_path)
            .await
            .unwrap();
        let pending = store.pending_ontology_proposals(10).await.unwrap();
        assert_eq!(pending.len(), 2, "{pending:?}");
        let class = pending
            .iter()
            .find(|(item, _)| item.kind == "class")
            .expect("the class proposal is queued");
        assert_eq!(class.0.label, "Feedstock Powder");
        assert_eq!(class.1, 1, "its citation is a sighting");
        let sightings = store
            .ontology_proposal_sightings(&class.0.item_id)
            .await
            .unwrap();
        assert_eq!(sightings.len(), 1);
        let citation: serde_json::Value =
            serde_json::from_str(&sightings[0].citation_json).unwrap();
        assert_eq!(
            citation["quoted_text"], "Feedstock powders were sieved before use.",
            "the evidence must ride with the proposal: {citation}"
        );
        assert_eq!(citation["from_line"], 1);
        assert_eq!(citation["to_line"], 1);
    }

    /// The re-verification surface, driven through the production entry
    /// point `prism reverify` dispatches to (and the agent tools call):
    /// LIST finds the span-unchecked population through the production
    /// read scope, RUN affirms from the exact cited lines against a mocked
    /// judge, and the durable effect is the LEDGER — a run that printed a
    /// verdict but recorded nothing would be the evaporation failure again.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn reverify_cli_lists_runs_and_ledgers_the_verdict() {
        let _guard = boot_checks::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        std::fs::create_dir_all(home.path().join(".prism")).unwrap();
        let _restore_home = HomeGuard::isolated(home.path());

        let source_text = "Header\nThe exact cited statement.\n";
        let source = home.path().join("paper.txt");
        std::fs::write(&source, source_text).unwrap();

        let db_path = home.path().join(".prism/provenance.db");
        let store = prism_provenance::ProvenanceStore::open(&db_path)
            .await
            .unwrap();
        let citation = prism_provenance::SourceCitation::new(
            2,
            2,
            "The exact cited statement.",
            prism_retrieval::text_revision_id(source_text),
            None,
        )
        .unwrap();
        let prov = prism_provenance::LocalProvenance {
            activity_id: "activity".into(),
            agent_id: "agent".into(),
            agent_kind: "SoftwareAgent".into(),
            source_entity_id: source.display().to_string(),
            source_kind: "Document".into(),
            tenant: "local".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
            ended_at: "2026-01-01T00:00:01Z".into(),
            locality: "local".into(),
            origin_source_id: None,
            origin_action_id: None,
        };
        let ontology = prism_provenance::OntologyClassification {
            version_iri: "urn:test:ontology:v1",
            artifact_sha256: "0000000000000000000000000000000000000000000000000000000000000000",
        };
        let fact = prism_provenance::MaterialFact {
            subject: "Alloy X".into(),
            predicate: "has_measurement".into(),
            object: "UTS".into(),
            value: None,
            unit: None,
            conditions: vec![],
            confidence: Some(0.8),
            kind: Some("relation".into()),
            evidence_class: prism_provenance::EvidenceClass::Research,
            // The fresh paper path's status: trusted-but-unverified, the
            // re-verification target population.
            verification: Some(prism_provenance::VerificationStatus::CitedByReader),
            verification_reason: None,
        };
        store
            .write_fact_with_classification_and_citation(&fact, &prov, ontology, None, &citation)
            .await
            .unwrap();
        let id = prism_provenance::conditioned_assertion_id(
            "local",
            "Alloy X",
            "has_measurement",
            "UTS",
            None,
            None,
            &[],
        )
        .unwrap();

        let project = tempfile::tempdir().expect("project tempdir");
        use crate::reverify_cmd::ReverifyCommands;

        // LIST: the production read scope finds the candidate.
        crate::reverify_cmd::run(
            ReverifyCommands::List {
                status: "cited_by_reader".into(),
                limit: 10,
                json: true,
            },
            project.path(),
        )
        .await
        .expect("list must succeed over the real store");

        // An unknown status fails honestly, naming the valid spellings —
        // generated from the enum, never a hardcoded list.
        let error = crate::reverify_cmd::run(
            ReverifyCommands::List {
                status: "bogus".into(),
                limit: 10,
                json: true,
            },
            project.path(),
        )
        .await
        .expect_err("an unknown status must fail, not fall back");
        assert!(
            format!("{error:#}").contains("cited_by_reader"),
            "the error must list valid spellings: {error:#}"
        );

        // RUN: a mocked judge affirms from the exact cited lines; the
        // verdict lands in the ledger.
        let mut server = mockito::Server::new_async().await;
        let affirmation = server
            .mock("POST", "/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "choices": [{
                        "message": {
                            "content": "{\"verdict\":\"affirmed\",\"reason\":\"line 2 states it exactly\"}"
                        }
                    }]
                })
                .to_string(),
            )
            .create_async()
            .await;
        crate::reverify_cmd::run(
            ReverifyCommands::Run {
                assertion: id.clone(),
                model: Some("test-judge".into()),
                llm_url: Some(server.url()),
                api_key: None,
                json: true,
            },
            project.path(),
        )
        .await
        .expect("the re-verification run must succeed");
        affirmation.assert_async().await;
        let ledger = store.reverify_verdicts(&id).await.unwrap();
        assert_eq!(ledger.len(), 1, "{ledger:?}");
        assert_eq!(ledger[0].verdict, "affirmed");
        assert_eq!(ledger[0].reviewer, "model:test-judge");
        assert_eq!(ledger[0].reason, "line 2 states it exactly");

        // HISTORY: the audit trail reads back with no model call.
        crate::reverify_cmd::run(
            ReverifyCommands::History {
                assertion: id.clone(),
                json: true,
            },
            project.path(),
        )
        .await
        .expect("history must succeed");
    }

    /// CONTRACT CHANGE: tools make prompt windows obsolete. One bounded loop
    /// receives the complete document as a searchable workspace, so material
    /// at both ends is available without multiplying model cost or evidence.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn text_ingest_reads_the_whole_document_in_one_agent_loop() {
        // CONTRACT CHANGE: old prompt windows launched several independent
        // completions. The single bounded loop now owns one searchable paper
        // workspace and records its two turns below.
        let _guard = boot_checks::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let mut server = mockito::Server::new_async().await;
        let text = three_chunk_text();
        let proposals = r#"{"facts":[
            {"subject":"EarlyFactium","predicate":"has_phase","object":"alpha","conditions":[],"confidence":0.9},
            {"subject":"LateFactium","predicate":"has_phase","object":"omega","conditions":[],"confidence":0.9,"kind":"phase","evidence_class":"research"}
        ]}"#;
        let extraction = server
            .mock("POST", "/chat/completions")
            .match_body(agent_request_for_document(&text))
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body_from_request(agentic_extraction_responder(proposals))
            .expect(2)
            .create_async()
            .await;

        let home = tempfile::tempdir().expect("home tempdir");
        std::fs::create_dir_all(home.path().join(".prism")).unwrap();
        let _restore_home = HomeGuard::isolated(home.path());

        // The `[ingest] chunk_bytes` knob is exercised here too: unread, the
        // whole text is one window and chunks_total collapses to 1.
        let project = project_with_ontology_config(
            "[ontology]\nid = \"emmo\"\n\n[ingest]\nchunk_bytes = 2000\n",
        );
        let root = project.path();
        let md = root.join("long-deck.md");
        std::fs::write(&md, &text).unwrap();
        let expected_chunks = 1usize;

        let summary = run_local_text_ingest_file(
            &md,
            root,
            Some("test-extractor"),
            Some(&server.url()),
            None,
            "http://192.0.2.1:1",
            false,
            None,
            prism_ingest::text_extract::SamplingPolicy::default(),
            VisionModelChoice::default(),
            &mut None,
        )
        .await
        .expect("a multi-window document must ingest");

        extraction.assert_async().await;

        assert_eq!(
            summary["chunks_total"].as_u64().unwrap() as usize,
            expected_chunks,
            "every window must be planned: {summary}"
        );
        assert_eq!(
            summary["chunks_processed"], summary["chunks_total"],
            "every window must be processed: {summary}"
        );
        assert_eq!(
            summary["paper_agent"]["loops"],
            serde_json::json!(expected_chunks),
            "one bounded reader loop per extraction sample: {summary}"
        );
        assert_eq!(
            summary["paper_agent"]["turns"],
            serde_json::json!(2),
            "the fixture reads first, then proposes and finishes: {summary}"
        );
        assert!(
            summary["paper_agent"]["tool_calls"]
                .as_u64()
                .is_some_and(|count| count >= (expected_chunks * 2) as u64),
            "each loop must at least read and finish: {summary}"
        );
        assert_eq!(
            summary["ontology_extensions"]["classes"],
            serde_json::json!([]),
            "the fixture proposed no ontology extension: {summary}"
        );
        assert_eq!(
            summary["facts_written"], 2,
            "the duplicated fact must merge, not double: {summary}"
        );
        // One loop proposed both endpoint facts and wrote both.
        let funnel = &summary["funnel"];
        assert_eq!(funnel["facts_proposed"], 2, "summary: {summary}");
        assert_eq!(funnel["deduped"], 0, "summary: {summary}");
        assert_eq!(funnel["facts_written"], 2, "summary: {summary}");
        assert_funnel_partitions(funnel);
        assert_eq!(summary["errors"].as_array().unwrap().len(), 0);
        assert_eq!(
            summary["dropped_facts"].as_array().unwrap().len(),
            0,
            "every fixture fact must reach the merge path: {summary}"
        );
        assert_eq!(ingest_summary_errors(&summary), 0);

        let db_path = home.path().join(".prism/provenance.db");
        let store = prism_provenance::ProvenanceStore::open(&db_path)
            .await
            .unwrap();
        // THE deliverable: a fact from the END of the document is in the
        // store. Under the old 60K front-truncation (or any single-window
        // regression) the last window's text was never read.
        let late = store
            .recall_with_context("LateFactium", "local", 10)
            .await
            .unwrap();
        assert_eq!(
            late.len(),
            1,
            "the end-of-document fact must land: {late:?}"
        );
        // One document contributes one exact evidence witness.
        let evidence = store
            .assertion_evidence("local", "EarlyFactium", "has_phase", "alpha")
            .await
            .unwrap();
        assert_eq!(
            evidence.len(),
            1,
            "one document must contribute one evidence row: {evidence:?}"
        );
        // CONTRACT CHANGE (agentic paper reading): that source contribution
        // now retains the exact raw line witness and full-document revision
        // selected by the paper agent.
        assert_eq!(evidence[0].line_start, Some(1));
        assert_eq!(evidence[0].line_end, Some(1));
        assert!(
            evidence[0]
                .evidence_span
                .as_deref()
                .is_some_and(|span| span.contains("EarlyFactium")),
            "agent citation was not persisted: {evidence:?}"
        );
        use sha2::{Digest as _, Sha256};
        let expected_revision = Sha256::digest(text.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(
            evidence[0].source_revision_id.as_deref(),
            Some(expected_revision.as_str())
        );
        // CONTRACT CHANGE (exact-source rereading): the reopen locator is an
        // immutable snapshot of the complete text representation, never a
        // per-window fragment and never binary PDF bytes. Independence still
        // keys on the original canonical document path via origin_source_id,
        // so chunk boundaries and text snapshots cannot fabricate support.
        let snapshot = summary["source_text_snapshot"]
            .as_str()
            .expect("summary reports the exact reopenable source text");
        assert_eq!(
            evidence[0].source_entity_id, snapshot,
            "evidence must reopen the exact full text snapshot"
        );
        assert_eq!(std::fs::read_to_string(snapshot).unwrap(), text);
        assert_eq!(
            evidence[0].source_key,
            format!("file:{}", std::fs::canonicalize(&md).unwrap().display()),
            "corroboration must remain keyed on the original document"
        );
        let early = store
            .recall_with_context("EarlyFactium", "local", 10)
            .await
            .unwrap();
        assert_eq!(early.len(), 1);
        assert!(
            (early[0].confidence - 0.9).abs() < 1e-9,
            "one paper read changed the proposed confidence: {}",
            early[0].confidence
        );
    }

    /// CONTRACT CHANGE: there is one whole-document loop rather than several
    /// independent chunk loops. A provider failure is reported against that
    /// one loop and cannot masquerade as partial paper coverage.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn text_ingest_reports_a_failed_document_loop_without_partial_facts() {
        // CONTRACT CHANGE: there is no later prompt chunk that can succeed
        // after this provider failure; coverage honestly reports one failed
        // whole-document loop and no partial paper result.
        let _guard = boot_checks::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let mut server = mockito::Server::new_async().await;
        let text = three_chunk_text();
        let failed_loop = server
            .mock("POST", "/chat/completions")
            .match_body(agent_request_for_document(&text))
            .with_status(400)
            .with_body("boom")
            .expect(1)
            .create_async()
            .await;

        let home = tempfile::tempdir().expect("home tempdir");
        std::fs::create_dir_all(home.path().join(".prism")).unwrap();
        let _restore_home = HomeGuard::isolated(home.path());

        let project = project_with_ontology_config(
            "[ontology]\nid = \"emmo\"\n\n[ingest]\nchunk_bytes = 2000\n",
        );
        let root = project.path();
        let md = root.join("long-deck.md");
        std::fs::write(&md, &text).unwrap();
        let summary = run_local_text_ingest_file(
            &md,
            root,
            Some("test-extractor"),
            Some(&server.url()),
            None,
            "http://192.0.2.1:1",
            false,
            None,
            prism_ingest::text_extract::SamplingPolicy::default(),
            VisionModelChoice::default(),
            &mut None,
        )
        .await
        .expect("a failed loop is a reported result, not a crash");

        failed_loop.assert_async().await;

        let errors = summary["errors"].as_array().unwrap();
        assert_eq!(errors.len(), 1, "{summary}");
        let error = errors[0].as_str().unwrap();
        assert!(
            error.contains("chunk 1/1"),
            "the failure must name the loop: {error}"
        );
        assert_eq!(summary["chunks_total"], 1, "{summary}");
        assert_eq!(summary["chunks_processed"], 0, "{summary}");
        assert_eq!(summary["facts_written"], 0, "{summary}");
        // The exit-code spine sees the failure…
        assert_eq!(ingest_summary_errors(&summary), 1);
        // …and the coverage line says it.
        let report = chunk_coverage_report(&summary).expect("chunk fields present");
        assert!(report.contains("FAILED"), "{report}");

        // No fictional partial coverage or endpoint survives a failed loop.
        let db_path = home.path().join(".prism/provenance.db");
        let store = prism_provenance::ProvenanceStore::open(&db_path)
            .await
            .unwrap();
        let survivor = store
            .recall_with_context("Survivium", "local", 10)
            .await
            .unwrap();
        assert!(
            survivor.is_empty(),
            "failed loop stored a fact: {survivor:?}"
        );
    }

    /// The `[ingest] batch_rows` knob reaches the tabular pipeline through
    /// the production entry point (`run_local_ingest_file`): 2 rows with
    /// `batch_rows = 1` must produce exactly 2 extraction calls, full row
    /// coverage, and per-batch accounting in the summary.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn ingest_batch_rows_knob_reaches_the_pipeline() {
        let _guard = boot_checks::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let mut server = mockito::Server::new_async().await;
        let extraction = r#"{"entities":[{"type":"Alloy","name":"Knobium","properties":{}}],"relationships":[]}"#;
        let calls = server
            .mock("POST", "/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(chat_body(extraction))
            .expect(2)
            .create_async()
            .await;

        let home = tempfile::tempdir().expect("home tempdir");
        std::fs::create_dir_all(home.path().join(".prism")).unwrap();
        let _restore_home = HomeGuard::isolated(home.path());

        let project =
            project_with_ontology_config("[ontology]\nid = \"emmo\"\n\n[ingest]\nbatch_rows = 1\n");
        let root = project.path();
        let csv = root.join("alloys.csv");
        std::fs::write(&csv, "alloy,uts\nrow_one,900\nrow_two,901\n").unwrap();

        let summary = run_local_ingest_file(
            &csv,
            root,
            Some("test-extractor"),
            Some(&server.url()),
            None,
            false,
            None,
        )
        .await
        .expect("the batched tabular ingest must run");

        // Exactly one extraction call per configured batch.
        calls.assert_async().await;
        let result = &summary["result"];
        assert_eq!(result["batches"], 2, "{summary}");
        assert_eq!(result["rows_processed"], 2, "{summary}");
        assert_eq!(result["row_count"], 2);
        assert_eq!(ingest_summary_errors(&summary), 0);
        // The coverage line the printer shows is derived from these fields.
        let report = row_coverage_report(result).expect("extraction ran");
        assert!(report.contains("2 of 2 row(s)"), "{report}");
        assert!(report.contains("2 batch(es)"), "{report}");
    }
}
