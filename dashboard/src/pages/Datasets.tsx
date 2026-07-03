import { useQuery } from "@tanstack/react-query";
import { api } from "../api/client";
import Card from "../components/Card";
import SessionNotice from "../components/SessionNotice";
import Table from "../components/Table";

export default function Datasets({ authenticated }: { authenticated: boolean }) {
  if (!authenticated) {
    return (
      <div className="space-y-6 max-w-3xl">
        <h1 className="text-xl font-bold">Datasets</h1>
        <SessionNotice detail="Dataset inventories and source metadata require a dashboard session." />
      </div>
    );
  }

  const { data, isLoading, error } = useQuery({
    queryKey: ["data-sources"],
    queryFn: api.getDataSources,
  });

  if (isLoading) return <p className="text-[var(--dim)]">Loading...</p>;
  if (error) return <p className="text-red-400">Error: {(error as Error).message}</p>;

  return (
    <div className="space-y-6 max-w-3xl">
      <h1 className="text-xl font-bold">Datasets</h1>
      <Card title="Data Sources">
        <Table
          columns={[
            { key: "name", header: "Name" },
            { key: "kind", header: "Type" },
            { key: "id", header: "ID" },
          ]}
          rows={data ?? []}
        />
      </Card>
    </div>
  );
}
