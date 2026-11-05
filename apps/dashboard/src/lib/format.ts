import type { IntentRow } from "../api/types";

/** An error as one line for the owner. */
export const errorText = (err: unknown): string =>
  err instanceof Error ? err.message : String(err);

/** First and last four characters, to check an address by eye. */
export function short(address: string): string {
  return address.length > 14 ? `${address.slice(0, 4)}…${address.slice(-4)}` : address;
}

export function when(iso: string): string {
  return new Date(iso).toLocaleString(undefined, {
    day: "numeric",
    month: "short",
    year: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

export function time(iso: string): string {
  return new Date(iso).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
}

/** Groups rows under "Today", "Yesterday" or the date, keeping their order. */
export function byDay(rows: IntentRow[]): [string, IntentRow[]][] {
  const label = (iso: string) => {
    const d = new Date(iso);
    if (d.toDateString() === new Date().toDateString()) return "Today";
    if (d.toDateString() === new Date(Date.now() - 86_400_000).toDateString()) return "Yesterday";
    return d.toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" });
  };
  const groups: [string, IntentRow[]][] = [];
  for (const row of rows) {
    const day = label(row.created_at);
    const last = groups[groups.length - 1];
    if (last && last[0] === day) last[1].push(row);
    else groups.push([day, [row]]);
  }
  return groups;
}
