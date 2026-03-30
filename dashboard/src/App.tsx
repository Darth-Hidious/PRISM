import { Routes, Route, NavLink } from "react-router-dom";
import NodeStatus from "./pages/NodeStatus";
import QueryPage from "./pages/QueryPage";
import Datasets from "./pages/Datasets";
import AuditLog from "./pages/AuditLog";
import Users from "./pages/Users";
import MeshStatus from "./pages/MeshStatus";

const navItems = [
  { to: "/", label: "Node" },
  { to: "/query", label: "Query" },
  { to: "/datasets", label: "Datasets" },
  { to: "/mesh", label: "Mesh" },
  { to: "/users", label: "Users" },
  { to: "/audit", label: "Audit" },
];

export default function App() {
  return (
    <div className="flex min-h-screen">
      <nav className="w-52 shrink-0 border-r border-[var(--border)] bg-[var(--card)] p-4 flex flex-col gap-1">
        <div className="text-2xl font-bold text-[var(--accent)] mb-6">PRISM</div>
        {navItems.map((item) => (
          <NavLink
            key={item.to}
            to={item.to}
            end={item.to === "/"}
            className={({ isActive }) =>
              `block px-3 py-2 rounded-lg text-sm transition-colors ${
                isActive
                  ? "bg-[var(--accent)]/10 text-[var(--accent)]"
                  : "text-[var(--dim)] hover:text-[var(--fg)]"
              }`
            }
          >
            {item.label}
          </NavLink>
        ))}
        <div className="mt-auto pt-4 text-xs text-[var(--dim)]">
          <a href="https://marc27.com" className="hover:text-[var(--accent)]">MARC27</a>
        </div>
      </nav>

      <main className="flex-1 p-8 overflow-auto">
        <Routes>
          <Route path="/" element={<NodeStatus />} />
          <Route path="/query" element={<QueryPage />} />
          <Route path="/datasets" element={<Datasets />} />
          <Route path="/mesh" element={<MeshStatus />} />
          <Route path="/users" element={<Users />} />
          <Route path="/audit" element={<AuditLog />} />
        </Routes>
      </main>
    </div>
  );
}
