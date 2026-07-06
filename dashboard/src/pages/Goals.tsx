import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, type CreateGoalRequest, type GoalSummary } from "../api/client";
import Card from "../components/Card";
import SessionNotice from "../components/SessionNotice";
import { formatRelative } from "../lib/format";

// Goals are durable research campaigns. The server exposes their last-saved
// checkpoint (goal text, iteration, candidate count) — it does NOT expose a
// live running/paused/detached state, so we render only the checkpoint facts
// and never invent a status badge.

function progressLine(goal: GoalSummary): string {
  const parts: string[] = [];
  if (goal.iteration != null) parts.push(`iteration ${goal.iteration}`);
  if (goal.candidates_evaluated != null)
    parts.push(`${goal.candidates_evaluated} candidate(s) evaluated`);
  return parts.length > 0 ? parts.join(" · ") : "no checkpoint progress recorded yet";
}

export default function Goals({ authenticated }: { authenticated: boolean }) {
  const queryClient = useQueryClient();
  const [goalText, setGoalText] = useState("");
  const [maxIterations, setMaxIterations] = useState("");
  const [budgetUsd, setBudgetUsd] = useState("");
  const [expandedId, setExpandedId] = useState<string | null>(null);

  const goals = useQuery({
    queryKey: ["goals"],
    queryFn: api.getGoals,
    enabled: authenticated,
  });

  const detail = useQuery({
    queryKey: ["goal", expandedId],
    queryFn: () => api.getGoal(expandedId as string),
    enabled: authenticated && expandedId != null,
  });

  const createMutation = useMutation({
    mutationFn: (body: CreateGoalRequest) => api.createGoal(body),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["goals"] });
      setGoalText("");
      setMaxIterations("");
      setBudgetUsd("");
    },
  });

  const resumeMutation = useMutation({
    mutationFn: (id: string) => api.resumeGoal(id),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["goals"] }),
  });

  if (!authenticated) {
    return (
      <div className="space-y-6 max-w-4xl">
        <h1 className="text-xl font-bold">Goals</h1>
        <SessionNotice detail="Research goals run real, long-lived work on this node and require a dashboard session." />
      </div>
    );
  }

  function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    const goal = goalText.trim();
    if (!goal) return;
    const body: CreateGoalRequest = { goal };
    const iters = Number(maxIterations);
    if (maxIterations.trim() && Number.isFinite(iters) && iters > 0) body.max_iterations = iters;
    const budget = Number(budgetUsd);
    if (budgetUsd.trim() && Number.isFinite(budget) && budget > 0) body.budget_usd = budget;
    createMutation.mutate(body);
  }

  const goalList = goals.data?.goals ?? [];

  return (
    <div className="space-y-6">
      <div>
        <div className="text-[11px] uppercase tracking-[0.2em] text-[var(--dim)]">Automation</div>
        <h1 className="mt-2 text-3xl font-semibold tracking-tight">Research Goals</h1>
        <p className="mt-2 max-w-3xl text-sm leading-6 text-[var(--dim)]">
          Long-running research campaigns. Starting or resuming a goal launches a detached
          worker on this node through the same executor the agent uses.
        </p>
      </div>

      <Card title="Start a Goal">
        <form onSubmit={handleSubmit} className="space-y-4">
          <textarea
            value={goalText}
            onChange={(e) => setGoalText(e.target.value)}
            placeholder="Describe the research goal, e.g. 'Discover a corrosion-resistant high-entropy alloy for oxidising gas flow'"
            rows={3}
            className="w-full resize-y rounded-lg border border-[var(--border)] bg-[var(--bg)] px-4 py-3 text-sm text-[var(--fg)] placeholder:text-[var(--dim)] focus:border-[var(--accent)] focus:outline-none"
          />
          <div className="flex flex-wrap items-end gap-4">
            <label className="flex flex-col gap-1 text-[11px] uppercase tracking-[0.16em] text-[var(--dim)]">
              Max iterations (optional)
              <input
                type="number"
                min={1}
                value={maxIterations}
                onChange={(e) => setMaxIterations(e.target.value)}
                placeholder="unset"
                className="w-40 rounded-lg border border-[var(--border)] bg-[var(--bg)] px-3 py-2 text-sm text-[var(--fg)] placeholder:text-[var(--dim)] focus:border-[var(--accent)] focus:outline-none"
              />
            </label>
            <label className="flex flex-col gap-1 text-[11px] uppercase tracking-[0.16em] text-[var(--dim)]">
              Budget USD (optional)
              <input
                type="number"
                min={0}
                step="0.01"
                value={budgetUsd}
                onChange={(e) => setBudgetUsd(e.target.value)}
                placeholder="unset"
                className="w-40 rounded-lg border border-[var(--border)] bg-[var(--bg)] px-3 py-2 text-sm text-[var(--fg)] placeholder:text-[var(--dim)] focus:border-[var(--accent)] focus:outline-none"
              />
            </label>
            <button
              type="submit"
              disabled={createMutation.isPending || !goalText.trim()}
              className="rounded-lg bg-[var(--accent)] px-5 py-2 text-sm font-medium text-[var(--bg)] transition-opacity hover:opacity-90 disabled:opacity-50"
            >
              {createMutation.isPending ? "Starting..." : "Start goal"}
            </button>
          </div>
        </form>

        {createMutation.error ? (
          <p className="mt-4 text-sm text-red-400">{(createMutation.error as Error).message}</p>
        ) : null}
        {createMutation.data ? (
          <div className="mt-4 border border-emerald-300/30 bg-emerald-400/10 px-4 py-3 text-sm text-emerald-200">
            <div className="font-medium">Goal accepted — a detached worker is running.</div>
            <pre className="mt-2 overflow-auto whitespace-pre-wrap break-words font-mono text-xs text-emerald-100/90">
              {JSON.stringify(createMutation.data.result, null, 2)}
            </pre>
          </div>
        ) : null}
      </Card>

      <Card title={goals.data ? `Goals on this node (${goals.data.count})` : "Goals on this node"}>
        {goals.isLoading ? (
          <p className="text-sm text-[var(--dim)]">Loading goals...</p>
        ) : goals.error ? (
          <p className="text-sm text-red-400">Error: {(goals.error as Error).message}</p>
        ) : goalList.length === 0 ? (
          <div className="border border-dashed border-[var(--border)] px-4 py-8 text-sm leading-6 text-[var(--dim)]">
            No goals have been started on this node yet. Use “Start a Goal” above to launch one —
            it will appear here with its checkpoint progress.
            {goals.data?.source ? (
              <div className="mt-2 text-[11px] uppercase tracking-[0.14em]">
                Checkpoints: {goals.data.source}
              </div>
            ) : null}
          </div>
        ) : (
          <div className="space-y-3">
            {goalList.map((goal) => {
              const resuming = resumeMutation.isPending && resumeMutation.variables === goal.id;
              const isOpen = expandedId === goal.id;
              return (
                <div key={goal.id} className="border border-[var(--border)] bg-[var(--panel)] px-4 py-4">
                  <div className="flex items-start justify-between gap-4">
                    <div className="min-w-0">
                      <div className="truncate text-[11px] font-semibold uppercase tracking-[0.16em] text-[var(--accent)]">
                        {goal.id}
                      </div>
                      <div className="mt-1 text-sm leading-6 text-[var(--fg)]">
                        {goal.goal ?? goal.error ?? "(goal text unavailable)"}
                      </div>
                      <div className="mt-2 text-[11px] uppercase tracking-[0.14em] text-[var(--dim)]">
                        {progressLine(goal)}
                        {goal.created ? ` · started ${formatRelative(goal.created)}` : ""}
                      </div>
                    </div>
                    <div className="flex shrink-0 flex-col items-end gap-2">
                      <button
                        type="button"
                        onClick={() => resumeMutation.mutate(goal.id)}
                        disabled={resuming}
                        className="rounded-sm border border-[var(--border-strong)] px-3 py-1.5 text-[10px] font-medium uppercase tracking-[0.16em] text-[var(--fg)] transition-colors hover:border-[var(--accent)] hover:text-[var(--accent)] disabled:opacity-50"
                      >
                        {resuming ? "Resuming..." : "Resume"}
                      </button>
                      <button
                        type="button"
                        onClick={() => setExpandedId(isOpen ? null : goal.id)}
                        className="rounded-sm border border-[var(--border)] px-3 py-1.5 text-[10px] font-medium uppercase tracking-[0.16em] text-[var(--dim)] transition-colors hover:text-[var(--fg)]"
                      >
                        {isOpen ? "Hide checkpoint" : "View checkpoint"}
                      </button>
                    </div>
                  </div>

                  {resumeMutation.error && resumeMutation.variables === goal.id ? (
                    <p className="mt-3 text-sm text-red-400">
                      {(resumeMutation.error as Error).message}
                    </p>
                  ) : null}

                  {isOpen ? (
                    <div className="mt-3 border-t border-[var(--border)] pt-3">
                      {detail.isLoading ? (
                        <p className="text-sm text-[var(--dim)]">Loading checkpoint...</p>
                      ) : detail.error ? (
                        <p className="text-sm text-red-400">{(detail.error as Error).message}</p>
                      ) : (
                        <pre className="max-h-96 overflow-auto rounded-lg bg-[var(--bg)] p-4 font-mono text-xs text-[var(--fg)]/85">
                          {JSON.stringify(detail.data, null, 2)}
                        </pre>
                      )}
                    </div>
                  ) : null}
                </div>
              );
            })}
          </div>
        )}
      </Card>
    </div>
  );
}
