use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use prism_ingest::llm::ToolDefinition;
use prism_workflows::{
    discover_workflows, execute_workflow_with_policy, find_workflow, parse_workflow_command_args,
    WorkflowRunResult, WorkflowSpec,
};
use serde_json::{json, Value};
use tokio::process::Command as TokioCommand;
use tokio::time::timeout;

use crate::permissions::PermissionMode;
use crate::tool_catalog::LoadedTool;

#[derive(Debug, Clone)]
pub struct CommandToolRuntime {
    pub current_exe: PathBuf,
    pub project_root: PathBuf,
    pub python_bin: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandToolKind {
    RootArgs,
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
    MeshPeers,
    MeshSubscriptions,
    MeshPublish,
    MeshSubscribe,
    MeshUnsubscribe,
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
        description: "Query the local PRISM graph/vector stores with typed fields instead of manual CLI args. Use `cypher=true` for direct Cypher, `semantic=true` for vector search, or plain text for graph-neighbor lookup.",
        permission_mode: PermissionMode::ReadOnly,
        requires_approval: false,
    },
    CommandToolSpec {
        name: "query_platform",
        root: "query",
        aliases: &[],
        kind: CommandToolKind::QueryPlatform,
        description: "Query the MARC27 platform knowledge APIs instead of local Neo4j/Qdrant. Use `semantic=true` for vector search and `json=true` for machine-readable output.",
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
        kind: CommandToolKind::RootArgs,
        description: "Run `prism marketplace ...` to search, inspect, or install PRISM marketplace resources such as downloadable workflows and tools. Pass one CLI token per `args` entry.",
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
        kind: CommandToolKind::RootArgs,
        description: "Run `prism mesh ...` for PRISM mesh operations. This is a PRISM-native operational command surface, not a shell command.",
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
        kind: CommandToolKind::RootArgs,
        description: "Run `prism node ...` for PRISM node operations. Pass structured argv tokens in `args`.",
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
        description: "Tail logs from a managed node service such as neo4j, qdrant, or kafka.",
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
        kind: CommandToolKind::RootArgs,
        description: "Run `prism deploy ...` for PRISM deployment flows. Pass structured argv tokens in `args` and expect approval for real deployment actions.",
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
        name: "models",
        root: "models",
        aliases: &["prism_models"],
        kind: CommandToolKind::RootArgs,
        description: "Run `prism models ...` to inspect hosted model discovery for the active MARC27 project. Use this for provider/model lookup without falling back to shell commands.",
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
        kind: CommandToolKind::RootArgs,
        description: "Run `prism discourse ...` for multi-agent debate workflows backed by the platform discourse API. Use structured argv tokens in `args` rather than a shell string.",
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
}

fn empty_schema() -> Value {
    json!({
        "type": "object",
        "properties": {},
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
                "description": "Natural-language query, entity text, or Cypher statement when `cypher=true`."
            },
            "cypher": {
                "type": "boolean",
                "description": "Run the `text` as a direct Cypher query against local Neo4j."
            },
            "semantic": {
                "type": "boolean",
                "description": "Use semantic vector search against local Qdrant instead of graph traversal."
            },
            "limit": {
                "type": "integer",
                "description": "Max number of results to return for semantic search.",
                "minimum": 1
            },
            "neo4j_url": {
                "type": "string",
                "description": "Override the local Neo4j HTTP endpoint."
            },
            "neo4j_user": {
                "type": "string",
                "description": "Override the local Neo4j username."
            },
            "neo4j_pass": {
                "type": "string",
                "description": "Override the local Neo4j password."
            },
            "qdrant_url": {
                "type": "string",
                "description": "Override the local Qdrant HTTP endpoint."
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
            "neo4j_url": {
                "type": "string",
                "description": "Override Neo4j HTTP endpoint."
            },
            "neo4j_user": {
                "type": "string",
                "description": "Override Neo4j username."
            },
            "neo4j_pass": {
                "type": "string",
                "description": "Override Neo4j password."
            },
            "qdrant_url": {
                "type": "string",
                "description": "Override Qdrant HTTP endpoint."
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
                "description": "Managed service name such as `neo4j`, `qdrant`, or `kafka`."
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
        CommandToolKind::NodeProbe | CommandToolKind::NodeStatus => empty_schema(),
        CommandToolKind::NodeLogs => node_logs_schema(),
        CommandToolKind::MeshDiscover => mesh_discover_schema(),
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
            if optional_bool(input, "cypher") {
                args.push("--cypher".to_string());
            }
            if optional_bool(input, "semantic") {
                args.push("--semantic".to_string());
            }
            for (flag, value) in [
                ("--neo4j-url", optional_string(input, "neo4j_url")),
                ("--neo4j-user", optional_string(input, "neo4j_user")),
                ("--neo4j-pass", optional_string(input, "neo4j_pass")),
                ("--qdrant-url", optional_string(input, "qdrant_url")),
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
        ("--neo4j-url", optional_string(input, "neo4j_url")),
        ("--neo4j-user", optional_string(input, "neo4j_user")),
        ("--neo4j-pass", optional_string(input, "neo4j_pass")),
        ("--qdrant-url", optional_string(input, "qdrant_url")),
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
            match find_workflow(&specs, name) {
                Some(spec) => match execute_workflow_with_policy(
                    spec,
                    values,
                    *execute,
                    policy,
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
        CommandExecution::Cli { .. } => {
            unreachable!("workflow executor only handles workflow commands")
        }
    };

    Ok(result)
}

pub fn command_tools() -> Vec<LoadedTool> {
    COMMAND_TOOLS
        .iter()
        .map(|spec| LoadedTool {
            name: spec.name.to_string(),
            description: spec.description.to_string(),
            input_schema: schema_for_spec(spec),
            requires_approval: spec.requires_approval,
            permission_mode: spec.permission_mode,
        })
        .collect()
}

pub fn is_command_tool(tool_name: &str) -> bool {
    spec_by_name(tool_name).is_some()
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
        CommandExecution::Cli { root, args } => {
            execute_cli_command(runtime, root, args, &invocation).await
        }
        CommandExecution::WorkflowList
        | CommandExecution::WorkflowShow { .. }
        | CommandExecution::WorkflowRun { .. } => {
            execute_workflow_command(runtime, &execution, &invocation, policy).await
        }
    }
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

    #[test]
    fn builds_internal_command_tools() {
        let tools = command_tools();
        let workflow_run = tools
            .iter()
            .find(|tool| tool.name == "workflow_run")
            .expect("workflow_run should exist");
        let query = tools
            .iter()
            .find(|tool| tool.name == "query")
            .expect("query should exist");
        let query_platform = tools
            .iter()
            .find(|tool| tool.name == "query_platform")
            .expect("query_platform should exist");

        assert!(tools.len() >= 30);
        assert_eq!(query.permission_mode, PermissionMode::ReadOnly);
        assert!(!query.requires_approval);
        assert_eq!(query_platform.permission_mode, PermissionMode::ReadOnly);
        assert_eq!(
            workflow_run.input_schema["properties"]["values"]["type"],
            serde_json::json!("object")
        );
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
}
