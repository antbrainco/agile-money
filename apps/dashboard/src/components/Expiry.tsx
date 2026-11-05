import { useNow } from "../lib/useNow";

const APPROVAL_WINDOW_MS = 15 * 60 * 1000;

/** Time left on an approval, as a countdown and a shrinking bar. */
export function Expiry({ at }: { at: string }) {
  const now = useNow(1000);
  const left = Math.max(0, new Date(at).getTime() - now);
  const minutes = Math.floor(left / 60000);
  const seconds = Math.floor((left % 60000) / 1000);
  return (
    <div className={left < 60000 ? "expiry soon" : "expiry"}>
      {left === 0 ? "Expired" : `Expires in ${minutes}:${String(seconds).padStart(2, "0")}`}
      <div className="bar">
        <span style={{ width: `${Math.min(100, (left / APPROVAL_WINDOW_MS) * 100)}%` }} />
      </div>
    </div>
  );
}
