import { useQuery } from "@tanstack/react-query";
import { api } from "../api/client";
import Card from "../components/Card";
import SessionNotice from "../components/SessionNotice";
import Table from "../components/Table";

export default function AuditLog({ authenticated }: { authenticated: boolean }) {
  if (!authenticated) {
    return (
      <div className="space-y-6 max-w-4xl">
        <h1 className="text-xl font-bold">Audit Log</h1>
        <SessionNotice detail="Audit history is permission-gated and requires a dashboard session." />
      </div>
    );
  }

  const { data, isLoading, error } = useQuery({
    queryKey: ["audit"],
    queryFn: api.getAuditLog,
  });

  if (isLoading) return <p className="text-[var(--dim)]">Loading...</p>;
  if (error) return <p className="text-red-400">Error: {(error as Error).message}</p>;

  return (
    <div className="space-y-6 max-w-4xl">
      <h1 className="text-xl font-bold">Audit Log</h1>
      <Card title="Recent Events">
        <Table
          columns={[
            { key: "timestamp", header: "Time" },
            { key: "action", header: "Action" },
            { key: "user_id", header: "User" },
            { key: "target", header: "Target" },
            { key: "outcome", header: "Outcome" },
            { key: "detail", header: "Detail" },
          ]}
          rows={data ?? []}
        />
      </Card>
    </div>
  );
}
