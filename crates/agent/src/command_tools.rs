use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use prism_ingest::llm::ToolDefinition;
use prism_workflows::{
    WorkflowRunResult, WorkflowSpec, discover_workflows, execute_workflow_with_policy,
    find_workflow, parse_workflow_command_args,
};
use serde_json::{Value, json};
use tokio::process::Command as TokioCommand;
use tokio::time::timeout;

use crate::permissions::PermissionMode;
use crate::tool_catalog::LoadedTool;

#[derive(Debug, Clone, Default)]
pub struct CommandToolRuntime {
    pub current_exe: PathBuf,
    pub project_root: PathBuf,
    pub python_bin: PathBuf,
    /// Resolved LLM endpoint for this agent process (from `build_llm_config` —
    /// the SAME config the chat path uses). Injected into a workflow's context
    /// as `llm_base_url` so `llm_*` steps reach the real model instead of the
    /// engine's built-in localhost default. `None` when unresolved (falls back
    /// to the workflow's own env resolution).
    pub llm_base_url: Option<String>,
    /// Resolved model id, injected into workflow context as `llm_model`.
    pub llm_model: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandToolKind {
    RootArgs,
    /// An umbrella root (`prism <root> ...`) whose first argument is one of a
    /// known, closed set of subcommands. This is the typed form of RootArgs:
    /// the model picks a real verb via the `subcommand` enum, then passes any
    /// verb-specific tokens via `args`. Execution prepends the subcommand.
    /// (TOOL_SURFACE_SPEC §1.1.3 — replaces the generic args:array<string>.)
    RootSubcommand {
        subcommands: &'static [&'static str],
    },
    QueryLocal,
    QueryPlatform,
    QueryFederated,
    JobStatusLookup,
    WorkflowList,
    WorkflowShow,
    WorkflowRun,
    MarketplaceSearch,
    MarketplaceInfo,
    MarketplaceInstall,
    MarketplaceFind,
    IngestFile,
    IngestWatch,
    ResearchQuery,
    ModelsList,
    ModelsSearch,
    ModelsInfo,
    DeployList,
    DeployStatus,
    DeployHealth,
    DeployCreate,
    DeployStop,
    DiscourseList,
    DiscourseCreate,
    DiscourseShow,
    DiscourseRun,
    DiscourseStatus,
    DiscourseTurns,
    NodeProbe,
    NodeStatus,
    NodeLogs,
    MeshDiscover,
    MeshHealth,
    MeshPeers,
    MeshSubscriptions,
    MeshPublish,
    MeshSubscribe,
    MeshUnsubscribe,
    RunSubmit,
    PublishArtifact,
    ComputeGpus,
    ComputeProviders,
    ComputeEstimate,
    ComputeStatus,
    ComputeCancel,
    ComputeSubmit,
    Predict,
    GoalStart,
    GoalStatus,
    GoalList,
    GoalResume,
    KnowledgeEntity,
    KnowledgePaths,
    KnowledgeCorpora,
    KnowledgeIngest,
    BillingBalance,
    BillingUsage,
    BillingHistory,
    BillingPrices,
    // ── Self-bug-report (reuses `prism report` machinery) ─────────────
    ReportBug,
    // ── In-app notebook kernel (crate::notebook) ──────────────────────
    NotebookExec,
    NotebookStatus,
    NotebookReset,
}

#[derive(Debug, Clone, Copy)]
struct CommandToolSpec {
    name: &'static str,
    root: &'static str,
    aliases: &'static [&'static str],
    kind: CommandToolKind,
    description: &'static str,
    permission_mode: PermissionMode,
    requires_approval: bool,
}

const COMMAND_TOOLS: &[CommandToolSpec] = &[
    CommandToolSpec {
        name: "status",
        root: "status",
        aliases: &["prism_status"],
        kind: CommandToolKind::RootArgs,
        description: "Run `prism status ...` through PRISM's Rust CLI. Pass one CLI argument per entry in `args`, not a shell string.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "tools",
        root: "tools",
        aliases: &["prism_tools"],
        kind: CommandToolKind::RootArgs,
        description: "Run `prism tools ...` through PRISM's Rust CLI. Use this when you need PRISM's own tool inventory or diagnostics.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "doctor",
        root: "doctor",
        aliases: &["prism_doctor"],
        kind: CommandToolKind::RootArgs,
        description: "Run `prism doctor` — a full runtime diagnostic: local setup (binaries, models, venv, credentials) plus platform connectivity (auth, knowledge graph, models, compute, marketplace, local node, policy engine). Use this first when something feels broken before guessing at a fix.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "query",
        root: "query",
        aliases: &["prism_query"],
        kind: CommandToolKind::RootArgs,
        description: "Run `prism query ...` for PRISM-native search and knowledge queries. Put each CLI argument in `args`; a query with spaces should stay one array element.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "query_local",
        root: "query",
        aliases: &[],
        kind: CommandToolKind::QueryLocal,
        description: "Query the local PRISM knowledge graph with typed fields instead of manual CLI args. Use `semantic=true` for vector search, or plain text for graph-neighbor lookup.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "query_platform",
        root: "query",
        aliases: &[],
        kind: CommandToolKind::QueryPlatform,
        description: "Search the MARC27 platform's OWN knowledge base — embedded corpora (NASA propulsion technical reports, Materials Project, JARVIS-DFT, MatKG, alloy/superalloy datasheets, additive-manufacturing and fatigue datasets) plus the materials knowledge graph. PREFER this before external literature searches (prior_art_search/web) for materials, alloy, propulsion, and manufacturing questions — the platform often already holds the answer with provenance. Plain text runs a graph-entity search ('find Ti-6Al-4V'); `semantic=true` searches corpus chunks by meaning ('materials for oxygen-rich preburner environments'). Use `knowledge_entity`/`knowledge_paths` for one-entity neighbors or relationship paths.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "query_federated",
        root: "query",
        aliases: &[],
        kind: CommandToolKind::QueryFederated,
        description: "Query the local node and its known mesh peers through the dashboard API. Use this when a running node should fan the query out across discovered peers.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "job-status",
        root: "job-status",
        aliases: &["prism_job_status"],
        kind: CommandToolKind::RootArgs,
        description: "Run `prism job-status ...` to inspect PRISM-managed jobs. Pass structured argv tokens in `args`.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "job_status_lookup",
        root: "job-status",
        aliases: &[],
        kind: CommandToolKind::JobStatusLookup,
        description: "Inspect a PRISM compute job by UUID without constructing CLI argv manually.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "workflow",
        root: "workflow",
        aliases: &["prism_workflow"],
        kind: CommandToolKind::RootArgs,
        description: "Run `prism workflow ...` for PRISM YAML workflows with Rust discovery and OPA-aware execution. Use `args=[\"list\"]`, `args=[\"show\",\"forge\"]`, `args=[\"run\",\"forge\",\"--set\",\"paper=alpha\"]`, or alias-style args like `args=[\"forge\",\"--paper\",\"alpha\"]`.",
        permission_mode: PermissionMode::WorkspaceWrite,
        requires_approval: true,
    },
    CommandToolSpec {
        name: "workflow_list",
        root: "workflow",
        aliases: &[],
        kind: CommandToolKind::WorkflowList,
        description: "List PRISM YAML workflows discovered from built-ins, project `.prism/workflows`, and user workflow directories.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "workflow_show",
        root: "workflow",
        aliases: &[],
        kind: CommandToolKind::WorkflowShow,
        description: "Show one PRISM workflow spec, including arguments, source path, and summary. Provide the workflow `name`.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "workflow_run",
        root: "workflow",
        aliases: &[],
        kind: CommandToolKind::WorkflowRun,
        description: "Run or dry-run a PRISM YAML workflow with typed inputs. Provide the workflow `name`, optional `values`, and set `execute=true` for real execution. This path stays OPA-aware.",
        permission_mode: PermissionMode::WorkspaceWrite,
        requires_approval: true,
    },
    CommandToolSpec {
        name: "marketplace",
        root: "marketplace",
        aliases: &["prism_marketplace"],
        kind: CommandToolKind::RootSubcommand {
            subcommands: &["search", "install", "info", "find", "update"],
        },
        description: "Run `prism marketplace <subcommand>` for marketplace resources (workflows, tools, models). Prefer the typed siblings marketplace_search / marketplace_find / marketplace_info / marketplace_install for those verbs; this umbrella covers `update` and any verb without a typed tool. Returns the CLI output (list, details, or install result).",
        permission_mode: PermissionMode::WorkspaceWrite,
        requires_approval: true,
    },
    CommandToolSpec {
        name: "marketplace_search",
        root: "marketplace",
        aliases: &[],
        kind: CommandToolKind::MarketplaceSearch,
        description: "Search the MARC27 marketplace for tools and workflows. Use this before installation when you need downloadable workflow definitions.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "marketplace_info",
        root: "marketplace",
        aliases: &[],
        kind: CommandToolKind::MarketplaceInfo,
        description: "Show detailed marketplace metadata for a named tool or workflow before installation.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "marketplace_install",
        root: "marketplace",
        aliases: &[],
        kind: CommandToolKind::MarketplaceInstall,
        description: "Install a marketplace tool or workflow into the local PRISM environment. Set `workflow=true` to install a YAML workflow instead of a Python tool.",
        permission_mode: PermissionMode::WorkspaceWrite,
        requires_approval: true,
    },
    CommandToolSpec {
        name: "marketplace_find",
        root: "marketplace",
        aliases: &[],
        kind: CommandToolKind::MarketplaceFind,
        description: "Semantic discovery over the marketplace — find tools/models/datasets by what they do, not by exact name (RBAC-aware cosine search). Use this when marketplace_search's lexical match comes up empty; the marketplace has a long tail (custom predictors, vendor MCPs, user-uploaded skills) not worth listing in the prompt. Optionally restrict by `types` (resource_type values).",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "ingest",
        root: "ingest",
        aliases: &["prism_ingest"],
        kind: CommandToolKind::RootArgs,
        description: "Run `prism ingest ...` for PRISM's unified ingest pipeline. Use this for CSV/Parquet local ingest, PDF/text-like file ingest into the platform knowledge stack, watch mode, and ingest status checks instead of inventing shell glue.",
        permission_mode: PermissionMode::WorkspaceWrite,
        requires_approval: true,
    },
    CommandToolSpec {
        name: "ingest_file",
        root: "ingest",
        aliases: &[],
        kind: CommandToolKind::IngestFile,
        description: "Ingest a specific file or path into PRISM's Rust ingest pipeline using typed flags instead of manual CLI assembly.",
        permission_mode: PermissionMode::WorkspaceWrite,
        requires_approval: true,
    },
    CommandToolSpec {
        name: "ingest_watch",
        root: "ingest",
        aliases: &[],
        kind: CommandToolKind::IngestWatch,
        description: "Watch a directory and continuously ingest new or updated files into the PRISM ingest pipeline.",
        permission_mode: PermissionMode::WorkspaceWrite,
        requires_approval: true,
    },
    CommandToolSpec {
        name: "mesh",
        root: "mesh",
        aliases: &["prism_mesh"],
        kind: CommandToolKind::RootSubcommand {
            subcommands: &[
                "discover",
                "peers",
                "publish",
                "subscribe",
                "unsubscribe",
                "subscriptions",
                "health",
            ],
        },
        description: "Run `prism mesh <subcommand>` for PRISM mesh operations. Prefer the typed siblings mesh_discover / mesh_health / mesh_peers / mesh_subscriptions / mesh_publish / mesh_subscribe / mesh_unsubscribe for those verbs; this umbrella exists only for any mesh verb without a typed tool. Read verbs are free; publish/subscribe mutate mesh state and are approval-gated.",
        permission_mode: PermissionMode::FullAccess,
        requires_approval: true,
    },
    CommandToolSpec {
        name: "mesh_discover",
        root: "mesh",
        aliases: &[],
        kind: CommandToolKind::MeshDiscover,
        description: "Discover PRISM mesh peers on the local network via mDNS. This is a typed read-only wrapper around `prism mesh discover`.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "mesh_health",
        root: "mesh",
        aliases: &[],
        kind: CommandToolKind::MeshHealth,
        description: "Quick health check for the mesh subsystem: online status, this node's ID, and peer count. Cheaper than mesh_peers — use this first to check the mesh is alive before listing peers or publishing. Requires a running local node (prism node up).",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "mesh_peers",
        root: "mesh",
        aliases: &[],
        kind: CommandToolKind::MeshPeers,
        description: "List peers from a running local node dashboard. Use this after bringing up a node or when investigating federated-query reachability.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "mesh_subscriptions",
        root: "mesh",
        aliases: &[],
        kind: CommandToolKind::MeshSubscriptions,
        description: "Show currently published datasets and active subscriptions from a running local node dashboard.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "mesh_publish",
        root: "mesh",
        aliases: &[],
        kind: CommandToolKind::MeshPublish,
        description: "Publish a dataset from a running local node to the mesh using typed fields instead of manual argv.",
        permission_mode: PermissionMode::FullAccess,
        requires_approval: true,
    },
    CommandToolSpec {
        name: "mesh_subscribe",
        root: "mesh",
        aliases: &[],
        kind: CommandToolKind::MeshSubscribe,
        description: "Subscribe the local node to a dataset from a remote publisher node.",
        permission_mode: PermissionMode::FullAccess,
        requires_approval: true,
    },
    CommandToolSpec {
        name: "mesh_unsubscribe",
        root: "mesh",
        aliases: &[],
        kind: CommandToolKind::MeshUnsubscribe,
        description: "Unsubscribe the local node from a previously subscribed remote dataset.",
        permission_mode: PermissionMode::FullAccess,
        requires_approval: true,
    },
    CommandToolSpec {
        name: "node",
        root: "node",
        aliases: &["prism_node"],
        kind: CommandToolKind::RootSubcommand {
            subcommands: &["up", "down", "status", "probe", "logs", "key"],
        },
        description: "Run `prism node <subcommand>` for PRISM node fabric operations. Prefer the typed siblings node_probe / node_status / node_logs for those verbs; this umbrella covers `up`/`down` (start/stop the local node daemon) and `key` (node key management), which have no typed tool. `up` starts the daemon as a supervised background child of this app (returns pid + platform node_id, no shell needed); `down` stops it gracefully (platform deregistration included). `up`/`down` change node state and are approval-gated; `status`/`probe`/`logs` are read-only.",
        permission_mode: PermissionMode::FullAccess,
        requires_approval: true,
    },
    CommandToolSpec {
        name: "node_probe",
        root: "node",
        aliases: &[],
        kind: CommandToolKind::NodeProbe,
        description: "Probe local machine capabilities without starting or registering a node.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "node_status",
        root: "node",
        aliases: &[],
        kind: CommandToolKind::NodeStatus,
        description: "Inspect the current local node daemon state through a typed wrapper.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "node_logs",
        root: "node",
        aliases: &[],
        kind: CommandToolKind::NodeLogs,
        description: "Tail logs from a managed node service such as kafka, spark, or firecrawl.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "agent",
        root: "agent",
        aliases: &["prism_agent"],
        kind: CommandToolKind::RootArgs,
        description: "Run `prism agent ...` for PRISM agent management commands. Use this for PRISM-native orchestration flows rather than `execute_bash`.",
        permission_mode: PermissionMode::FullAccess,
        requires_approval: true,
    },
    CommandToolSpec {
        name: "run",
        root: "run",
        aliases: &["prism_run"],
        kind: CommandToolKind::RootArgs,
        description: "Run `prism run ...` for PRISM execution flows. Pass one CLI argument per `args` element.",
        permission_mode: PermissionMode::FullAccess,
        requires_approval: true,
    },
    CommandToolSpec {
        name: "run_submit",
        root: "run",
        aliases: &[],
        kind: CommandToolKind::RunSubmit,
        description: "Submit a compute job with typed fields instead of manually assembling `prism run` arguments. Use this for local, MARC27, or BYOC execution backends.",
        permission_mode: PermissionMode::FullAccess,
        requires_approval: true,
    },
    CommandToolSpec {
        name: "research",
        root: "research",
        aliases: &["prism_research"],
        kind: CommandToolKind::RootArgs,
        description: "Run `prism research ...` to enter PRISM's higher-level research loop. Treat this as an orchestrated research workflow entrypoint, not a plain one-shot search command.",
        permission_mode: PermissionMode::FullAccess,
        requires_approval: true,
    },
    CommandToolSpec {
        name: "research_query",
        root: "research",
        aliases: &[],
        kind: CommandToolKind::ResearchQuery,
        description: "Start the PRISM/MARC27 research loop from a typed request body. This may trigger iterative retrieval and synthesis rather than a single search call. Supports the same platform auth path as `query`: MARC27_API_KEY if present, otherwise the logged-in PRISM session.",
        permission_mode: PermissionMode::FullAccess,
        requires_approval: true,
    },
    CommandToolSpec {
        name: "deploy",
        root: "deploy",
        aliases: &["prism_deploy"],
        kind: CommandToolKind::RootSubcommand {
            subcommands: &["create", "list", "status", "stop", "health"],
        },
        description: "Run `prism deploy <subcommand>` for PRISM deployment flows. Prefer the typed siblings deploy_list / deploy_status / deploy_health / deploy_create / deploy_stop for those verbs; this umbrella exists only for any deploy verb without a typed tool. Deployments spend compute and mutate platform state — approval-gated.",
        permission_mode: PermissionMode::FullAccess,
        requires_approval: true,
    },
    CommandToolSpec {
        name: "deploy_list",
        root: "deploy",
        aliases: &[],
        kind: CommandToolKind::DeployList,
        description: "List deployments visible to the current MARC27 auth context. Use this before status or stop when you need a deployment ID.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "deploy_status",
        root: "deploy",
        aliases: &[],
        kind: CommandToolKind::DeployStatus,
        description: "Inspect one deployment by ID and return structured deployment state.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "deploy_health",
        root: "deploy",
        aliases: &[],
        kind: CommandToolKind::DeployHealth,
        description: "Trigger and inspect one deployment health check. Use this when you need the latest health signal for a deployment.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "deploy_create",
        root: "deploy",
        aliases: &[],
        kind: CommandToolKind::DeployCreate,
        description: "Create a persistent deployment with typed fields instead of hand-building CLI args. Provide exactly one of `image` or `resource_slug`.",
        permission_mode: PermissionMode::FullAccess,
        requires_approval: true,
    },
    CommandToolSpec {
        name: "deploy_stop",
        root: "deploy",
        aliases: &[],
        kind: CommandToolKind::DeployStop,
        description: "Stop a deployment by ID. Use this for cleanup or to halt a failed deployment loop.",
        permission_mode: PermissionMode::FullAccess,
        requires_approval: true,
    },
    CommandToolSpec {
        name: "compute_gpus",
        root: "compute",
        aliases: &[],
        kind: CommandToolKind::ComputeGpus,
        description: "List purchasable GPU offers on the MARC27 compute broker (type, VRAM, region, provider, $/hr).",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "compute_providers",
        root: "compute",
        aliases: &[],
        kind: CommandToolKind::ComputeProviders,
        description: "List registered compute-broker providers/backends (PRISM mesh nodes, RunPod, Lambda, ...).",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "compute_estimate",
        root: "compute",
        aliases: &[],
        kind: CommandToolKind::ComputeEstimate,
        description: "Preview the cost of a compute-broker job WITHOUT dispatching it (free). Requires `image`.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "compute_status",
        root: "compute",
        aliases: &[],
        kind: CommandToolKind::ComputeStatus,
        description: "Poll one compute-broker job by ID; returns status, cost, and output when finished.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "compute_cancel",
        root: "compute",
        aliases: &[],
        kind: CommandToolKind::ComputeCancel,
        description: "Cancel a queued/running compute-broker job by ID (idempotent; stops further spend).",
        permission_mode: PermissionMode::FullAccess,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "compute_submit",
        root: "compute",
        aliases: &[],
        kind: CommandToolKind::ComputeSubmit,
        description: "Dispatch a real, BILLABLE containerized GPU/CPU job to the MARC27 compute broker. Provide `image` and `inputs`; set `budget_max_usd` to cap spend.",
        permission_mode: PermissionMode::FullAccess,
        requires_approval: true,
    },
    CommandToolSpec {
        name: "predict",
        root: "predict",
        aliases: &["run_model", "model_predict"],
        kind: CommandToolKind::Predict,
        description: "Run a marketplace model on the cloud in ONE call (BILLABLE): reuses a running deployment of the model or creates one (waits until ready), POSTs your inputs to it, returns the model's real result, and auto-stops anything it created. Discover models with `models_search`/`marketplace_search`. `model` = marketplace slug (e.g. 'mace-mh-1'); `task` e.g. 'single_point'|'relax'|'md'; `inputs` = the model's JSON inputs (e.g. {\"structure\": {...}}). Optional `node_id` pins a specific mesh node; default lets the platform pick.",
        permission_mode: PermissionMode::FullAccess,
        requires_approval: true,
    },
    CommandToolSpec {
        name: "goal_start",
        root: "campaign",
        aliases: &["campaign_start", "start_goal"],
        kind: CommandToolKind::GoalStart,
        description: "Start a LONG-RUNNING research goal (discovery campaign): propose → evaluate → rank loops that keep working across turns (BILLABLE — LLM + compute per iteration). The goal becomes a durable object: checkpointed to disk, visible at GET /api/goals, resumable after restarts. Use for open-ended discovery ('find a W-Mo alloy with better creep resistance'), NOT for one-shot questions (use research/search tools for those). Set `budget_usd` to cap spend and `approval_gates` to pause for human sign-off at given iterations.",
        permission_mode: PermissionMode::FullAccess,
        requires_approval: true,
    },
    CommandToolSpec {
        name: "goal_status",
        root: "campaign",
        aliases: &["campaign_status"],
        kind: CommandToolKind::GoalStatus,
        description: "Show a long-running goal's progress from its checkpoint: iteration, candidates evaluated, best-so-far, spend. Use goal_list to find ids.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "goal_list",
        root: "campaign",
        aliases: &["campaign_list", "list_goals"],
        kind: CommandToolKind::GoalList,
        description: "List all long-running goals (discovery campaigns) on this node with their ids and progress.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "goal_resume",
        root: "campaign",
        aliases: &["campaign_resume"],
        kind: CommandToolKind::GoalResume,
        description: "Resume a paused long-running goal from its checkpoint (BILLABLE — iterations continue spending). Goals pause at approval gates or on budget/iteration caps.",
        permission_mode: PermissionMode::FullAccess,
        requires_approval: true,
    },
    CommandToolSpec {
        name: "knowledge_entity",
        root: "knowledge",
        aliases: &[],
        kind: CommandToolKind::KnowledgeEntity,
        description: "Look up one entity plus its 1-hop neighbors in the MARC27 knowledge graph. Requires `name`. For plain term search use `query_platform`; for conceptual/vector search use `query_platform` with semantic=true.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "knowledge_paths",
        root: "knowledge",
        aliases: &[],
        kind: CommandToolKind::KnowledgePaths,
        description: "Find shortest hop-paths between two entities in the MARC27 knowledge graph ('how does X relate to Y?'). Requires `from_entity` and `to_entity`.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "knowledge_corpora",
        root: "knowledge",
        aliases: &[],
        kind: CommandToolKind::KnowledgeCorpora,
        description: "List available corpora from the MARC27 catalog (Materials Project, JARVIS-DFT, QMOF, MatKG, ...). Filter by `domain` or `kind`.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "knowledge_ingest",
        root: "knowledge",
        aliases: &[],
        kind: CommandToolKind::KnowledgeIngest,
        description: "Submit a background extraction job into the MARC27 knowledge graph from a `url` or free-text `query`. Entity extraction runs asynchronously server-side; poll graph growth via `ingest` --status.",
        permission_mode: PermissionMode::FullAccess,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "models",
        root: "models",
        aliases: &["prism_models"],
        kind: CommandToolKind::RootSubcommand {
            subcommands: &["list", "search", "info"],
        },
        description: "Run `prism models <subcommand>` for hosted model discovery for the active MARC27 project. Prefer the typed siblings models_list / models_search / models_info for those verbs; this umbrella exists only for any models verb without a typed tool. Read-only and free. Returns provider/model listings or details.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "models_list",
        root: "models",
        aliases: &[],
        kind: CommandToolKind::ModelsList,
        description: "List hosted LLM models for the active MARC27 project. Filter by provider when you need a narrower catalog.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "models_search",
        root: "models",
        aliases: &[],
        kind: CommandToolKind::ModelsSearch,
        description: "Search the hosted model catalog by model ID, display name, or provider using typed fields.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "models_info",
        root: "models",
        aliases: &[],
        kind: CommandToolKind::ModelsInfo,
        description: "Fetch one hosted model by exact model ID and return its structured metadata.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "discourse",
        root: "discourse",
        aliases: &["prism_discourse"],
        kind: CommandToolKind::RootSubcommand {
            subcommands: &["create", "list", "show", "run", "status", "turns"],
        },
        description: "Run `prism discourse <subcommand>` for multi-agent debate workflows backed by the platform discourse API. Prefer the typed siblings discourse_list / discourse_create / discourse_show / discourse_run / discourse_status / discourse_turns for those verbs; this umbrella exists only for any discourse verb without a typed tool. Running a discourse instance spends compute and is approval-gated; list/show/status/turns are read-only.",
        permission_mode: PermissionMode::WorkspaceWrite,
        requires_approval: true,
    },
    CommandToolSpec {
        name: "discourse_list",
        root: "discourse",
        aliases: &[],
        kind: CommandToolKind::DiscourseList,
        description: "List discourse specs for the current account. Use this to discover available multi-agent debate workflows.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "discourse_create",
        root: "discourse",
        aliases: &[],
        kind: CommandToolKind::DiscourseCreate,
        description: "Create a discourse spec from a YAML file path. Use this when a research debate workflow is already defined on disk.",
        permission_mode: PermissionMode::WorkspaceWrite,
        requires_approval: true,
    },
    CommandToolSpec {
        name: "discourse_show",
        root: "discourse",
        aliases: &[],
        kind: CommandToolKind::DiscourseShow,
        description: "Inspect one discourse spec by UUID, including stored YAML and parsed structure.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "discourse_run",
        root: "discourse",
        aliases: &[],
        kind: CommandToolKind::DiscourseRun,
        description: "Run a discourse workflow with typed parameters. After completion, use `discourse_status` for the final result and `discourse_turns` when the platform turn store is populated.",
        permission_mode: PermissionMode::WorkspaceWrite,
        requires_approval: true,
    },
    CommandToolSpec {
        name: "discourse_status",
        root: "discourse",
        aliases: &[],
        kind: CommandToolKind::DiscourseStatus,
        description: "Inspect one discourse instance by UUID and return structured status, result, cost, and turn counts.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "discourse_turns",
        root: "discourse",
        aliases: &[],
        kind: CommandToolKind::DiscourseTurns,
        description: "Fetch the stored turn list for a discourse instance by UUID.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "publish",
        root: "publish",
        aliases: &["prism_publish"],
        kind: CommandToolKind::RootArgs,
        description: "Run `prism publish ...` for PRISM publishing flows. Pass structured argv tokens in `args`.",
        permission_mode: PermissionMode::FullAccess,
        requires_approval: true,
    },
    CommandToolSpec {
        name: "publish_artifact",
        root: "publish",
        aliases: &[],
        kind: CommandToolKind::PublishArtifact,
        description: "Publish a model, dataset, or workflow artifact with typed fields instead of manual CLI argv assembly.",
        permission_mode: PermissionMode::FullAccess,
        requires_approval: true,
    },
    CommandToolSpec {
        name: "billing",
        root: "billing",
        aliases: &["prism_billing"],
        kind: CommandToolKind::RootSubcommand {
            subcommands: &["usage", "history", "prices", "topup", "balance"],
        },
        description: "Run `prism billing <subcommand>` for MARC27 credits. Prefer the typed siblings billing_balance / billing_usage / billing_history / billing_prices for the common read-only checks; use this umbrella for `topup` (opens a real Stripe checkout and spends money — approval-gated) or any billing verb without a typed tool.",
        permission_mode: PermissionMode::FullAccess,
        requires_approval: true,
    },
    CommandToolSpec {
        name: "billing_balance",
        root: "billing",
        aliases: &[],
        kind: CommandToolKind::BillingBalance,
        description: "Check the current MARC27 credits balance and dollar value for the active org. Use before starting a billable action (goal_start, predict, deploy, ...) so spend is never a surprise.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "billing_usage",
        root: "billing",
        aliases: &[],
        kind: CommandToolKind::BillingUsage,
        description: "Show credits spent this billing period, broken down by service.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "billing_history",
        root: "billing",
        aliases: &[],
        kind: CommandToolKind::BillingHistory,
        description: "Show recent MARC27 credit transactions (charges and top-ups) for the active org.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "billing_prices",
        root: "billing",
        aliases: &[],
        kind: CommandToolKind::BillingPrices,
        description: "Show the current MARC27 credit price sheet (credits per unit and markup) for every metered service.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    // ── Self-bug-report ───────────────────────────────────────────────
    // The agent's escape hatch when it hits a problem it cannot resolve on its
    // own — a broken/erroring tool, a platform error, a missing capability.
    // Reuses the existing `prism report` machinery (system-context capture +
    // file to GitHub + MARC27 support ticket). Approval-gated because it files
    // an external issue/ticket; the prompt hint tells the agent WHEN to use it.
    CommandToolSpec {
        name: "report_bug",
        root: "report",
        aliases: &["bug_report", "report_issue"],
        kind: CommandToolKind::ReportBug,
        description: "File a bug or issue report when you hit a problem you cannot resolve on your own — a tool that errors or returns broken output, a platform failure, or a missing capability. Captures system context automatically and files it (GitHub issue + MARC27 support ticket) so the team can fix it. Say what you tried and what went wrong in `description`; optionally attach a log/error file path. Use this instead of silently failing or looping on a broken tool.",
        permission_mode: PermissionMode::FullAccess,
        requires_approval: true,
    },
    // ── In-app notebook kernel ────────────────────────────────────────
    // A persistent Python kernel shared with the human's TUI notebook pane
    // (crate::notebook). Lets the agent write AND run code, then read the
    // outputs, building an analysis up cell by cell.
    CommandToolSpec {
        name: "notebook_exec",
        root: "notebook",
        aliases: &["notebook_run", "run_python_notebook"],
        kind: CommandToolKind::NotebookExec,
        description: "Execute Python in PRISM's persistent in-app notebook kernel and read the outputs (stdout, stderr, the last expression's value, and any plots). The kernel is SHARED with the human's notebook pane and KEEPS STATE across calls — variables, imports, and loaded data persist between cells, so build an analysis up incrementally instead of re-running everything each time. Plots/figures are saved to PNG files whose paths are returned; pass include_images_base64=true only when you actually need the raw bytes. This runs real, un-sandboxed Python on the local machine (exactly like a Jupyter cell), so it is approval-gated.",
        permission_mode: PermissionMode::FullAccess,
        requires_approval: true,
    },
    CommandToolSpec {
        name: "notebook_status",
        root: "notebook",
        aliases: &[],
        kind: CommandToolKind::NotebookStatus,
        description: "Report the in-app notebook kernel status: whether it is running, which backend it uses (a real Jupyter/IPython kernel or the zero-setup stdlib fallback), the Python version, and how many cells have run this session.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "notebook_reset",
        root: "notebook",
        aliases: &["notebook_restart"],
        kind: CommandToolKind::NotebookReset,
        description: "Restart the in-app notebook kernel and clear all cell state — a clean slate. Use this when the session's variables are wrong or a cell hung. Everything defined so far is lost. Approval-gated because the kernel is SHARED with the human's notebook pane — a reset wipes their session too.",
        permission_mode: PermissionMode::WorkspaceWrite,
        requires_approval: true,
    },
];

#[derive(Debug, Clone)]
enum CommandExecution {
    Cli {
        root: &'static str,
        args: Vec<String>,
    },
    WorkflowList,
    WorkflowShow {
        name: String,
    },
    WorkflowRun {
        name: String,
        values: BTreeMap<String, String>,
        execute: bool,
    },
    NotebookExec {
        code: String,
        timeout: Option<u64>,
        reset: bool,
        include_images_base64: bool,
    },
    NotebookStatus,
    NotebookReset,
}

fn empty_schema() -> Value {
    json!({
        "type": "object",
        "properties": {},
        "additionalProperties": false
    })
}

fn notebook_exec_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "code": {
                "type": "string",
                "description": "Python source to run as one notebook cell. Multi-line is fine; the last bare expression's value is echoed like a Jupyter cell."
            },
            "timeout": {
                "type": "integer",
                "description": "Per-cell wall-clock limit in seconds (default 120, max 600). On timeout the kernel restarts and its variables are lost.",
                "minimum": 1,
                "maximum": 600
            },
            "reset": {
                "type": "boolean",
                "description": "Restart the kernel (clearing all prior variables) BEFORE running this cell.",
                "default": false
            },
            "include_images_base64": {
                "type": "boolean",
                "description": "Also return each produced plot as base64 bytes, not just its saved file path. Off by default to keep results small.",
                "default": false
            }
        },
        "required": ["code"],
        "additionalProperties": false
    })
}

