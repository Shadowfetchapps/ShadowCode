import { afterEach, expect, it, vi } from "vitest";
import { act, cleanup, renderHook } from "@testing-library/react";
import { useConversation } from "./useConversation";
import {
  api,
  type EventRow,
  type HistoryPage,
  type Job,
  type SessionDetail,
} from "../api";
import { jobEvents, type JobStream } from "../lib/jobEvents";
vi.mock("../api", () => ({
  api: { historyPage: vi.fn(), job: vi.fn(), session: vi.fn() },
}));
vi.mock("../lib/jobEvents", () => ({
  jobEvents: vi.fn(() => ({
    onopen: null,
    onmessage: null,
    onerror: null,
    close: vi.fn(),
  })),
}));
vi.mock("../lib/transport", () => ({ isNative: () => true }));
const event = (id: number, text: string): EventRow => ({
  id,
  ts: id,
  type: "model.delta",
  task_id: "task",
  payload: { text, message_id: `message-${id}` },
});
const detail = (id = "session"): SessionDetail => ({
  id,
  workspace: "/project",
  status: "active",
  updated_at: 1,
  tasks: [],
  events: [event(200, "Recent answer")],
  event_cursor: 200,
  history_page: { first_cursor: 200, has_older: true },
});
const job: Job = {
  id: "job",
  session_id: "session",
  workspace: "/project",
  status: "running",
  started_at: 1,
  event_cursor: 200,
  task_id: "task",
};
const page: HistoryPage = {
  events: [event(100, "Older answer")],
  first_cursor: 100,
  event_cursor: 199,
  has_older: true,
};
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});
it("promotes a queued local task on preparation without waiting for agent.started", () => {
  const { result } = renderHook(() => useConversation(vi.fn()));
  act(() => result.current.load(detail(), { ...job, status: "queued" }));
  const stream = vi.mocked(jobEvents).mock.results.at(-1)!.value as JobStream;
  act(() =>
    stream.onmessage?.({
      data: JSON.stringify({
        id: 201,
        ts: 2,
        type: "local.runtime_progress",
        task_id: "task",
        payload: { phase: "loading" },
      }),
    }),
  );
  expect(result.current.job?.status).toBe("running");
  expect(result.current.transcript.activity.task.localPhase).toBe("loading");
});
it("pages independently while live events continue and returns to the latest state", async () => {
  vi.mocked(api.historyPage).mockResolvedValue(page);
  const { result } = renderHook(() => useConversation(vi.fn()));
  act(() => result.current.load(detail(), job));
  const stream = vi.mocked(jobEvents).mock.results.at(-1)!.value as JobStream;
  await act(async () => {
    await result.current.history.older();
  });
  expect(api.historyPage).toHaveBeenCalledWith("session", 200);
  expect(result.current.transcript.items.map((i) => i.text)).toEqual([
    "Older answer",
  ]);
  act(() =>
    stream.onmessage?.({
      data: JSON.stringify(event(201, "Live answer while browsing")),
    }),
  );
  expect(result.current.transcript.items.map((i) => i.text)).toEqual([
    "Older answer",
  ]);
  expect(result.current.busy).toBe(true);
  act(() => result.current.history.latest());
  expect(result.current.transcript.items.map((i) => i.text)).toEqual([
    "Recent answer",
    "Live answer while browsing",
  ]);
});
it("uses exclusive cursors for older/newer pages without caching their bodies", async () => {
  vi.mocked(api.historyPage)
    .mockResolvedValueOnce(page)
    .mockResolvedValueOnce({
      ...page,
      first_cursor: 10,
      events: [event(10, "First answer")],
      has_older: false,
    })
    .mockResolvedValueOnce(page);
  const { result } = renderHook(() => useConversation(vi.fn()));
  act(() => result.current.load(detail(), null));
  await act(async () => {
    await result.current.history.older();
  });
  await act(async () => {
    await result.current.history.older();
  });
  expect(result.current.history.hasOlder).toBe(false);
  expect(result.current.transcript.items).toHaveLength(1);
  await act(async () => {
    await result.current.history.newer();
  });
  expect(api.historyPage).toHaveBeenNthCalledWith(3, "session", 200);
  expect(result.current.transcript.items[0].text).toBe("Older answer");
  await act(async () => {
    await result.current.history.newer();
  });
  expect(result.current.history.viewing).toBe(false);
  expect(result.current.transcript.items[0].text).toBe("Recent answer");
});
it("ignores a late page from a previously selected session and reports retryable failures", async () => {
  let resolve!: (page: HistoryPage) => void;
  vi.mocked(api.historyPage).mockImplementationOnce(
    () =>
      new Promise((done) => {
        resolve = done;
      }),
  );
  const { result } = renderHook(() => useConversation(vi.fn()));
  act(() => result.current.load(detail(), null));
  let pending!: Promise<void>;
  act(() => {
    pending = result.current.history.older();
  });
  act(() => result.current.load(detail("other"), null));
  await act(async () => {
    resolve(page);
    await pending;
  });
  expect(result.current.history.viewing).toBe(false);
  expect(result.current.history.loading).toBe(false);
  vi.mocked(api.historyPage).mockRejectedValueOnce(
    new Error("History temporarily unavailable"),
  );
  await act(async () => {
    await result.current.history.older();
  });
  expect(result.current.history.error).toContain("temporarily unavailable");
  expect(result.current.transcript.items[0].text).toBe("Recent answer");
});
it("reattaches the same running job after a snapshot and can retain the viewed page", async () => {
  vi.mocked(api.historyPage).mockResolvedValue(page);
  const { result } = renderHook(() => useConversation(vi.fn()));
  act(() => result.current.load(detail(), job));
  const previous = vi.mocked(jobEvents).mock.results.at(-1)!.value as JobStream;
  await act(async () => {
    await result.current.history.older();
  });
  act(() =>
    result.current.load(
      { ...detail(), events: [event(220, "New snapshot")], event_cursor: 220 },
      job,
      true,
    ),
  );
  expect(previous.close).toHaveBeenCalled();
  expect(jobEvents).toHaveBeenLastCalledWith("job", 220);
  expect(result.current.transcript.items[0].text).toBe("Older answer");
  const current = vi.mocked(jobEvents).mock.results.at(-1)!.value as JobStream;
  act(() => {
    previous.onmessage?.({
      data: JSON.stringify(event(221, "Obsolete stream")),
    });
    current.onmessage?.({ data: JSON.stringify(event(222, "New stream")) });
    result.current.history.latest();
  });
  expect(result.current.transcript.items.map((i) => i.text)).toEqual([
    "New snapshot",
    "New stream",
  ]);
});
