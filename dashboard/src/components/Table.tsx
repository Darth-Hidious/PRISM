interface Column<T> {
  key: string;
  header: string;
  render?: (row: T) => ReactNode;
}

import type { ReactNode } from "react";

// eslint-disable-next-line @typescript-eslint/no-explicit-any
export default function Table<T extends Record<string, any>>({
  columns,
  rows,
}: {
  columns: Column<T>[];
  rows: T[];
}) {
  return (
    <div className="overflow-x-auto">
      <table className="w-full text-sm">
        <thead>
          <tr className="border-b border-[var(--border)]">
            {columns.map((col) => (
              <th
                key={col.key}
                className="px-3 py-2 text-left text-[10px] font-medium uppercase tracking-[0.18em] text-[var(--dim)]"
              >
                {col.header}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((row, i) => (
            <tr key={i} className="border-b border-[var(--border)]/70 last:border-b-0 hover:bg-white/2">
              {columns.map((col) => (
                <td key={col.key} className="px-3 py-3 text-[13px] text-[var(--fg)]">
                  {col.render ? col.render(row) : String(row[col.key] ?? "")}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
      {rows.length === 0 && (
        <div className="border border-dashed border-[var(--border)] px-4 py-8 text-center text-[12px] uppercase tracking-[0.16em] text-[var(--dim)]">
          No data
        </div>
      )}
    </div>
  );
}
