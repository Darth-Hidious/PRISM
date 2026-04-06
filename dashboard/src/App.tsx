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

  useEffect(() => {
    setToken(bootstrapSessionTokenFromUrl());
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
    <div className="flex min-h-screen bg-[var(--bg)] text-[var(--fg)]">
      <nav className="w-64 shrink-0 border-r border-[var(--border)] bg-[var(--card)] px-5 py-6 flex flex-col gap-2">
        <div className="mb-6">
          <div className="text-[11px] uppercase tracking-[0.18em] text-[var(--dim)]">MARC27 Research Ops</div>
          <div className="mt-2 text-3xl font-semibold tracking-tight text-[var(--fg)]">PRISM</div>
          <div className="mt-3 rounded-2xl border border-[var(--border)] bg-[var(--panel)] px-3 py-3">
            <div className="text-xs font-medium text-[var(--accent)]">{sessionState.label}</div>
            <div className="mt-1 text-xs leading-5 text-[var(--dim)]">{sessionState.detail}</div>
            {token ? (
              <button
                type="button"
                onClick={() => {
                  clearSessionToken();
                  setToken(getSessionToken());
                }}
                className="mt-3 text-xs text-[var(--dim)] underline-offset-4 hover:text-[var(--fg)] hover:underline"
              >
                Clear local session
              </button>
            ) : null}
          </div>
        </div>
        {navItems.map((item) => (
          <NavLink
            key={item.to}
            to={item.to}
            end={item.to === "/"}
            className={({ isActive }) =>
              `block rounded-xl px-3 py-2.5 text-sm transition-colors ${
                isActive
                  ? "bg-[var(--accent)]/12 text-[var(--accent)] shadow-[inset_0_0_0_1px_rgba(240,160,88,0.18)]"
                  : "text-[var(--dim)] hover:bg-[var(--panel)] hover:text-[var(--fg)]"
              }`
            }
          >
            {item.label}
          </NavLink>
        ))}
        <div className="mt-auto pt-6 text-xs text-[var(--dim)] space-y-2">
          <div className="rounded-xl border border-[var(--border)] bg-[var(--panel)] px-3 py-3">
            <div className="font-medium text-[var(--fg)]">Ops Signal</div>
            <div className="mt-1 leading-5">
              {connected ? "The dashboard is receiving live node updates." : "The dashboard is in snapshot mode."}
            </div>
          </div>
          <a href="https://marc27.com" className="inline-block hover:text-[var(--accent)]">MARC27</a>
        </div>
      </nav>

      <main className="flex-1 overflow-auto px-8 py-7">
        <Routes>
          <Route path="/" element={<NodeStatus connected={connected} authenticated={Boolean(token)} />} />
          <Route path="/query" element={<QueryPage authenticated={Boolean(token)} />} />
          <Route path="/datasets" element={<Datasets authenticated={Boolean(token)} />} />
          <Route path="/mesh" element={<MeshStatus />} />
          <Route path="/users" element={<Users authenticated={Boolean(token)} />} />
          <Route path="/audit" element={<AuditLog authenticated={Boolean(token)} />} />
        </Routes>
      </main>
    </div>
  );
}
