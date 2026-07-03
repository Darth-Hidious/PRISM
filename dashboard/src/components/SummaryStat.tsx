export default function SummaryStat({
  label,
  value,
  detail,
  tone = "neutral",
}: {
  label: string;
  value: string;
  detail?: string;
  tone?: "neutral" | "good" | "warn" | "bad";
}) {
  const toneClass =
    tone === "good"
      ? "text-emerald-300"
      : tone === "warn"
        ? "text-amber-300"
        : tone === "bad"
          ? "text-rose-300"
          : "text-[var(--fg)]";

  return (
    <div className="rounded-2xl border border-[var(--border)] bg-[var(--panel)] px-4 py-4">
      <div className="text-[11px] uppercase tracking-[0.16em] text-[var(--dim)]">{label}</div>
      <div className={`mt-3 text-2xl font-semibold ${toneClass}`}>{value}</div>
      {detail ? <div className="mt-2 text-sm text-[var(--dim)]">{detail}</div> : null}
    </div>
  );
}