fn root_args_schema(root: &str) -> Value {
    json!({
        "type": "object",
        "properties": {
            "args": {
                "type": "array",
                "description": format!("CLI argument tokens after `prism {root}`. Use one array element per argument, not a shell string."),
                "items": { "type": "string" }
            }
        },
        "additionalProperties": false
    })
}

/// Typed schema for an umbrella root whose first argument is a known
/// subcommand. Gives the model a closed `subcommand` enum (real verbs) plus an
/// `args` array for verb-specific tokens — far stronger selection/arg-filling
/// signal than the generic `args: array<string>` escape. (SPEC §1.1.3.)
fn root_subcommand_schema(root: &str, subcommands: &'static [&'static str]) -> Value {
    json!({
        "type": "object",
        "properties": {
            "subcommand": {
                "type": "string",
                "enum": subcommands,
                "description": format!("The `prism {root}` verb to run. Pick the closest match; the tool prefers a typed sibling (e.g. billing_balance) when one exists for this verb.")
            },
            "args": {
                "type": "array",
                "description": "Optional verb-specific tokens after the subcommand (e.g. an id, --flags). One array element per token, not a shell string.",
                "items": { "type": "string" }
            }
        },
        "required": ["subcommand"],
        "additionalProperties": false
    })
}

fn workflow_show_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": {
                "type": "string",
                "description": "Workflow name or command alias to inspect."
            }
        },
        "required": ["name"],
        "additionalProperties": false
    })
}

fn workflow_run_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": {
                "type": "string",
                "description": "Workflow name or command alias to run."
            },
            "execute": {
                "type": "boolean",
                "description": "Set true for real execution. False keeps the workflow in dry-run mode."
            },
            "values": {
                "type": "object",
                "description": "Workflow argument values keyed by argument name.",
                "additionalProperties": {
                    "type": "string"
                }
            }
        },
        "required": ["name"],
        "additionalProperties": false
    })
}

fn query_local_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "text": {
                "type": "string",
                "description": "Entity name or search text."
            },
            "semantic": {
                "type": "boolean",
                "description": "Use semantic vector search over the local entity vectors instead of graph traversal."
            },
            "limit": {
                "type": "integer",
                "description": "Max number of results to return for semantic search.",
                "minimum": 1
            },
            "llm_url": {
                "type": "string",
                "description": "Override the local LLM base URL used for semantic embedding generation."
            },
            "model": {
                "type": "string",
                "description": "Override the model used for local query-time embedding generation."
            },
            "api_key": {
                "type": "string",
                "description": "Optional API key for authenticated local LLM providers."
            }
        },
        "required": ["text"],
        "additionalProperties": false
    })
}

fn query_platform_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "text": {
                "type": "string",
                "description": "Graph-search text or semantic-search query for the MARC27 platform."
            },
            "semantic": {
                "type": "boolean",
                "description": "Use the platform semantic search endpoint instead of graph search."
            },
            "json": {
                "type": "boolean",
                "description": "Return machine-readable JSON output."
            },
            "limit": {
                "type": "integer",
                "description": "Max number of platform results to request.",
                "minimum": 1
            }
        },
        "required": ["text"],
        "additionalProperties": false
    })
}

fn query_federated_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "text": {
                "type": "string",
                "description": "Natural-language query to send to the local node and discovered mesh peers."
            },
            "dashboard_url": {
                "type": "string",
                "description": "Dashboard base URL for the running local node.",
                "default": "http://127.0.0.1:7327"
            }
        },
        "required": ["text"],
        "additionalProperties": false
    })
}

fn job_status_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "job_id": {
                "type": "string",
                "description": "Compute job UUID returned by `run`, `deploy`, or a workflow step."
            }
        },
        "required": ["job_id"],
        "additionalProperties": false
    })
}

fn report_bug_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "description": {
                "type": "string",
                "description": "What went wrong and what you tried. Be specific: name the tool, the error message or broken output you got, and the steps you attempted to recover. This becomes the bug report body — the team fixes what they can see."
            },
            "log_file": {
                "type": "string",
                "description": "Optional path to a log or error-output file to attach. Omit if there is no file."
            }
        },
        "required": ["description"],
        "additionalProperties": false
    })
}

fn marketplace_query_schema(field_desc: &str, required: bool) -> Value {
    let key = if required { "name" } else { "query" };
    let mut properties = serde_json::Map::new();
    properties.insert(
        key.to_string(),
        json!({
            "type": "string",
            "description": field_desc
        }),
    );
    json!({
        "type": "object",
        "properties": Value::Object(properties),
        "required": if required { json!(["name"]) } else { json!([]) },
        "additionalProperties": false
    })
}

fn marketplace_install_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": {
                "type": "string",
                "description": "Marketplace item name to install."
            },
            "workflow": {
                "type": "boolean",
                "description": "Install into ~/.prism/workflows as a YAML workflow instead of ~/.prism/tools as a Python tool."
            }
        },
        "required": ["name"],
        "additionalProperties": false
    })
}

fn marketplace_find_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": {
                "type": "string",
                "description": "Natural-language description of what you're looking for, e.g. \"predict elastic moduli of a Ti-Al alloy\"."
            },
            "types": {
                "type": "array",
                "description": "Restrict to specific resource_type values (OR'd). Omit to search every type.",
                "items": { "type": "string" }
            },
            "limit": {
                "type": "integer",
                "description": "Max number of hits to return. Typical: 3-10.",
                "default": 5
            }
        },
        "required": ["query"],
        "additionalProperties": false
    })
}

fn ingest_schema(path_description: &str) -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": path_description
            },
            "schema_only": {
                "type": "boolean",
                "description": "Skip LLM extraction and graph/vector writes."
            },
            "mapping_path": {
                "type": "string",
                "description": "Optional YAML ontology mapping file."
            },
            "corpus": {
                "type": "string",
                "description": "Optional corpus slug to attach to the ingest job."
            },
            "model": {
                "type": "string",
                "description": "Override generation model for ingest."
            },
            "llm_url": {
                "type": "string",
                "description": "Override LLM base URL for ingest."
            },
            "api_key": {
                "type": "string",
                "description": "Optional API key for authenticated LLM providers."
            },
            "runtime_url": {
                "type": "string",
                "description": "Override the local runtime URL used for PDF text extraction."
            },
            "json": {
                "type": "boolean",
                "description": "Return JSON output instead of human-readable summaries."
            }
        },
        "required": ["path"],
        "additionalProperties": false
    })
}

fn research_query_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": {
                "type": "string",
                "description": "Materials-science research goal or question. This is the entrypoint for a higher-level research loop, not just a plain search string."
            },
            "depth": {
                "type": "integer",
                "description": "Research depth. Use `0` for the cheapest smoke test; higher values can trigger more iterative retrieval and LLM work.",
                "minimum": 0,
                "default": 0
            },
            "json": {
                "type": "boolean",
                "description": "Return the raw platform response if available instead of the human-readable summary."
            }
        },
        "required": ["query"],
        "additionalProperties": false
    })
}

fn models_list_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "provider": {
                "type": "string",
                "description": "Optional provider filter such as `anthropic`, `openai`, `google`, or `openrouter`."
            }
        },
        "additionalProperties": false
    })
}

fn models_search_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": {
                "type": "string",
                "description": "Free-text query matched against model IDs, display names, and provider names."
            },
            "provider": {
                "type": "string",
                "description": "Optional provider filter applied before the text search."
            }
        },
        "required": ["query"],
        "additionalProperties": false
    })
}

fn models_info_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "model_id": {
                "type": "string",
                "description": "Exact hosted model ID such as `gemini-3.1-pro-preview`."
            }
        },
        "required": ["model_id"],
        "additionalProperties": false
    })
}

fn deploy_list_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "status": {
                "type": "string",
                "description": "Optional deployment status filter such as `running`, `provisioning`, or `stopped`."
            }
        },
        "additionalProperties": false
    })
}

fn deploy_id_schema(field_name: &str, description: &str) -> Value {
    json!({
        "type": "object",
        "properties": {
            field_name: {
                "type": "string",
                "description": description
            }
        },
        "required": [field_name],
        "additionalProperties": false
    })
}

fn deploy_create_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": {
                "type": "string",
                "description": "Deployment name shown in the platform UI."
            },
            "image": {
                "type": "string",
                "description": "Container image to deploy directly."
            },
            "resource_slug": {
                "type": "string",
                "description": "Marketplace resource slug to deploy instead of a raw image."
            },
            "target": {
                "type": "string",
                "description": "Target backend: `local`, `mesh`, `prism_node`, `runpod`, or `lambda`.",
                "default": "local"
            },
            "gpu": {
                "type": "string",
                "description": "GPU type to request.",
                "default": "A100-80GB"
            },
            "budget": {
                "type": "number",
                "description": "Optional max budget in USD."
            },
            "node_id": {
                "type": "string",
                "description": "Optional PRISM node UUID for pinned local or mesh placement."
            },
            "env_vars": {
                "type": "object",
                "description": "Environment variables injected into the deployment container.",
                "additionalProperties": {
                    "type": "string"
                }
            },
            "port": {
                "type": "integer",
                "description": "Service port exposed by the deployed container.",
                "minimum": 1
            },
            "health_path": {
                "type": "string",
                "description": "Health-check path on the deployed service."
            }
        },
        "required": ["name"],
        "additionalProperties": false
    })
}

fn compute_estimate_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "image": { "type": "string", "description": "Container image or marketplace slug to price." },
            "gpu": { "type": "string", "description": "GPU class, e.g. 'A100-80GB'. Omit to let the broker choose." },
            "timeout": { "type": "integer", "description": "Wall-time cap in seconds (default 3600)." }
        },
        "required": ["image"],
        "additionalProperties": false
    })
}

fn predict_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "model": { "type": "string", "description": "Marketplace model slug (e.g. 'mace-mh-1', 'chgnet'). Discover with models_search/marketplace_search." },
            "task": { "type": "string", "description": "Model task: 'single_point' (default), 'relax', 'md', 'predict_property', 'generate', ..." },
            "inputs": { "type": "object", "description": "Model inputs as a JSON object, e.g. {\"structure\": {\"atoms\": [...], \"coords\": [...]}}. Pass {} if none." },
            "node_id": { "type": "string", "description": "Pin execution to a specific PRISM node UUID (mesh target). Omit to let the platform pick." },
            "gpu": { "type": "string", "description": "GPU class for a NEW deployment, e.g. 'A100-80GB'. Omit for CPU models." },
            "budget_max_usd": { "type": "number", "description": "Budget cap (USD) for a NEW deployment." },
            "keep_alive": { "type": "boolean", "description": "Keep a newly-created deployment running after the result (bills per minute until stopped). Default false: auto-stop." }
        },
        "required": ["model", "inputs"],
        "additionalProperties": false
    })
}

fn compute_submit_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "image": { "type": "string", "description": "Container image (Docker tag) or marketplace slug." },
            "inputs": { "type": "object", "description": "JSON input payload for the container. Pass {} if none." },
            "gpu": { "type": "string", "description": "GPU class, e.g. 'A100-80GB'." },
            "budget_max_usd": { "type": "number", "description": "Hard cost cap in USD; broker refuses dispatch above it." },
            "provider": { "type": "string", "description": "Routing: 'cheapest' (default), 'fastest', or a provider name." },
            "timeout": { "type": "integer", "description": "Wall-time cap in seconds (default 3600)." },
            "env_vars": { "type": "object", "description": "Environment variables for the container (KEY: VALUE)." }
        },
        "required": ["image", "inputs"],
        "additionalProperties": false
    })
}

fn goal_start_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "goal": { "type": "string", "description": "Natural-language description of what to discover, e.g. 'refractory alloy with creep resistance beyond CMSX-4 above 1100C'." },
            "elements": { "type": "array", "items": {"type": "string"}, "description": "Allowed elements, e.g. [\"W\",\"Mo\",\"Ta\",\"Nb\"]. Omit for no restriction." },
            "objective": { "type": "string", "description": "What to optimize, e.g. 'maximize creep resistance', 'minimize density'." },
            "max_iterations": { "type": "integer", "description": "Hard cap on discovery iterations (default 50)." },
            "batch_size": { "type": "integer", "description": "Candidates proposed per iteration (default 10)." },
            "budget_usd": { "type": "number", "description": "USD spend cap; the goal stops when cumulative compute cost exceeds it." },
            "approval_gates": { "type": "array", "items": {"type": "integer"}, "description": "Iteration numbers to pause at for human approval, e.g. [10, 25]." }
        },
        "required": ["goal"],
        "additionalProperties": false
    })
}

fn goal_id_schema(description: &str) -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": { "type": "string", "description": description }
        },
        "required": ["id"],
        "additionalProperties": false
    })
}

fn knowledge_entity_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": { "type": "string", "description": "Entity name to resolve in the knowledge graph." },
            "limit": { "type": "integer", "description": "Max neighbors to return (default 10)." }
        },
        "required": ["name"],
        "additionalProperties": false
    })
}

fn knowledge_paths_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "from_entity": { "type": "string", "description": "Path start entity." },
            "to_entity": { "type": "string", "description": "Path end entity." },
            "max_hops": { "type": "integer", "description": "Max path length in hops (default 3)." }
        },
        "required": ["from_entity", "to_entity"],
        "additionalProperties": false
    })
}

fn knowledge_corpora_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "domain": { "type": "string", "description": "Filter by domain: materials/chemistry/biomedical/physics." },
            "kind": { "type": "string", "description": "Filter by kind: structured_db/knowledge_graph/literature/ontology." },
            "limit": { "type": "integer", "description": "Max results (default 50)." }
        },
        "additionalProperties": false
    })
}

fn knowledge_ingest_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "url": { "type": "string", "description": "Source URL to fetch and extract. Provide this OR `query`." },
            "query": { "type": "string", "description": "Free-text to extract entities/embeddings from. Provide this OR `url`." },
            "mode": { "type": "string", "description": "Extraction mode: graph/embed/full (default full)." }
        },
        "additionalProperties": false
    })
}

fn run_submit_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "image": {
                "type": "string",
                "description": "Container image to run."
            },
            "name": {
                "type": "string",
                "description": "Optional job name."
            },
            "backend": {
                "type": "string",
                "description": "Execution backend: `local`, `marc27`, or `byoc`."
            },
            "platform_url": {
                "type": "string",
                "description": "Override the MARC27 platform API URL when using the `marc27` backend."
            },
            "inputs": {
                "type": "object",
                "description": "Key-value input bindings passed as repeated `--input key=value` flags.",
                "additionalProperties": { "type": "string" }
            },
            "ssh": {
                "type": "string",
                "description": "BYOC SSH target such as `user@host`."
            },
            "ssh_key": {
                "type": "string",
                "description": "SSH private-key path for BYOC SSH."
            },
            "ssh_port": {
                "type": "integer",
                "description": "SSH port for BYOC SSH."
            },
            "k8s_context": {
                "type": "string",
                "description": "Kubernetes context for BYOC K8s."
            },
            "k8s_namespace": {
                "type": "string",
                "description": "Kubernetes namespace for BYOC K8s."
            },
            "slurm": {
                "type": "string",
                "description": "SLURM head node target such as `user@host`."
            },
            "slurm_partition": {
                "type": "string",
                "description": "SLURM partition name."
            }
        },
        "required": ["image"],
        "additionalProperties": false
    })
}

fn publish_artifact_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Local path to the artifact to publish."
            },
            "to": {
                "type": "string",
                "description": "Target registry, such as `marc27`, `huggingface`, or a custom URL."
            },
            "repo": {
                "type": "string",
                "description": "Target repository name, such as `username/my-model`."
            },
            "private": {
                "type": "boolean",
                "description": "Publish the artifact privately when the target supports it."
            }
        },
        "required": ["path"],
        "additionalProperties": false
    })
}

fn discourse_create_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "yaml_file": {
                "type": "string",
                "description": "Path to the YAML discourse spec file to upload."
            },
            "slug": {
                "type": "string",
                "description": "Optional slug override. Defaults to the YAML file stem."
            }
        },
        "required": ["yaml_file"],
        "additionalProperties": false
    })
}

fn discourse_show_schema(field_name: &str, description: &str) -> Value {
    json!({
        "type": "object",
        "properties": {
            field_name: {
                "type": "string",
                "description": description
            }
        },
        "required": [field_name],
        "additionalProperties": false
    })
}

fn discourse_run_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "spec_id": {
                "type": "string",
                "description": "UUID of the discourse spec to execute."
            },
            "params": {
                "type": "object",
                "description": "Parameter bindings passed to the discourse workflow.",
                "additionalProperties": {
                    "type": "string"
                }
            }
        },
        "required": ["spec_id"],
        "additionalProperties": false
    })
}

fn node_logs_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "service": {
                "type": "string",
                "description": "Managed service name such as `kafka`, `spark`, or `firecrawl`."
            },
            "tail": {
                "type": "integer",
                "description": "Number of trailing log lines to show.",
                "minimum": 1
            }
        },
        "required": ["service"],
        "additionalProperties": false
    })
}

fn mesh_discover_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "timeout": {
                "type": "integer",
                "description": "Discovery window in seconds.",
                "minimum": 1
            }
        },
        "additionalProperties": false
    })
}

fn dashboard_url_schema(description: &str) -> Value {
    json!({
        "type": "object",
        "properties": {
            "dashboard_url": {
                "type": "string",
                "description": description,
                "default": "http://127.0.0.1:7327"
            }
        },
        "additionalProperties": false
    })
}

