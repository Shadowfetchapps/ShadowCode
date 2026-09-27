import { afterEach, describe, expect, it, vi } from "vitest";
import { compactStreamRows, nativeJobStream } from "./jobEvents";
import { replay } from "./transcript";
import type { EventRow, Job } from "../api";

afterEach(() => vi.useRealTimers());
const job = (status: string, cursor: number) =>
  ({ id: "job", status, event_cursor: cursor }) as Job;
const rows = (start: number, count: number): EventRow[] =>
  Array.from({ length: count }, (_, i) => ({
    id: start + i,
    ts: 0,
    type: "model.stream",
    payload: { text: String(start + i) },
  }));

describe("native event replay", () => {
  it("keeps paused tasks live and replays resumed output before completing", async () => {
    vi.useFakeTimers();
    const stop = vi.fn();
    let wake!: () => void;
    let page = { events: rows(21, 1), job: job("paused", 21) };
    const read = vi.fn(async () => page);
    const stream = nativeJobStream(20, {
      read,
      subscribe: async (notify) => {
        wake = notify;
        return stop;
      },
    });
    const received: EventRow[] = [];
    stream.onmessage = (event) => received.push(JSON.parse(event.data));
    await vi.advanceTimersByTimeAsync(30);
    expect(received.map((event) => event.type)).toEqual([
      "job.state",
      "model.stream",
    ]);
    expect(received[0].payload.status).toBe("paused");
    expect(stop).not.toHaveBeenCalled();

    // Polling must still work while paused, even without a notification.
    page = { events: [], job: job("paused", 21) };
    await vi.advanceTimersByTimeAsync(2530);
    expect(read).toHaveBeenCalledTimes(2);
    expect(stop).not.toHaveBeenCalled();
    page = { events: rows(22, 1), job: job("running", 22) };
    wake();
    await vi.advanceTimersByTimeAsync(30);
    expect(received.at(-2)?.payload.status).toBe("running");
    expect(received.at(-1)?.id).toBe(22);
    page = { events: rows(23, 1), job: job("completed", 23) };
    wake();
    await vi.advanceTimersByTimeAsync(30);
    expect(received.slice(-2).map((event) => event.type)).toEqual([
      "model.stream",
      "job.done",
    ]);
    expect(received.filter((event) => event.type === "job.done")).toHaveLength(
      1,
    );
    expect(stop).toHaveBeenCalledTimes(1);
  });

  it("compacts response fragments without changing replay or durable completion", async () => {
    vi.useFakeTimers();
    const fragments = rows(1, 512).map((row) => ({
      ...row,
      task_id: "task",
      payload: { message_id: "response", text: `片段${row.id}😀` },
    }));
    const original = JSON.stringify(fragments);
    const compacted = compactStreamRows(fragments, 0);
    expect(compacted).toHaveLength(1);
    expect(replay(compacted)).toEqual(replay(fragments));
    expect(JSON.stringify(fragments)).toBe(original);
    const final: EventRow = {
      id: 513,
      ts: 1,
      task_id: "task",
      type: "model.delta",
      payload: { message_id: "response", text: "Final answer" },
    };
    const read = vi.fn(async (cursor: number) => ({
      events: cursor === 0 ? fragments : [final],
      job: job("completed", 513),
    }));
    const stream = nativeJobStream(0, {
      read,
      subscribe: async () => () => {},
    });
    const received: EventRow[] = [];
    stream.onmessage = (event) => received.push(JSON.parse(event.data));
    await vi.advanceTimersByTimeAsync(30);
    expect(read.mock.calls.map(([cursor]) => cursor)).toEqual([0, 512]);
    expect(received.map((event) => event.type)).toEqual([
      "model.stream",
      "model.delta",
      "job.done",
    ]);
    expect(replay(received.slice(0, -1))).toEqual(
      replay([...fragments, final]),
    );
  });

  it("preserves task, response, metadata and non-text boundaries with bounded merging", () => {
    const fragment = (
      id: number,
      message_id = "one",
      task_id = "task",
    ): EventRow => ({
      id,
      ts: id,
      task_id,
      type: "model.stream",
      payload: { message_id, text: "x" },
    });
    const events = [
      fragment(1),
      fragment(2),
      fragment(3, "two"),
      fragment(4, "two", "other"),
      { ...fragment(5), type: "model.stream_end" },
      fragment(6),
      {
        ...fragment(7),
        payload: { message_id: "one", text: "y", extra: true },
      },
      fragment(8),
      { ...fragment(9), payload: { text: "No response ID" } },
    ];
    expect(compactStreamRows(events, 0).map((event) => event.id)).toEqual([
      2, 3, 4, 5, 6, 7, 8, 9,
    ]);
    expect(replay(compactStreamRows(events, 0))).toEqual(replay(events));
    expect(
      compactStreamRows(
        [fragment(1), fragment(2), fragment(2), fragment(3)],
        1,
      ),
    ).toEqual([{ ...fragment(3), payload: { message_id: "one", text: "xx" } }]);
    const large = Array.from({ length: 512 }, (_, index) => ({
      ...fragment(index + 1),
      payload: { message_id: "one", text: "😀".repeat(8192) },
    }));
    const compacted = compactStreamRows(large, 0);
    expect(compacted).toHaveLength(128);
    expect(
      compacted.every((event) => String(event.payload.text).length <= 65536),
    ).toBe(true);
    expect(replay(compacted)).toEqual(replay(large));
  });

  it("drains every page before completion and unsubscribes exactly once", async () => {
    vi.useFakeTimers();
    const stop = vi.fn();
    const read = vi.fn(async (cursor: number) => ({
      events: cursor === 0 ? rows(1, 512) : rows(513, 8),
      job: job("completed", 520),
    }));
    const stream = nativeJobStream(0, { read, subscribe: async () => stop });
    const received: { id?: number; type: string }[] = [];
    stream.onmessage = (event) => received.push(JSON.parse(event.data));
    await vi.advanceTimersByTimeAsync(30);
    expect(read.mock.calls.map(([cursor]) => cursor)).toEqual([0, 512]);
    expect(received.slice(0, -1).map((e) => e.id)).toEqual(
      Array.from({ length: 520 }, (_, i) => i + 1),
    );
    expect(received.at(-1)?.type).toBe("job.done");
    stream.close();
    expect(stop).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(10000);
    expect(read).toHaveBeenCalledTimes(2);
  });
  it("ignores an in-flight response and releases a listener registered after navigation", async () => {
    vi.useFakeTimers();
    let resolveRead!: (value: { events: EventRow[]; job: Job }) => void;
    let resolveListener!: (stop: () => void) => void;
    const stop = vi.fn();
    const stream = nativeJobStream(10, {
      read: () =>
        new Promise((resolve) => {
          resolveRead = resolve;
        }),
      subscribe: () =>
        new Promise((resolve) => {
          resolveListener = resolve;
        }),
    });
    const receive = vi.fn();
    stream.onmessage = receive;
    await vi.advanceTimersByTimeAsync(30);
    stream.close();
    resolveListener(stop);
    resolveRead({ events: rows(11, 1), job: job("completed", 11) });
    await vi.advanceTimersByTimeAsync(30);
    expect(receive).not.toHaveBeenCalled();
    expect(stop).toHaveBeenCalledTimes(1);
  });
  it("recovers a missed notification through polling from the last durable cursor", async () => {
    vi.useFakeTimers();
    const read = vi
      .fn()
      .mockRejectedValueOnce(new Error("transport failed"))
      .mockResolvedValueOnce({
        events: rows(21, 2),
        job: job("completed", 22),
      });
    const stream = nativeJobStream(20, {
      read,
      subscribe: async () => () => {},
    });
    const failed = vi.fn(),
      opened = vi.fn(),
      received = vi.fn();
    stream.onerror = failed;
    stream.onopen = opened;
    stream.onmessage = received;
    await vi.advanceTimersByTimeAsync(30);
    expect(failed).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(2530);
    expect(opened).toHaveBeenCalledTimes(1);
    expect(read.mock.calls).toEqual([[20], [20]]);
    expect(received).toHaveBeenCalledTimes(3);
    stream.close();
  });
});
