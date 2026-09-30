import { useEffect, useState } from "react";

/** The current time in seconds. The component renders again at the next
 * local midnight and at `until` (while it is still ahead), so words like
 * "tomorrow at 3:40 AM" and a button that ends at a time stay right in a
 * window left open, even inside a memoized row. A given `fixed` time
 * (tests) is used as is. */
export function useWallClock(fixed?: number, until?: number): number {
  const [turns, setTurns] = useState(0);
  useEffect(() => {
    if (fixed !== undefined) return;
    const now = Date.now() / 1000;
    const midnight = new Date();
    midnight.setHours(24, 0, 0, 0);
    let next = midnight.getTime() / 1000;
    if (until && until > now && until < next) next = until;
    // A second late, so the day (or the time) has surely turned.
    const timer = setTimeout(
      () => setTurns((n) => n + 1),
      Math.max(0, next - now) * 1000 + 1000,
    );
    return () => clearTimeout(timer);
  }, [fixed, until, turns]);
  return fixed ?? Date.now() / 1000;
}
