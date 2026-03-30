import { useQuery } from "@tanstack/react-query";
import { api } from "../api/client";
import Card from "../components/Card";

export default function NodeStatus() {
  const { data, isLoading, error } = useQuery({
    queryKey: ["node-info"],
    queryFn: api.getNodeInfo,
  });

  if (isLoading) return <p className="text-[var(--dim)]">Loading...</p>;
  if (error) return <p className="text-red-400">Error: {(error as Error).message}</p>;
  if (!data) return null;

  const uptime = data.uptime_secs;
  const hours = Math.floor(uptime / 3600);
  const mins = Math.floor((uptime % 3600) / 60);

  return (
    <div className="space-y-6 max-w-2xl">
      <h1 className="text-xl font-bold">Node Status</h1>

      <Card title="Overview">
        <dl className="grid grid-cols-2 gap-y-3 text-sm">
          <dt className="text-[var(--dim)]">Name</dt>
          <dd className="font-mono">{data.name}</dd>
          <dt className="text-[var(--dim)]">Version</dt>
          <dd className="font-mono">{data.version}</dd>
          <dt className="text-[var(--dim)]">Uptime</dt>
          <dd className="font-mono">{hours}h {mins}m</dd>
        </dl>
      </Card>

      <Card title="Services">
        <div className="space-y-2">
          {data.services.map((s) => (
            <div
              key={s.name}
              className="flex items-center justify-between text-sm border-b border-[var(--border)] pb-2 last:border-0"
            >
              <span>{s.name}</span>
              <span className="font-mono text-xs">
                <span className="text-[var(--dim)]">:{s.port}</span>{" "}
                <span
                  className={
                    s.status === "healthy"
                      ? "text-green-400"
                      : s.status === "starting"
                        ? "text-[var(--accent)]"
                        : "text-red-400"
                  }
                >
                  {s.status}
                </span>
              </span>
            </div>
          ))}
        </div>
      </Card>
    </div>
  );
}
