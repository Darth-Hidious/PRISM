import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { api, type ToolInfo } from "../api/client";
import Card from "../components/Card";
import SessionNotice from "../components/SessionNotice";

// The tool catalog is the node's live registry — names, versions, and the
// commands/args each tool exposes. Read-only; nothing here is fabricated.

function ToolRow({ tool }: { tool: ToolInfo }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="border border-[var(--border)] bg-[var(--panel)] px-4 py-4">
      <button
        type="button"
        onClick={() => setOpen(!open)}
        className="flex w-full items-start justify-between gap-4 text-left"
      >
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <span className="text-[11px] font-semibold uppercase tracking-[0.16em] text-[var(--accent)]">
              {tool.name}
            </span>
            <span className="text-[10px] uppercase tracking-[0.14em] text-[var(--dim)]">
              v{tool.version}
            </span>
          </div>
          <div className="mt-1 text-sm leading-6 text-[var(--fg)]">
            {tool.description || "(no description provided)"}
          </div>
          <div className="mt-2 text-[11px] uppercase tracking-[0.14em] text-[var(--dim)]">
            {tool.commands.length} command(s)
          </div>
        </div>
        <span className="shrink-0 text-[10px] uppercase tracking-[0.16em] text-[var(--dim)]">
          {open ? "Hide" : "Show"}
        </span>
      </button>

      {open ? (
        <div className="mt-4 space-y-3 border-t border-[var(--border)] pt-4">
          {tool.commands.length === 0 ? (
            <p className="text-sm text-[var(--dim)]">This tool exposes no commands.</p>
          ) : (
            tool.commands.map((cmd) => (
              <div key={cmd.name} className="border border-[var(--border)] bg-[var(--bg)] px-3 py-3">
                <div className="text-[11px] font-semibold uppercase tracking-[0.14em] text-[var(--fg)]">
                  {cmd.name}
                </div>
                {cmd.description ? (
                  <div className="mt-1 text-sm text-[var(--dim)]">{cmd.description}</div>
                ) : null}
                {cmd.args.length > 0 ? (
                  <ul className="mt-2 space-y-1">
                    {cmd.args.map((arg) => (
                      <li key={arg.name} className="text-[13px] text-[var(--fg)]/85">
                        <span className="font-mono text-[var(--accent)]">{arg.name}</span>
                        <span className="text-[var(--dim)]">
                          {" "}
                          : {arg.arg_type}
                          {arg.required ? " · required" : " · optional"}
                        </span>
                        {arg.description ? (
                          <span className="text-[var(--dim)]"> — {arg.description}</span>
                        ) : null}
                      </li>
                    ))}
                  </ul>
                ) : (
                  <div className="mt-2 text-[13px] text-[var(--dim)]">No arguments.</div>
                )}
              </div>
            ))
          )}
        </div>
      ) : null}
    </div>
  );
}

export default function Tools({ authenticated }: { authenticated: boolean }) {
  const [filter, setFilter] = useState("");

  const tools = useQuery({
    queryKey: ["tools"],
    queryFn: api.getTools,
    enabled: authenticated,
  });

  const filtered = useMemo(() => {
    const all = tools.data ?? [];
    const q = filter.trim().toLowerCase();
    if (!q) return all;
    return all.filter(
      (t) =>
        t.name.toLowerCase().includes(q) || t.description.toLowerCase().includes(q),
    );
  }, [tools.data, filter]);

  if (!authenticated) {
    return (
      <div className="space-y-6 max-w-4xl">
        <h1 className="text-xl font-bold">Tools</h1>
        <SessionNotice detail="The tool catalog requires a dashboard session." />
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <div>
        <div className="text-[11px] uppercase tracking-[0.2em] text-[var(--dim)]">Capabilities</div>
        <h1 className="mt-2 text-3xl font-semibold tracking-tight">Tool Catalog</h1>
        <p className="mt-2 max-w-3xl text-sm leading-6 text-[var(--dim)]">
          Every tool this node has registered, with the commands and arguments it exposes.
        </p>
      </div>

      <Card
        title={
          tools.data
            ? `Registered tools (${tools.data.length})`
            : "Registered tools"
        }
      >
        <div className="space-y-4">
          <input
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            placeholder="Filter tools by name or description..."
            className="w-full rounded-lg border border-[var(--border)] bg-[var(--bg)] px-4 py-2 text-sm text-[var(--fg)] placeholder:text-[var(--dim)] focus:border-[var(--accent)] focus:outline-none"
          />

          {tools.isLoading ? (
            <p className="text-sm text-[var(--dim)]">Loading tools...</p>
          ) : tools.error ? (
            <p className="text-sm text-red-400">Error: {(tools.error as Error).message}</p>
          ) : (tools.data ?? []).length === 0 ? (
            <div className="border border-dashed border-[var(--border)] px-4 py-8 text-sm leading-6 text-[var(--dim)]">
              No tools are registered on this node yet.
            </div>
          ) : filtered.length === 0 ? (
            <div className="border border-dashed border-[var(--border)] px-4 py-8 text-sm text-[var(--dim)]">
              No tools match “{filter}”.
            </div>
          ) : (
            <div className="space-y-3">
              {filtered.map((tool) => (
                <ToolRow key={tool.name} tool={tool} />
              ))}
            </div>
          )}
        </div>
      </Card>
    </div>
  );
}