fn mesh_publish_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": {
                "type": "string",
                "description": "Dataset name to publish from the running local node."
            },
            "schema_version": {
                "type": "string",
                "description": "Dataset schema version.",
                "default": "1.0"
            },
            "dashboard_url": {
                "type": "string",
                "description": "Dashboard base URL for the running local node.",
                "default": "http://127.0.0.1:7327"
            }
        },
        "required": ["name"],
        "additionalProperties": false
    })
}

fn mesh_subscription_schema(action: &str) -> Value {
    json!({
        "type": "object",
        "properties": {
            "dataset_name": {
                "type": "string",
                "description": format!("Dataset name to {action}.")
            },
            "publisher": {
                "type": "string",
                "description": "Publisher node UUID."
            },
            "dashboard_url": {
                "type": "string",
                "description": "Dashboard base URL for the running local node.",
                "default": "http://127.0.0.1:7327"
            }
        },
        "required": ["dataset_name", "publisher"],
        "additionalProperties": false
    })
}

fn schema_for_spec(spec: &CommandToolSpec) -> Value {
    match spec.kind {
        CommandToolKind::RootArgs => root_args_schema(spec.root),
        CommandToolKind::RootSubcommand { subcommands } => {
            root_subcommand_schema(spec.root, subcommands)
        }
        CommandToolKind::QueryLocal => query_local_schema(),
        CommandToolKind::QueryPlatform => query_platform_schema(),
        CommandToolKind::QueryFederated => query_federated_schema(),
        CommandToolKind::JobStatusLookup => job_status_schema(),
        CommandToolKind::WorkflowList => empty_schema(),
        CommandToolKind::WorkflowShow => workflow_show_schema(),
        CommandToolKind::WorkflowRun => workflow_run_schema(),
        CommandToolKind::MarketplaceSearch => marketplace_query_schema(
            "Optional marketplace search query. Leave empty to browse the default listing.",
            false,
        ),
        CommandToolKind::MarketplaceInfo => {
            marketplace_query_schema("Marketplace item name to inspect.", true)
        }
        CommandToolKind::MarketplaceInstall => marketplace_install_schema(),
        CommandToolKind::MarketplaceFind => marketplace_find_schema(),
        CommandToolKind::IngestFile => {
            ingest_schema("File or local path to ingest into PRISM's graph/vector pipeline.")
        }
        CommandToolKind::IngestWatch => {
            ingest_schema("Directory to watch continuously for ingestable files.")
        }
        CommandToolKind::ResearchQuery => research_query_schema(),
        CommandToolKind::ModelsList => models_list_schema(),
        CommandToolKind::ModelsSearch => models_search_schema(),
        CommandToolKind::ModelsInfo => models_info_schema(),
        CommandToolKind::DeployList => deploy_list_schema(),
        CommandToolKind::DeployStatus => {
            deploy_id_schema("deployment_id", "Deployment UUID to inspect.")
        }
        CommandToolKind::DeployHealth => {
            deploy_id_schema("deployment_id", "Deployment UUID to health-check.")
        }
        CommandToolKind::DeployCreate => deploy_create_schema(),
        CommandToolKind::DeployStop => {
            deploy_id_schema("deployment_id", "Deployment UUID to stop.")
        }
        CommandToolKind::RunSubmit => run_submit_schema(),
        CommandToolKind::DiscourseList => empty_schema(),
        CommandToolKind::DiscourseCreate => discourse_create_schema(),
        CommandToolKind::DiscourseShow => {
            discourse_show_schema("spec_id", "UUID of the discourse spec to inspect.")
        }
        CommandToolKind::DiscourseRun => discourse_run_schema(),
        CommandToolKind::DiscourseStatus => {
            discourse_show_schema("instance_id", "UUID of the discourse instance to inspect.")
        }
        CommandToolKind::DiscourseTurns => discourse_show_schema(
            "instance_id",
            "UUID of the discourse instance whose stored turns should be fetched.",
        ),
        CommandToolKind::PublishArtifact => publish_artifact_schema(),
        CommandToolKind::ComputeGpus | CommandToolKind::ComputeProviders => empty_schema(),
        CommandToolKind::ComputeEstimate => compute_estimate_schema(),
        CommandToolKind::ComputeStatus => deploy_id_schema(
            "job_id",
            "Compute-broker job ID returned by compute_submit.",
        ),
        CommandToolKind::ComputeCancel => {
            deploy_id_schema("job_id", "Compute-broker job ID to cancel.")
        }
        CommandToolKind::ComputeSubmit => compute_submit_schema(),
        CommandToolKind::Predict => predict_schema(),
        CommandToolKind::GoalStart => goal_start_schema(),
        CommandToolKind::GoalStatus => {
            goal_id_schema("Goal (campaign) id from goal_list or goal_start output.")
        }
        CommandToolKind::GoalList => empty_schema(),
        CommandToolKind::GoalResume => {
            goal_id_schema("Goal (campaign) id to resume from its checkpoint.")
        }
        CommandToolKind::KnowledgeEntity => knowledge_entity_schema(),
        CommandToolKind::KnowledgePaths => knowledge_paths_schema(),
        CommandToolKind::KnowledgeCorpora => knowledge_corpora_schema(),
        CommandToolKind::KnowledgeIngest => knowledge_ingest_schema(),
        CommandToolKind::BillingBalance
        | CommandToolKind::BillingUsage
        | CommandToolKind::BillingHistory
        | CommandToolKind::BillingPrices => empty_schema(),
        CommandToolKind::ReportBug => report_bug_schema(),
        CommandToolKind::NotebookExec => notebook_exec_schema(),
        CommandToolKind::NotebookStatus | CommandToolKind::NotebookReset => empty_schema(),
        CommandToolKind::NodeProbe | CommandToolKind::NodeStatus => empty_schema(),
        CommandToolKind::NodeLogs => node_logs_schema(),
        CommandToolKind::MeshDiscover => mesh_discover_schema(),
        CommandToolKind::MeshHealth => {
            dashboard_url_schema("Dashboard base URL for the running local node.")
        }
        CommandToolKind::MeshPeers => {
            dashboard_url_schema("Dashboard base URL for the running local node.")
        }
        CommandToolKind::MeshSubscriptions => {
            dashboard_url_schema("Dashboard base URL for the running local node.")
        }
        CommandToolKind::MeshPublish => mesh_publish_schema(),
        CommandToolKind::MeshSubscribe => mesh_subscription_schema("subscribe to"),
        CommandToolKind::MeshUnsubscribe => mesh_subscription_schema("unsubscribe from"),
    }
}

fn spec_by_name(tool_name: &str) -> Option<&'static CommandToolSpec> {
    COMMAND_TOOLS.iter().find(|spec| {
        spec.name.eq_ignore_ascii_case(tool_name)
            || spec.root.eq_ignore_ascii_case(tool_name)
            || spec
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(tool_name))
    })
}

/// VS2-P1 FIX-5: resolve a model-supplied tool name to its CANONICAL name if it
/// is a code-execution tool, else return the raw name unchanged.
///
/// The model can invoke notebook_exec via its aliases (`notebook_run`,
/// `run_python_notebook`) or its root (`notebook`), case-insensitively
/// (spec_by_name). But the repair cap (h7c) and the PROV-O chain
/// (chain_code_run) keyed on the raw model-supplied name, so an alias call
/// executed but never counted toward the cap and never got the `code_exec` tag.
/// This normalizer lets both key on the canonical name so aliases count.
pub fn canonical_code_exec_tool(name: &str) -> &str {
    // Fast path: the common canonical names pass through verbatim.
    if matches!(name, "execute_python" | "execute_bash" | "notebook_exec") {
        return name;
    }
    // Resolve via spec_by_name (handles aliases + root + case-insensitivity).
    // Only command-tools go through spec_by_name; execute_python/execute_bash
    // are Python-registered and have no aliases, so they hit the fast path above.
    if let Some(spec) = spec_by_name(name)
        && matches!(spec.name, "notebook_exec")
    {
        return "notebook_exec";
    }
    name
}

fn parse_args(input: &Value) -> Result<Vec<String>> {
    let Some(raw_args) = input.get("args") else {
        return Ok(Vec::new());
    };

    // Command tools use argv-style arrays on purpose. That keeps quoting and
    // escaping out of the model's hands and aligns the agent path with the CLI.
    let args = raw_args
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("`args` must be an array of strings"))?;
    args.iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| anyhow::anyhow!("`args` entries must be strings"))
        })
        .collect()
}

fn required_string(input: &Value, key: &str) -> Result<String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("`{key}` is required"))
}

fn optional_string(input: &Value, key: &str) -> Option<String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn optional_bool(input: &Value, key: &str) -> bool {
    input.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn parse_string_map(input: &Value, key: &str) -> Result<BTreeMap<String, String>> {
    let Some(raw_map) = input.get(key) else {
        return Ok(BTreeMap::new());
    };
    let object = raw_map
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("`{key}` must be an object with string values"))?;
    object
        .iter()
        .map(|(map_key, value)| {
            value
                .as_str()
                .map(|value| (map_key.clone(), value.to_string()))
                .ok_or_else(|| anyhow::anyhow!("`{key}.{map_key}` must be a string"))
        })
        .collect()
}

fn optional_usize(input: &Value, key: &str) -> Option<usize> {
    input
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
}

fn optional_f64(input: &Value, key: &str) -> Option<f64> {
    input.get(key).and_then(Value::as_f64)
}

fn build_query_args(input: &Value, mode: QueryMode) -> Result<Vec<String>> {
    let text = required_string(input, "text")?;
    let mut args = vec![text];

    match mode {
        QueryMode::Local => {
            if optional_bool(input, "semantic") {
                args.push("--semantic".to_string());
            }
            for (flag, value) in [
                ("--llm-url", optional_string(input, "llm_url")),
                ("--model", optional_string(input, "model")),
                ("--api-key", optional_string(input, "api_key")),
            ] {
                if let Some(value) = value {
                    args.push(flag.to_string());
                    args.push(value);
                }
            }
        }
        QueryMode::Platform => {
            args.push("--platform".to_string());
            if optional_bool(input, "semantic") {
                args.push("--semantic".to_string());
            }
            if optional_bool(input, "json") {
                args.push("--json".to_string());
            }
        }
        QueryMode::Federated => {
            args.push("--federated".to_string());
            if let Some(dashboard_url) = optional_string(input, "dashboard_url") {
                args.push("--dashboard-url".to_string());
                args.push(dashboard_url);
            }
        }
    }

    if let Some(limit) = optional_usize(input, "limit") {
        args.push("--limit".to_string());
        args.push(limit.to_string());
    }

    Ok(args)
}

fn shell_command_join(tokens: &[String]) -> String {
    tokens
        .iter()
        .map(|token| {
            if token.is_empty() {
                return "''".to_string();
            }

            if !token
                .chars()
                .any(|ch| ch.is_whitespace() || matches!(ch, '\'' | '"' | '\\'))
            {
                return token.clone();
            }

            format!("'{}'", token.replace('\'', "'\"'\"'"))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn format_invocation(root: &str, args: &[String]) -> String {
    if args.is_empty() {
        return format!("prism {root}");
    }
    format!("prism {root} {}", shell_command_join(args))
}

fn workflow_run_display_args(
    name: &str,
    values: &BTreeMap<String, String>,
    execute: bool,
) -> Vec<String> {
    let mut args = vec!["run".to_string(), name.to_string()];
    for (key, value) in values {
        if key == "role" {
            continue;
        }
        args.push("--set".to_string());
        args.push(format!("{key}={value}"));
    }
    if execute {
        args.push("--execute".to_string());
    }
    args
}

fn format_execution_invocation(execution: &CommandExecution) -> String {
    match execution {
        CommandExecution::Cli { root, args } => format_invocation(root, args),
        CommandExecution::WorkflowList => format_invocation("workflow", &["list".to_string()]),
        CommandExecution::WorkflowShow { name } => {
            format_invocation("workflow", &["show".to_string(), name.clone()])
        }
        CommandExecution::WorkflowRun {
            name,
            values,
            execute,
        } => format_invocation(
            "workflow",
            &workflow_run_display_args(name, values, *execute),
        ),
        CommandExecution::NotebookExec { code, reset, .. } => {
            // Preview the first code line only — the full blob would flood the
            // approval prompt / transcript. `reset` MUST be surfaced: it wipes
            // the shared kernel, so the human sees it before approving.
            let first_line = code.lines().next().unwrap_or("").trim();
            let head: String = first_line.chars().take(60).collect();
            let ellipsis = if first_line.chars().count() > 60 || code.lines().count() > 1 {
                "…"
            } else {
                ""
            };
            let reset_note = if *reset { " [resets kernel first]" } else { "" };
            format!(
                "notebook exec: {head}{ellipsis} ({} chars){reset_note}",
                code.chars().count()
            )
        }
        CommandExecution::NotebookStatus => "notebook status".to_string(),
        CommandExecution::NotebookReset => "notebook reset".to_string(),
    }
}

fn command_timeout_for_root(root: &str) -> Duration {
    match root {
        "workflow" | "ingest" | "query" | "run" | "research" | "deploy" | "publish"
        | "marketplace" => Duration::from_secs(300),
        "node" | "mesh" => Duration::from_secs(60),
        _ => Duration::from_secs(30),
    }
}

fn truncate_for_ui(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    text.chars().take(max_chars).collect::<String>() + "\n\n[Output truncated]"
}

// VS2-P1a FIX-3 helper: flush a run of consecutive library frames into a single
// collapse-marker, accumulating the FRAME count (not marker count) into
// `total`. A free function (not a closure) so it can take multiple &mut borrows
// without conflicting with the filter loop's other borrows.
fn flush_library_run(run: &mut u32, total: &mut u32, kept: &mut Vec<String>) {
    if *run > 0 {
        kept.push(format!(
            "[... {run} library frame(s) elided — notebook pane keeps the full trace]\n"
        ));
        *total += *run;
        *run = 0;
    }
}

/// Strip ANSI SGR escape sequences (`\x1b[...m`) from a string.
///
/// G1: the jupyter/ipykernel backend (the PREFERRED notebook backend when
/// installed) emits ANSI-colored tracebacks — every keyword is wrapped, e.g.
/// `\x1b[96mFile \x1b[39m`. Without stripping, `starts_with("File ")` never
/// matches and the filter is a verbatim NO-OP on jupyter traces, leaking raw
/// site-packages frames to the model. Stripping is also good hygiene: ANSI
/// codes in tool output are noise to the model.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            // CSI sequence: skip `\x1b[` ... `m` (and other final bytes).
            i += 2;
            while i < bytes.len() {
                let c = bytes[i];
                i += 1;
                // SGR ends at 'm'; be lenient and also break on any letter
                // (covers other CSI sequences ipykernel might emit).
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            // Safe UTF-8 append: push the whole char starting at this byte.
            let ch_start = i;
            let ch_len = utf8_len(bytes[i]);
            if let Some(slice) = s.get(ch_start..ch_start + ch_len) {
                out.push_str(slice);
            }
            i += ch_len;
        }
    }
    out
}

/// Length in bytes of the UTF-8 codepoint starting at `first`.
fn utf8_len(first: u8) -> usize {
    if first < 0x80 {
        1
    } else if first >> 5 == 0b110 {
        2
    } else if first >> 4 == 0b1110 {
        3
    } else if first >> 3 == 0b11110 {
        4
    } else {
        1 // invalid lead byte — drop just it
    }
}

/// F2 (PEP 654): strip a leading ExceptionGroup / TaskGroup gutter so the
/// remaining content keeps its NATURAL traceback indentation for classification.
///
/// CPython prefixes every line inside an ExceptionGroup traceback with a gutter:
/// `  | ` (outer) or `    | ` (nested sub-exception), and the group header with
/// `  + `. Without stripping, a `File ".../site-packages/..."` frame reads as
/// `|   File ...` — `is_frame_line`/`is_library` miss it and the ENTIRE group
/// traceback leaks verbatim (the H1 leak class). Strip exactly the
/// `<indent>[|+] ` gutter: the remainder keeps its own indentation (frame
/// `  File`, body `    src`, and the final `EType: msg` at column 0) so the
/// normal frame/body/final-line checks apply to `| File ".../site-packages/…"`.
///
/// Divider lines (`+-+---------------- 1 ----------------`,
/// `+------------------------------------`) have a `-`/`+` immediately after the
/// corner — NOT a space — so they don't match the `[|+] ` gutter and are
/// returned unchanged; `is_group_divider` keeps them classified as structure
/// (kept verbatim, never consumed as a frame body).
fn strip_group_margin(line: &str) -> &str {
    let trimmed = line.trim_start_matches(' ');
    trimmed
        .strip_prefix("| ")
        .or_else(|| trimmed.strip_prefix("+ "))
        .unwrap_or(line)
}

/// F2: a PEP 654 group divider — after indentation, a `+` corner followed only
/// by `+`/`-`/space/digit runs (`+-+---------------- 1 ----------------`,
/// `+------------------------------------`). Group STRUCTURE: never a frame or a
/// body line, always kept verbatim so the sub-exception boundaries survive.
fn is_group_divider(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with('+')
        && t.contains("--")
        && t.chars()
            .all(|c| matches!(c, '+' | '-' | ' ') || c.is_ascii_digit())
}

/// G1: recognize BOTH stdlib and IPython frame-header forms (after ANSI strip).
/// stdlib form: `  File "...", line N, in func`. ipykernel forms:
/// `File ~/path:N, in func` (unquoted, `~`-collapsed, colon) and
/// `Cell In[N], line M, in func` (live kernel cell). All start with `File ` or
/// `Cell In[` once ANSI-stripped + trimmed. F2: also after stripping any
/// ExceptionGroup gutter, so `| File …` inside a group is recognized.
fn is_frame_line(line: &str) -> bool {
    let t = strip_group_margin(line).trim_start();
    t.starts_with("File ") || t.starts_with("Cell In[")
}

/// FIX-3(H1): is `line` a BODY/continuation line of the current frame (as
/// opposed to the next frame header or the final `EType: msg` exception line)?
///
/// Covers BOTH traceback dialects the shared kernel can emit:
///   - stdlib (`python -c`): 4-space-indented source lines + PEP 657
///     caret/annotation lines (`    ~~~~^~~`).
///   - IPython/ipykernel "Context" xmode (the PREFERRED backend, never
///     overridden): indented numbered source (`    642 ...`), `(...)`
///     context-elision lines (`   (...)   639`), and — critically — the
///     failing line rendered as a COLUMN-0 arrow (`--> 642 ...` / `----> 3
///     ...`). The old consumer only grabbed `starts_with("    ")` lines, so the
///     column-0 arrow BROKE the loop and the library's raw source leaked into
///     the fallback next to the elision marker (H1).
///
/// Blank lines are NOT classified here (the caller treats them as body
/// separators so consecutive library frames still collapse into one marker); a
/// column-0, non-arrow, non-blank line is the final exception line and ENDS the
/// body.
fn is_traceback_body_line(line: &str) -> bool {
    // F2: a PEP 654 group divider is STRUCTURE, never a frame body — it must
    // BREAK the continuation run so the `+---- N ----` boundaries survive
    // (otherwise, being space-indented, it would be swallowed as body).
    if is_group_divider(line) {
        return false;
    }
    // F2: strip any ExceptionGroup gutter so the content keeps its natural
    // indentation — a `  |     src` body stays `    src` (still indented → body),
    // while the final `  | EType: msg` line drops to column 0 and correctly ENDS
    // the body instead of being kept as an indented continuation.
    let line = strip_group_margin(line);
    if line.trim().is_empty() {
        return false;
    }
    // Indented => source / caret / numbered / `(...)` continuation.
    if line.starts_with(' ') || line.starts_with('\t') {
        return true;
    }
    // Column-0 IPython arrow: one-or-more `-` then `>` (distinct from the
    // all-dashes separator rule `-----`, which has no `>`).
    let after_dashes = line.trim_start_matches('-');
    after_dashes.len() < line.len() && after_dashes.starts_with('>')
}

/// H2/F3: compute the `~`-compressed form of `cwd` given `$HOME`, so a USER
/// frame IPython emits tilde-collapsed (`File ~/proj/foo.py:3`) matches the cwd.
///
/// F3: a trailing slash on `$HOME` (`HOME=/Users/x/`) made `strip_prefix` yield
/// `~proj…` (no separator) while IPython emits `~/proj/…`, so the user frame was
/// over-elided. Trim a trailing `/` from HOME first. Returns None when HOME is
/// empty (or `/`, which trims to empty) or when cwd is not under HOME.
fn cwd_tilde_form(home: &str, cwd: &str) -> Option<String> {
    let home = home.trim_end_matches('/');
    if home.is_empty() {
        return None;
    }
    cwd.strip_prefix(home).map(|rest| format!("~{rest}"))
}

// VS2-P1a: agent-facing traceback filter for notebook_exec. The kernel is
// SHARED with the human pane, so the filter is applied HERE (the agent-facing
// composition), not in the sidecar — the human debug pane keeps the raw
// traceback. Mirrors app/tools/code.py::_filter_traceback:
//   - keep the "Traceback (most recent call last):" header
//   - keep user frames (File "<string>" or the cwd)
//   - collapse consecutive library frames (site-packages / stdlib / Frameworks)
//     to one marker naming how many were elided
//   - ALWAYS keep the final exception line(s); for a chain
//     ("During handling..." / "The above exception...") keep BOTH final blocks
//     (VS1/F2 lesson: the root cause lives in the earlier block)
// Returns (filtered_text, n_elided_frames). When nothing was elided, returns
// the input verbatim with n=0 so we don't pollute clean traces with markers.
//
// FIX-3: the count is FRAMES (sum of each collapsed run), not marker lines.
fn filter_notebook_traceback(stderr: &str, cwd: &str) -> (String, usize) {
    if stderr.trim().is_empty() {
        return (stderr.to_string(), 0);
    }
    // G1: STRIP ANSI FIRST. The jupyter/ipykernel backend (preferred when
    // installed) emits ANSI-colored tracebacks — `\x1b[96mFile \x1b[39m` etc.
    // Without stripping, the frame matcher never matches and the filter is a
    // verbatim NO-OP on jupyter traces, leaking raw site-packages frames. Work
    // on the stripped text for the whole filter; ANSI codes in tool output are
    // noise to the model anyway.
    let stderr_owned: String;
    let stderr = if stderr.contains('\u{1b}') {
        stderr_owned = strip_ansi(stderr);
        &stderr_owned
    } else {
        stderr
    };
    // Only filter when this actually looks like a Python traceback.
    let has_header = stderr.contains("Traceback (most recent call last)");
    if !has_header {
        return (stderr.to_string(), 0);
    }

    let chain_markers = [
        "During handling of the above exception, another exception occurred:",
        "The above exception was the direct cause of the following exception:",
    ];
    // G3: split library markers into RELIABLE (unconditional — a venv under cwd
    // is still library, keep the F2 fix) and AMBIGUOUS (substring false
    // positives: `python3.`/`/lib/python`/`/Frameworks/` match user paths like
    // `<cwd>/scripts/port_python3.14_helpers.py`). For the ambiguous ones,
    // require the frame is NOT under cwd (a real stdlib/framework path won't be
    // under the project cwd). cwd is now load-bearing here.
    //
    // H2: IPython tilde-compresses under-$HOME paths — it emits
    // `File ~/project/scripts/foo.py:3` where the header path starts with `~/`,
    // NOT the absolute cwd. Since most projects live under $HOME, the plain
    // `line.contains(cwd)` (cwd absolute) missed the tilde form, so a USER frame
    // under $HOME was misclassified library and elided (the frame G3 exists to
    // protect). Precompute the `~`-compressed form of cwd and accept EITHER
    // spelling as "under cwd".
    let cwd_tilde: Option<String> = std::env::var("HOME")
        .ok()
        .and_then(|h| cwd_tilde_form(&h, cwd));
    let is_library = |line: &str| {
        // F2: strip an ExceptionGroup gutter first so `| File ".../site-packages/…"`
        // is recognized as a frame; the `line.contains(...)` marker/cwd checks
        // below still see the (unstripped) full path, so they're unaffected.
        let t = strip_group_margin(line).trim_start();
        if !(t.starts_with("File ") || t.starts_with("Cell In[")) {
            return false;
        }
        // Reliable markers: unconditional library.
        let reliable = ["site-packages", "dist-packages"];
        if reliable.iter().any(|m| line.contains(m)) {
            return true;
        }
        // Ambiguous markers: library only if the frame path is NOT under cwd
        // (matching either the absolute cwd or its `~`-compressed form).
        let ambiguous = ["python3.", "/lib/python", "/Frameworks/"];
        if ambiguous.iter().any(|m| line.contains(m)) {
            if cwd.is_empty() {
                // Can't prove under cwd -> keep as user (safer under-elide).
                return false;
            }
            let under_cwd =
                line.contains(cwd) || cwd_tilde.as_deref().is_some_and(|t| line.contains(t));
            return !under_cwd;
        }
        false
    };

    let lines: Vec<&str> = stderr.split_inclusive('\n').collect();
    let mut kept: Vec<String> = Vec::new();
    let mut run_of_library = 0u32;
    let mut i = 0usize;
    let n = lines.len();
    // FIX-3: count FRAMES elided (sum of each run), not marker lines. The old
    // code counted "[... N library frame(s) elided]" marker lines via
    // kept.iter().filter(...).count(), so a single collapsed run of N frames
    // reported traceback_elided_frames=1.
    let mut total_elided_frames: u32 = 0;

    while i < n {
        let ln = lines[i];
        let ln_trim = ln.trim_end_matches('\n');

        // Chain marker: keep everything from here to the end verbatim.
        if chain_markers.iter().any(|m| ln_trim.contains(m)) {
            // G4: a chain marker ("During handling..." / "The above exception...")
            // starts a NEW exception block. Flush any pending library run, keep
            // the marker, and CONTINUE the normal frame classification into the
            // next block — so library frames in the SECOND block are also
            // collapsed (the old code kept the entire tail verbatim, leaking raw
            // site-packages frames after the marker). Both final exception lines
            // survive (they're non-frame, non-indented lines kept by the fallback).
            flush_library_run(&mut run_of_library, &mut total_elided_frames, &mut kept);
            kept.push(ln.to_string());
            i += 1;
            continue;
        }

        if is_frame_line(ln_trim) {
            // FIX-3: a frame is the File/Cell line + ALL following BODY lines
            // (source, PEP 657 carets, IPython numbered/`(...)`/`--> N` arrow
            // lines, and the blank separator that follows the frame). The body
            // ends at the NEXT frame header, a chain marker, or the final
            // column-0 exception line.
            //
            // FIX-3(H1): the old loop only consumed `starts_with("    ")` lines,
            // so IPython's column-0 arrow (`--> 642 _assert_stacked_square(a)`)
            // broke the loop and the library's raw source leaked verbatim into
            // the fallback next to the elision marker. Consuming the trailing
            // blank too keeps consecutive (blank-separated) library frames in
            // ONE run so they collapse to a single marker.
            let mut consumed = 1usize;
            let mut continuation: Vec<&str> = Vec::new();
            while i + consumed < n {
                let next = lines[i + consumed].trim_end_matches('\n');
                if is_frame_line(next) || chain_markers.iter().any(|m| next.contains(m)) {
                    break;
                }
                if next.trim().is_empty() || is_traceback_body_line(next) {
                    continuation.push(lines[i + consumed]);
                    consumed += 1;
                } else {
                    // Column-0, non-arrow, non-blank => the final exception line.
                    break;
                }
            }
            // G1: a `Cell In[N]` frame is the user's notebook cell -> always KEPT
            // (never library). A `File ...` frame is classified by is_library.
            let frame_is_library = is_library(ln_trim);
            if frame_is_library {
                run_of_library += 1;
            } else {
                flush_library_run(&mut run_of_library, &mut total_elided_frames, &mut kept);
                kept.push(ln.to_string());
                for cl in continuation {
                    kept.push(cl.to_string());
                }
            }
            i += consumed;
            continue;
        }

        // Header, code line, or final exception line.
        flush_library_run(&mut run_of_library, &mut total_elided_frames, &mut kept);
        kept.push(ln.to_string());
        i += 1;
    }
    // Final flush (loop may have ended in a library run). Use the helper which
    // only zeroes run_of_library when it actually flushed, avoiding a dead-store
    // warning on this last call.
    flush_library_run(&mut run_of_library, &mut total_elided_frames, &mut kept);

    let elided = total_elided_frames as usize;
    if elided == 0 {
        return (stderr.to_string(), 0);
    }
    (kept.concat(), elided)
}

fn parse_workflow_run_subcommand_args(
    args: &[String],
) -> Result<(String, BTreeMap<String, String>, bool)> {
    let Some(name) = args.first() else {
        bail!("workflow run requires a workflow name");
    };

    let mut values = BTreeMap::new();
    let mut execute = false;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--set" => {
                let pair = args.get(index + 1).ok_or_else(|| {
                    anyhow::anyhow!("workflow run requires `--set key=value` pairs")
                })?;
                let (key, value) = pair.split_once('=').ok_or_else(|| {
                    anyhow::anyhow!("invalid workflow `--set` pair: {pair}. Expected key=value.")
                })?;
                values.insert(key.to_string(), value.to_string());
                index += 2;
            }
            "--execute" => {
                execute = true;
                index += 1;
            }
            other => bail!("unexpected workflow run argument: {other}"),
        }
    }

    values.insert("role".to_string(), "agent".to_string());
    Ok((name.clone(), values, execute))
}

