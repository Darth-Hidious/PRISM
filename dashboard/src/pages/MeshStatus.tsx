import { useQuery } from "@tanstack/react-query";
import { api } from "../api/client";
import Card from "../components/Card";
import SummaryStat from "../components/SummaryStat";
import Table from "../components/Table";

export default function MeshStatus() {
  const nodes = useQuery({ queryKey: ["mesh-nodes"], queryFn: api.getMeshNodes });
  const subs = useQuery({ queryKey: ["mesh-subs"], queryFn: api.getMeshSubscriptions });

  const peerCount = nodes.data?.peer_count ?? 0;
  const publishedCount = subs.data?.published.length ?? 0;
  const subscribedCount = subs.data?.subscribed.length ?? 0;

  return (
    <div className="max-w-6xl space-y-7">
      <div className="flex items-end justify-between gap-6">
        <div>
          <div className="text-[11px] uppercase tracking-[0.2em] text-[var(--dim)]">Federation Surface</div>
          <h1 className="mt-2 text-3xl font-semibold tracking-tight">Mesh Control</h1>
          <p className="mt-2 max-w-3xl text-sm leading-6 text-[var(--dim)]">
            Track node presence, published datasets, and active subscriptions across the PRISM mesh.
          </p>
        </div>
      </div>

      <div className="grid gap-4 md:grid-cols-3">
        <SummaryStat
          label="Mesh State"
          value={nodes.data?.online ? "online" : "offline"}
          detail={nodes.data?.node_id ? `Node ${nodes.data.node_id}` : "No active mesh identity."}
          tone={nodes.data?.online ? "good" : "warn"}
        />
        <SummaryStat
          label="Peer Count"
          value={peerCount.toString()}
          detail={peerCount > 0 ? "Remote peers are visible." : "No peer nodes discovered yet."}
          tone={peerCount > 0 ? "good" : "neutral"}
        />
        <SummaryStat
          label="Dataset Links"
          value={`${publishedCount}/${subscribedCount}`}
          detail="Published versus subscribed datasets."
          tone={publishedCount + subscribedCount > 0 ? "good" : "neutral"}
        />
      </div>

      <Card title="Peer Nodes">
        {nodes.isLoading ? (
          <p className="text-[var(--dim)]">Loading...</p>
        ) : nodes.error ? (
          <p className="text-red-400">{(nodes.error as Error).message}</p>
        ) : (
          <>
            <p className="mb-3 text-sm text-[var(--dim)]">
              Status: {nodes.data?.online ? "Online" : "Offline"}
              {nodes.data?.node_id && ` — Node ID: ${nodes.data.node_id}`}
              {` — ${nodes.data?.peer_count ?? 0} peer(s)`}
            </p>
            <Table
              columns={[
                { key: "name", header: "Name" },
                { key: "address", header: "Address" },
                { key: "port", header: "Port" },
                { key: "last_seen", header: "Last Seen" },
              ]}
              rows={nodes.data?.peers ?? []}
            />
          </>
        )}
      </Card>

      <Card title="Published Datasets">
        {subs.isLoading ? (
          <p className="text-[var(--dim)]">Loading...</p>
        ) : subs.error ? (
          <p className="text-red-400">{(subs.error as Error).message}</p>
        ) : (
          <Table
            columns={[
              { key: "name", header: "Dataset" },
              { key: "schema_version", header: "Schema" },
              { key: "subscriber_count", header: "Subscribers" },
            ]}
            rows={subs.data?.published ?? []}
          />
        )}
      </Card>

      <Card title="Active Subscriptions">
        {subs.isLoading ? null : subs.error ? null : (
          <Table
            columns={[
              { key: "dataset_name", header: "Dataset" },
              { key: "publisher_node", header: "Publisher" },
              { key: "subscribed_at", header: "Subscribed At" },
            ]}
            rows={subs.data?.subscribed ?? []}
          />
        )}
      </Card>
    </div>
  );
}
