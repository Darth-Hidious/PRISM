import type { WsEvent } from "../hooks/useWebSocket";
import Card from "../components/Card";
import SessionNotice from "../components/SessionNotice";

// Live activity is the node's real WebSocket stream — NodeStatusUpdate (every
// 5s), MeshPeerChange (on peer add/remove), and AuditEntry (on each audited
// action). Events are captured by the single app-level WS connection and
// passed in here; this page never polls and never fabricates liveness.

export interface ActivityItem {
  id: number;
  event: WsEvent;
  at: Date;
}

function describe(event: WsEvent): { label: string; detail: string; dot: string } {
  switch (event.type) {
    case "NodeStatusUpdate": {
      const healthy = event.data.services.filter((s) => s.healthy).length;
      const total = event.data.services.length;
      const hours = Math.floor(event.data.uptime_secs / 3600);
      const mins = Math.floor((event.data.uptime_secs % 3600) / 60);
      return {
        label: "Node status",
        detail: `uptime ${hours}h ${mins}m · ${healthy}/${total} service(s) healthy`,
        dot: "bg-[var(--accent)]",
      };
    }
    case "MeshPeerChange":
      return {
        label: `Mesh peer ${event.data.action}`,
        detail: `${event.data.name} (${event.data.node_id})`,
        dot: "bg-emerald-300",
      };
    case "AuditEntry":
      return {
        label: `Audit · ${event.data.action}`,
        detail: `${event.data.user} · ${new Date(event.data.timestamp).toLocaleString()}`,
        dot: "bg-[var(--accent-warm)]",
      };
  }
}

export default function Activity({
  events,
  connected,
  authenticated,
}: {
  events: ActivityItem[];
  connected: boolean;
  authenticated: boolean;
}) {
  if (!authenticated) {
    return (
      <div className="space-y-6 max-w-4xl">
        <h1 className="text-xl font-bold">Activity</h1>
        <SessionNotice detail="The live event stream is delivered over an authenticated WebSocket and requires a dashboard session." />
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <div className="flex items-end justify-between gap-6">
        <div>
          <div className="text-[11px] uppercase tracking-[0.2em] text-[var(--dim)]">Realtime</div>
          <h1 className="mt-2 text-3xl font-semibold tracking-tight">Live Activity</h1>
          <p className="mt-2 max-w-3xl text-sm leading-6 text-[var(--dim)]">
            Events streamed from this node as they happen — status ticks, mesh peer changes, and
            audited actions. This is the live feed, not a persistent log; the Audit tab holds history.
          </p>
        </div>
        <div
          className={
            connected
              ? "rounded-sm border border-emerald-300/30 bg-emerald-400/12 px-3 py-2 text-[10px] font-medium uppercase tracking-[0.16em] text-emerald-300"
              : "rounded-sm border border-amber-300/30 bg-amber-400/12 px-3 py-2 text-[10px] font-medium uppercase tracking-[0.16em] text-amber-300"
          }
        >
          {connected ? "Stream connected" : "Reconnecting..."}
        </div>
      </div>

      <Card title={`Event stream (${events.length})`}>
        {events.length === 0 ? (
          <div className="border border-dashed border-[var(--border)] px-4 py-8 text-sm leading-6 text-[var(--dim)]">
            {connected
              ? "Connected — waiting for the first event. The node broadcasts a status tick every 5 seconds."
              : "Not connected to the event stream yet. If this persists, the session token may be invalid or the node may be down."}
          </div>
        ) : (
          <div className="space-y-2">
            {events.map((item) => {
              const { label, detail, dot } = describe(item.event);
              return (
                <div
                  key={item.id}
                  className="flex items-start gap-3 border border-[var(--border)] bg-[var(--panel)] px-4 py-3"
                >
                  <span className={`mt-1.5 h-2 w-2 shrink-0 rounded-full ${dot}`} />
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center justify-between gap-4">
                      <div className="text-[11px] font-semibold uppercase tracking-[0.16em] text-[var(--fg)]">
                        {label}
                      </div>
                      <div className="shrink-0 text-[10px] uppercase tracking-[0.16em] text-[var(--dim)]">
                        {item.at.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" })}
                      </div>
                    </div>
                    <div className="mt-1 truncate text-sm text-[var(--dim)]">{detail}</div>
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </Card>
    </div>
  );
}
