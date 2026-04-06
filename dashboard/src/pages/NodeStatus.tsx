import { useQueries } from "@tanstack/react-query";
import { api } from "../api/client";
import Card from "../components/Card";
import SessionNotice from "../components/SessionNotice";
import SummaryStat from "../components/SummaryStat";

function formatUptime(uptime: number) {
  const hours = Math.floor(uptime / 3600);
  const mins = Math.floor((uptime % 3600) / 60);
  return `${hours}h ${mins}m`;
}

function formatRelative(value: string) {
  const timestamp = Date.parse(value);
  if (Number.isNaN(timestamp)) {
    return value;
  }

  const deltaMinutes = Math.max(0, Math.round((Date.now() - timestamp) / 60_000));
  if (deltaMinutes < 1) {
    return "just now";
  }
  if (deltaMinutes < 60) {
    return `${deltaMinutes}m ago`;
  }
  const hours = Math.round(deltaMinutes / 60);
  if (hours < 48) {
    return `${hours}h ago`;
  }
  return `${Math.round(hours / 24)}d ago`;
}

export default function NodeStatus({
  connected,
  authenticated,
}: {
  connected: boolean;
  authenticated: boolean;
}) {
  if (!authenticated) {
    return (
      <div className="space-y-7">
        <div>
          <div className="text-[11px] uppercase tracking-[0.2em] text-[var(--dim)]">Operations Summary</div>
          <h1 className="mt-2 text-3xl font-semibold tracking-tight">Research Node Control</h1>
          <p className="mt-2 max-w-3xl text-sm leading-6 text-[var(--dim)]">
            This surface pulls protected node, audit, and operator state. Load it with a dashboard
            session token to switch from preview mode into a live PM console.
          </p>
        </div>
        <SessionNotice detail="Node health, operator posture, and recent audit activity require a dashboard session." />
      </div>
    );
  }

  const [node, mesh, audit, users, datasets] = useQueries({
    queries: [
      { queryKey: ["node-info"], queryFn: api.getNodeInfo, enabled: authenticated },
      { queryKey: ["mesh-nodes"], queryFn: api.getMeshNodes, enabled: authenticated },
      { queryKey: ["audit"], queryFn: api.getAuditLog, enabled: authenticated },
      { queryKey: ["users"], queryFn: api.getUsers, enabled: authenticated },
      { queryKey: ["data-sources"], queryFn: api.getDataSources, enabled: authenticated },
    ],
  });

  if (node.isLoading) return <p className="text-[var(--dim)]">Loading dashboard...</p>;
  if (node.error) return <p className="text-red-400">Error: {(node.error as Error).message}</p>;
  if (!node.data) return null;

  const healthyServices = node.data.services.filter((service) => service.status === "healthy").length;
  const degradedServices = node.data.services.length - healthyServices;
  const auditEntries = audit.data ?? [];
  const recentFailures = auditEntries.filter((entry) => entry.outcome !== "success").length;
  const peers = mesh.data?.peer_count ?? 0;
  const usersCount = users.data?.length ?? 0;
  const sources = datasets.data ?? [];
  const recentEvents = auditEntries.slice(0, 8);
  const currentNodeCells = [
    {
      id: "self",
      name: node.data.name,
      status: connected ? "current" : "snapshot",
      detail: connected ? "live feed" : "snapshot",
    },
    ...(mesh.data?.peers ?? []).map((peer) => ({
      id: peer.id,
      name: peer.name,
      status: "peer",
      detail: formatRelative(peer.last_seen),
    })),
  ];
  const sourceKinds = Array.from(
    sources.reduce((counts, source) => {
      counts.set(source.kind, (counts.get(source.kind) ?? 0) + 1);
      return counts;
    }, new Map<string, number>()),
  ).sort((a, b) => b[1] - a[1]);

  return (
    <div className="space-y-7">
      <div className="flex items-end justify-between gap-6">
        <div>
          <div className="text-[11px] uppercase tracking-[0.2em] text-[var(--dim)]">Operations Summary</div>
          <h1 className="mt-2 text-3xl font-semibold tracking-tight">Research Node Control</h1>
          <p className="mt-2 max-w-3xl text-sm leading-6 text-[var(--dim)]">
            Live view of the node, mesh presence, audit pressure, and operator posture.
            This should answer whether the system is healthy before anyone drills into a lower-level pane.
          </p>
        </div>
        <div className="rounded-2xl border border-[var(--border)] bg-[var(--panel)] px-4 py-3 text-sm text-[var(--dim)]">
          <div className="font-medium text-[var(--fg)]">{node.data.name}</div>
          <div className="mt-1">
            {authenticated ? "Authenticated dashboard session" : "Guest preview"}{" "}
            · {connected ? "live feed connected" : "snapshot mode"}
          </div>
        </div>
      </div>

      <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-6">
        <SummaryStat
          label="Runtime"
          value={node.data.status}
          detail={`Uptime ${formatUptime(node.data.uptime_secs)} · v${node.data.version}`}
          tone={degradedServices === 0 ? "good" : "warn"}
        />
        <SummaryStat
          label="Services"
          value={`${healthyServices}/${node.data.services.length}`}
          detail={degradedServices === 0 ? "All tracked services healthy." : `${degradedServices} service(s) still warming or degraded.`}
          tone={degradedServices === 0 ? "good" : "warn"}
        />
        <SummaryStat
          label="Mesh Presence"
          value={mesh.data?.online ? `${peers} peers` : "offline"}
          detail={mesh.data?.online ? `Node ${mesh.data.node_id ?? "?"} is visible on the mesh.` : "This node is not currently participating in mesh discovery."}
          tone={mesh.data?.online ? "good" : "warn"}
        />
        <SummaryStat
          label="Data Sources"
          value={sources.length.toString()}
          detail={sources.length > 0 ? `${sourceKinds.length} source type(s) visible.` : "No registered sources yet."}
          tone={sources.length > 0 ? "good" : "neutral"}
        />
        <SummaryStat
          label="Audit Pressure"
          value={recentFailures.toString()}
          detail={`${auditEntries.length} recent event(s) captured · ${usersCount} known user(s)`}
          tone={recentFailures === 0 ? "good" : "bad"}
        />
        <SummaryStat
          label="Operators"
          value={usersCount.toString()}
          detail={usersCount > 0 ? "RBAC entries available for this node." : "No dashboard users returned."}
          tone={usersCount > 0 ? "good" : "neutral"}
        />
      </div>

      <div className="grid gap-6 xl:grid-cols-[1.2fr_0.8fr]">
        <Card title="Mesh Field">
          <div className="space-y-4">
            <div className="grid grid-cols-4 gap-3 md:grid-cols-6 xl:grid-cols-8">
              {currentNodeCells.map((entry) => (
                <div key={entry.id} className="border border-[var(--border)] bg-[var(--panel)] px-3 py-3 text-center">
                  <div
                    className={`mx-auto h-3 w-3 rounded-full ${
                      entry.status === "current"
                        ? "bg-[var(--accent)]"
                        : entry.status === "snapshot"
                          ? "bg-[var(--accent-warm)]"
                          : "bg-emerald-300"
                    }`}
                  />
                  <div className="mt-3 truncate text-[11px] font-semibold uppercase tracking-[0.14em] text-[var(--fg)]">
                    {entry.name}
                  </div>
                  <div className="mt-1 text-[10px] uppercase tracking-[0.14em] text-[var(--dim)]">{entry.detail}</div>
                </div>
              ))}
            </div>
            <div className="border-t border-[var(--border)] pt-4 text-[11px] uppercase tracking-[0.18em] text-[var(--dim)]">
              {mesh.data?.online
                ? `Current node is visible on the mesh as ${mesh.data.node_id ?? "unknown"}.`
                : "Mesh discovery is not currently active for this node."}
            </div>
          </div>
        </Card>

        <Card title="Operator Journal">
          <div className="space-y-3">
            {recentEvents.map((entry) => (
              <div key={entry.id} className="border-l border-[var(--border-strong)] pl-4">
                <div className="flex items-center justify-between gap-4">
                  <div className="text-[11px] font-semibold uppercase tracking-[0.16em] text-[var(--fg)]">
                    {entry.action}
                  </div>
                  <div className="text-[10px] uppercase tracking-[0.16em] text-[var(--dim)]">
                    {new Date(entry.timestamp).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}
                  </div>
                </div>
                <div className="mt-1 text-sm text-[var(--dim)]">
                  {entry.user_id} · {entry.target}
                </div>
                {entry.detail ? <div className="mt-2 text-sm leading-6 text-[var(--fg)]/80">{entry.detail}</div> : null}
              </div>
            ))}
            {recentEvents.length === 0 ? (
              <div className="border border-dashed border-[var(--border)] px-4 py-6 text-sm text-[var(--dim)]">
                No audit activity is visible yet.
              </div>
            ) : null}
          </div>
        </Card>
      </div>

      <div className="grid gap-6 xl:grid-cols-[1.05fr_0.95fr_0.8fr]">
        <Card title="Service Board">
          <div className="space-y-3">
            {node.data.services.map((service) => (
              <div
                key={service.name}
                className="flex items-start justify-between border border-[var(--border)] bg-[var(--panel)] px-4 py-4"
              >
                <div>
                  <div className="text-[11px] font-semibold uppercase tracking-[0.16em] text-[var(--fg)]">{service.name}</div>
                  <div className="mt-1 text-sm text-[var(--dim)]">Local endpoint :{service.port}</div>
                </div>
                <span
                  className={
                    service.status === "healthy"
                      ? "border border-emerald-300/30 bg-emerald-400/12 px-3 py-1 text-xs font-medium uppercase tracking-[0.16em] text-emerald-300"
                      : "border border-amber-300/30 bg-amber-400/12 px-3 py-1 text-xs font-medium uppercase tracking-[0.16em] text-amber-300"
                  }
                >
                  {service.status}
                </span>
              </div>
            ))}
          </div>
        </Card>

        <Card title="Knowledge Estate">
          <div className="space-y-3">
            {sourceKinds.slice(0, 6).map(([kind, count]) => (
              <div key={kind} className="border border-[var(--border)] bg-[var(--panel)] px-4 py-4">
                <div className="flex items-center justify-between gap-4">
                  <div className="text-[11px] font-semibold uppercase tracking-[0.16em] text-[var(--fg)]">{kind}</div>
                  <div className="text-[10px] uppercase tracking-[0.16em] text-[var(--dim)]">{count} source(s)</div>
                </div>
                <div className="mt-2 text-sm text-[var(--dim)]">
                  {sources
                    .filter((source) => source.kind === kind)
                    .slice(0, 3)
                    .map((source) => source.name)
                    .join(" · ")}
                </div>
              </div>
            ))}
            {sourceKinds.length === 0 ? (
              <div className="border border-dashed border-[var(--border)] px-4 py-6 text-sm text-[var(--dim)]">
                No dataset sources are registered yet.
              </div>
            ) : null}
          </div>
        </Card>

        <Card title="Access Surface">
          <div className="space-y-3">
            {(users.data ?? []).slice(0, 8).map((user) => (
              <div key={user.id} className="border border-[var(--border)] bg-[var(--panel)] px-4 py-4">
                <div className="flex items-center justify-between gap-4">
                  <div className="text-[11px] font-semibold uppercase tracking-[0.16em] text-[var(--fg)]">{user.id}</div>
                  <div className="text-[10px] uppercase tracking-[0.16em] text-[var(--accent)]">{user.role}</div>
                </div>
                <div className="mt-2 text-sm text-[var(--dim)]">
                  {user.permissions.length > 0 ? user.permissions.join(" · ") : "No explicit permissions listed."}
                </div>
              </div>
            ))}
            {usersCount === 0 ? (
              <div className="border border-dashed border-[var(--border)] px-4 py-6 text-sm text-[var(--dim)]">
                No RBAC users were returned by the node.
              </div>
            ) : null}
          </div>
        </Card>
      </div>
    </div>
  );
}
