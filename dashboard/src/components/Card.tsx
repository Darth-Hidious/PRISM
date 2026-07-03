import type { ReactNode } from "react";

export default function Card({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div className="border border-[var(--border)] bg-[var(--card)] p-5 shadow-[0_16px_40px_rgba(0,0,0,0.18)]">
      <div className="mb-4 flex items-center justify-between border-b border-[var(--border)] pb-3">
        <h2 className="text-[11px] font-semibold uppercase tracking-[0.22em] text-[var(--accent)]">{title}</h2>
        <span className="text-[10px] uppercase tracking-[0.18em] text-[var(--dim)]">Live Module</span>
      </div>
      {children}
    </div>
  );
}
