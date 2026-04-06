import { useEffect, useMemo, useState } from "react";
import { Routes, Route, NavLink } from "react-router-dom";
import NodeStatus from "./pages/NodeStatus";
import QueryPage from "./pages/QueryPage";
import Datasets from "./pages/Datasets";
import AuditLog from "./pages/AuditLog";
import Users from "./pages/Users";
import MeshStatus from "./pages/MeshStatus";
import { bootstrapSessionTokenFromUrl, clearSessionToken, getSessionToken } from "./lib/session";
import { useWebSocket } from "./hooks/useWebSocket";

const navItems = [
  { to: "/", label: "Node" },
  { to: "/query", label: "Query" },
  { to: "/datasets", label: "Datasets" },
  { to: "/mesh", label: "Mesh" },
  { to: "/users", label: "Users" },
  { to: "/audit", label: "Audit" },
];

export default function App() {
  const [token, setToken] = useState<string | null>(null);
  const [clock, setClock] = useState(() => new Date());

  useEffect(() => {
    setToken(bootstrapSessionTokenFromUrl());
  }, []);

  useEffect(() => {
    const timer = window.setInterval(() => setClock(new Date()), 1000);
    return () => window.clearInterval(timer);
  }, []);

  const { connected } = useWebSocket({ token: token ?? "" });
  const sessionState = useMemo(() => {
    if (!token) {
      return {
        label: "Guest Preview",
        detail: "Protected panes need a dashboard session token in the URL.",
      };
    }
    return {
      label: connected ? "Live Session" : "Session Loaded",
      detail: connected ? "Realtime feed connected." : "Waiting for realtime feed.",
    };
  }, [connected, token]);

  return (
    <div className="min-h-screen bg-[var(--bg)] bg-grid text-[var(--fg)]">
      <header className="sticky top-0 z-40 border-b border-[var(--border)] bg-[var(--card)]/88 backdrop-blur-md">
        <div className="mx-auto flex h-16 max-w-[1560px] items-center justify-between gap-6 px-6">
          <div className="flex items-center gap-8">
            <div className="flex items-center gap-3">
              <div className="flex h-8 w-8 items-center justify-center border border-[var(--border-strong)] bg-[var(--panel-strong)] text-sm font-semibold tracking-tight text-[var(--fg)]">
                P
              </div>
              <div>
                <div className="text-[10px] uppercase tracking-[0.22em] text-[var(--dim)]">MARC27 Research Ops</div>
                <div className="text-lg font-semibold tracking-tight text-[var(--fg)]">PRISM Board</div>
              </div>
            </div>
            <nav className="hidden items-center gap-2 lg:flex">
              {navItems.map((item) => (
                <NavLink
                  key={item.to}
                  to={item.to}
                  end={item.to === "/"}
                  className={({ isActive }) =>
                    `rounded-sm px-3 py-1.5 text-[11px] font-medium uppercase tracking-[0.18em] transition-colors ${
                      isActive
                        ? "bg-[var(--panel-strong)] text-[var(--accent)]"
                        : "text-[var(--dim)] hover:bg-[var(--panel)] hover:text-[var(--fg)]"
                    }`
                  }
                >
                  {item.label}
                </NavLink>
              ))}
            </nav>
          </div>

          <div className="flex items-center gap-3">
            <div className="hidden min-w-[220px] items-center gap-2 border border-[var(--border)] bg-[var(--panel)] px-3 py-2 md:flex">
              <span className="text-[10px] uppercase tracking-[0.18em] text-[var(--dim)]">Command</span>
              <input
                readOnly
                value="OPEN / TOKEN / FILTER / PROJECT"
                className="w-full bg-transparent text-[11px] font-medium uppercase tracking-[0.16em] text-[var(--fg)] outline-none"
              />
            </div>
            <div className="rounded-sm border border-[var(--border)] bg-[var(--panel)] px-3 py-2 text-[10px] uppercase tracking-[0.18em] text-[var(--dim)]">
              {clock.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false })} UTC
            </div>
            <div className="rounded-sm border border-[var(--border-strong)] bg-[var(--panel-strong)] px-3 py-2 text-[10px] uppercase tracking-[0.18em] text-[var(--accent)]">
              {sessionState.label}
            </div>
          </div>
        </div>
      </header>

      <div className="border-b border-[var(--border)] bg-[var(--panel)]/78">
        <div className="mx-auto flex max-w-[1560px] flex-wrap items-center justify-between gap-4 px-6 py-4">
          <div>
            <div className="text-[10px] uppercase tracking-[0.22em] text-[var(--dim)]">Control Surface</div>
            <div className="mt-1 text-sm text-[var(--fg)]">{sessionState.detail}</div>
          </div>
          <div className="flex items-center gap-3">
            <div className="rounded-sm border border-[var(--border)] bg-[var(--card)] px-3 py-2 text-[10px] uppercase tracking-[0.18em] text-[var(--dim)]">
              {connected ? "Realtime feed live" : "Snapshot mode"}
            </div>
            {token ? (
              <button
                type="button"
                onClick={() => {
                  clearSessionToken();
                  setToken(getSessionToken());
                }}
                className="rounded-sm border border-[var(--border)] px-3 py-2 text-[10px] uppercase tracking-[0.18em] text-[var(--dim)] transition-colors hover:border-[var(--accent)] hover:text-[var(--accent)]"
              >
                Clear Session
              </button>
            ) : null}
          </div>
        </div>
      </div>

      <main className="mx-auto w-full max-w-[1560px] px-6 py-8">
        <Routes>
          <Route path="/" element={<NodeStatus connected={connected} authenticated={Boolean(token)} />} />
          <Route path="/query" element={<QueryPage authenticated={Boolean(token)} />} />
          <Route path="/datasets" element={<Datasets authenticated={Boolean(token)} />} />
          <Route path="/mesh" element={<MeshStatus />} />
          <Route path="/users" element={<Users authenticated={Boolean(token)} />} />
          <Route path="/audit" element={<AuditLog authenticated={Boolean(token)} />} />
        </Routes>
      </main>

      <footer className="border-t border-[var(--border)] bg-[var(--card)]">
        <div className="mx-auto flex max-w-[1560px] items-center justify-between gap-4 px-6 py-3 text-[10px] uppercase tracking-[0.18em] text-[var(--dim)]">
          <span>PRISM Board Preview</span>
          <span>CLI, TUI, and dashboard share one backend contract</span>
        </div>
      </footer>
    </div>
  );
}