fn parse_workflow_execution_from_root_args(args: &[String]) -> Result<CommandExecution> {
    if args.is_empty() {
        return Ok(CommandExecution::WorkflowList);
    }

    match args[0].as_str() {
        "list" if args.len() == 1 => Ok(CommandExecution::WorkflowList),
        "show" if args.len() == 2 => Ok(CommandExecution::WorkflowShow {
            name: args[1].clone(),
        }),
        "run" => {
            let (name, values, execute) = parse_workflow_run_subcommand_args(&args[1..])?;
            Ok(CommandExecution::WorkflowRun {
                name,
                values,
                execute,
            })
        }
        _ => {
            let mut request = parse_workflow_command_args(args)?;
            // The autonomous agent path must not be able to spoof an elevated
            // workflow role through arbitrary CLI-style args.
            request
                .values
                .insert("role".to_string(), "agent".to_string());
            Ok(CommandExecution::WorkflowRun {
                name: request.name,
                values: request.values,
                execute: request.execute,
            })
        }
    }
}

fn build_ingest_args(input: &Value, watch: bool) -> Result<Vec<String>> {
    let path = required_string(input, "path")?;
    let mut args = Vec::new();

    if watch {
        args.push("--watch".to_string());
    }
    if optional_bool(input, "schema_only") {
        args.push("--schema-only".to_string());
    }
    if let Some(mapping_path) = optional_string(input, "mapping_path") {
        args.push("--mapping".to_string());
        args.push(mapping_path);
    }
    if let Some(corpus) = optional_string(input, "corpus") {
        args.push("--corpus".to_string());
        args.push(corpus);
    }
    for (flag, value) in [
        ("--model", optional_string(input, "model")),
        ("--llm-url", optional_string(input, "llm_url")),
        ("--api-key", optional_string(input, "api_key")),
        ("--runtime-url", optional_string(input, "runtime_url")),
    ] {
        if let Some(value) = value {
            args.push(flag.to_string());
            args.push(value);
        }
    }
    if optional_bool(input, "json") {
        args.push("--json".to_string());
    }

    args.push(path);
    Ok(args)
}

#[derive(Debug, Clone, Copy)]
enum QueryMode {
    Local,
    Platform,
    Federated,
}

fn build_execution(spec: &CommandToolSpec, input: &Value) -> Result<CommandExecution> {
    match spec.kind {
        CommandToolKind::RootArgs => {
            let args = parse_args(input)?;
            if spec.root == "workflow" {
                parse_workflow_execution_from_root_args(&args)
            } else {
                Ok(CommandExecution::Cli {
                    root: spec.root,
                    args,
                })
            }
        }
        CommandToolKind::RootSubcommand { .. } => {
            // Typed umbrella: subcommand (required) + optional verb-specific
            // tokens. Prepend the chosen subcommand, then run as a CLI command.
            // `workflow` keeps its specialized parser (it has structured
            // WorkflowList/Show/Run execution variants).
            let subcommand = required_string(input, "subcommand")?;
            let extra = parse_args(input)?;
            let mut args = Vec::with_capacity(extra.len() + 1);
            args.push(subcommand);
            args.extend(extra);
            if spec.root == "workflow" {
                parse_workflow_execution_from_root_args(&args)
            } else {
                Ok(CommandExecution::Cli {
                    root: spec.root,
                    args,
                })
            }
        }
        CommandToolKind::QueryLocal => Ok(CommandExecution::Cli {
            root: spec.root,
            args: build_query_args(input, QueryMode::Local)?,
        }),
        CommandToolKind::QueryPlatform => Ok(CommandExecution::Cli {
            root: spec.root,
            args: build_query_args(input, QueryMode::Platform)?,
        }),
        CommandToolKind::QueryFederated => Ok(CommandExecution::Cli {
            root: spec.root,
            args: build_query_args(input, QueryMode::Federated)?,
        }),
        CommandToolKind::JobStatusLookup => Ok(CommandExecution::Cli {
            root: spec.root,
            args: vec![required_string(input, "job_id")?],
        }),
        CommandToolKind::WorkflowList => Ok(CommandExecution::WorkflowList),
        CommandToolKind::WorkflowShow => Ok(CommandExecution::WorkflowShow {
            name: required_string(input, "name")?,
        }),
        CommandToolKind::WorkflowRun => {
            let mut values = parse_string_map(input, "values")?;
            values.insert("role".to_string(), "agent".to_string());
            Ok(CommandExecution::WorkflowRun {
                name: required_string(input, "name")?,
                values,
                execute: optional_bool(input, "execute"),
            })
        }
        CommandToolKind::MarketplaceSearch => {
            let mut args = vec!["search".to_string()];
            if let Some(query) = optional_string(input, "query") {
                args.push(query);
            }
            Ok(CommandExecution::Cli {
                root: spec.root,
                args,
            })
        }
        CommandToolKind::MarketplaceInfo => Ok(CommandExecution::Cli {
            root: spec.root,
            args: vec!["info".to_string(), required_string(input, "name")?],
        }),
        CommandToolKind::MarketplaceInstall => {
            let mut args = vec!["install".to_string(), required_string(input, "name")?];
            if optional_bool(input, "workflow") {
                args.push("--workflow".to_string());
            }
            Ok(CommandExecution::Cli {
                root: spec.root,
                args,
            })
        }
        CommandToolKind::MarketplaceFind => {
            let mut args = vec!["find".to_string(), required_string(input, "query")?];
            if let Some(types) = input.get("types").and_then(Value::as_array) {
                for t in types.iter().filter_map(Value::as_str) {
                    args.push("--type".to_string());
                    args.push(t.to_string());
                }
            }
            if let Some(limit) = optional_usize(input, "limit") {
                args.push("--limit".to_string());
                args.push(limit.to_string());
            }
            // Structured output — this tool is agent-only, never rendered
            // raw to a human, so always request JSON.
            args.push("--json".to_string());
            Ok(CommandExecution::Cli {
                root: spec.root,
                args,
            })
        }
        CommandToolKind::IngestFile => Ok(CommandExecution::Cli {
            root: spec.root,
            args: build_ingest_args(input, false)?,
        }),
        CommandToolKind::IngestWatch => Ok(CommandExecution::Cli {
            root: spec.root,
            args: build_ingest_args(input, true)?,
        }),
        CommandToolKind::ResearchQuery => {
            let mut args = vec![required_string(input, "query")?];
            if let Some(depth) = optional_usize(input, "depth") {
                args.push("--depth".to_string());
                args.push(depth.to_string());
            }
            if optional_bool(input, "json") {
                args.push("--json".to_string());
            }
            Ok(CommandExecution::Cli {
                root: spec.root,
                args,
            })
        }
        CommandToolKind::ModelsList => {
            let mut args = vec!["list".to_string()];
            if let Some(provider) = optional_string(input, "provider") {
                args.push("--provider".to_string());
                args.push(provider);
            }
            args.push("--json".to_string());
            Ok(CommandExecution::Cli {
                root: spec.root,
                args,
            })
        }
        CommandToolKind::ModelsSearch => {
            let mut args = vec!["search".to_string(), required_string(input, "query")?];
            if let Some(provider) = optional_string(input, "provider") {
                args.push("--provider".to_string());
                args.push(provider);
            }
            args.push("--json".to_string());
            Ok(CommandExecution::Cli {
                root: spec.root,
                args,
            })
        }
        CommandToolKind::ModelsInfo => Ok(CommandExecution::Cli {
            root: spec.root,
            args: vec![
                "info".to_string(),
                required_string(input, "model_id")?,
                "--json".to_string(),
            ],
        }),
        CommandToolKind::DeployList => {
            let mut args = vec!["list".to_string()];
            if let Some(status) = optional_string(input, "status") {
                args.push("--status".to_string());
                args.push(status);
            }
            args.push("--json".to_string());
            Ok(CommandExecution::Cli {
                root: spec.root,
                args,
            })
        }
        CommandToolKind::DeployStatus => Ok(CommandExecution::Cli {
            root: spec.root,
            args: vec![
                "status".to_string(),
                required_string(input, "deployment_id")?,
                "--json".to_string(),
            ],
        }),
        CommandToolKind::DeployHealth => Ok(CommandExecution::Cli {
            root: spec.root,
            args: vec![
                "health".to_string(),
                required_string(input, "deployment_id")?,
                "--json".to_string(),
            ],
        }),
        CommandToolKind::DeployCreate => {
            let name = required_string(input, "name")?;
            let image = optional_string(input, "image");
            let resource_slug = optional_string(input, "resource_slug");
            if image.is_some() == resource_slug.is_some() {
                bail!("Provide exactly one of `image` or `resource_slug`.");
            }

            let mut args = vec!["create".to_string(), "--name".to_string(), name];
            if let Some(image) = image {
                args.push("--image".to_string());
                args.push(image);
            }
            if let Some(resource_slug) = resource_slug {
                args.push("--resource-slug".to_string());
                args.push(resource_slug);
            }
            if let Some(target) = optional_string(input, "target") {
                args.push("--target".to_string());
                args.push(target);
            }
            if let Some(gpu) = optional_string(input, "gpu") {
                args.push("--gpu".to_string());
                args.push(gpu);
            }
            if let Some(budget) = optional_f64(input, "budget") {
                args.push("--budget".to_string());
                args.push(budget.to_string());
            }
            if let Some(node_id) = optional_string(input, "node_id") {
                args.push("--node".to_string());
                args.push(node_id);
            }
            for (key, value) in parse_string_map(input, "env_vars")? {
                args.push("--env".to_string());
                args.push(format!("{key}={value}"));
            }
            if let Some(port) = optional_usize(input, "port") {
                args.push("--port".to_string());
                args.push(port.to_string());
            }
            if let Some(health_path) = optional_string(input, "health_path") {
                args.push("--health-path".to_string());
                args.push(health_path);
            }
            args.push("--json".to_string());

            Ok(CommandExecution::Cli {
                root: spec.root,
                args,
            })
        }
        CommandToolKind::DeployStop => Ok(CommandExecution::Cli {
            root: spec.root,
            args: vec![
                "stop".to_string(),
                required_string(input, "deployment_id")?,
                "--json".to_string(),
            ],
        }),
        CommandToolKind::RunSubmit => {
            let mut args = Vec::new();
            if let Some(name) = optional_string(input, "name") {
                args.push("--name".to_string());
                args.push(name);
            }
            if let Some(backend) = optional_string(input, "backend") {
                args.push("--backend".to_string());
                args.push(backend);
            }
            if let Some(platform_url) = optional_string(input, "platform_url") {
                args.push("--platform-url".to_string());
                args.push(platform_url);
            }
            for (key, value) in parse_string_map(input, "inputs")? {
                args.push("--input".to_string());
                args.push(format!("{key}={value}"));
            }
            if let Some(ssh) = optional_string(input, "ssh") {
                args.push("--ssh".to_string());
                args.push(ssh);
            }
            if let Some(ssh_key) = optional_string(input, "ssh_key") {
                args.push("--ssh-key".to_string());
                args.push(ssh_key);
            }
            if let Some(ssh_port) = optional_usize(input, "ssh_port") {
                args.push("--ssh-port".to_string());
                args.push(ssh_port.to_string());
            }
            if let Some(k8s_context) = optional_string(input, "k8s_context") {
                args.push("--k8s-context".to_string());
                args.push(k8s_context);
            }
            if let Some(k8s_namespace) = optional_string(input, "k8s_namespace") {
                args.push("--k8s-namespace".to_string());
                args.push(k8s_namespace);
            }
            if let Some(slurm) = optional_string(input, "slurm") {
                args.push("--slurm".to_string());
                args.push(slurm);
            }
            if let Some(slurm_partition) = optional_string(input, "slurm_partition") {
                args.push("--slurm-partition".to_string());
                args.push(slurm_partition);
            }
            args.push(required_string(input, "image")?);
            args.push("--json".to_string());
            Ok(CommandExecution::Cli {
                root: spec.root,
                args,
            })
        }
        CommandToolKind::DiscourseList => Ok(CommandExecution::Cli {
            root: spec.root,
            args: vec!["list".to_string(), "--json".to_string()],
        }),
        CommandToolKind::DiscourseCreate => {
            let mut args = vec!["create".to_string(), required_string(input, "yaml_file")?];
            if let Some(slug) = optional_string(input, "slug") {
                args.push("--slug".to_string());
                args.push(slug);
            }
            args.push("--json".to_string());
            Ok(CommandExecution::Cli {
                root: spec.root,
                args,
            })
        }
        CommandToolKind::DiscourseShow => Ok(CommandExecution::Cli {
            root: spec.root,
            args: vec![
                "show".to_string(),
                required_string(input, "spec_id")?,
                "--json".to_string(),
            ],
        }),
        CommandToolKind::DiscourseRun => {
            let mut args = vec!["run".to_string(), required_string(input, "spec_id")?];
            for (key, value) in parse_string_map(input, "params")? {
                args.push("--param".to_string());
                args.push(format!("{key}={value}"));
            }
            args.push("--json".to_string());
            Ok(CommandExecution::Cli {
                root: spec.root,
                args,
            })
        }
        CommandToolKind::DiscourseStatus => Ok(CommandExecution::Cli {
            root: spec.root,
            args: vec![
                "status".to_string(),
                required_string(input, "instance_id")?,
                "--json".to_string(),
            ],
        }),
        CommandToolKind::DiscourseTurns => Ok(CommandExecution::Cli {
            root: spec.root,
            args: vec![
                "turns".to_string(),
                required_string(input, "instance_id")?,
                "--json".to_string(),
            ],
        }),
        CommandToolKind::PublishArtifact => {
            let mut args = vec![required_string(input, "path")?];
            if let Some(target) = optional_string(input, "to") {
                args.push("--to".to_string());
                args.push(target);
            }
            if let Some(repo) = optional_string(input, "repo") {
                args.push("--repo".to_string());
                args.push(repo);
            }
            if optional_bool(input, "private") {
                args.push("--private".to_string());
            }
            args.push("--json".to_string());
            Ok(CommandExecution::Cli {
                root: spec.root,
                args,
            })
        }
        CommandToolKind::ComputeGpus => Ok(CommandExecution::Cli {
            root: spec.root,
            args: vec!["gpus".to_string()],
        }),
        CommandToolKind::ComputeProviders => Ok(CommandExecution::Cli {
            root: spec.root,
            args: vec!["providers".to_string()],
        }),
        CommandToolKind::ComputeEstimate => {
            let mut args = vec![
                "estimate".to_string(),
                "--image".to_string(),
                required_string(input, "image")?,
            ];
            if let Some(gpu) = optional_string(input, "gpu") {
                args.push("--gpu".to_string());
                args.push(gpu);
            }
            if let Some(timeout) = optional_usize(input, "timeout") {
                args.push("--timeout".to_string());
                args.push(timeout.to_string());
            }
            Ok(CommandExecution::Cli {
                root: spec.root,
                args,
            })
        }
        CommandToolKind::ComputeStatus => Ok(CommandExecution::Cli {
            root: spec.root,
            args: vec!["status".to_string(), required_string(input, "job_id")?],
        }),
        CommandToolKind::ComputeCancel => Ok(CommandExecution::Cli {
            root: spec.root,
            args: vec!["cancel".to_string(), required_string(input, "job_id")?],
        }),
        CommandToolKind::Predict => {
            // `predict` is a TOP-LEVEL prism verb (root "predict"), so the
            // model slug is the first positional arg, not a subcommand.
            let mut args = vec![required_string(input, "model")?];
            if let Some(task) = optional_string(input, "task") {
                args.push("--task".to_string());
                args.push(task);
            }
            let inputs = input.get("inputs").cloned().unwrap_or_else(|| json!({}));
            args.push("--input".to_string());
            args.push(serde_json::to_string(&inputs).unwrap_or_else(|_| "{}".to_string()));
            if let Some(node_id) = optional_string(input, "node_id") {
                args.push("--node-id".to_string());
                args.push(node_id);
            }
            if let Some(gpu) = optional_string(input, "gpu") {
                args.push("--gpu".to_string());
                args.push(gpu);
            }
            if let Some(budget) = optional_f64(input, "budget_max_usd") {
                args.push("--budget".to_string());
                args.push(budget.to_string());
            }
            if input
                .get("keep_alive")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                args.push("--keep".to_string());
            }
            Ok(CommandExecution::Cli {
                root: spec.root,
                args,
            })
        }
        CommandToolKind::GoalStart => {
            let mut args = vec![
                "start".to_string(),
                "--goal".to_string(),
                required_string(input, "goal")?,
            ];
            if let Some(elements) = input.get("elements").and_then(Value::as_array) {
                let list = elements
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(",");
                if !list.is_empty() {
                    args.push("--elements".to_string());
                    args.push(list);
                }
            }
            if let Some(objective) = optional_string(input, "objective") {
                args.push("--objective".to_string());
                args.push(objective);
            }
            if let Some(n) = optional_usize(input, "max_iterations") {
                args.push("--max-iterations".to_string());
                args.push(n.to_string());
            }
            if let Some(n) = optional_usize(input, "batch_size") {
                args.push("--batch-size".to_string());
                args.push(n.to_string());
            }
            if let Some(budget) = optional_f64(input, "budget_usd") {
                args.push("--budget".to_string());
                args.push(budget.to_string());
            }
            if let Some(gates) = input.get("approval_gates").and_then(Value::as_array) {
                let list = gates
                    .iter()
                    .filter_map(Value::as_u64)
                    .map(|g| g.to_string())
                    .collect::<Vec<_>>()
                    .join(",");
                if !list.is_empty() {
                    args.push("--approval-gates".to_string());
                    args.push(list);
                }
            }
            // Long-research semantics: the tool call returns the goal id
            // immediately; a detached worker owns the loop and the agent
            // polls goal_status across turns. A blocking multi-hour tool
            // call would freeze the whole conversation.
            args.push("--detach".to_string());
            Ok(CommandExecution::Cli {
                root: spec.root,
                args,
            })
        }
        CommandToolKind::GoalStatus => Ok(CommandExecution::Cli {
            root: spec.root,
            args: vec!["status".to_string(), required_string(input, "id")?],
        }),
        CommandToolKind::GoalList => Ok(CommandExecution::Cli {
            root: spec.root,
            args: vec!["list".to_string()],
        }),
        CommandToolKind::GoalResume => Ok(CommandExecution::Cli {
            root: spec.root,
            args: vec![
                "resume".to_string(),
                required_string(input, "id")?,
                // Same long-research semantics as goal_start: never block
                // the tool call on a resumed multi-hour loop.
                "--detach".to_string(),
            ],
        }),
        CommandToolKind::BillingBalance => Ok(CommandExecution::Cli {
            root: spec.root,
            args: vec![],
        }),
        CommandToolKind::BillingUsage => Ok(CommandExecution::Cli {
            root: spec.root,
            args: vec!["usage".to_string()],
        }),
        CommandToolKind::BillingHistory => Ok(CommandExecution::Cli {
            root: spec.root,
            args: vec!["history".to_string()],
        }),
        CommandToolKind::BillingPrices => Ok(CommandExecution::Cli {
            root: spec.root,
            args: vec!["prices".to_string()],
        }),
        CommandToolKind::ReportBug => {
            // Reuses the `prism report` machinery (handle_report: captures
            // system context, files a GitHub issue, sends a MARC27 support
            // ticket). `description` is the CLI's required positional;
            // `log_file` maps to --log-file. We pass --no-github from the
            // agent path: an automated agent filing issues directly to the
            // public GitHub repo is a spam/abuse risk, and the MARC27 support
            // ticket (auth-gated, attributable to the user's account) is the
            // safer default. The human can always file a GitHub issue via the
            // `prism report` CLI themselves.
            let mut args = vec![required_string(input, "description")?];
            if let Some(log_file) = optional_string(input, "log_file") {
                args.push("--log-file".to_string());
                args.push(log_file);
            }
            args.push("--no-github".to_string());
            Ok(CommandExecution::Cli {
                root: spec.root,
                args,
            })
        }
        CommandToolKind::ComputeSubmit => {
            let mut args = vec![
                "submit".to_string(),
                "--image".to_string(),
                required_string(input, "image")?,
            ];
            let inputs = input.get("inputs").cloned().unwrap_or_else(|| json!({}));
            args.push("--inputs".to_string());
            args.push(serde_json::to_string(&inputs).unwrap_or_else(|_| "{}".to_string()));
            if let Some(gpu) = optional_string(input, "gpu") {
                args.push("--gpu".to_string());
                args.push(gpu);
            }
            if let Some(budget) = optional_f64(input, "budget_max_usd") {
                args.push("--budget".to_string());
                args.push(budget.to_string());
            }
            if let Some(provider) = optional_string(input, "provider") {
                args.push("--provider".to_string());
                args.push(provider);
            }
            if let Some(timeout) = optional_usize(input, "timeout") {
                args.push("--timeout".to_string());
                args.push(timeout.to_string());
            }
            for (key, value) in parse_string_map(input, "env_vars")? {
                args.push("--env".to_string());
                args.push(format!("{key}={value}"));
            }
            Ok(CommandExecution::Cli {
                root: spec.root,
                args,
            })
        }
        CommandToolKind::KnowledgeEntity => {
            let mut args = vec!["entity".to_string(), required_string(input, "name")?];
            if let Some(limit) = optional_usize(input, "limit") {
                args.push("--limit".to_string());
                args.push(limit.to_string());
            }
            Ok(CommandExecution::Cli {
                root: spec.root,
                args,
            })
        }
        CommandToolKind::KnowledgePaths => {
            let mut args = vec![
                "paths".to_string(),
                required_string(input, "from_entity")?,
                required_string(input, "to_entity")?,
            ];
            if let Some(max_hops) = optional_usize(input, "max_hops") {
                args.push("--max-hops".to_string());
                args.push(max_hops.to_string());
            }
            Ok(CommandExecution::Cli {
                root: spec.root,
                args,
            })
        }
        CommandToolKind::KnowledgeCorpora => {
            let mut args = vec!["corpora".to_string()];
            if let Some(domain) = optional_string(input, "domain") {
                args.push("--domain".to_string());
                args.push(domain);
            }
            if let Some(kind) = optional_string(input, "kind") {
                args.push("--kind".to_string());
                args.push(kind);
            }
            if let Some(limit) = optional_usize(input, "limit") {
                args.push("--limit".to_string());
                args.push(limit.to_string());
            }
            Ok(CommandExecution::Cli {
                root: spec.root,
                args,
            })
        }
        CommandToolKind::KnowledgeIngest => {
            let url = optional_string(input, "url");
            let query = optional_string(input, "query");
            if url.is_none() && query.is_none() {
                bail!("knowledge_ingest requires `url` or `query`");
            }
            let mut args = vec!["ingest".to_string()];
            if let Some(url) = url {
                args.push("--url".to_string());
                args.push(url);
            }
            if let Some(query) = query {
                args.push("--query".to_string());
                args.push(query);
            }
            if let Some(mode) = optional_string(input, "mode") {
                args.push("--mode".to_string());
                args.push(mode);
            }
            Ok(CommandExecution::Cli {
                root: spec.root,
                args,
            })
        }
        CommandToolKind::NodeProbe => Ok(CommandExecution::Cli {
            root: spec.root,
            args: vec!["probe".to_string()],
        }),
        CommandToolKind::NodeStatus => Ok(CommandExecution::Cli {
            root: spec.root,
            args: vec!["status".to_string()],
        }),
        CommandToolKind::NodeLogs => {
            let mut args = vec!["logs".to_string(), required_string(input, "service")?];
            if let Some(tail) = optional_usize(input, "tail") {
                args.push("--tail".to_string());
                args.push(tail.to_string());
            }
            Ok(CommandExecution::Cli {
                root: spec.root,
                args,
            })
        }
        CommandToolKind::MeshDiscover => {
            let mut args = vec!["discover".to_string()];
            if let Some(timeout) = optional_usize(input, "timeout") {
                args.push("--timeout".to_string());
                args.push(timeout.to_string());
            }
            Ok(CommandExecution::Cli {
                root: spec.root,
                args,
            })
        }
        CommandToolKind::MeshHealth => {
            let mut args = vec!["health".to_string()];
            if let Some(dashboard_url) = optional_string(input, "dashboard_url") {
                args.push("--dashboard-url".to_string());
                args.push(dashboard_url);
            }
            Ok(CommandExecution::Cli {
                root: spec.root,
                args,
            })
        }
        CommandToolKind::MeshPeers => {
            let mut args = vec!["peers".to_string()];
            if let Some(dashboard_url) = optional_string(input, "dashboard_url") {
                args.push("--dashboard-url".to_string());
                args.push(dashboard_url);
            }
            Ok(CommandExecution::Cli {
                root: spec.root,
                args,
            })
        }
        CommandToolKind::MeshSubscriptions => {
            let mut args = vec!["subscriptions".to_string()];
            if let Some(dashboard_url) = optional_string(input, "dashboard_url") {
                args.push("--dashboard-url".to_string());
                args.push(dashboard_url);
            }
            Ok(CommandExecution::Cli {
                root: spec.root,
                args,
            })
        }
        CommandToolKind::MeshPublish => {
            let mut args = vec!["publish".to_string(), required_string(input, "name")?];
            if let Some(schema_version) = optional_string(input, "schema_version") {
                args.push("--schema-version".to_string());
                args.push(schema_version);
            }
            if let Some(dashboard_url) = optional_string(input, "dashboard_url") {
                args.push("--dashboard-url".to_string());
                args.push(dashboard_url);
            }
            Ok(CommandExecution::Cli {
                root: spec.root,
                args,
            })
        }
        CommandToolKind::MeshSubscribe => {
            let mut args = vec![
                "subscribe".to_string(),
                required_string(input, "dataset_name")?,
                "--publisher".to_string(),
                required_string(input, "publisher")?,
            ];
            if let Some(dashboard_url) = optional_string(input, "dashboard_url") {
                args.push("--dashboard-url".to_string());
                args.push(dashboard_url);
            }
            Ok(CommandExecution::Cli {
                root: spec.root,
                args,
            })
        }
        CommandToolKind::MeshUnsubscribe => {
            let mut args = vec![
                "unsubscribe".to_string(),
                required_string(input, "dataset_name")?,
                "--publisher".to_string(),
                required_string(input, "publisher")?,
            ];
            if let Some(dashboard_url) = optional_string(input, "dashboard_url") {
                args.push("--dashboard-url".to_string());
                args.push(dashboard_url);
            }
            Ok(CommandExecution::Cli {
                root: spec.root,
                args,
            })
        }
        CommandToolKind::NotebookExec => Ok(CommandExecution::NotebookExec {
            code: required_string(input, "code")?,
            timeout: optional_usize(input, "timeout").map(|value| value as u64),
            reset: optional_bool(input, "reset"),
            include_images_base64: optional_bool(input, "include_images_base64"),
        }),
        CommandToolKind::NotebookStatus => Ok(CommandExecution::NotebookStatus),
        CommandToolKind::NotebookReset => Ok(CommandExecution::NotebookReset),
    }
}

