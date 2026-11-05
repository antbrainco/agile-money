import { useEffect, useState } from "react";

/** The current time, refreshed every `every` ms while `active`. For countdowns. */
export function useNow(every: number, active = true): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    const id = setInterval(() => setNow(Date.now()), every);
    return () => clearInterval(id);
  }, [every, active]);
  return now;
}
