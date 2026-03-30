import { useState } from "react";
import { useMutation } from "@tanstack/react-query";
import { api } from "../api/client";
import Card from "../components/Card";

export default function QueryPage() {
  const [query, setQuery] = useState("");

  const mutation = useMutation({
    mutationFn: (q: string) => api.query(q),
  });

  function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    if (query.trim()) mutation.mutate(query.trim());
  }

  return (
    <div className="space-y-6 max-w-4xl">
      <h1 className="text-xl font-bold">Query</h1>

      <Card title="Execute Query">
        <form onSubmit={handleSubmit} className="flex gap-3">
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Enter a query..."
            className="flex-1 rounded-lg bg-[var(--bg)] border border-[var(--border)] px-4 py-2 text-sm text-[var(--fg)] placeholder:text-[var(--dim)] focus:outline-none focus:border-[var(--accent)]"
          />
          <button
            type="submit"
            disabled={mutation.isPending}
            className="rounded-lg bg-[var(--accent)] px-5 py-2 text-sm font-medium text-[var(--bg)] hover:opacity-90 disabled:opacity-50"
          >
            {mutation.isPending ? "Running..." : "Run"}
          </button>
        </form>
      </Card>

      {mutation.data && (
        <Card title={`Results (${mutation.data.results.length})`}>
          <pre className="overflow-auto text-xs font-mono bg-[var(--bg)] rounded-lg p-4 max-h-96">
            {JSON.stringify(mutation.data.results, null, 2)}
          </pre>
        </Card>
      )}

      {mutation.error && (
        <Card title="Error">
          <p className="text-red-400 text-sm">{(mutation.error as Error).message}</p>
        </Card>
      )}
    </div>
  );
}