fn render_workflow_spec(spec: &WorkflowSpec) -> String {
    let mut lines = vec![
        format!("{}\t{}", spec.name, spec.command_name),
        spec.description.clone(),
        format!("source: {}", spec.source_path),
    ];

    for argument in &spec.arguments {
        let required = if argument.required {
            "required"
        } else {
            "optional"
        };
        lines.push(format!(
            "--{}\t{}\t{}\t{}",
            argument.name, argument.r#type, required, argument.help
        ));
    }

    lines.join("\n")
}

fn render_workflow_result(spec: &WorkflowSpec, result: &WorkflowRunResult) -> String {
    let mut lines = vec![
        format!("{}\t{}", spec.command_name, result.mode),
        spec.description.clone(),
    ];

    for step in &result.steps {
        lines.push(format!(
            "{}\t{}\t{}\t{}",
            step.id, step.action, step.status, step.summary
        ));
    }

    lines.join("\n")
}

fn structured_success(root: &str, invocation: &str, stdout: String, extra: Value) -> Value {
    let mut object = serde_json::Map::new();
    object.insert("root".to_string(), json!(root));
    object.insert("invocation".to_string(), json!(invocation));
    object.insert("success".to_string(), json!(true));
    object.insert("timed_out".to_string(), json!(false));
    object.insert("exit_code".to_string(), json!(0));
    object.insert(
        "stdout".to_string(),
        json!(truncate_for_ui(stdout.trim(), 30_000)),
    );
    object.insert("stderr".to_string(), json!(""));
    if let Some(extra) = extra.as_object() {
        for (key, value) in extra {
            object.insert(key.clone(), value.clone());
        }
    }
    Value::Object(object)
}

fn structured_failure(root: &str, invocation: &str, error: &anyhow::Error) -> Value {
    json!({
        "root": root,
        "invocation": invocation,
        "success": false,
        "timed_out": false,
        "exit_code": 1,
        "stdout": "",
        "stderr": error.to_string(),
    })
}

