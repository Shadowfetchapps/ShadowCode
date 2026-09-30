import { useCallback, useEffect, useRef, useState } from "react";
import { api, type Approval, type Job } from "../api";
import { isNative, listen } from "../lib/transport";

export type FeedPage = {
  approvals: Approval[];
  jobs: Job[];
  events?: string[];
  /** Every conversation with a pending approval or spending card (sidebar
   * badges). */
  waiting?: string[];
  /** The conversations of `waiting` whose only wait is a spending card. */
  spending?: string[];
};
export type FeedDependencies = {
  /** One read of the feed for the selected conversation ("" = all). */
  read: (sessionId: string) => Promise<FeedPage>;
  /** Engine wake-ups; the payload carries the broadcast `type` when known. */
  subscribe: (wake: (payload: unknown) => void) => Promise<() => void>;
};

/** Broadcast types that can change approvals or jobs. The engine sends its
 * own list with every feed page (`events`); this one covers the first read. */
export const FEED_EVENTS = [
  "approval.requested",
  "approval.resolved",
  "job.changed",
  "agent.started",
  "agent.completed",
  "agent.paused",
  "agent.resumed",
  "limit.fallback",
  "spend.limit_reached",
  "spend.limit_resolved",
];
/** Wake-ups are the signal; this slow timer only covers a missed one. */
export const FEED_BACKSTOP_MS = 15000;
/** Bursts of wake-ups (a tool call and its approval) become one read. */
export const FEED_COALESCE_MS = 30;

/** Untyped wake-ups (a lagged or reattached stream) may hide anything. */
export function isFeedEvent(payload: unknown, kinds: ReadonlySet<string>) {
  const type = (payload as { type?: unknown } | null | undefined)?.type;
  if (typeof type !== "string" || !type) return true;
  return kinds.has(type) || type.startsWith("view.");
}

const same = (a: unknown, b: unknown) =>
  a === b || JSON.stringify(a) === JSON.stringify(b);
const NO_APPROVALS: Approval[] = [];

export const defaultFeed: FeedDependencies = {
  read: (sessionId) => api.feed(sessionId),
  subscribe: (wake) =>
    isNative()
      ? listen("shadowcode:events", wake)
      : Promise.resolve(() => undefined),
};

/** Pending approvals for the open conversation and the project job list,
 * pushed by engine wake-ups instead of polled. Unchanged reads keep the
 * previous arrays, so nothing re-renders for them. */
export function useFeed(
  sessionId: string,
  deps: FeedDependencies = defaultFeed,
) {
  const [approvalPage, setApprovalPage] = useState({
    sessionId: "",
    items: NO_APPROVALS,
  });
  // Scope cards during render, before a changed selection's effect/read runs.
  // Reselecting the same conversation keeps its current pending approvals.
  const approvals =
    sessionId && approvalPage.sessionId === sessionId
      ? approvalPage.items
      : NO_APPROVALS;
  const [jobs, setJobsState] = useState<Job[]>([]);
  const [waiting, setWaiting] = useState<string[]>([]);
  const [spending, setSpending] = useState<string[]>([]);
  const session = useRef(sessionId);
  session.current = sessionId;
  const dependencies = useRef(deps);
  dependencies.current = deps;
  const kinds = useRef<ReadonlySet<string>>(new Set(FEED_EVENTS));
  const reading = useRef<Promise<void> | null>(null);
  const again = useRef(false);
  const live = useRef(true);

  const setJobs = useCallback((next: Job[] | ((prev: Job[]) => Job[])) => {
    setJobsState((prev) => {
      const value = typeof next === "function" ? next(prev) : next;
      return same(prev, value) ? prev : value;
    });
  }, []);

  /** Read now; a read already under way runs once more when it ends. */
  const refresh = useCallback((): Promise<void> => {
    if (reading.current) {
      again.current = true;
      return reading.current;
    }
    const run = (async () => {
      do {
        again.current = false;
        const selected = session.current;
        try {
          const page = await dependencies.current.read(selected);
          if (!live.current) return;
          if (selected === session.current) {
            const scoped = selected
              ? page.approvals.filter((a) => a.session_id === selected)
              : NO_APPROVALS;
            const items = scoped.length ? scoped : NO_APPROVALS;
            setApprovalPage((prev) =>
              prev.sessionId === selected && same(prev.items, items)
                ? prev
                : { sessionId: selected, items },
            );
          }
          setJobs(page.jobs);
          const nextWaiting = page.waiting || [];
          setWaiting((prev) => (same(prev, nextWaiting) ? prev : nextWaiting));
          const nextSpending = page.spending || [];
          setSpending((prev) =>
            same(prev, nextSpending) ? prev : nextSpending,
          );
          if (page.events?.length) kinds.current = new Set(page.events);
        } catch {
          /* The reconnect banner covers outages; keep the last state. */
        }
      } while (again.current && live.current);
    })().finally(() => {
      reading.current = null;
    });
    reading.current = run;
    return run;
  }, [setJobs]);

  useEffect(() => {
    live.current = true;
    let stopped = false;
    let unsubscribe: (() => void) | undefined;
    let scheduled: ReturnType<typeof setTimeout> | undefined;
    const wake = (payload: unknown) => {
      if (stopped || scheduled || !isFeedEvent(payload, kinds.current)) return;
      scheduled = setTimeout(() => {
        scheduled = undefined;
        void refresh();
      }, FEED_COALESCE_MS);
    };
    void dependencies.current
      .subscribe(wake)
      .then((stop) => {
        if (stopped) stop();
        else unsubscribe = stop;
      })
      .catch(() => undefined);
    const backstop = setInterval(() => void refresh(), FEED_BACKSTOP_MS);
    return () => {
      stopped = true;
      live.current = false;
      unsubscribe?.();
      clearTimeout(scheduled);
      clearInterval(backstop);
    };
  }, [refresh]);

  // A different conversation has different approvals: read at once.
  useEffect(() => {
    void refresh();
  }, [sessionId, refresh]);

  return { approvals, jobs, waiting, spending, setJobs, refresh };
}
