// Small display helpers shared by the operator pages. Kept defensive:
// checkpoint/spec fields come straight from disk and may be missing or of a
// surprising type, so these never throw — they degrade to a readable string.

/** Coerce an ISO string or epoch (seconds or ms) into a millisecond timestamp. */
function toMillis(value: string | number | null | undefined): number | null {
  if (value == null) return null;
  if (typeof value === "number") {
    // Heuristic: <1e12 is almost certainly epoch seconds, not milliseconds.
    return value < 1e12 ? value * 1000 : value;
  }
  const asNumber = Number(value);
  if (!Number.isNaN(asNumber) && value.trim() !== "") {
    return asNumber < 1e12 ? asNumber * 1000 : asNumber;
  }
  const parsed = Date.parse(value);
  return Number.isNaN(parsed) ? null : parsed;
}

/** "just now" / "5m ago" / "3h ago" / "2d ago", or the raw value if unparseable. */
export function formatRelative(value: string | number | null | undefined): string {
  const ms = toMillis(value);
  if (ms == null) return value == null ? "unknown" : String(value);

  const deltaMinutes = Math.max(0, Math.round((Date.now() - ms) / 60_000));
  if (deltaMinutes < 1) return "just now";
  if (deltaMinutes < 60) return `${deltaMinutes}m ago`;
  const hours = Math.round(deltaMinutes / 60);
  if (hours < 48) return `${hours}h ago`;
  return `${Math.round(hours / 24)}d ago`;
}
