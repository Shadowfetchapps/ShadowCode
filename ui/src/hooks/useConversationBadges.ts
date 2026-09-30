import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Job, Session } from "../api";
import {
  markRead,
  nextUnread,
  pruneUnread,
  sessionBadge,
  type Badge,
  type Unread,
} from "../lib/badges";
import { readStore, writeStore } from "../lib/storage";

const UNREAD_KEY = "shadow:unread";
const NONE: string[] = [];

function readUnread(): Unread {
  try {
    const value = JSON.parse(readStore(UNREAD_KEY) || "{}");
    return value && typeof value === "object" && !Array.isArray(value)
      ? (value as Unread)
      : {};
  } catch {
    return {};
  }
}

/** Sidebar badges (running, needs approval, waiting at a spending limit,
 * failed, finished-unread) and the
 * most recently opened conversations (Ctrl+Tab). Unread state survives a
 * restart. */
export function useConversationBadges({
  jobs,
  waiting,
  spending = NONE,
  sessionId,
  sessions,
}: {
  jobs: Job[];
  waiting: string[];
  /** The conversations of `waiting` whose only wait is a spending card. */
  spending?: string[];
  sessionId: string;
  sessions: Session[];
}) {
  const [unread, setUnread] = useState<Unread>(readUnread);
  const previous = useRef<Job[] | null>(null);
  const visible = useRef(sessionId);
  visible.current = sessionId;
  const recent = useRef<string[]>([]);

  useEffect(() => {
    const before = previous.current;
    previous.current = jobs;
    if (before) setUnread((u) => nextUnread(u, before, jobs, visible.current));
  }, [jobs]);
  useEffect(() => {
    if (!sessionId) return;
    setUnread((u) => markRead(u, sessionId));
    recent.current = [
      sessionId,
      ...recent.current.filter((id) => id !== sessionId),
    ].slice(0, 20);
  }, [sessionId]);
  useEffect(() => {
    if (sessions.length) setUnread((u) => pruneUnread(u, sessions));
  }, [sessions]);
  useEffect(() => {
    writeStore(
      UNREAD_KEY,
      Object.keys(unread).length ? JSON.stringify(unread) : null,
    );
  }, [unread]);

  const waitingSet = useMemo(() => new Set(waiting), [waiting]);
  const spendingSet = useMemo(() => new Set(spending), [spending]);
  const badges = useMemo(() => {
    const result: Record<string, Badge> = {};
    const ids = new Set([
      ...sessions.map((s) => s.id),
      ...jobs.map((j) => j.session_id),
      ...waiting,
    ]);
    for (const id of ids) {
      const badge = sessionBadge(id, jobs, waitingSet, unread, spendingSet);
      if (badge) result[id] = badge;
    }
    return result;
  }, [sessions, jobs, waiting, waitingSet, spendingSet, unread]);

  /** The conversation opened before the current one (Ctrl+Tab). */
  const mostRecent = useCallback(
    (exists: (id: string) => boolean) =>
      recent.current.find((id) => id !== visible.current && exists(id)) || null,
    [],
  );

  return { badges, unread, mostRecent };
}
