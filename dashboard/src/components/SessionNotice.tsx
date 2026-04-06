export default function SessionNotice({
  title = "Session Required",
  detail,
}: {
  title?: string;
  detail: string;
}) {
  return (
    <div className="rounded-3xl border border-dashed border-[var(--border)] bg-[var(--card)] px-6 py-6">
      <div className="text-[11px] uppercase tracking-[0.18em] text-[var(--dim)]">Dashboard Access</div>
      <h2 className="mt-2 text-2xl font-semibold tracking-tight text-[var(--fg)]">{title}</h2>
      <p className="mt-3 max-w-2xl text-sm leading-6 text-[var(--dim)]">{detail}</p>
      <div className="mt-4 rounded-2xl border border-[var(--border)] bg-[var(--panel)] px-4 py-4 text-sm leading-6 text-[var(--dim)]">
        Open the dashboard with a session token in the URL, for example
        <span className="mx-1 text-[var(--fg)]">`?token=&lt;session-token&gt;`</span>
        or
        <span className="mx-1 text-[var(--fg)]">`?session=&lt;session-token&gt;`</span>.
      </div>
    </div>
  );
}
