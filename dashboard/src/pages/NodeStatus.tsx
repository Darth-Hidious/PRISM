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

  const [node, mesh, audit, users] = useQueries({
    queries: [
      { queryKey: ["node-info"], queryFn: api.getNodeInfo, enabled: authenticated },
      { queryKey: ["mesh-nodes"], queryFn: api.getMeshNodes, enabled: authenticated },
      { queryKey: ["audit"], queryFn: api.getAuditLog, enabled: authenticated },
      { queryKey: ["users"], queryFn: api.getUsers, enabled: authenticated },
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

      <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-4">
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
          label="Audit Pressure"
          value={recentFailures.toString()}
          detail={`${auditEntries.length} recent event(s) captured · ${usersCount} known user(s)`}
          tone={recentFailures === 0 ? "good" : "bad"}
        />
      </div>

      <div className="grid gap-6 xl:grid-cols-[1.35fr_0.95fr]">
        <Card title="Service Board">
          <div className="space-y-3">
            {node.data.services.map((service) => (
              <div
                key={service.name}
                className="flex items-start justify-between rounded-2xl border border-[var(--border)] bg-[var(--panel)] px-4 py-4"
              >
                <div>
                  <div className="font-medium text-[var(--fg)]">{service.name}</div>
                  <div className="mt-1 text-sm text-[var(--dim)]">Local endpoint :{service.port}</div>
                </div>
                <span
                  className={
                    service.status === "healthy"
                      ? "rounded-full bg-emerald-400/12 px-3 py-1 text-xs font-medium text-emerald-300"
                      : "rounded-full bg-amber-400/12 px-3 py-1 text-xs font-medium text-amber-300"
                  }
                >
                  {service.status}
                </span>
              </div>
            ))}
          </div>
        </Card>

        <Card title="Recent Audit Signal">
          <div className="space-y-3">
            {auditEntries.slice(0, 6).map((entry) => (
              <div key={entry.id} className="rounded-2xl border border-[var(--border)] bg-[var(--panel)] px-4 py-4">
                <div className="flex items-start justify-between gap-4">
                  <div>
                    <div className="font-medium text-[var(--fg)]">{entry.action}</div>
                    <div className="mt-1 text-sm text-[var(--dim)]">
                      {entry.user_id} · {entry.target}
                    </div>
                  </div>
                  <div className="text-right text-xs text-[var(--dim)]">
                    <div>{entry.outcome}</div>
                    <div className="mt-1">{new Date(entry.timestamp).toLocaleString()}</div>
                  </div>
                </div>
                {entry.detail ? <div className="mt-3 text-sm leading-6 text-[var(--dim)]">{entry.detail}</div> : null}
              </div>
            ))}
            {auditEntries.length === 0 ? (
              <div className="rounded-2xl border border-dashed border-[var(--border)] px-4 py-6 text-sm text-[var(--dim)]">
                No audit activity is visible yet.
              </div>
            ) : null}
          </div>
        </Card>
      </div>
    </div>
  );
}
