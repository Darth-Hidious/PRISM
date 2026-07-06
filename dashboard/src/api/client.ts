import { getSessionToken } from "../lib/session";

const BASE = import.meta.env.DEV ? "" : "";

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const headers = new Headers(init?.headers);
  const token = getSessionToken();
  if (token) {
    headers.set("X-Session-Token", token);
  }
  const res = await fetch(`${BASE}${path}`, { ...init, headers });
  if (!res.ok) {
    const body = await res.text();
    const detail = body.trim();
    throw new Error(detail ? `${res.status} ${res.statusText} — ${detail}` : `${res.status} ${res.statusText}`);
  }
  return res.json();
}

// ── Types matching server handler responses ────────────────────────

export interface DataSource {
  id: string;
  name: string;
  kind: string;
}

export interface AuditEntry {
  id: number;
  timestamp: string;
  user_id: string;
  action: string;
  target: string;
  detail: string | null;
  outcome: string;
}

export interface UserInfo {
  id: string;
  role: string;
  permissions: string[];
}

export interface MeshNode {
  id: string;
  name: string;
  address: string;
  port: number;
  last_seen: string;
  capabilities: string[];
}

export interface MeshStatus {
  online: boolean;
  node_id: string | null;
  peer_count: number;
  peers: MeshNode[];
}

export interface SubscriptionInfo {
  dataset_name: string;
  publisher_node: string;
  subscribed_at: string;
}

export interface PublishedInfo {
  name: string;
  schema_version: string;
  subscriber_count: number;
}

export interface SubscriptionsResponse {
  published: PublishedInfo[];
  subscribed: SubscriptionInfo[];
}

export interface NodeInfo {
  name: string;
  version: string;
  status: string;
  uptime_secs: number;
  services: { name: string; port: number; status: string }[];
}

// Goals — long-running research goals (server reads campaign checkpoints).
export interface GoalSummary {
  id: string;
  goal: string | null;
  candidates_evaluated: number | null;
  iteration: number | null;
  created: string | null;
  /** Present when the checkpoint file could not be parsed. */
  error?: string;
}

export interface GoalsResponse {
  goals: GoalSummary[];
  count: number;
  /** Absolute path the node read checkpoints from. */
  source: string;
}

export interface CreateGoalRequest {
  goal: string;
  max_iterations?: number;
  budget_usd?: number;
}

/** POST /api/goals and /resume both return the tool-executor envelope. */
export interface ToolRunEnvelope {
  tool: string;
  result: unknown;
}

// Workflows — discovered workflow specs.
export interface WorkflowSummary {
  name: string;
  description: string;
  steps: number;
  arguments: string[];
}

export interface WorkflowsResponse {
  workflows: WorkflowSummary[];
  count: number;
}

// Tools — the node's tool registry.
export interface ToolArgInfo {
  name: string;
  arg_type: string;
  required: boolean;
  description: string | null;
}

export interface ToolCommandInfo {
  name: string;
  description: string;
  args: ToolArgInfo[];
}

export interface ToolInfo {
  name: string;
  description: string;
  version: string;
  commands: ToolCommandInfo[];
}

// ── API client ─────────────────────────────────────────────────────

export const api = {
  getHealth: () => request<{ status: string }>("/api/health"),

  getNodeInfo: () => request<NodeInfo>("/api/v1/node"),

  query: (q: string) =>
    request<{ results: unknown[] }>("/api/query", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ query: q }),
    }),

  getDataSources: () => request<DataSource[]>("/api/data/sources"),

  getAuditLog: () => request<AuditEntry[]>("/api/audit"),

  getUsers: () => request<UserInfo[]>("/api/users"),

  getMeshNodes: () => request<MeshStatus>("/api/mesh/nodes"),

  getMeshSubscriptions: () =>
    request<SubscriptionsResponse>("/api/mesh/subscriptions"),

  // ── Goals (GET open to any session; POST needs ExecuteTools) ──────
  getGoals: () => request<GoalsResponse>("/api/goals"),

  getGoal: (id: string) =>
    request<Record<string, unknown>>(`/api/goals/${encodeURIComponent(id)}`),

  createGoal: (body: CreateGoalRequest) =>
    request<ToolRunEnvelope>("/api/goals", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    }),

  resumeGoal: (id: string) =>
    request<ToolRunEnvelope>(`/api/goals/${encodeURIComponent(id)}/resume`, {
      method: "POST",
    }),

  // ── Workflows (GET open; run needs ExecuteTools) ──────────────────
  getWorkflows: () => request<WorkflowsResponse>("/api/workflows"),

  // `execute: false` is a dry run — resolve + plan, run nothing. The server
  // defaults to true, so we always send the flag explicitly from the UI.
  runWorkflow: (name: string, values: Record<string, string>, execute: boolean) =>
    request<Record<string, unknown>>(
      `/api/workflows/${encodeURIComponent(name)}/run`,
      {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ values, execute }),
      },
    ),

  // ── Tools (needs ViewDashboard) ───────────────────────────────────
  getTools: () => request<ToolInfo[]>("/api/tools"),
};
