import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, renderHook } from "@testing-library/react";
import {
  FEED_BACKSTOP_MS,
  isFeedEvent,
  useFeed,
  type FeedDependencies,
  type FeedPage,
} from "./useFeed";
import type { Approval, Job } from "../api";

vi.mock("../lib/transport", () => ({
  isNative: () => false,
  listen: vi.fn(),
}));

const approval = (id: string, session = "s1") =>
  ({ id, session_id: session, tool: "exec", command: "npm test" }) as Approval;
const job = (id: string, status = "running") =>
  ({ id, status, session_id: "s1", workspace: "/p" }) as Job;

function fakeEngine() {
  let wake: (payload: unknown) => void = () => undefined;
  const pages: Record<string, FeedPage> = {};
  const read = vi.fn(
    async (session: string): Promise<FeedPage> =>
      pages[session] || { approvals: [], jobs: [] },
  );
  const unsubscribe = vi.fn();
  const deps: FeedDependencies = {
    read,
    subscribe: async (handler) => {
      wake = handler;
      return unsubscribe;
    },
  };
  return {
    deps,
    read,
    pages,
    unsubscribe,
    wake: (payload: unknown) => wake(payload),
  };
}

beforeEach(() => vi.useFakeTimers());
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

const settle = () => act(() => vi.advanceTimersByTimeAsync(50));

it("reads once on mount and again only for feed events", async () => {
  const engine = fakeEngine();
  const { result } = renderHook(() => useFeed("s1", engine.deps));
  await settle();
  expect(engine.read).toHaveBeenCalledTimes(1);
  expect(engine.read).toHaveBeenLastCalledWith("s1");

  engine.pages.s1 = { approvals: [approval("a1")], jobs: [job("j1")] };
  engine.wake({ type: "model.stream", session_id: "s1" });
  engine.wake({ type: "tool.started", session_id: "s1" });
  await settle();
  expect(engine.read).toHaveBeenCalledTimes(1);
  expect(result.current.approvals).toEqual([]);

  engine.wake({ type: "approval.requested", session_id: "s1" });
  await settle();
  expect(engine.read).toHaveBeenCalledTimes(2);
  expect(result.current.approvals.map((a) => a.id)).toEqual(["a1"]);
  expect(result.current.jobs.map((j) => j.id)).toEqual(["j1"]);
});

it("coalesces a burst of wake-ups into one read", async () => {
  const engine = fakeEngine();
  renderHook(() => useFeed("s1", engine.deps));
  await settle();
  for (const type of ["job.changed", "agent.started", "approval.requested"])
    engine.wake({ type });
  await settle();
  expect(engine.read).toHaveBeenCalledTimes(2);
});

it("treats untyped and view wake-ups as possible changes", () => {
  const kinds = new Set(["approval.requested"]);
  expect(isFeedEvent({}, kinds)).toBe(true);
  expect(isFeedEvent(undefined, kinds)).toBe(true);
  expect(isFeedEvent({ type: "view.lagged" }, kinds)).toBe(true);
  expect(isFeedEvent({ type: "approval.requested" }, kinds)).toBe(true);
  expect(isFeedEvent({ type: "model.delta" }, kinds)).toBe(false);
});

it("keeps the same arrays when a read changes nothing", async () => {
  const engine = fakeEngine();
  engine.pages.s1 = { approvals: [approval("a1")], jobs: [job("j1")] };
  const { result } = renderHook(() => useFeed("s1", engine.deps));
  await settle();
  const approvals = result.current.approvals;
  const jobs = result.current.jobs;
  engine.pages.s1 = { approvals: [approval("a1")], jobs: [job("j1")] };
  engine.wake({ type: "job.changed" });
  await settle();
  expect(engine.read).toHaveBeenCalledTimes(2);
  expect(result.current.approvals).toBe(approvals);
  expect(result.current.jobs).toBe(jobs);
});

it("falls back to a slow backstop read, not a fast poll", async () => {
  const engine = fakeEngine();
  renderHook(() => useFeed("s1", engine.deps));
  await settle();
  await act(() => vi.advanceTimersByTimeAsync(FEED_BACKSTOP_MS - 100));
  expect(engine.read).toHaveBeenCalledTimes(1);
  expect(FEED_BACKSTOP_MS).toBeGreaterThanOrEqual(10000);
  await act(() => vi.advanceTimersByTimeAsync(200));
  expect(engine.read).toHaveBeenCalledTimes(2);
});

it("follows the engine's list of feed events", async () => {
  const engine = fakeEngine();
  engine.pages.s1 = { approvals: [], jobs: [], events: ["custom.change"] };
  renderHook(() => useFeed("s1", engine.deps));
  await settle();
  engine.wake({ type: "custom.change" });
  await settle();
  expect(engine.read).toHaveBeenCalledTimes(2);
  engine.wake({ type: "approval.requested" });
  await settle();
  expect(engine.read).toHaveBeenCalledTimes(2);
});

it("reads the new conversation's approvals and drops a stale answer", async () => {
  const engine = fakeEngine();
  engine.pages.s1 = { approvals: [approval("a1")], jobs: [] };
  engine.pages.s2 = { approvals: [approval("b1", "s2")], jobs: [] };
  let release: () => void = () => undefined;
  const { result, rerender } = renderHook(
    ({ session }) => useFeed(session, engine.deps),
    { initialProps: { session: "s1" } },
  );
  await settle();
  expect(result.current.approvals.map((a) => a.id)).toEqual(["a1"]);
  // A read for s1 still under way when s2 opens must not show s1's cards.
  engine.read.mockImplementationOnce(
    (session: string) =>
      new Promise((resolve) => {
        release = () =>
          resolve({ approvals: [approval("late", session)], jobs: [] });
      }),
  );
  engine.wake({ type: "approval.requested" });
  await settle();
  rerender({ session: "s2" });
  // Old cards must disappear in the first render of the new conversation,
  // even while the old request is held and no new feed result can arrive.
  expect(result.current.approvals).toEqual([]);
  const waiting = result.current.approvals;
  rerender({ session: "s2" });
  expect(result.current.approvals).toBe(waiting);
  await act(async () => release());
  await settle();
  expect(result.current.approvals.map((a) => a.id)).toEqual(["b1"]);
  expect(engine.read).toHaveBeenLastCalledWith("s2");
});

it("shows approvals only for the selected conversation", async () => {
  const engine = fakeEngine();
  const mixed = { approvals: [approval("a1"), approval("b1", "s2")], jobs: [] };
  engine.pages.s1 = mixed;
  engine.pages[""] = mixed;
  const { result, rerender } = renderHook(
    ({ session }) => useFeed(session, engine.deps),
    { initialProps: { session: "s1" } },
  );
  await settle();
  expect(result.current.approvals.map((a) => a.id)).toEqual(["a1"]);
  rerender({ session: "" });
  expect(result.current.approvals).toEqual([]);
  await settle();
  expect(result.current.approvals).toEqual([]);
});

it("unsubscribes and stops the backstop on unmount", async () => {
  const engine = fakeEngine();
  const { unmount } = renderHook(() => useFeed("s1", engine.deps));
  await settle();
  unmount();
  expect(engine.unsubscribe).toHaveBeenCalledTimes(1);
  await act(() => vi.advanceTimersByTimeAsync(FEED_BACKSTOP_MS * 2));
  expect(engine.read).toHaveBeenCalledTimes(1);
});
