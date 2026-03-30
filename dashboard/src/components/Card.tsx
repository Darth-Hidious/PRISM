import type { ReactNode } from "react";

export default function Card({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div className="rounded-xl border border-[var(--border)] bg-[var(--card)] p-5">
      <h2 className="text-sm font-semibold text-[var(--accent)] mb-4">{title}</h2>
      {children}
    </div>
  );
}
