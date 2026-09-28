import { useEffect, useState } from "react";
import { api, type UpdateStatus } from "../api";

/** Fired after Settings › About changes the update state (check, hide,
 * setting), so the status-bar notice re-reads it. */
export const UPDATES_CHANGED = "shadowcode:updates-changed";

/** The first daily check waits until the window has settled. */
export const FIRST_CHECK_DELAY_MS = 15_000;
/** How often the window asks; the engine checks GitHub at most once a day. */
export const RECHECK_MS = 60 * 60 * 1000;

export function announceUpdates() {
  window.dispatchEvent(new Event(UPDATES_CHANGED));
}

/** The update notice for the status bar. At start it shows what the last
 * check found (no network); shortly after, and then every hour, it asks the
 * engine for its daily check (`GET /api/updates?auto=1`). The engine decides
 * whether GitHub is asked at all: at most once a day, never when turned off
 * or offline. Failures stay silent here; Settings › About shows them. */
export function useUpdateNotice(enabled: boolean) {
  const [status, setStatus] = useState<UpdateStatus | null>(null);
  useEffect(() => {
    if (!enabled) {
      setStatus(null);
      return;
    }
    let stopped = false;
    const load = (auto: boolean) =>
      void api
        .updates(auto)
        .then((next) => {
          if (!stopped) setStatus(next);
        })
        .catch(() => undefined);
    load(false);
    const first = window.setTimeout(() => load(true), FIRST_CHECK_DELAY_MS);
    const hourly = window.setInterval(() => load(true), RECHECK_MS);
    const changed = () => load(false);
    window.addEventListener(UPDATES_CHANGED, changed);
    return () => {
      stopped = true;
      window.clearTimeout(first);
      window.clearInterval(hourly);
      window.removeEventListener(UPDATES_CHANGED, changed);
    };
  }, [enabled]);
  return status;
}

/** The version to announce, or "" when there is nothing to show. */
export function noticeVersion(status: UpdateStatus | null) {
  if (!status || !status.available || status.dismissed || !status.latest)
    return "";
  return status.latest.version;
}
