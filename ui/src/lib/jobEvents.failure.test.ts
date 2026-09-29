import { afterEach, describe, expect, it, vi } from "vitest";
import { nativeJobStream } from "./jobEvents";
import { replay } from "./transcript";
import type { EventRow, Job } from "../api";

afterEach(() => vi.useRealTimers());

// RUN-02: the engine fails a task before the model streams anything (an
// internal error or a model that cannot load). The engine saves the job as
// failed with one terminal event at its cursor; the window must end the task
// from that alone, with no endless spinner.
const failed = (cursor: number) =>
  ({ id: "job", status: "failed", event_cursor: cursor }) as Job;
const terminal: EventRow = {
  id: 22,
  ts: 0,
  type: "agent.completed",
  task_id: "t1",
  payload: {
    success: false,
    cancelled: false,
    summary:
      "ShadowCode hit an internal error and stopped this task. Your files and this conversation were kept; you can send the message again.",
  },
};
const started: EventRow = {
  id: 21,
  ts: 0,
  type: "user.message",
  task_id: "t1",
  payload: { text: "Summarize the notes" },
};

describe("a task that fails before any output", () => {
  it("ends the stream on the durable terminal event and shows the reason", async () => {
    vi.useFakeTimers();
    const stop = vi.fn();
    const read = vi.fn(async () => ({
      events: [started, terminal],
      job: failed(22),
    }));
    const stream = nativeJobStream(20, {
      read,
      subscribe: async () => stop,
    });
    const received: EventRow[] = [];
    stream.onmessage = (event) => received.push(JSON.parse(event.data));
    await vi.advanceTimersByTimeAsync(30);
    expect(received.map((event) => event.type)).toEqual([
      "user.message",
      "agent.completed",
      "job.done",
    ]);
    expect(stop).toHaveBeenCalledTimes(1);
    const state = replay([started, terminal]);
    expect(state.stage).toBe("FAILED");
    expect(state.activeTaskId).toBeUndefined();
    expect(
      state.items.some(
        (item) =>
          item.kind === "agent" &&
          item.who === "Needs attention" &&
          item.text.includes("internal error"),
      ),
    ).toBe(true);
  });

  it("keeps reading until the terminal row arrives, then ends once", async () => {
    vi.useFakeTimers();
    let page = { events: [started], job: failed(22) };
    const read = vi.fn(async () => page);
    const stream = nativeJobStream(20, {
      read,
      subscribe: async () => () => {},
    });
    const received: EventRow[] = [];
    stream.onmessage = (event) => received.push(JSON.parse(event.data));
    await vi.advanceTimersByTimeAsync(30);
    // Saved as failed, but its terminal row is not readable yet.
    expect(received.map((event) => event.type)).toEqual(["user.message"]);
    page = { events: [terminal], job: failed(22) };
    await vi.advanceTimersByTimeAsync(2530);
    expect(received.map((event) => event.type)).toEqual([
      "user.message",
      "agent.completed",
      "job.done",
    ]);
    await vi.advanceTimersByTimeAsync(10000);
    expect(received.filter((event) => event.type === "job.done")).toHaveLength(
      1,
    );
  });
});
