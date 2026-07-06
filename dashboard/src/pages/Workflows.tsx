import { useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { api, type WorkflowSummary } from "../api/client";
import Card from "../components/Card";
import SessionNotice from "../components/SessionNotice";

// Workflows are declarative specs discovered from the node's search paths.
// "Dry run" resolves + plans without executing (execute:false); "Run" executes
// the real engine (execute:true) — an operator action gated by ExecuteTools.

function WorkflowRow({ workflow }: { workflow: WorkflowSummary }) {
  const [open, setOpen] = useState(false);
  const [values, setValues] = useState<Record<string, string>>({});

  const run = useMutation({
    mutationFn: (execute: boolean) => api.runWorkflow(workflow.name, values, execute),
  });

  return (
    <div className="border border-[var(--border)] bg-[var(--panel)] px-4 py-4">
      <div className="flex items-start justify-between gap-4">
        <div className="min-w-0">
          <div className="text-[11px] font-semibold uppercase tracking-[0.16em] text-[var(--accent)]">
            {workflow.name}
          </div>
          <div className="mt-1 text-sm leading-6 text-[var(--fg)]">
            {workflow.description || "(no description provided)"}
          </div>
          <div className="mt-2 text-[11px] uppercase tracking-[0.14em] text-[var(--dim)]">
            {workflow.steps} step(s)
            {workflow.arguments.length > 0
              ? ` · args: ${workflow.arguments.join(", ")}`
              : " · no arguments"}
          </div>
        </div>
        <button
          type="button"
          onClick={() => setOpen(!open)}
          className="shrink-0 rounded-sm border border-[var(--border-strong)] px-3 py-1.5 text-[10px] font-medium uppercase tracking-[0.16em] text-[var(--fg)] transition-colors hover:border-[var(--accent)] hover:text-[var(--accent)]"
        >
          {open ? "Close" : "Run"}
        </button>
      </div>

      {open ? (
        <div className="mt-4 space-y-4 border-t border-[var(--border)] pt-4">
          {workflow.arguments.length > 0 ? (
            <div className="grid gap-3 md:grid-cols-2">
              {workflow.arguments.map((arg) => (
                <label
                  key={arg}
                  className="flex flex-col gap-1 text-[11px] uppercase tracking-[0.16em] text-[var(--dim)]"
                >
                  {arg}
                  <input
                    value={values[arg] ?? ""}
                    onChange={(e) => setValues({ ...values, [arg]: e.target.value })}
                    className="rounded-lg border border-[var(--border)] bg-[var(--bg)] px-3 py-2 text-sm normal-case tracking-normal text-[var(--fg)] focus:border-[var(--accent)] focus:outline-none"
                  />
                </label>
              ))}
            </div>
          ) : (
            <p className="text-sm text-[var(--dim)]">This workflow takes no arguments.</p>
          )}

          <div className="flex items-center gap-3">
            <button
              type="button"
              onClick={() => run.mutate(false)}
              disabled={run.isPending}
              className="rounded-lg border border-[var(--border-strong)] px-4 py-2 text-sm font-medium text-[var(--fg)] transition-colors hover:border-[var(--accent)] hover:text-[var(--accent)] disabled:opacity-50"
            >
              {run.isPending && run.variables === false ? "Planning..." : "Dry run"}
            </button>
            <button
              type="button"
              onClick={() => run.mutate(true)}
              disabled={run.isPending}
              className="rounded-lg bg-[var(--accent)] px-4 py-2 text-sm font-medium text-[var(--bg)] transition-opacity hover:opacity-90 disabled:opacity-50"
            >
              {run.isPending && run.variables === true ? "Running..." : "Run for real"}
            </button>
          </div>

          {run.error ? (
            <p className="text-sm text-red-400">{(run.error as Error).message}</p>
          ) : null}
          {run.data ? (
            <pre className="max-h-96 overflow-auto rounded-lg bg-[var(--bg)] p-4 font-mono text-xs text-[var(--fg)]/85">
              {JSON.stringify(run.data, null, 2)}
            </pre>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

export default function Workflows({ authenticated }: { authenticated: boolean }) {
  const workflows = useQuery({
    queryKey: ["workflows"],
    queryFn: api.getWorkflows,
    enabled: authenticated,
  });

  if (!authenticated) {
    return (
      <div className="space-y-6 max-w-4xl">
        <h1 className="text-xl font-bold">Workflows</h1>
        <SessionNotice detail="Workflow specs and execution require a dashboard session." />
      </div>
    );
  }

  const list = workflows.data?.workflows ?? [];

  return (
    <div className="space-y-6">
      <div>
        <div className="text-[11px] uppercase tracking-[0.2em] text-[var(--dim)]">Automation</div>
        <h1 className="mt-2 text-3xl font-semibold tracking-tight">Workflows</h1>
        <p className="mt-2 max-w-3xl text-sm leading-6 text-[var(--dim)]">
          Declarative specs discovered on this node. Dry-run to see the resolved plan, or run for
          real to execute the engine.
        </p>
      </div>

      <Card title={workflows.data ? `Installed workflows (${workflows.data.count})` : "Installed workflows"}>
        {workflows.isLoading ? (
          <p className="text-sm text-[var(--dim)]">Loading workflows...</p>
        ) : workflows.error ? (
          <p className="text-sm text-red-400">Error: {(workflows.error as Error).message}</p>
        ) : list.length === 0 ? (
          <div className="border border-dashed border-[var(--border)] px-4 py-8 text-sm leading-6 text-[var(--dim)]">
            No workflows were discovered on this node. Drop a workflow spec into one of the node’s
            workflow search paths and it will appear here.
          </div>
        ) : (
          <div className="space-y-3">
            {list.map((workflow) => (
              <WorkflowRow key={workflow.name} workflow={workflow} />
            ))}
          </div>
        )}
      </Card>
    </div>
  );
}
