import type { Job, Session } from "../api";

/** What the sidebar shows next to a conversation. */
export type Badge =
  "approval" | "spending" | "running" | "queued" | "failed" | "unread";

/** Finished-but-not-yet-seen conversations: session id → finish time. */
export type Unread = Record<string, number>;

const RUNNING = new Set(["running", "paused", "cancelling"]);
const NO_IDS: ReadonlySet<string> = new Set();
const FAILED = new Set(["failed", "interrupted", "limit_reached"]);
const isActive = (status: string) => RUNNING.has(status) || status === "queued";

/** The badge for one conversation: one waiting for an answer wins (an
 * approval, or a spending card for those in `spending`), then running,
 * queued, and a finished result not yet looked at (failed or not). */
export function sessionBadge(
  id: string,
  jobs: Job[],
  waiting: ReadonlySet<string>,
  unread: Unread,
  spending: ReadonlySet<string> = NO_IDS,
): Badge | null {
  if (waiting.has(id)) return spending.has(id) ? "spending" : "approval";
  const own = jobs.filter((job) => job.session_id === id);
  if (own.some((job) => RUNNING.has(job.status))) return "running";
  if (own.some((job) => job.status === "queued")) return "queued";
  if (!unread[id]) return null;
  const latest = own
    .filter((job) => !isActive(job.status))
    .sort((a, b) => (b.finished_at || 0) - (a.finished_at || 0))[0];
  return latest && FAILED.has(latest.status) ? "failed" : "unread";
}

/** Plain words for a badge (screen readers and tooltips). */
export const BADGE_LABELS: Record<Badge, string> = {
  approval: "Needs your approval",
  spending: "Waiting at a spending limit",
  running: "Task running",
  queued: "Task queued",
  failed: "Task failed; not opened yet",
  unread: "Finished; not opened yet",
};

/** Conversations whose task finished since `previous` while they were not
 * the one on screen become unread. Cancelled tasks were stopped by the
 * user and stay read. Returns `unread` itself when nothing changed. */
export function nextUnread(
  unread: Unread,
  previous: Job[],
  jobs: Job[],
  visible: string,
): Unread {
  const was = new Map(previous.map((job) => [job.id, job.status]));
  let next = unread;
  for (const job of jobs) {
    const before = was.get(job.id);
    if (!before || !isActive(before) || isActive(job.status)) continue;
    if (job.status === "cancelled" || job.session_id === visible) continue;
    if (next === unread) next = { ...unread };
    next[job.session_id] = job.finished_at || Date.now() / 1000;
  }
  return next;
}

export function markRead(unread: Unread, id: string): Unread {
  if (!(id in unread)) return unread;
  const next = { ...unread };
  delete next[id];
  return next;
}

/** At most `limit` entries, newest kept, and none for deleted sessions. */
export function pruneUnread(
  unread: Unread,
  sessions: Pick<Session, "id">[] | null,
  limit = 200,
): Unread {
  const known = sessions && new Set(sessions.map((s) => s.id));
  const entries = Object.entries(unread)
    .filter(([id]) => !known || known.has(id))
    .sort((a, b) => b[1] - a[1])
    .slice(0, limit);
  return entries.length === Object.keys(unread).length
    ? unread
    : Object.fromEntries(entries);
}

/** The project a conversation is listed under: a worktree task's
 * conversation belongs to its project, not to its worktree folder. */
export const projectOf = (
  session: Pick<Session, "workspace" | "worktree_source">,
) => session.worktree_source || session.workspace;

/** Conversations in the order the sidebar lists them: pinned first, then
 * by project (the open project first, then the project list), each
 * newest first as the engine returns them. */
export function sidebarOrder(
  sessions: Session[],
  pins: string[],
  groups: string[],
): Session[] {
  const pinned = sessions.filter((s) => pins.includes(s.id));
  const rest = groups.flatMap((path) =>
    sessions.filter((s) => projectOf(s) === path && !pins.includes(s.id)),
  );
  const seen = new Set([...pinned, ...rest].map((s) => s.id));
  return [...pinned, ...rest, ...sessions.filter((s) => !seen.has(s.id))];
}

/** Alt+↑ / Alt+↓: the conversation before or after `current`, wrapping. */
export function stepConversation(
  order: Pick<Session, "id">[],
  current: string,
  delta: 1 | -1,
): string | null {
  if (!order.length) return null;
  const index = order.findIndex((s) => s.id === current);
  if (index < 0) return order[delta > 0 ? 0 : order.length - 1].id;
  const next = order[(index + delta + order.length) % order.length].id;
  return next === current ? null : next;
}
