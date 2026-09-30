import { describe, expect, it } from "vitest";
import type { Job, Session } from "../api";
import {
  BADGE_LABELS,
  markRead,
  nextUnread,
  pruneUnread,
  sessionBadge,
  sidebarOrder,
  stepConversation,
} from "./badges";

const job = (
  id: string,
  session_id: string,
  status: string,
  finished_at?: number,
): Job => ({
  id,
  session_id,
  status,
  workspace: "/p",
  event_cursor: 0,
  started_at: 1,
  finished_at,
});
const session = (id: string, extra: Partial<Session> = {}): Session => ({
  id,
  workspace: "/p",
  status: "active",
  updated_at: 1,
  ...extra,
});

describe("conversation badges", () => {
  it("prefers approval, then running, queued, failed and unread", () => {
    const jobs = [
      job("1", "a", "running"),
      job("2", "b", "queued"),
      job("3", "c", "failed", 5),
      job("4", "d", "completed", 6),
      job("5", "e", "completed", 7),
    ];
    const waiting = new Set(["a"]);
    const unread = { c: 5, d: 6 };
    expect(sessionBadge("a", jobs, waiting, unread)).toBe("approval");
    expect(sessionBadge("a", jobs, new Set(), unread)).toBe("running");
    expect(sessionBadge("b", jobs, waiting, unread)).toBe("queued");
    expect(sessionBadge("c", jobs, waiting, unread)).toBe("failed");
    expect(sessionBadge("d", jobs, waiting, unread)).toBe("unread");
    // Finished and already seen: no badge.
    expect(sessionBadge("e", jobs, waiting, unread)).toBeNull();
  });

  it("says a conversation waits at a spending limit, not for an approval", () => {
    const jobs = [job("1", "a", "running"), job("2", "b", "running")];
    // `b` has only a spending card; `a` an approval.
    const waiting = new Set(["a", "b"]);
    const spending = new Set(["b"]);
    expect(sessionBadge("a", jobs, waiting, {}, spending)).toBe("approval");
    expect(sessionBadge("b", jobs, waiting, {}, spending)).toBe("spending");
    expect(BADGE_LABELS.spending).toBe("Waiting at a spending limit");
  });

  it("marks conversations unread when their task finishes out of view", () => {
    const before = [
      job("1", "a", "running"),
      job("2", "b", "running"),
      job("3", "c", "running"),
    ];
    const after = [
      job("1", "a", "completed", 10),
      job("2", "b", "failed", 11),
      job("3", "c", "cancelled", 12),
    ];
    const unread = nextUnread({}, before, after, "a");
    expect(unread).toEqual({ b: 11 });
    // Nothing changed: the same object comes back.
    expect(nextUnread(unread, after, after, "a")).toBe(unread);
    expect(markRead(unread, "b")).toEqual({});
    expect(markRead(unread, "zzz")).toBe(unread);
    expect(pruneUnread({ b: 1, gone: 2 }, [session("b")])).toEqual({ b: 1 });
  });

  it("orders conversations as the sidebar lists them and steps through", () => {
    const rows = [
      session("w", { workspace: "/tree", worktree_source: "/p" }),
      session("x", { workspace: "/q" }),
      session("y"),
      session("z"),
    ];
    const order = sidebarOrder(rows, ["z"], ["/p", "/q"]).map((s) => s.id);
    expect(order).toEqual(["z", "w", "y", "x"]);
    const listed = order.map((id) => ({ id }));
    expect(stepConversation(listed, "w", 1)).toBe("y");
    expect(stepConversation(listed, "z", -1)).toBe("x");
    expect(stepConversation(listed, "x", 1)).toBe("z");
    expect(stepConversation(listed, "missing", 1)).toBe("z");
    expect(stepConversation([{ id: "only" }], "only", 1)).toBeNull();
  });
});