async fn execute_cli_command(
    runtime: &CommandToolRuntime,
    root: &'static str,
    args: &[String],
    invocation: &str,
) -> Result<Value> {
    let mut cmd = TokioCommand::new(&runtime.current_exe);
    cmd.arg("--project-root")
        .arg(&runtime.project_root)
        .arg("--python")
        .arg(&runtime.python_bin)
        .arg(root)
        .args(args)
        .current_dir(&runtime.project_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let timeout_window = command_timeout_for_root(root);
    let timeout_secs = timeout_window.as_secs();
    let output = match timeout(timeout_window, cmd.output()).await {
        Ok(result) => result.context("failed to run internal PRISM command tool")?,
        Err(_) => {
            return Ok(json!({
                "root": root,
                "invocation": invocation,
                "success": false,
                "timed_out": true,
                "exit_code": Value::Null,
                "stdout": "",
                "stderr": format!("`{invocation}` is still running after {timeout_secs} seconds. Run the command directly if you need an interactive or long-lived session."),
            }));
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    Ok(json!({
        "root": root,
        "args": args,
        "invocation": invocation,
        "success": output.status.success(),
        "timed_out": false,
        "exit_code": output.status.code(),
        "stdout": truncate_for_ui(stdout.trim(), 30_000),
        "stderr": truncate_for_ui(stderr.trim(), 30_000),
    }))
}

/// Mint a best-effort loopback session token so a workflow's `tool` steps can
/// authenticate to the local node's `/api/tools/{name}/run` endpoint (auth- and
/// `ExecuteTools`-gated when the node is online). Injected into the workflow
/// context under the reserved `_node_token` key, which `run_tool_step` forwards
/// as a Bearer credential and strips from the returned context.
///
/// Returns `None` when the agent isn't logged in or the node is unreachable —
/// the workflow then runs tokenless (tool-free workflows are unaffected; an
/// online tool step fails honestly with 401 rather than silently succeeding).
pub(crate) async fn mint_agent_node_token() -> Option<String> {
    let paths = prism_runtime::PrismPaths::discover().ok()?;
    let state = paths.load_cli_state().ok()?;
    let user_id = state.credentials.as_ref()?.user_id.clone()?;
    prism_client::node_session::mint_local_session("http://127.0.0.1:7327", &user_id, None)
        .await
        .ok()
}

async fn execute_workflow_command(
    runtime: &CommandToolRuntime,
    execution: &CommandExecution,
    invocation: &str,
    policy: Option<&mut prism_policy::PolicyEngine>,
) -> Result<Value> {
    let result = match execution {
        CommandExecution::WorkflowList => {
            let specs = discover_workflows(Some(&runtime.project_root))?;
            let output = if specs.is_empty() {
                "No workflows found.".to_string()
            } else {
                specs
                    .values()
                    .map(|spec| {
                        format!("{}\t{}\t{}", spec.name, spec.command_name, spec.description)
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            structured_success(
                "workflow",
                invocation,
                output,
                json!({
                    "workflows": specs.values().map(|spec| {
                        json!({
                            "name": spec.name,
                            "command_name": spec.command_name,
                            "description": spec.description,
                            "source_path": spec.source_path,
                        })
                    }).collect::<Vec<_>>()
                }),
            )
        }
        CommandExecution::WorkflowShow { name } => {
            let specs = discover_workflows(Some(&runtime.project_root))?;
            match find_workflow(&specs, name) {
                Some(spec) => structured_success(
                    "workflow",
                    invocation,
                    render_workflow_spec(spec),
                    json!({
                        "workflow": {
                            "name": spec.name,
                            "command_name": spec.command_name,
                            "description": spec.description,
                            "source_path": spec.source_path,
                        }
                    }),
                ),
                None => structured_failure(
                    "workflow",
                    invocation,
                    &anyhow::anyhow!("Workflow not found: {name}"),
                ),
            }
        }
        CommandExecution::WorkflowRun {
            name,
            values,
            execute,
        } => {
            let specs = discover_workflows(Some(&runtime.project_root))?;
            // Authenticate the workflow's `tool` steps to the local node.
            // Execute mode only — dry runs plan without calling tools.
            let mut values = values.clone();
            // `or_insert`: an explicit workflow `_node_token` value wins over
            // the minted one (consistent with the slash/CLI paths).
            if *execute && let Some(token) = mint_agent_node_token().await {
                values.entry("_node_token".to_string()).or_insert(token);
            }
            // Point `llm_*` steps at the resolved chat endpoint (the SAME
            // config the agent's chat path uses). `or_insert` lets an explicit
            // workflow value win. Both modes: dry runs render the endpoint too.
            if let Some(base_url) = &runtime.llm_base_url {
                values
                    .entry("llm_base_url".to_string())
                    .or_insert_with(|| base_url.clone());
            }
            if let Some(model) = &runtime.llm_model {
                values
                    .entry("llm_model".to_string())
                    .or_insert_with(|| model.clone());
            }
            match find_workflow(&specs, name) {
                Some(spec) => match execute_workflow_with_policy(
                    spec,
                    &values,
                    *execute,
                    policy,
                    Some("agent"),
                    // Autonomous agent runs under the "agent" role — never a
                    // role smuggled through `values`.
                    Some("agent"),
                )
                .await
                {
                    Ok(result) => structured_success(
                        "workflow",
                        invocation,
                        render_workflow_result(spec, &result),
                        json!({
                            "workflow": result.workflow,
                            "mode": result.mode,
                            "steps": result.steps,
                            "context": result.context,
                        }),
                    ),
                    Err(error) => structured_failure("workflow", invocation, &error),
                },
                None => structured_failure(
                    "workflow",
                    invocation,
                    &anyhow::anyhow!("Workflow not found: {name}"),
                ),
            }
        }
        CommandExecution::Cli { .. }
        | CommandExecution::NotebookExec { .. }
        | CommandExecution::NotebookStatus
        | CommandExecution::NotebookReset => {
            unreachable!("workflow executor only handles workflow commands")
        }
    };

    Ok(result)
}

/// Tools that hit the LOCAL knowledge graph (the bundled Turso store behind
/// the local node) or the local node dashboard. Offering them while the node is
/// down produced dead-end failures for every semantic/graph call, so they are
/// only listed in the default catalog when the node is reachable. The specs
/// stay registered — `execute_command_tool` still resolves them — so
/// nothing breaks if an older transcript or client calls one by name.
const LOCAL_NODE_TOOLS: &[&str] = &["query", "query_local", "query_federated"];

/// Command tools EXCLUDED from the agent tool surface (but still resolvable
/// by `execute_command_tool` for backward compatibility with old transcripts).
///
/// Per `docs/PRISM_TOOL_SURFACE_AUDIT.md` (TASK 0). A spec lives here when it is
/// a **CLI-wrapper red herring** — a bare `prism <x>` passthrough whose only
/// input is an untyped `args: array<string>` and which duplicates one or more
/// *typed* siblings. These inflate the "126 tools" count and confuse model
/// selection (the model sees two tools for every verb). They stay as human CLI
/// commands; they are just not offered to the agent.
///
/// `agent` / `run` / `research` are the first batch (zero prompt coupling,
/// unambiguous typed siblings `run_submit` / `research_query`).
///
/// NOTE on `billing_balance`: an earlier revision of this list also excluded
/// the Rust `billing_balance` on the belief it 404'd on prod. Live re-probing
/// (2026-07-22, 3/3 runs) proved that wrong — bare `prism billing` (the exact
/// argv this tool emits, `args: vec![]`) returns the balance correctly
/// ("Balance: N credits ($N.NN)"). The transient 404 observed once was a
/// platform hiccup, not a persistent bug. Furthermore, even if exclusion were
/// warranted, it would be *ineffective*: dispatch routes by NAME via
/// `is_command_tool` (agent_loop.rs:1374, service.rs:328) BEFORE the Python
/// tool server, so the Rust path executes regardless of catalog membership.
/// So `billing_balance` is correctly NOT in this list.
///
/// Collapsing is incremental and gated — each batch is paired with a check
/// that nothing in the system prompt or catalog dangles (see the audit doc's
/// fix plan F2/F5). The specs stay registered so old sessions keep working.
const AGENT_SURFACE_EXCLUDED: &[&str] = &[
    "agent",  // CLI-wrapper red herring → no typed sibling needed (mgmt shell)
    "run",    // CLI-wrapper red herring → typed sibling `run_submit`
    "research", // CLI-wrapper red herring → typed sibling `research_query`
];

/// Cheap connectivity probe for the local node dashboard — the same
/// `127.0.0.1:7327` endpoint the boot checks use. TCP-level only: a refused
/// connection on localhost fails in microseconds, so building the catalog
/// is not delayed when the node is down.
fn local_node_reachable() -> bool {
    use std::net::{Ipv4Addr, SocketAddr, TcpStream};
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, 7327));
    TcpStream::connect_timeout(&addr, Duration::from_millis(150)).is_ok()
}

/// Default tool catalog entries: local-store tools appear only when the
/// local node is actually running.
pub fn command_tools() -> Vec<LoadedTool> {
    command_tools_filtered(local_node_reachable())
}

/// Deterministic variant of [`command_tools`] for callers (and tests) that
/// already know whether the local node is up.
#[must_use]
pub fn command_tools_filtered(local_node_online: bool) -> Vec<LoadedTool> {
    COMMAND_TOOLS
        .iter()
        .filter(|spec| local_node_online || !LOCAL_NODE_TOOLS.contains(&spec.name))
        // Exclude the collapsed/red-herring/broken tools from the AGENT surface.
        // Specs stay registered (execute_command_tool still resolves them) so old
        // transcripts keep working — this only changes what is *offered* to the
        // model. See docs/PRISM_TOOL_SURFACE_AUDIT.md.
        .filter(|spec| !AGENT_SURFACE_EXCLUDED.contains(&spec.name))
        .map(|spec| LoadedTool {
            name: spec.name.to_string(),
            description: spec.description.to_string(),
            input_schema: schema_for_spec(spec),
            requires_approval: spec.requires_approval,
            permission_mode: spec.permission_mode,
            source: Some("prism-command".to_string()),
            source_detail: None,
        })
        .collect()
}

pub fn is_command_tool(tool_name: &str) -> bool {
    spec_by_name(tool_name).is_some()
}

/// Whether a command tool is approval-gated (`None` if no such command tool).
/// Used by the single-tool executor to refuse approval-gated tools for callers
/// with nobody at the keyboard (e.g. the platform relay) — even when the tool
/// is hidden from the offered catalog (hidden ≠ unexecutable).
pub fn command_tool_requires_approval(tool_name: &str) -> Option<bool> {
    spec_by_name(tool_name).map(|spec| spec.requires_approval)
}

pub fn command_tool_preview(tool_name: &str, args: &Value) -> Option<String> {
    let spec = spec_by_name(tool_name)?;
    let execution = build_execution(spec, args).ok()?;
    Some(format_execution_invocation(&execution))
}

pub async fn execute_command_tool(
    runtime: &CommandToolRuntime,
    tool_name: &str,
    args: &Value,
    policy: Option<&mut prism_policy::PolicyEngine>,
) -> Result<Value> {
    let spec = spec_by_name(tool_name)
        .ok_or_else(|| anyhow::anyhow!("unknown internal command tool: {tool_name}"))?;
    let execution = build_execution(spec, args)?;
    let invocation = format_execution_invocation(&execution);

    match &execution {
        CommandExecution::Cli { root, args }
            if *root == "node" && is_node_lifecycle_subcommand(args) =>
        {
            execute_node_lifecycle(runtime, args, &invocation).await
        }
        CommandExecution::Cli { root, args } => {
            execute_cli_command(runtime, root, args, &invocation).await
        }
        CommandExecution::WorkflowList
        | CommandExecution::WorkflowShow { .. }
        | CommandExecution::WorkflowRun { .. } => {
            execute_workflow_command(runtime, &execution, &invocation, policy).await
        }
        CommandExecution::NotebookExec {
            code,
            timeout,
            reset,
            include_images_base64,
        } => {
            execute_notebook(
                runtime,
                code,
                *timeout,
                *reset,
                *include_images_base64,
                &invocation,
            )
            .await
        }
        CommandExecution::NotebookStatus => Ok(notebook_status_result(&invocation)),
        CommandExecution::NotebookReset => Ok(notebook_reset_result(&invocation).await),
    }
}

/// Run one notebook cell through the shared in-app kernel
/// ([`crate::notebook`]) — the same kernel the human's TUI notebook pane
/// drives, so agent and user share variables and history. Returns the
/// standard command-tool result shape plus structured notebook extras
/// (`result`, `images`, `execution_count`, `kernel_backend`).
async fn execute_notebook(
    runtime: &CommandToolRuntime,
    code: &str,
    timeout: Option<u64>,
    reset: bool,
    include_images_base64: bool,
    invocation: &str,
) -> Result<Value> {
    // Point the kernel at PRISM's managed interpreter + the project root, the
    // same environment the Python tool server runs in.
    crate::notebook::configure(runtime.python_bin.clone(), runtime.project_root.clone());
    if reset && let Err(error) = crate::notebook::reset().await {
        // The caller asked for a clean slate — running the cell anyway on the
        // old state would silently betray that, so fail honestly instead.
        return Ok(json!({
            "root": "notebook",
            "invocation": invocation,
            "success": false,
            "timed_out": false,
            "exit_code": 1,
            "stdout": "",
            "stderr": truncate_for_ui(&format!("reset failed: {error:#}"), 30_000),
        }));
    }

    let cell = match crate::notebook::execute(code, timeout, "agent").await {
        Ok(cell) => cell,
        // A spawn failure (e.g. Python missing) — surface the actionable
        // message as an honest failure, not a crash.
        Err(error) => {
            return Ok(json!({
                "root": "notebook",
                "invocation": invocation,
                "success": false,
                "timed_out": false,
                "exit_code": 1,
                "stdout": "",
                "stderr": truncate_for_ui(&format!("{error:#}"), 30_000),
            }));
        }
    };

    Ok(compose_notebook_result(
        &cell,
        &runtime.project_root.to_string_lossy(),
        include_images_base64,
        invocation,
        crate::notebook::status().backend.as_deref().unwrap_or(""),
    ))
}

/// Build the model-facing notebook_exec result JSON from a executed `Cell`.
///
/// Extracted from `execute_notebook` (FIX-1) so the agent-facing composition —
/// including the traceback filter — is unit-testable without a live runtime.
/// The filter runs on `cell.error` (where an uncaught exception's traceback
/// lives; `cell.stderr` is empty for a plain raise). The raw trace is never
/// pushed to the model: stdout and the `error` field both carry the FILTERED
/// trace. The human TUI pane is a separate path (`emit_notebook_cell` emits
/// the raw `Cell`) and does not go through here.
fn compose_notebook_result(
    cell: &crate::notebook::Cell,
    cwd: &str,
    include_images_base64: bool,
    invocation: &str,
    kernel_backend: &str,
) -> Value {
    // Compose the readable cell output the model sees in `stdout`.
    let mut display = String::new();
    if !cell.stdout.is_empty() {
        display.push_str(&cell.stdout);
    }
    if let Some(result) = &cell.result {
        if !display.is_empty() && !display.ends_with('\n') {
            display.push('\n');
        }
        display.push_str(&format!("=> {result}"));
    }
    for path in &cell.image_paths {
        if !display.is_empty() && !display.ends_with('\n') {
            display.push('\n');
        }
        display.push_str(&format!("[plot saved: {path}]"));
    }
    // NOTE: the raw cell.error is NOT pushed into display here. The agent-facing
    // FILTERED trace is appended below (FIX-1) so stdout and the `error` field
    // agree and the model never sees raw library frames.

    let images: Vec<Value> = cell
        .image_paths
        .iter()
        .map(|path| {
            let mut entry = json!({ "path": path });
            if include_images_base64 && let Ok(bytes) = std::fs::read(path) {
                use base64::Engine as _;
                entry["base64"] = json!(base64::engine::general_purpose::STANDARD.encode(bytes));
            }
            entry
        })
        .collect();

    // VS2-P1a (FIX-1): the agent-facing traceback filter. An uncaught exception
    // lands its traceback in `cell.error` (NOT cell.stderr, which is empty for a
    // plain raise — see notebook.rs::format_error). The filter must therefore
    // run on `cell.error`. We push the FILTERED trace into display/stdout (not
    // the raw error) and expose it as the model-facing `error` field too, so
    // the model sees ONE filtered trace — never the raw library frames. The
    // human TUI pane is a separate path (protocol.rs emit_notebook_cell emits
    // the raw Cell) and is untouched.
    let (filtered_error, elided_error_frames) = match &cell.error {
        Some(raw) => filter_notebook_traceback(raw, cwd),
        None => (String::new(), 0),
    };
    // stderr rarely carries a traceback, but filter it for parity (a
    // sys.stderr.write of a trace, or a warning that includes frames).
    // G5c: SUM (was .max) — when both channels elide frames, max under-reports.
    let (filtered_stderr, elided_stderr_frames) = filter_notebook_traceback(&cell.stderr, cwd);
    let traceback_elided_frames = elided_error_frames + elided_stderr_frames;

    // Push the FILTERED error (not raw) into display/stdout so the model-facing
    // stdout matches the filtered error field.
    if cell.error.is_some() && !filtered_error.is_empty() {
        if !display.is_empty() && !display.ends_with('\n') {
            display.push('\n');
        }
        display.push_str(&filtered_error);
    }
    // The model-facing `error` field is the filtered trace (when present).
    let model_error: Option<String> = if cell.error.is_some() && !filtered_error.is_empty() {
        Some(filtered_error)
    } else {
        cell.error.clone()
    };

    json!({
        "root": "notebook",
        "invocation": invocation,
        "success": cell.success,
        "timed_out": false,
        "exit_code": if cell.success { 0 } else { 1 },
        "stdout": truncate_for_ui(&display, 30_000),
        "stderr": truncate_for_ui(&filtered_stderr, 30_000),
        "traceback_elided_frames": traceback_elided_frames,
        "execution_count": cell.execution_count,
        "result": cell.result,
        "error": model_error,
        "images": images,
        "kernel_backend": kernel_backend,
    })
}

fn notebook_status_result(invocation: &str) -> Value {
    let status = crate::notebook::status();
    let summary = if status.running {
        format!(
            "Kernel running — backend {}, Python {}, {} cell(s) this session.\n{}",
            status.backend.as_deref().unwrap_or("?"),
            status.python.as_deref().unwrap_or("?"),
            status.cell_count,
            status.detail.as_deref().unwrap_or(""),
        )
    } else {
        format!(
            "Kernel not running — it starts on the first notebook_exec. {} cell(s) recorded.",
            status.cell_count
        )
    };
    json!({
        "root": "notebook",
        "invocation": invocation,
        "success": true,
        "timed_out": false,
        "exit_code": 0,
        "stdout": summary.trim(),
        "stderr": "",
        "running": status.running,
        "backend": status.backend,
        "python": status.python,
        "cell_count": status.cell_count,
    })
}

async fn notebook_reset_result(invocation: &str) -> Value {
    // Surface the real reason on failure (e.g. "kernel is busy running a
    // cell") — a generic "encountered an error" would hide the fix.
    let (success, stdout, stderr) = match crate::notebook::reset().await {
        Ok(()) => (
            true,
            "Notebook kernel reset — all cell state cleared.".to_string(),
            String::new(),
        ),
        Err(error) => (
            false,
            String::new(),
            format!("Notebook kernel reset failed: {error:#}"),
        ),
    };
    json!({
        "root": "notebook",
        "invocation": invocation,
        "success": success,
        "timed_out": false,
        "exit_code": if success { 0 } else { 1 },
        "stdout": stdout,
        "stderr": stderr,
    })
}

/// `node up` / `node down` need supervision, not a fire-and-forget subprocess:
/// `up` is a long-running daemon (the plain CLI path would block until the
/// 60s timeout and then kill it) and `down` should reap the child that `up`
/// left behind. Both route through [`crate::node_supervisor`] — the same
/// machinery behind the TUI's `/node up|stop|status` slash commands, so agent
/// and user drive one implementation.
fn is_node_lifecycle_subcommand(args: &[String]) -> bool {
    matches!(
        args.first().map(String::as_str),
        Some("up") | Some("down") | Some("stop")
    )
}

async fn execute_node_lifecycle(
    runtime: &CommandToolRuntime,
    args: &[String],
    invocation: &str,
) -> Result<Value> {
    let result = match args[0].as_str() {
        "up" => crate::node_supervisor::node_up(runtime, &args[1..]).await,
        // "down" (the CLI verb) and "stop" (the palette verb) are synonyms.
        _ => crate::node_supervisor::node_stop().await,
    };
    // Same output contract as execute_cli_command so tool consumers see one
    // shape regardless of how a node verb executes.
    let (success, stdout, stderr) = match result {
        Ok(report) => (true, report, String::new()),
        Err(error) => (false, String::new(), format!("{error:#}")),
    };
    Ok(json!({
        "root": "node",
        "args": args,
        "invocation": invocation,
        "success": success,
        "timed_out": false,
        "exit_code": if success { 0 } else { 1 },
        "stdout": truncate_for_ui(&stdout, 30_000),
        "stderr": truncate_for_ui(&stderr, 30_000),
    }))
}

pub fn to_definitions() -> Vec<ToolDefinition> {
    command_tools()
        .into_iter()
        .map(|tool| tool.to_definition())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── VS2-P1a: filter_notebook_traceback ─────────────────────────────

    #[test]
    fn p1a_notebook_filter_keeps_final_line_elides_library_frames() {
        let stderr = "Traceback (most recent call last):\n\
  File \"<string>\", line 4, in <module>\n\
    numpy.linalg.inv(mat)\n\
  File \"/opt/homebrew/lib/python3.14/site-packages/numpy/linalg/linalg.py\", line 540, in inv\n\
    ainv = _umath_linalg.inv(a)\n\
  File \"/opt/homebrew/lib/python3.14/site-packages/numpy/linalg/linalg.py\", line 100, in _commonType\n\
    raise ValueError(msg)\n\
ValueError: Singular matrix\n";
        let (filtered, n) = filter_notebook_traceback(stderr, "");
        assert!(filtered.contains("ValueError: Singular matrix"));
        assert!(filtered.contains("File \"<string>\""));
        assert!(n > 0, "library frames must be collapsed: {filtered}");
        assert!(
            !filtered.contains("site-packages/numpy"),
            "raw library path must not appear in agent-facing stderr"
        );
    }

    #[test]
    fn p1a_notebook_filter_unchanged_when_nothing_to_elide() {
        let stderr = "Traceback (most recent call last):\n\
  File \"<string>\", line 2, in <module>\n\
    1/0\n\
ZeroDivisionError: division by zero\n";
        let (filtered, n) = filter_notebook_traceback(stderr, "");
        assert_eq!(filtered, stderr, "verbatim when no library frames");
        assert_eq!(n, 0);
    }

    #[test]
    fn fix2_venv_under_cwd_library_frames_still_elided() {
        // FIX-2: a project-local venv (<cwd>/.venv/.../site-packages) CONTAINS
        // cwd, so the old order (is_user before is_library) kept every library
        // frame. Library classification must win regardless of cwd.
        let cwd = "/Users/me/project";
        let stderr = format!(
            "Traceback (most recent call last):\n\
  File \"{cwd}/my_script.py\", line 4, in <module>\n\
    numpy.linalg.inv(mat)\n\
  File \"{cwd}/.venv/lib/python3.14/site-packages/numpy/linalg/linalg.py\", line 540, in inv\n\
    ainv = _umath_linalg.inv(a)\n\
ValueError: Singular matrix\n"
        );
        let (filtered, n) = filter_notebook_traceback(&stderr, cwd);
        assert!(filtered.contains("my_script.py"), "user frame survives");
        assert!(filtered.contains("ValueError: Singular matrix"));
        assert!(
            !filtered.contains(".venv/lib"),
            "venv-under-cwd library frames must be elided, not kept as user: {filtered}"
        );
        assert!(n >= 1);
    }

    #[test]
    fn g3_ambiguous_marker_under_cwd_kept_as_user() {
        // G3: a user path containing an ambiguous substring (python3.) must NOT
        // be elided. <cwd>/scripts/port_python3.14_helpers.py was wrongly elided
        // by the unconditional substring match. Ambiguous markers now require
        // the frame to NOT be under cwd.
        let cwd = "/Users/me/project";
        let stderr = format!(
            "Traceback (most recent call last):\n\
  File \"{cwd}/scripts/port_python3.14_helpers.py\", line 1, in <module>\n\
    do_thing()\n\
  File \"/usr/lib/python3.14/site-packages/numpy/core.py\", line 2, in f\n\
    pass\n\
RuntimeError: boom\n"
        );
        let (filtered, n) = filter_notebook_traceback(&stderr, cwd);
        assert!(
            filtered.contains("port_python3.14_helpers.py"),
            "ambiguous marker under cwd must be kept as user frame: {filtered}"
        );
        assert!(
            !filtered.contains("site-packages/numpy"),
            "reliable library marker still elided: {filtered}"
        );
        assert!(n >= 1);
    }

    #[test]
    fn fix3_python311_plus_caret_lines_collapse_correctly() {
        // FIX-3: Python 3.11+ emits a caret/annotation line (`    ~~~~^~~`)
        // AFTER the source line. The old filter consumed only ONE indented
        // line per frame, so the caret fell through and flushed the library
        // run each iteration -> one marker PER library frame + orphaned carets,
        // and traceback_elided_frames counted markers not frames. This is the
        // production format (the repo runs python3.14).
        //
        // NOTE: built with concat! (not "\<newline>" continuation, which strips
        // leading whitespace) so the 4-space indents on source/caret lines are
        // real — matching the production traceback format.
        let stderr = concat!(
            "Traceback (most recent call last):\n",
            "  File \"<string>\", line 4, in <module>\n",
            "    numpy.linalg.inv(mat)\n",
            "    ~~~~~~~~~~~~~~~~~~~~~~\n",
            "  File \"/x/site-packages/numpy/linalg/a.py\", line 10, in inv\n",
            "    ainv = _umath_linalg.inv(a)\n",
            "    ~~~~~~~~~~~~~~~~~~~~~~~~~~~\n",
            "  File \"/x/site-packages/numpy/linalg/b.py\", line 20, in _common\n",
            "    raise ValueError(msg)\n",
            "    ~~~~~~~~~~~~~~~~~~~~~\n",
            "ValueError: Singular matrix\n",
        );
        let (filtered, n) = filter_notebook_traceback(stderr, "/cwd");
        // ONE collapse marker for the run of 2 library frames (not 2 markers).
        let marker_count = filtered.matches("library frame(s) elided").count();
        assert_eq!(
            marker_count, 1,
            "a run of consecutive library frames must collapse to ONE marker (got {marker_count}): {filtered}"
        );
        // The frame COUNT is reported, not the marker count.
        assert_eq!(
            n, 2,
            "traceback_elided_frames must be the FRAME count (2): got {n}"
        );
        // The final exception line survives.
        assert!(filtered.contains("ValueError: Singular matrix"));
        // The user frame survives (kept verbatim, not elided).
        assert!(filtered.contains("File \"<string>\""));
        // No raw library path leaks (the library frames + their carets are gone).
        assert!(
            !filtered.contains("site-packages/numpy"),
            "raw library path must not leak: {filtered}"
        );
    }

    #[test]
    fn g1_real_ipykernel_ansi_trace_is_filtered_not_passthrough() {
        // G1 (the gap the green gate missed): a REAL captured ipykernel 7.2 ANSI
        // traceback. The old filter was a verbatim NO-OP here — ipykernel wraps
        // `File` in ANSI (`\x1b[96mFile \x1b[39m`), so `starts_with("File ")`
        // never matched -> 0 frames -> raw site-packages frames leaked. This
        // fixture is the exact byte sequence from InteractiveTB.stb2text on a
        // failing numpy call (captured live, not hand-written).
        let ansi_trace = concat!(
            "\x1b[31m---------------------------------------------------------------------------\x1b[39m\n",
            "\x1b[31mIndexError\x1b[39m                                Traceback (most recent call last)\n",
            // ipykernel frame form: cyan "File ", green "path:lineno", cyan func, blue "()"
            "\x1b[36mFile \x1b[39m\x1b[32m/var/folders/.../site-packages/numpy/linalg/_linalg.py:648\x1b[39m, in \x1b[36minv\x1b[39m\x1b[34m()\x1b[39m\n",
            // source lines with line-number + caret coloring
            "\x1b[32m    646\x1b[39m     ainv = _umath_linalg.inv(a)\n",
            "\x1b[32m--> 648\x1b[39m     ainv = _umath_linalg.inv(a, signature=signature)\n",
            "\x1b[31mIndexError\x1b[39m: only integers, slices (`:`) are valid indices",
        );
        let (filtered, n) = filter_notebook_traceback(ansi_trace, "/cwd");
        // ANSI must be stripped (no escape bytes in the output).
        assert!(
            !filtered.contains('\u{1b}'),
            "ANSI must be stripped from the agent-facing trace: {filtered:?}"
        );
        // The final exception line survives.
        assert!(
            filtered.contains("IndexError: only integers"),
            "final exception line must survive: {filtered}"
        );
        // The library frame must be collapsed (not leaked raw).
        assert!(
            !filtered.contains("site-packages/numpy"),
            "raw library path must not leak post-strip: {filtered}"
        );
        assert!(n >= 1, "at least one library frame elided: got {n}");
    }

    /// The exact byte sequence PRISM's notebook sidecar sees for a plain
    /// library failure: a REAL ipykernel 7.2 / IPython 9.10 ANSI traceback,
    /// captured live via a `jupyter_client` kernel round-trip (the kernel's
    /// `error` message `traceback` array joined with `\n`, exactly as
    /// notebook.rs::format_error produces it). NOT a hand-written 4-space
    /// fixture — this is the `----> N` / `(...)` "Context" xmode form that the
    /// 4-space-only consumer leaked on (H1). A user cell calls
    /// `np.linalg.inv(np.zeros((2, 3)))`; numpy raises two frames deep.
    const REAL_IPYKERNEL_ARROW_TRACE: &str = concat!(
        "\x1b[31m---------------------------------------------------------------------------\x1b[39m\n",
        "\x1b[31mLinAlgError\x1b[39m                               Traceback (most recent call last)\n",
        "\x1b[36mCell\x1b[39m\x1b[36m \x1b[39m\x1b[32mIn[1]\x1b[39m\x1b[32m, line 2\x1b[39m\n",
        "\x1b[32m      1\x1b[39m \x1b[38;5;28;01mimport\x1b[39;00m\x1b[38;5;250m \x1b[39m\x1b[34;01mnumpy\x1b[39;00m\x1b[38;5;250m \x1b[39m\x1b[38;5;28;01mas\x1b[39;00m\x1b[38;5;250m \x1b[39m\x1b[34;01mnp\x1b[39;00m\n",
        "\x1b[32m----> \x1b[39m\x1b[32m2\x1b[39m \x1b[43mnp\x1b[49m\x1b[43m.\x1b[49m\x1b[43mlinalg\x1b[49m\x1b[43m.\x1b[49m\x1b[43minv\x1b[49m\x1b[43m(\x1b[49m\x1b[43mnp\x1b[49m\x1b[43m.\x1b[49m\x1b[43mzeros\x1b[49m\x1b[43m(\x1b[49m\x1b[43m(\x1b[49m\x1b[32;43m2\x1b[39;49m\x1b[43m,\x1b[49m\x1b[43m \x1b[49m\x1b[32;43m3\x1b[39;49m\x1b[43m)\x1b[49m\x1b[43m)\x1b[49m\x1b[43m)\x1b[49m\n",
        "\n",
        "\x1b[36mFile \x1b[39m\x1b[32m/opt/homebrew/lib/python3.14/site-packages/numpy/linalg/_linalg.py:642\x1b[39m, in \x1b[36minv\x1b[39m\x1b[34m(a)\x1b[39m\n",
        "\x1b[32m    538\x1b[39m \x1b[38;5;250m\x1b[39m\x1b[33;03m\"\"\"\x1b[39;00m\n",
        "\x1b[32m    539\x1b[39m \x1b[33;03mCompute the inverse of a matrix.\x1b[39;00m\n",
        "\x1b[32m    540\x1b[39m \n",
        "\x1b[32m   (...)\x1b[39m\x1b[32m    639\x1b[39m \n",
        "\x1b[32m    640\x1b[39m \x1b[33;03m\"\"\"\x1b[39;00m\n",
        "\x1b[32m    641\x1b[39m a, wrap = _makearray(a)\n",
        "\x1b[32m--> \x1b[39m\x1b[32m642\x1b[39m \x1b[43m_assert_stacked_square\x1b[49m\x1b[43m(\x1b[49m\x1b[43ma\x1b[49m\x1b[43m)\x1b[49m\n",
        "\x1b[32m    643\x1b[39m t, result_t = _commonType(a)\n",
        "\x1b[32m    645\x1b[39m signature = \x1b[33m'\x1b[39m\x1b[33mD->D\x1b[39m\x1b[33m'\x1b[39m \x1b[38;5;28;01mif\x1b[39;00m isComplexType(t) \x1b[38;5;28;01melse\x1b[39;00m \x1b[33m'\x1b[39m\x1b[33md->d\x1b[39m\x1b[33m'\x1b[39m\n",
        "\n",
        "\x1b[36mFile \x1b[39m\x1b[32m/opt/homebrew/lib/python3.14/site-packages/numpy/linalg/_linalg.py:246\x1b[39m, in \x1b[36m_assert_stacked_square\x1b[39m\x1b[34m(*arrays)\x1b[39m\n",
        "\x1b[32m    243\x1b[39m     \x1b[38;5;28;01mraise\x1b[39;00m LinAlgError(\x1b[33m'\x1b[39m\x1b[38;5;132;01m%d\x1b[39;00m\x1b[33m-dimensional array given. Array must be \x1b[39m\x1b[33m'\x1b[39m\n",
        "\x1b[32m    244\x1b[39m             \x1b[33m'\x1b[39m\x1b[33mat least two-dimensional\x1b[39m\x1b[33m'\x1b[39m % a.ndim)\n",
        "\x1b[32m    245\x1b[39m \x1b[38;5;28;01mif\x1b[39;00m m != n:\n",
        "\x1b[32m--> \x1b[39m\x1b[32m246\x1b[39m     \x1b[38;5;28;01mraise\x1b[39;00m LinAlgError(\x1b[33m'\x1b[39m\x1b[33mLast 2 dimensions of the array must be square\x1b[39m\x1b[33m'\x1b[39m)\n",
        "\n",
        "\x1b[31mLinAlgError\x1b[39m: Last 2 dimensions of the array must be square",
    );

    #[test]
    fn h1_real_ipykernel_arrow_body_no_library_source_leaks() {
        // H1 (the gap the green gate missed TWICE): IPython's default "Context"
        // xmode renders the failing line as a COLUMN-0 arrow (`--> 642 ...`),
        // not a 4-space line. The old consumer broke there and leaked the
        // library's raw SOURCE verbatim next to the elision marker. This asserts
        // against a REAL ipykernel capture (see REAL_IPYKERNEL_ARROW_TRACE) that
        // NO library source survives — only the elision marker + final line.
        let (filtered, n) = filter_notebook_traceback(REAL_IPYKERNEL_ARROW_TRACE, "/some/project");

        // ANSI stripped.
        assert!(
            !filtered.contains('\u{1b}'),
            "ANSI must be stripped: {filtered:?}"
        );
        // Final exception line ALWAYS survives.
        assert!(
            filtered.contains("LinAlgError: Last 2 dimensions of the array must be square"),
            "final exception line must survive: {filtered}"
        );
        // The user's Cell frame (their own code) is KEPT.
        assert!(
            filtered.contains("Cell In[1], line 2"),
            "user cell frame must be kept: {filtered}"
        );
        assert!(
            filtered.contains("np.linalg.inv(np.zeros((2, 3)))"),
            "user cell source must be kept: {filtered}"
        );
        // The two library frames collapse to exactly ONE marker.
        let marker_count = filtered.matches("library frame(s) elided").count();
        assert_eq!(
            marker_count, 1,
            "consecutive library frames collapse to ONE marker: {filtered}"
        );
        assert_eq!(n, 2, "both library frames counted: got {n}");
        // NO library SOURCE line may leak — this is the H1 assertion the old
        // green-but-leaking tests never made. Every one of these is a real line
        // from inside numpy that the old arrow-broken consumer leaked verbatim.
        for leaked in [
            "_assert_stacked_square(a)",            // `--> 642` arrow body
            "raise LinAlgError('Last 2 dimensions", // `--> 246` arrow body
            "t, result_t = _commonType(a)",         // plain library source
            "signature = 'D->D'",                   // plain library source
            "Compute the inverse of a matrix.",     // library docstring fragment
            "a, wrap = _makearray(a)",              // plain library source
            "site-packages/numpy",                  // raw library path
        ] {
            assert!(
                !filtered.contains(leaked),
                "library source/path leaked into agent-facing trace: {leaked:?}\n---\n{filtered}"
            );
        }
    }

    /// REAL ipykernel capture (same live round-trip as REAL_IPYKERNEL_ARROW_TRACE)
    /// where the user's helper lives UNDER $HOME, so IPython tilde-compresses its
    /// frame to `File ~/.prism-review-tmp-g3/scripts/port_python3.14_helpers.py:3`.
    /// The filename contains the ambiguous marker `python3.`, so without the H2
    /// tilde normalization the absolute-cwd contains-check misses it and the USER
    /// frame is misclassified library + elided.
    const REAL_IPYKERNEL_TILDE_TRACE: &str = concat!(
        "\x1b[31m---------------------------------------------------------------------------\x1b[39m\n",
        "\x1b[31mLinAlgError\x1b[39m                               Traceback (most recent call last)\n",
        "\x1b[36mCell\x1b[39m\x1b[36m \x1b[39m\x1b[32mIn[1]\x1b[39m\x1b[32m, line 4\x1b[39m\n",
        "\x1b[32m      2\x1b[39m p = os.path.join(os.getcwd(), \x1b[33m'\x1b[39m\x1b[33mscripts\x1b[39m\x1b[33m'\x1b[39m, \x1b[33m'\x1b[39m\x1b[33mport_python3.14_helpers.py\x1b[39m\x1b[33m'\x1b[39m)\n",
        "\x1b[32m      3\x1b[39m exec(\x1b[38;5;28mcompile\x1b[39m(\x1b[38;5;28mopen\x1b[39m(p).read(), p, \x1b[33m'\x1b[39m\x1b[33mexec\x1b[39m\x1b[33m'\x1b[39m))\n",
        "\x1b[32m----> \x1b[39m\x1b[32m4\x1b[39m \x1b[43mdo_thing\x1b[49m\x1b[43m(\x1b[49m\x1b[43m)\x1b[49m\n",
        "\n",
        "\x1b[36mFile \x1b[39m\x1b[32m~/.prism-review-tmp-g3/scripts/port_python3.14_helpers.py:3\x1b[39m, in \x1b[36mdo_thing\x1b[39m\x1b[34m()\x1b[39m\n",
        "\x1b[32m      2\x1b[39m \x1b[38;5;28;01mdef\x1b[39;00m\x1b[38;5;250m \x1b[39m\x1b[34mdo_thing\x1b[39m():\n",
        "\x1b[32m----> \x1b[39m\x1b[32m3\x1b[39m     \x1b[38;5;28;01mreturn\x1b[39;00m \x1b[43mnp\x1b[49m\x1b[43m.\x1b[49m\x1b[43mlinalg\x1b[49m\x1b[43m.\x1b[49m\x1b[43minv\x1b[49m\x1b[43m(\x1b[49m\x1b[43mnp\x1b[49m\x1b[43m.\x1b[49m\x1b[43mzeros\x1b[49m\x1b[43m(\x1b[49m\x1b[43m(\x1b[49m\x1b[32;43m2\x1b[39;49m\x1b[43m,\x1b[49m\x1b[43m \x1b[49m\x1b[32;43m3\x1b[39;49m\x1b[43m)\x1b[49m\x1b[43m)\x1b[49m\x1b[43m)\x1b[49m\n",
        "\n",
        "\x1b[36mFile \x1b[39m\x1b[32m/opt/homebrew/lib/python3.14/site-packages/numpy/linalg/_linalg.py:642\x1b[39m, in \x1b[36minv\x1b[39m\x1b[34m(a)\x1b[39m\n",
        "\x1b[32m    641\x1b[39m a, wrap = _makearray(a)\n",
        "\x1b[32m--> \x1b[39m\x1b[32m642\x1b[39m \x1b[43m_assert_stacked_square\x1b[49m\x1b[43m(\x1b[49m\x1b[43ma\x1b[49m\x1b[43m)\x1b[49m\n",
        "\x1b[32m    643\x1b[39m t, result_t = _commonType(a)\n",
        "\n",
        "\x1b[36mFile \x1b[39m\x1b[32m/opt/homebrew/lib/python3.14/site-packages/numpy/linalg/_linalg.py:246\x1b[39m, in \x1b[36m_assert_stacked_square\x1b[39m\x1b[34m(*arrays)\x1b[39m\n",
        "\x1b[32m    245\x1b[39m \x1b[38;5;28;01mif\x1b[39;00m m != n:\n",
        "\x1b[32m--> \x1b[39m\x1b[32m246\x1b[39m     \x1b[38;5;28;01mraise\x1b[39;00m LinAlgError(\x1b[33m'\x1b[39m\x1b[33mLast 2 dimensions of the array must be square\x1b[39m\x1b[33m'\x1b[39m)\n",
        "\n",
        "\x1b[31mLinAlgError\x1b[39m: Last 2 dimensions of the array must be square",
    );

    #[test]
    fn h2_real_ipykernel_tilde_userfile_is_kept_not_elided() {
        // H2: a user file UNDER $HOME whose name contains the ambiguous marker
        // `python3.`. IPython emits its frame tilde-compressed (`File ~/…`). The
        // absolute-cwd contains-check misses the `~` form, so the pre-fix filter
        // elided the user frame. cwd is <$HOME>/.prism-review-tmp-g3 so its
        // `~`-compressed form (`~/.prism-review-tmp-g3`) matches the frame path.
        let Ok(home) = std::env::var("HOME") else {
            eprintln!("SKIP: HOME unset");
            return;
        };
        if home.is_empty() {
            eprintln!("SKIP: HOME empty");
            return;
        }
        let cwd = format!("{home}/.prism-review-tmp-g3");
        let (filtered, n) = filter_notebook_traceback(REAL_IPYKERNEL_TILDE_TRACE, &cwd);

        // The USER frame under $HOME is KEPT (the H2 assertion) — its filename
        // and its own source both reach the model.
        assert!(
            filtered.contains("port_python3.14_helpers.py"),
            "tilde-path user frame under $HOME must be KEPT, not elided: {filtered}"
        );
        assert!(
            filtered.contains("return np.linalg.inv(np.zeros((2, 3)))"),
            "the user frame's own source must survive: {filtered}"
        );
        // The final exception line ALWAYS survives.
        assert!(
            filtered.contains("LinAlgError: Last 2 dimensions of the array must be square"),
            "final exception line must survive: {filtered}"
        );
        // Only the TWO numpy site-packages frames are elided (the user frame is
        // NOT counted). Pre-H2 this was 3 (the user frame wrongly included).
        assert_eq!(
            n, 2,
            "exactly the 2 numpy library frames elided (user frame kept): got {n}"
        );
        // And no numpy library source leaks (H1 still holds on the tilde trace).
        for leaked in [
            "_assert_stacked_square(a)",
            "t, result_t = _commonType(a)",
            "site-packages/numpy",
        ] {
            assert!(
                !filtered.contains(leaked),
                "library source/path leaked: {leaked:?}\n---\n{filtered}"
            );
        }
    }

    /// REAL PEP 654 ExceptionGroup capture from ipykernel (`fresh11_group_stderr.txt`,
    /// left by the P1-fix-3 reviewers): a user cell raises an `ExceptionGroup`
    /// whose outer traceback and both sub-exceptions run through a fake
    /// `site-packages/fakelib`. Every line carries a CPython group gutter
    /// (`  | ` / `    | `) or is a group divider (`+---- N ----`). Embedded
    /// BYTE-FOR-BYTE. Pre-F2 the whole group leaked verbatim (`|   File` never
    /// matched the frame check).
    const REAL_IPYKERNEL_GROUP_TRACE: &str = r#"  + Exception Group Traceback (most recent call last):
  |   File "/opt/homebrew/lib/python3.14/site-packages/IPython/core/interactiveshell.py", line 3701, in run_code
  |     exec(code_obj, self.user_global_ns, self.user_ns)
  |     ~~~~^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
  |   File "/var/folders/g9/ctk1s9tx0j79d2_bq782vnpm0000gn/T/ipykernel_33660/2764102629.py", line 3, in <module>
  |     group_entry(0)
  |     ~~~~~~~~~~~^^^
  |   File "/private/tmp/claude-501/-Users-siddharthakovid-Downloads/8394e7bd-d04b-41d8-97bf-ff6832a752dc/scratchpad/vs2fix3/site-packages/fakelib/core.py", line 105, in group_entry
  |     raise ExceptionGroup("several fakelib failures", errs)  # SECRET_SRC_GROUP_RAISE
  |     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
  | ExceptionGroup: several fakelib failures (2 sub-exceptions)
  +-+---------------- 1 ----------------
    | Traceback (most recent call last):
    |   File "/private/tmp/claude-501/-Users-siddharthakovid-Downloads/8394e7bd-d04b-41d8-97bf-ff6832a752dc/scratchpad/vs2fix3/site-packages/fakelib/core.py", line 102, in group_entry
    |     _inner_transform(i)  # SECRET_SRC_GROUP_INNER
    |     ~~~~~~~~~~~~~~~~^^^
    |   File "/private/tmp/claude-501/-Users-siddharthakovid-Downloads/8394e7bd-d04b-41d8-97bf-ff6832a752dc/scratchpad/vs2fix3/site-packages/fakelib/core.py", line 39, in _inner_transform
    |     raise ValueError("fakelib inner transform exploded")  # SECRET_SRC_RAISE
    |     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
    | ValueError: fakelib inner transform exploded
    +---------------- 2 ----------------
    | Traceback (most recent call last):
    |   File "/private/tmp/claude-501/-Users-siddharthakovid-Downloads/8394e7bd-d04b-41d8-97bf-ff6832a752dc/scratchpad/vs2fix3/site-packages/fakelib/core.py", line 102, in group_entry
    |     _inner_transform(i)  # SECRET_SRC_GROUP_INNER
    |     ~~~~~~~~~~~~~~~~^^^
    |   File "/private/tmp/claude-501/-Users-siddharthakovid-Downloads/8394e7bd-d04b-41d8-97bf-ff6832a752dc/scratchpad/vs2fix3/site-packages/fakelib/core.py", line 39, in _inner_transform
    |     raise ValueError("fakelib inner transform exploded")  # SECRET_SRC_RAISE
    |     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
    | ValueError: fakelib inner transform exploded
    +------------------------------------
"#;

    /// REAL PEP 654 ExceptionGroup capture from `python -c` (`stdlib2_group.txt`).
    /// Same shape, but the user frame is `File "<string>"` (no ipykernel temp).
    /// Embedded BYTE-FOR-BYTE.
    const REAL_STDLIB_GROUP_TRACE: &str = r#"  + Exception Group Traceback (most recent call last):
  |   File "<string>", line 3, in <module>
  |     group_entry(0)
  |     ~~~~~~~~~~~^^^
  |   File "/private/tmp/claude-501/-Users-siddharthakovid-Downloads/8394e7bd-d04b-41d8-97bf-ff6832a752dc/scratchpad/vs2fix3/site-packages/fakelib/core.py", line 105, in group_entry
  |     raise ExceptionGroup("several fakelib failures", errs)  # SECRET_SRC_GROUP_RAISE
  |     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
  | ExceptionGroup: several fakelib failures (2 sub-exceptions)
  +-+---------------- 1 ----------------
    | Traceback (most recent call last):
    |   File "/private/tmp/claude-501/-Users-siddharthakovid-Downloads/8394e7bd-d04b-41d8-97bf-ff6832a752dc/scratchpad/vs2fix3/site-packages/fakelib/core.py", line 102, in group_entry
    |     _inner_transform(i)  # SECRET_SRC_GROUP_INNER
    |     ~~~~~~~~~~~~~~~~^^^
    |   File "/private/tmp/claude-501/-Users-siddharthakovid-Downloads/8394e7bd-d04b-41d8-97bf-ff6832a752dc/scratchpad/vs2fix3/site-packages/fakelib/core.py", line 39, in _inner_transform
    |     raise ValueError("fakelib inner transform exploded")  # SECRET_SRC_RAISE
    |     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
    | ValueError: fakelib inner transform exploded
    +---------------- 2 ----------------
    | Traceback (most recent call last):
    |   File "/private/tmp/claude-501/-Users-siddharthakovid-Downloads/8394e7bd-d04b-41d8-97bf-ff6832a752dc/scratchpad/vs2fix3/site-packages/fakelib/core.py", line 102, in group_entry
    |     _inner_transform(i)  # SECRET_SRC_GROUP_INNER
    |     ~~~~~~~~~~~~~~~~^^^
    |   File "/private/tmp/claude-501/-Users-siddharthakovid-Downloads/8394e7bd-d04b-41d8-97bf-ff6832a752dc/scratchpad/vs2fix3/site-packages/fakelib/core.py", line 39, in _inner_transform
    |     raise ValueError("fakelib inner transform exploded")  # SECRET_SRC_RAISE
    |     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
    | ValueError: fakelib inner transform exploded
    +------------------------------------
"#;

    // Shared assertions for a filtered ExceptionGroup: no library SOURCE or path
    // leaks, but the group STRUCTURE (header + `+---- N ----` dividers) and every
    // final exception line survive (fail-open collapse, never a mask).
    fn assert_group_filtered(filtered: &str, n: usize) {
        // The library FRAMES collapse: no site-packages path, no raw library
        // source, none of the SECRET_SRC_* markers embedded in library source.
        for leaked in [
            "site-packages/fakelib",
            "site-packages/IPython",
            "SECRET_SRC_GROUP_RAISE",
            "SECRET_SRC_GROUP_INNER",
            "SECRET_SRC_RAISE",
            "_inner_transform(i)",
            "raise ValueError(\"fakelib inner transform exploded\")",
            "raise ExceptionGroup(",
            "exec(code_obj",
        ] {
            assert!(
                !filtered.contains(leaked),
                "ExceptionGroup library source/path leaked: {leaked:?}\n---\n{filtered}"
            );
        }
        // Group STRUCTURE survives: the header and BOTH sub-exception dividers +
        // the closing divider.
        assert!(
            filtered.contains("Exception Group Traceback (most recent call last):"),
            "group header must survive: {filtered}"
        );
        assert!(
            filtered.contains("---------------- 1 ----------------"),
            "sub-exception 1 divider must survive: {filtered}"
        );
        assert!(
            filtered.contains("---------------- 2 ----------------"),
            "sub-exception 2 divider must survive: {filtered}"
        );
        assert!(
            filtered.contains("+------------------------------------"),
            "closing divider must survive: {filtered}"
        );
        // Final exception lines survive (outer group + each sub-exception).
        assert!(
            filtered.contains("ExceptionGroup: several fakelib failures (2 sub-exceptions)"),
            "outer ExceptionGroup final line must survive: {filtered}"
        );
        assert!(
            filtered.contains("ValueError: fakelib inner transform exploded"),
            "sub-exception final line must survive: {filtered}"
        );
        // Something WAS collapsed.
        assert!(n >= 1, "at least one library frame elided: got {n}");
    }

    #[test]
    fn f2_real_ipykernel_exception_group_collapses_library_keeps_structure() {
        // F2 (PEP 654): the last leak variant. Every group line is gutter-prefixed
        // (`  | File ...`), so the pre-fix frame check missed all of them and the
        // ENTIRE group traceback leaked verbatim. The margin-strip must collapse
        // the site-packages frames while KEEPING the group dividers + final lines.
        // Sanity: the fixture really is the raw capture (secrets present).
        assert!(REAL_IPYKERNEL_GROUP_TRACE.contains("SECRET_SRC_GROUP_RAISE"));
        assert!(REAL_IPYKERNEL_GROUP_TRACE.contains("site-packages/fakelib"));
        let (filtered, n) = filter_notebook_traceback(REAL_IPYKERNEL_GROUP_TRACE, "/some/project");
        assert_group_filtered(&filtered, n);
        // The ipykernel outer block: IPython + fakelib (2) library frames plus, in
        // each of the 2 sub-exceptions, 2 fakelib frames -> 6 library frames total.
        assert_eq!(
            n, 6,
            "all six library frames across the group elided: got {n}\n{filtered}"
        );
    }

    #[test]
    fn f2_real_stdlib_exception_group_collapses_library_keeps_structure() {
        // F2: the `python -c` form (stderr). Same margin logic; the user frame is
        // `File "<string>"` (kept), the fakelib frames collapse.
        assert!(REAL_STDLIB_GROUP_TRACE.contains("SECRET_SRC_RAISE"));
        let (filtered, n) = filter_notebook_traceback(REAL_STDLIB_GROUP_TRACE, "");
        assert_group_filtered(&filtered, n);
        // The user `<string>` frame is KEPT (not a library frame).
        assert!(
            filtered.contains("File \"<string>\""),
            "user <string> frame must survive: {filtered}"
        );
        // Outer block: 1 fakelib frame; each sub-exception: 2 -> 5 total.
        assert_eq!(n, 5, "all five library frames elided: got {n}\n{filtered}");
    }

    #[test]
    fn f3_cwd_tilde_form_trims_trailing_slash_home() {
        // F3: a trailing-slash $HOME must still yield IPython's `~/proj`
        // spelling. Before the trim, HOME=/Users/x/ produced `~proj` (no
        // separator), so the under-$HOME user frame `File ~/proj/foo.py` didn't
        // match cwd_tilde and was over-elided. Pure helper -> no racy env mutation.
        assert_eq!(
            cwd_tilde_form("/Users/x/", "/Users/x/proj"),
            Some("~/proj".to_string()),
            "trailing-slash HOME must keep the `/` separator"
        );
        // No-trailing-slash case unchanged.
        assert_eq!(
            cwd_tilde_form("/Users/x", "/Users/x/proj"),
            Some("~/proj".to_string())
        );
        // Multiple trailing slashes also trimmed.
        assert_eq!(
            cwd_tilde_form("/Users/x//", "/Users/x/proj"),
            Some("~/proj".to_string())
        );
        // cwd IS home -> `~`.
        assert_eq!(
            cwd_tilde_form("/Users/x/", "/Users/x"),
            Some("~".to_string())
        );
        // cwd not under home -> None.
        assert_eq!(cwd_tilde_form("/Users/x", "/tmp/proj"), None);
        // Empty / root HOME -> None (no spurious `~/…` matching everything).
        assert_eq!(cwd_tilde_form("", "/Users/x/proj"), None);
        assert_eq!(cwd_tilde_form("/", "/Users/x/proj"), None);
    }

    #[test]
    fn g1_strip_ansi_helper() {
        assert_eq!(strip_ansi("\x1b[96mFile \x1b[39mhello"), "File hello");
        assert_eq!(strip_ansi("\x1b[1;31mErr\x1b[39m: msg"), "Err: msg");
        assert_eq!(strip_ansi("\x1b[38;5;15mX\x1b[39m"), "X");
        assert_eq!(strip_ansi("no ansi here"), "no ansi here");
        // multibyte safe
        assert_eq!(strip_ansi("\x1b[32m✓\x1b[39m ok"), "✓ ok");
    }

    #[test]
    fn p1a_notebook_filter_keeps_both_final_lines_for_chained_exception() {
        let stderr = "Traceback (most recent call last):\n\
  File \"<string>\", line 2, in <module>\n\
    int(\"abc\")\n\
ValueError: invalid literal for int() with base 10: 'abc'\n\
\n\
During handling of the above exception, another exception occurred:\n\
\n\
Traceback (most recent call last):\n\
  File \"<string>\", line 4, in <module>\n\
    raise RuntimeError(\"wrapped\")\n\
RuntimeError: wrapped\n";
        let (filtered, _) = filter_notebook_traceback(stderr, "");
        // VS1/F2 lesson: for a chain, BOTH final blocks survive.
        assert!(filtered.contains("ValueError: invalid literal"));
        assert!(filtered.contains("During handling"));
        assert!(filtered.contains("RuntimeError: wrapped"));
    }

    #[test]
    fn g4_chained_exception_collapses_library_frames_in_both_blocks() {
        // G4: library frames in the SECOND exception block must be collapsed
        // too. The old code kept the entire chain tail verbatim after the
        // marker, leaking raw site-packages frames. Now each block is filtered.
        let stderr = concat!(
            "Traceback (most recent call last):\n",
            "  File \"<string>\", line 2, in <module>\n",
            "    int(\"abc\")\n",
            "  File \"/x/site-packages/numpy/a.py\", line 1, in f\n",
            "    pass\n",
            "  File \"/x/site-packages/numpy/b.py\", line 2, in g\n",
            "    pass\n",
            "ValueError: invalid literal for int() with base 10: 'abc'\n",
            "\n",
            "During handling of the above exception, another exception occurred:\n",
            "\n",
            "Traceback (most recent call last):\n",
            "  File \"<string>\", line 4, in <module>\n",
            "    raise RuntimeError(\"wrapped\")\n",
            "  File \"/x/site-packages/numpy/c.py\", line 3, in h\n",
            "    pass\n",
            "  File \"/x/site-packages/numpy/d.py\", line 4, in k\n",
            "    pass\n",
            "RuntimeError: wrapped\n",
        );
        let (filtered, n) = filter_notebook_traceback(stderr, "/cwd");
        assert!(
            !filtered.contains("site-packages/numpy"),
            "library frames must collapse in BOTH chained blocks: {filtered}"
        );
        assert!(filtered.contains("ValueError: invalid literal"));
        assert!(filtered.contains("RuntimeError: wrapped"));
        assert!(
            n >= 4,
            "all 4 library frames across both blocks counted: got {n}"
        );
    }

    #[test]
    fn p1a_notebook_filter_no_traceback_passthrough() {
        // A plain warning (no "Traceback" header) passes through verbatim.
        let s = "DeprecationWarning: foo is deprecated\n";
        let (filtered, n) = filter_notebook_traceback(s, "");
        assert_eq!(filtered, s);
        assert_eq!(n, 0);
    }

    #[test]
    fn p1a_notebook_filter_empty_input() {
        let (filtered, n) = filter_notebook_traceback("", "");
        assert_eq!(filtered, "");
        assert_eq!(n, 0);
    }

    // ── VS2-P1 FIX-1: the REAL composition (cell.error channel) ────────
    //
    // The original tests only exercised the standalone filter_notebook_traceback
    // fn. The adversarial review found the composition wired the filter to
    // cell.stderr (empty for a raise) while the traceback leaked raw via
    // cell.error -> stdout + error. These tests hit compose_notebook_result
    // (the actual model-facing path) with a Cell shaped like a real raise.

    fn raising_cell(raw_traceback: &str) -> crate::notebook::Cell {
        // Mirror what notebook.rs::format_error produces for an uncaught
        // exception: stderr empty, error = Some(<joined traceback>).
        crate::notebook::Cell {
            execution_count: 1,
            origin: "agent".to_string(),
            code: "raise ValueError('boom')".to_string(),
            stdout: String::new(),
            stderr: String::new(),
            result: None,
            image_paths: Vec::new(),
            error: Some(raw_traceback.to_string()),
            success: false,
        }
    }

    #[test]
    fn fix1_model_facing_error_is_filtered_not_raw() {
        // A traceback with library frames in cell.error (stderr EMPTY — the real
        // raise shape). The model-facing "error" field must contain the FILTERED
        // trace (final line + elided marker), NOT the raw library paths.
        let tb = "Traceback (most recent call last):\n\
  File \"<string>\", line 4, in <module>\n\
    numpy.linalg.inv(mat)\n\
  File \"/opt/homebrew/lib/python3.14/site-packages/numpy/linalg/linalg.py\", line 540, in inv\n\
    ainv = _umath_linalg.inv(a)\n\
ValueError: Singular matrix\n";
        let cell = raising_cell(tb);
        let result = compose_notebook_result(&cell, "/project", false, "cell[0]", "builtin");

        let model_error = result["error"].as_str().unwrap_or("");
        assert!(
            model_error.contains("ValueError: Singular matrix"),
            "final error line must reach the model: {model_error}"
        );
        assert!(
            !model_error.contains("site-packages/numpy"),
            "raw library path must NOT reach the model via error: {model_error}"
        );
    }

    #[test]
    fn fix1_model_facing_stdout_carries_filtered_trace_not_raw() {
        // stdout (display) previously push_str'd the RAW cell.error. Now it must
        // carry the filtered trace too.
        let tb = "Traceback (most recent call last):\n\
  File \"<string>\", line 1, in <module>\n\
    f()\n\
  File \"/x/site-packages/numpy/core.py\", line 1, in f\n\
    pass\n\
RuntimeError: boom\n";
        let cell = raising_cell(tb);
        let result = compose_notebook_result(&cell, "/project", false, "cell[0]", "builtin");
        let stdout = result["stdout"].as_str().unwrap_or("");
        assert!(stdout.contains("RuntimeError: boom"));
        assert!(
            !stdout.contains("site-packages/numpy"),
            "raw library path must NOT reach the model via stdout: {stdout}"
        );
    }

    #[test]
    fn fix1_stderr_empty_raise_still_reports_elided_frames() {
        // The bug: stderr-empty raise -> filter on stderr -> elided=0 (false
        // "nothing to filter"). Now the error channel drives the count.
        let tb = "Traceback (most recent call last):\n\
  File \"<string>\", line 1, in <module>\n\
    f()\n\
  File \"/x/site-packages/numpy/a.py\", line 1, in f\n\
    pass\n\
  File \"/x/site-packages/numpy/b.py\", line 2, in g\n\
    pass\n\
RuntimeError: boom\n";
        let cell = raising_cell(tb);
        let result = compose_notebook_result(&cell, "/project", false, "cell[0]", "builtin");
        let elided = result["traceback_elided_frames"].as_u64().unwrap_or(0);
        assert!(
            elided >= 2,
            "elided frame count must reflect the error channel, not the empty stderr: got {elided}"
        );
    }

    #[test]
    fn fix1_successful_cell_no_error_passthrough() {
        // A successful cell (no error) must compose unchanged — no spurious
        // filtered-trace injection.
        let cell = crate::notebook::Cell {
            execution_count: 1,
            origin: "agent".to_string(),
            code: "1+1".to_string(),
            stdout: String::new(),
            stderr: String::new(),
            result: Some("2".to_string()),
            image_paths: Vec::new(),
            error: None,
            success: true,
        };
        let result = compose_notebook_result(&cell, "/project", false, "cell[0]", "builtin");
        assert_eq!(result["error"].as_str(), None);
        assert!(result["stdout"].as_str().unwrap_or("").contains("=> 2"));
        assert_eq!(result["traceback_elided_frames"].as_u64(), Some(0));
    }

    #[test]
    fn g5c_elided_frame_count_sums_error_and_stderr() {
        // G5c: traceback_elided_frames must SUM the error + stderr channel
        // counts (was .max, which under-reported when both channels elide).
        let tb = "Traceback (most recent call last):\n\
  File \"/x/site-packages/numpy/a.py\", line 1, in f\n\
    pass\n\
  File \"/x/site-packages/numpy/b.py\", line 2, in g\n\
    pass\n\
ValueError: boom\n";
        // Both cell.error and cell.stderr carry a traceback with 2 library
        // frames each -> the composed count must be 4 (sum), not 2 (max).
        let cell = crate::notebook::Cell {
            execution_count: 1,
            origin: "agent".to_string(),
            code: "raise".to_string(),
            stdout: String::new(),
            stderr: tb.to_string(),
            result: None,
            image_paths: Vec::new(),
            error: Some(tb.to_string()),
            success: false,
        };
        let result = compose_notebook_result(&cell, "/project", false, "cell[0]", "builtin");
        let elided = result["traceback_elided_frames"].as_u64().unwrap_or(0);
        assert!(
            elided >= 4,
            "elided count must SUM both channels (>=4), got {elided}"
        );
    }

    #[test]
    fn builds_internal_command_tools() {
        let tools = command_tools_filtered(true);
        let workflow_run = tools
            .iter()
            .find(|tool| tool.name == "workflow_run")
            .expect("workflow_run should exist");
        let query = tools
            .iter()
            .find(|tool| tool.name == "query")
            .expect("query should exist");

        assert!(tools.len() >= 30);
        assert_eq!(query.permission_mode, PermissionMode::ReadOnly);
        assert!(!query.requires_approval);
        assert_eq!(
            workflow_run.input_schema["properties"]["values"]["type"],
            serde_json::json!("object")
        );
    }

    #[test]
    fn agent_surface_excludes_cli_wrapper_red_herrings() {
        // Per docs/PRISM_TOOL_SURFACE_AUDIT.md (TASK 0). The collapsed tools are
        // NOT offered to the agent (they inflate the count / confuse selection),
        // but they STAY REGISTERED so old transcripts keep resolving.
        let tools = command_tools_filtered(true);
        let offered_names: std::collections::HashSet<&str> =
            tools.iter().map(|t| t.name.as_str()).collect();

        // The 3 CLI-wrapper red herrings are gone from the offered surface...
        assert!(!offered_names.contains("agent"), "agent wrapper collapsed");
        assert!(!offered_names.contains("run"), "run wrapper collapsed");
        assert!(
            !offered_names.contains("research"),
            "research wrapper collapsed"
        );

        // ...but their typed siblings ARE still offered (the collapse is safe).
        assert!(offered_names.contains("run_submit"), "typed sibling kept");
        assert!(
            offered_names.contains("research_query"),
            "typed sibling kept"
        );

        // ...and the specs stay REGISTERED (backward compat for old transcripts).
        assert!(
            is_command_tool("run"),
            "spec stays registered even though excluded from the surface"
        );
        assert!(is_command_tool("research"));
        assert!(is_command_tool("agent"));
    }

    #[test]
    fn billing_balance_executes_the_balance_path_not_a_bare_404() {
        // Adversarial-review correction (2026-07-22): an earlier commit excluded
        // Rust billing_balance from the surface on the belief bare `prism
        // billing` 404'd. Live re-proving (3/3) shows bare `prism billing` DOES
        // return the balance ("Balance: N credits"). Dispatch also routes by
        // NAME via is_command_tool before the Python server, so surface
        // exclusion wouldn't change execution anyway. This test pins the REAL
        // contract: a billing_balance call must build the argv that returns the
        // balance, and it must be offered + resolvable.
        let tools = command_tools_filtered(true);
        let offered_names: std::collections::HashSet<&str> =
            tools.iter().map(|t| t.name.as_str()).collect();
        assert!(
            offered_names.contains("billing_balance"),
            "billing_balance must be offered (bare `prism billing` works; not excluded)"
        );
        assert!(is_command_tool("billing_balance"));

        // The execution arm must build `prism billing` (no subcommand) — that is
        // the CLI's documented balance action. `prism billing balance` is an
        // UNRECOGNIZED subcommand (verified live), so adding "balance" would
        // break it. Empty args is correct here.
        assert_eq!(
            command_tool_preview("billing_balance", &json!({})),
            Some("prism billing".to_string()),
            "billing_balance runs bare `prism billing`, which returns the balance"
        );
        // And it's a free, unapproved read.
        assert_eq!(
            command_tool_requires_approval("billing_balance"),
            Some(false)
        );
    }

    #[test]
    fn notebook_tools_registered_with_correct_gating_and_schema() {
        // The three notebook tools exist and resolve by name + alias.
        assert!(is_command_tool("notebook_exec"));
        assert!(is_command_tool("notebook_run"), "alias resolves");
        assert!(is_command_tool("notebook_status"));
        assert!(is_command_tool("notebook_reset"));

        // Exec runs arbitrary code, so it is approval-gated at FullAccess;
        // status is read-only and ungated; reset is gated too because it wipes
        // the kernel the human shares.
        assert_eq!(command_tool_requires_approval("notebook_exec"), Some(true));
        assert_eq!(
            command_tool_requires_approval("notebook_status"),
            Some(false)
        );
        assert_eq!(command_tool_requires_approval("notebook_reset"), Some(true));

        let tools = command_tools_filtered(true);
        let exec = tools
            .iter()
            .find(|tool| tool.name == "notebook_exec")
            .expect("notebook_exec offered");
        assert_eq!(exec.permission_mode, PermissionMode::FullAccess);
        // `code` is a required string; the schema forbids extra keys.
        assert_eq!(exec.input_schema["required"], serde_json::json!(["code"]));
        assert_eq!(
            exec.input_schema["properties"]["code"]["type"],
            serde_json::json!("string")
        );
    }

    #[test]
    fn notebook_exec_preview_truncates_code_blob() {
        let long = "x".repeat(200);
        let preview = command_tool_preview("notebook_exec", &json!({ "code": long }))
            .expect("preview renders");
        // Shows a bounded head + the total char count, never the whole blob.
        assert!(preview.starts_with("notebook exec:"), "{preview}");
        assert!(preview.contains("200 chars"), "{preview}");
        assert!(preview.len() < 120, "preview stays short: {preview}");

        // reset:true is surfaced so the human sees it before approving.
        let with_reset =
            command_tool_preview("notebook_exec", &json!({ "code": "1", "reset": true }))
                .expect("preview renders");
        assert!(with_reset.contains("resets kernel"), "{with_reset}");
    }

    #[test]
    fn local_store_tools_hidden_when_node_offline() {
        let tools = command_tools_filtered(false);
        // Only the local-node tools are gated offline; query_platform hits the
        // remote API and stays offered (see offline_catalog_offers_platform_knowledge_path).
        for name in ["query", "query_local", "query_federated"] {
            assert!(
                tools.iter().all(|tool| tool.name != name),
                "{name} must not be offered while the local node is offline"
            );
        }
        // Capability is gated, not deleted: every hidden tool still resolves
        // and executes if called by name (older transcripts, aliases).
        for name in ["query", "query_local", "query_federated"] {
            assert!(is_command_tool(name), "{name} must remain executable");
        }
    }

    #[test]
    fn local_store_tools_offered_when_node_online() {
        let tools = command_tools_filtered(true);
        for name in ["query", "query_local", "query_federated"] {
            assert!(
                tools.iter().any(|tool| tool.name == name),
                "{name} should be offered when the local node is running"
            );
        }
        // query_platform hits the remote API, so it is offered in both states.
        assert!(tools.iter().any(|tool| tool.name == "query_platform"));
    }

    #[test]
    fn offline_catalog_offers_platform_knowledge_path() {
        // The retired Python `knowledge` tool used to be the single offered
        // platform search path (with query_platform hidden behind it). After
        // the knowledge.py → Rust migration, query_platform IS the offered
        // platform search surface, and the node-gated local query tools stay
        // hidden when the local node is down.
        let catalog = command_tools_filtered(false);
        let names: Vec<&str> = catalog.iter().map(|tool| tool.name.as_str()).collect();

        // The platform search path is offered even with the local node down.
        assert!(names.contains(&"query_platform"));
        // Node-gated local query tools are hidden offline.
        assert!(!names.contains(&"query_local"));
        assert!(!names.contains(&"query_federated"));
        // The typed knowledge command-tools are always offered (remote API).
        for name in [
            "knowledge_entity",
            "knowledge_paths",
            "knowledge_corpora",
            "knowledge_ingest",
        ] {
            assert!(names.contains(&name), "{name} must be offered offline");
        }
        // The old unified Python `knowledge` tool is gone.
        assert!(!names.contains(&"knowledge"));
    }

    #[test]
    fn predict_is_a_billable_approval_gated_tool_with_correct_invocation() {
        // The one-call "run a marketplace model on the cloud" surface: it
        // creates real billable deployments, so it MUST be approval-gated.
        assert!(is_command_tool("predict"));
        assert!(is_command_tool("run_model"), "alias must resolve");
        assert_eq!(command_tool_requires_approval("predict"), Some(true));

        let preview = command_tool_preview(
            "predict",
            &json!({
                "model": "mace-mh-1",
                "task": "relax",
                "inputs": {"structure": {"atoms": ["Si"]}},
                "node_id": "1f0c2a2e-0000-4000-8000-000000000001",
                "keep_alive": true
            }),
        )
        .expect("preview renders");
        // Top-level verb, slug positional, inputs as one JSON arg.
        assert!(preview.starts_with("prism predict mace-mh-1"), "{preview}");
        assert!(preview.contains("--task relax"), "{preview}");
        assert!(preview.contains("--input"), "{preview}");
        assert!(preview.contains("--node-id"), "{preview}");
        assert!(preview.contains("--keep"), "{preview}");

        // Missing model → honest arg error, no execution.
        assert!(build_execution(spec_by_name("predict").unwrap(), &json!({"inputs": {}})).is_err());
    }

    #[test]
    fn goal_tools_unify_agent_with_campaign_engine() {
        // Goal unification: the agent gets the SAME campaign engine the CLI
        // has. Starting/resuming spends money → approval-gated; status/list
        // are read-only and free.
        assert!(is_command_tool("goal_start"));
        assert!(is_command_tool("campaign_start"), "alias must resolve");
        assert_eq!(command_tool_requires_approval("goal_start"), Some(true));
        assert_eq!(command_tool_requires_approval("goal_resume"), Some(true));
        assert_eq!(command_tool_requires_approval("goal_status"), Some(false));
        assert_eq!(command_tool_requires_approval("goal_list"), Some(false));

        let preview = command_tool_preview(
            "goal_start",
            &json!({
                "goal": "W-Mo alloy with creep resistance beyond CMSX-4",
                "elements": ["W", "Mo", "Ta"],
                "objective": "maximize creep resistance",
                "max_iterations": 20,
                "budget_usd": 5.0,
                "approval_gates": [10]
            }),
        )
        .expect("preview renders");
        assert!(
            preview.starts_with("prism campaign start --goal"),
            "{preview}"
        );
        assert!(preview.contains("--elements W,Mo,Ta"), "{preview}");
        assert!(preview.contains("--max-iterations 20"), "{preview}");
        assert!(preview.contains("--budget 5"), "{preview}");
        assert!(preview.contains("--approval-gates 10"), "{preview}");
        // Long-research semantics: goal tools NEVER block the tool call.
        assert!(preview.contains("--detach"), "{preview}");

        let status = command_tool_preview("goal_status", &json!({"id": "camp_abc"}))
            .expect("status preview");
        assert_eq!(status, "prism campaign status camp_abc");

        let resume = command_tool_preview("goal_resume", &json!({"id": "camp_abc"}))
            .expect("resume preview");
        assert_eq!(resume, "prism campaign resume camp_abc --detach");

        // Missing goal → honest arg error, no execution.
        assert!(build_execution(spec_by_name("goal_start").unwrap(), &json!({})).is_err());
    }

    #[test]
    fn doctor_is_a_read_only_command_tool() {
        // Parity drain (GAP-A #5): the agent had no self-diagnostic tool.
        assert!(is_command_tool("doctor"));
        assert_eq!(command_tool_requires_approval("doctor"), Some(false));
        let preview = command_tool_preview("doctor", &json!({})).expect("doctor preview renders");
        assert_eq!(preview, "prism doctor");
    }

    #[test]
    fn report_bug_is_registered_approval_gated_and_builds_prism_report() {
        // TASK 4: the agent's self-bug-report escape hatch. Reuses the
        // `prism report` machinery; approval-gated because it files an external
        // issue/ticket; --no-github by default (agent path files to the
        // attributable MARC27 support ticket, not the public repo).
        assert!(is_command_tool("report_bug"));
        assert!(is_command_tool("bug_report"), "alias resolves");
        assert!(is_command_tool("report_issue"), "alias resolves");
        // Files an external issue/ticket → FullAccess + approval-gated.
        assert_eq!(
            command_tool_requires_approval("report_bug"),
            Some(true),
            "report_bug must be approval-gated (outward-facing)"
        );

        // Description-only → prism report "<desc>" --no-github.
        let preview = command_tool_preview(
            "report_bug",
            &json!({"description": "materials_search returned HTTP 500"}),
        )
        .expect("preview renders");
        assert!(
            preview.starts_with("prism report "),
            "must shell to prism report: {preview}"
        );
        assert!(
            preview.contains("--no-github"),
            "agent path defaults to --no-github: {preview}"
        );

        // With a log file → --log-file appended.
        let preview_with_log = command_tool_preview(
            "report_bug",
            &json!({"description": "compute_submit hung", "log_file": "/tmp/err.log"}),
        )
        .expect("preview with log renders");
        assert!(
            preview_with_log.contains("--log-file /tmp/err.log"),
            "log_file maps to --log-file: {preview_with_log}"
        );

        // Offered in the agent surface (not excluded).
        let tools = command_tools_filtered(true);
        assert!(
            tools.iter().any(|t| t.name == "report_bug"),
            "report_bug must be offered to the agent"
        );
        // Typed schema: description required, log_file optional.
        let spec = tools.iter().find(|t| t.name == "report_bug").unwrap();
        assert_eq!(spec.input_schema["required"], json!(["description"]));
    }

    #[test]
    fn marketplace_find_is_semantic_discovery_and_always_requests_json() {
        // Parity drain (GAP-A #4): marketplace's semantic-discovery verb had
        // no agent tool, even though its own CLI help says "useful from
        // agent tools" and ships a --json flag for exactly this caller.
        assert!(is_command_tool("marketplace_find"));
        assert_eq!(
            command_tool_requires_approval("marketplace_find"),
            Some(false)
        );

        let preview = command_tool_preview(
            "marketplace_find",
            &json!({
                "query": "predict elastic moduli of a Ti-Al alloy",
                "types": ["model", "tool"],
                "limit": 3
            }),
        )
        .expect("preview renders");
        assert!(preview.starts_with("prism marketplace find"), "{preview}");
        assert!(preview.contains("--type model"), "{preview}");
        assert!(preview.contains("--type tool"), "{preview}");
        assert!(preview.contains("--limit 3"), "{preview}");
        assert!(preview.contains("--json"), "{preview}");

        // Missing query → honest arg error, no execution.
        assert!(build_execution(spec_by_name("marketplace_find").unwrap(), &json!({})).is_err());
    }

    #[test]
    fn billing_read_tools_are_free_and_unapproved() {
        // Parity drain (GAP-A #3): the agent could not check spend before a
        // billable op. These four are pure reads — no approval gate.
        for name in [
            "billing_balance",
            "billing_usage",
            "billing_history",
            "billing_prices",
        ] {
            assert!(is_command_tool(name), "{name} must be registered");
            assert_eq!(
                command_tool_requires_approval(name),
                Some(false),
                "{name} is read-only and must not require approval"
            );
        }

        assert_eq!(
            command_tool_preview("billing_balance", &json!({})).unwrap(),
            "prism billing"
        );
        assert_eq!(
            command_tool_preview("billing_usage", &json!({})).unwrap(),
            "prism billing usage"
        );
        assert_eq!(
            command_tool_preview("billing_history", &json!({})).unwrap(),
            "prism billing history"
        );
        assert_eq!(
            command_tool_preview("billing_prices", &json!({})).unwrap(),
            "prism billing prices"
        );

        // The raw `billing` root-args tool can reach `topup`, which spends
        // real money via a Stripe checkout — that one stays approval-gated.
        assert_eq!(command_tool_requires_approval("billing"), Some(true));
    }

    #[test]
    fn requires_approval_lookup_matches_specs() {
        // Read-only tools are not gated; unknown names are None (not false —
        // "no such tool" must stay distinguishable from "not gated").
        assert_eq!(command_tool_requires_approval("mesh_health"), Some(false));
        assert_eq!(command_tool_requires_approval("query"), Some(false));
        assert_eq!(command_tool_requires_approval("no_such_tool"), None);
        // Every spec answers, and the answer mirrors the spec itself.
        for spec in COMMAND_TOOLS {
            assert_eq!(
                command_tool_requires_approval(spec.name),
                Some(spec.requires_approval)
            );
        }
    }

    #[test]
    fn mesh_health_is_a_read_only_command_tool() {
        // Ported from the retired Python mesh.py — mesh is spine, so it lives in
        // Rust. Read-only health check that execs `prism mesh health`.
        assert!(is_command_tool("mesh_health"));
        let spec = spec_by_name("mesh_health").expect("mesh_health spec exists");
        assert!(
            matches!(spec.permission_mode, PermissionMode::ReadOnly),
            "mesh_health is read-only"
        );
        assert!(
            !spec.requires_approval,
            "a health check must not be approval-gated"
        );
        let preview = command_tool_preview("mesh_health", &json!({})).expect("preview renders");
        assert_eq!(preview, "prism mesh health");
    }

    #[test]
    fn compute_broker_lives_in_rust_command_tools() {
        // Ported from the retired Python compute.py (which needed an uninstalled
        // `marc27` SDK). Compute dispatch is spine → Rust command-tools calling
        // `prism compute …`. Reads are safe; submit is money-spending → approval.
        assert!(is_command_tool("compute_gpus"));
        assert!(is_command_tool("compute_submit"));

        let submit = spec_by_name("compute_submit").expect("compute_submit spec exists");
        assert!(
            matches!(submit.permission_mode, PermissionMode::FullAccess),
            "compute_submit spends real money"
        );
        assert!(
            submit.requires_approval,
            "a billable job dispatch must be approval-gated"
        );

        let status = spec_by_name("compute_status").expect("compute_status spec exists");
        assert!(
            matches!(status.permission_mode, PermissionMode::ReadOnly),
            "compute_status is a read-only poll"
        );
        assert!(!status.requires_approval);

        assert_eq!(
            command_tool_preview("compute_gpus", &json!({})).expect("preview renders"),
            "prism compute gpus"
        );
        assert_eq!(
            command_tool_preview("compute_status", &json!({ "job_id": "job_123" }))
                .expect("preview renders"),
            "prism compute status job_123"
        );
        let submit_preview = command_tool_preview(
            "compute_submit",
            &json!({ "image": "vasp:6.5.0", "inputs": {} }),
        )
        .expect("preview renders");
        assert!(
            submit_preview.starts_with("prism compute submit --image vasp:6.5.0 --inputs"),
            "got: {submit_preview}"
        );
    }

    #[test]
    fn knowledge_plane_lives_in_rust_command_tools() {
        // Ported from the retired Python knowledge.py (which drove a thin
        // `_platform_client` and needed the `marc27` SDK for some paths).
        // entity/paths/corpora are read-only graph/catalog lookups; ingest is
        // an async platform write. search/semantic live under query_platform;
        // graph stats under `ingest --status`; promote_artifact was dropped.
        assert!(is_command_tool("knowledge_entity"));
        assert!(is_command_tool("knowledge_paths"));
        assert!(is_command_tool("knowledge_corpora"));
        assert!(is_command_tool("knowledge_ingest"));

        let entity = spec_by_name("knowledge_entity").expect("knowledge_entity spec exists");
        assert!(matches!(entity.permission_mode, PermissionMode::ReadOnly));
        assert!(!entity.requires_approval);

        assert_eq!(
            command_tool_preview("knowledge_entity", &json!({ "name": "Ti-6Al-4V" }))
                .expect("preview renders"),
            "prism knowledge entity Ti-6Al-4V"
        );
        assert_eq!(
            command_tool_preview(
                "knowledge_paths",
                &json!({ "from_entity": "TiAl", "to_entity": "aerospace", "max_hops": 4 })
            )
            .expect("preview renders"),
            "prism knowledge paths TiAl aerospace --max-hops 4"
        );
        assert_eq!(
            command_tool_preview("knowledge_corpora", &json!({ "domain": "materials" }))
                .expect("preview renders"),
            "prism knowledge corpora --domain materials"
        );
        assert_eq!(
            command_tool_preview(
                "knowledge_ingest",
                &json!({ "url": "https://example.com/x" })
            )
            .expect("preview renders"),
            "prism knowledge ingest --url https://example.com/x"
        );

        // ingest with neither url nor query is a hard error (exec bails →
        // preview is None), not a silent no-op.
        assert!(command_tool_preview("knowledge_ingest", &json!({})).is_none());
    }

    #[test]
    fn renders_preview_from_structured_args() {
        let preview = command_tool_preview(
            "query",
            &json!({ "args": ["band gap materials", "--json"] }),
        )
        .expect("preview should render");

        assert_eq!(preview, "prism query 'band gap materials' --json");
    }

    #[test]
    fn renders_typed_workflow_preview() {
        let preview = command_tool_preview(
            "workflow_run",
            &json!({
                "name": "forge",
                "execute": true,
                "values": {
                    "paper": "alpha",
                    "dataset": "beta"
                }
            }),
        )
        .expect("workflow preview should render");

        assert!(preview.starts_with("prism workflow run forge"));
        assert!(preview.contains("--set dataset=beta"));
        assert!(preview.contains("--execute"));
        assert!(!preview.contains("role=agent"));
    }

    #[test]
    fn renders_typed_platform_query_preview() {
        let preview = command_tool_preview(
            "query_platform",
            &json!({
                "text": "high entropy alloys",
                "semantic": true,
                "json": true,
                "limit": 5
            }),
        )
        .expect("platform query preview should render");

        assert_eq!(
            preview,
            "prism query 'high entropy alloys' --platform --semantic --json --limit 5"
        );
    }

    #[test]
    fn renders_typed_models_search_preview() {
        let preview = command_tool_preview(
            "models_search",
            &json!({
                "query": "gemini",
                "provider": "google"
            }),
        )
        .expect("models preview should render");

        assert_eq!(
            preview,
            "prism models search gemini --provider google --json"
        );
    }

    #[test]
    fn renders_typed_discourse_run_preview() {
        let preview = command_tool_preview(
            "discourse_run",
            &json!({
                "spec_id": "abc-123",
                "params": {
                    "alloy": "IN718"
                }
            }),
        )
        .expect("discourse preview should render");

        assert_eq!(
            preview,
            "prism discourse run abc-123 --param alloy=IN718 --json"
        );
    }

    #[test]
    fn renders_typed_run_preview() {
        let preview = command_tool_preview(
            "run_submit",
            &json!({
                "image": "ghcr.io/acme/model:latest",
                "name": "trial",
                "backend": "marc27",
                "inputs": {
                    "alloy": "IN718"
                }
            }),
        )
        .expect("run preview should render");

        assert_eq!(
            preview,
            "prism run --name trial --backend marc27 --input alloy=IN718 ghcr.io/acme/model:latest --json"
        );
    }

    #[test]
    fn renders_typed_publish_preview() {
        let preview = command_tool_preview(
            "publish_artifact",
            &json!({
                "path": "models/mace.ckpt",
                "to": "marc27",
                "repo": "team/mace",
                "private": true
            }),
        )
        .expect("publish preview should render");

        assert_eq!(
            preview,
            "prism publish models/mace.ckpt --to marc27 --repo team/mace --private --json"
        );
    }

    // ── TOOL_SURFACE_SPEC definition-of-ready gates (D2, D3) ─────────────
    //
    // These tests encode the schema + description floor from
    // docs/TOOL_SURFACE_SPEC.md so the command-tool surface cannot silently
    // regress. The `RootArgs` umbrella tools (generic `args: array<string>`)
    // are tracked in `ROOTARGS_ALLOWLIST` — Batch 1 of the tool-surface
    // upgrade converts them to typed/subcommand schemas and shrinks this
    // list to zero. Until then, the list is the explicit, reviewed inventory
    // of the known gap (audit §2.2).

    /// Command-tools whose schema is the generic `args: array<string>` escape.
    /// This is the audited gap; the list MUST only shrink over time. Batch 1
    /// converted the 7 highest-overlap umbrellas (billing, deploy, discourse,
    /// marketplace, mesh, models, node) to typed `RootSubcommand` schemas;
    /// the remaining entries are genuine few-purpose roots with low overlap.
    const ROOTARGS_ALLOWLIST: &[&str] = &[
        "agent",
        "doctor",
        "ingest",
        "job-status",
        "publish",
        "query",
        "research",
        "run",
        "status",
        "tools",
        "workflow",
    ];

    #[test]
    fn every_command_tool_has_a_real_schema_or_is_known_rootargs() {
        // D3: a tool's schema is typed, honest-empty (additionalProperties:false),
        // or — only for the explicit escape-hatch umbrellas — the generic
        // RootArgs form. Any NEW tool landing in the generic form without being
        // in the allowlist fails here, preventing the gap from growing.
        let tools = command_tools_filtered(true);
        assert!(!tools.is_empty());
        for tool in &tools {
            let schema = &tool.input_schema;
            // The generic RootArgs escape is a schema whose ONLY property is
            // `args` (an untyped string array). RootSubcommand schemas also
            // carry an `args` field but additionally have a `subcommand` enum,
            // so they are NOT flagged here.
            let props = schema.get("properties").and_then(|p| p.as_object());
            let is_generic_rootargs = props.is_some_and(|p| {
                p.len() == 1 && p.contains_key("args") && !p.contains_key("subcommand")
            });
            if is_generic_rootargs {
                assert!(
                    ROOTARGS_ALLOWLIST.contains(&tool.name.as_str()),
                    "tool `{}` uses the generic args schema but is not in \
                     ROOTARGS_ALLOWLIST — give it a typed schema (SPEC D3) or \
                     add it to the allowlist with a reason",
                    tool.name
                );
                continue;
            }
            // Every other tool: schema must be an object. Honest-empty shapes
            // (empty_schema()) set additionalProperties:false; typed shapes
            // have ≥1 property. Both are accepted.
            assert_eq!(
                schema["type"], "object",
                "tool `{}` schema is not type:object",
                tool.name
            );
        }
    }

    #[test]
    fn empty_schema_tools_close_additional_properties() {
        // D3b: a genuinely parameter-less tool must declare
        // {type:object, properties:{}, additionalProperties:false} so the model
        // is not invited to invent arguments.
        let tools = command_tools_filtered(true);
        for tool in &tools {
            let props = tool
                .input_schema
                .get("properties")
                .and_then(|p| p.as_object());
            let Some(props) = props else { continue };
            if props.is_empty() {
                assert_eq!(
                    tool.input_schema.get("additionalProperties"),
                    Some(&serde_json::json!(false)),
                    "tool `{}` has an empty properties object but does not set \
                     additionalProperties:false (SPEC D3b)",
                    tool.name
                );
            }
        }
    }

    #[test]
    fn every_command_tool_description_meets_the_floor() {
        // D2 (spirit): a description must be long enough to carry a
        // when-to-use / returns signal. The floor here is deliberately
        // lenient (40 chars) to match the existing surface; Batch 3 of the
        // upgrade raises the floor and the descriptions together.
        const FLOOR: usize = 40;
        let tools = command_tools_filtered(true);
        for tool in &tools {
            let len = tool.description.trim().len();
            assert!(
                len >= FLOOR,
                "tool `{}` description is {len} chars (< {FLOOR}) — add a \
                 when-to-use / returns signal (SPEC D2)",
                tool.name
            );
        }
    }

    #[test]
    fn root_subcommand_schema_is_typed_with_enum_and_prepend_execution() {
        // SPEC §1.1.3: an umbrella converted to RootSubcommand exposes a
        // closed `subcommand` enum (real verbs) and prepends the chosen verb
        // at execution time. Verify the schema shape and the execution build
        // for a representative converted umbrella (`billing`).
        let billing = command_tools_filtered(true)
            .into_iter()
            .find(|t| t.name == "billing")
            .expect("billing umbrella exists");

        // D3c: schema has a subcommand enum, not a bare args array.
        let props = billing.input_schema["properties"]
            .as_object()
            .expect("billing schema has properties");
        assert!(
            props.contains_key("subcommand"),
            "billing schema must expose a subcommand field"
        );
        let enum_vals = billing.input_schema["properties"]["subcommand"]["enum"]
            .as_array()
            .expect("subcommand has an enum");
        assert!(
            enum_vals.iter().any(|v| v == "topup"),
            "billing subcommand enum should include topup"
        );
        assert_eq!(
            billing.input_schema["required"],
            serde_json::json!(["subcommand"]),
        );

        // Execution: the chosen subcommand is prepended to the CLI argv.
        let spec = spec_by_name("billing").unwrap();
        let exec = build_execution(spec, &json!({"subcommand": "usage"}))
            .expect("billing usage execution builds");
        match exec {
            CommandExecution::Cli { root, args } => {
                assert_eq!(root, "billing");
                assert_eq!(args, vec!["usage".to_string()]);
            }
            other => panic!("expected Cli, got {other:?}"),
        }

        // With extra verb-specific tokens, they follow the subcommand.
        let exec2 = build_execution(
            spec_by_name("marketplace").unwrap(),
            &json!({"subcommand": "info", "args": ["acme-model"]}),
        )
        .expect("marketplace info execution builds");
        match exec2 {
            CommandExecution::Cli { root, args } => {
                assert_eq!(root, "marketplace");
                assert_eq!(args, vec!["info".to_string(), "acme-model".to_string()]);
            }
            other => panic!("expected Cli, got {other:?}"),
        }
    }
}
