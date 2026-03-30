import { useQuery } from "@tanstack/react-query";
import { api } from "../api/client";
import Card from "../components/Card";
import Table from "../components/Table";

export default function MeshStatus() {
  const nodes = useQuery({ queryKey: ["mesh-nodes"], queryFn: api.getMeshNodes });
  const subs = useQuery({ queryKey: ["mesh-subs"], queryFn: api.getMeshSubscriptions });

  return (
    <div className="space-y-6 max-w-4xl">
      <h1 className="text-xl font-bold">Mesh</h1>

      <Card title="Peer Nodes">
        {nodes.isLoading ? (
          <p className="text-[var(--dim)]">Loading...</p>
        ) : nodes.error ? (
          <p className="text-red-400">{(nodes.error as Error).message}</p>
        ) : (
          <>
            <p className="text-sm text-[var(--dim)] mb-2">
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
