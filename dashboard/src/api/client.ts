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
};
