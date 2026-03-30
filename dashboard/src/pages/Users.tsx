import { useQuery } from "@tanstack/react-query";
import { api } from "../api/client";
import Card from "../components/Card";
import Table from "../components/Table";

export default function Users() {
  const { data, isLoading, error } = useQuery({
    queryKey: ["users"],
    queryFn: api.getUsers,
  });

  if (isLoading) return <p className="text-[var(--dim)]">Loading...</p>;
  if (error) return <p className="text-red-400">Error: {(error as Error).message}</p>;

  return (
    <div className="space-y-6 max-w-3xl">
      <h1 className="text-xl font-bold">Users</h1>
      <Card title="Node Users">
        <Table
          columns={[
            { key: "id", header: "User ID" },
            { key: "role", header: "Role" },
            {
              key: "permissions",
              header: "Permissions",
              render: (row) => (row as { permissions: string[] }).permissions.join(", "),
            },
          ]}
          rows={data ?? []}
        />
      </Card>
    </div>
  );
}
