import { describe, expect, it } from "vitest";
import { applyEvent, emptyTranscript, replay } from "./transcript";
import type { EventRow } from "../api";

const ev = (
  id: number,
  type: string,
  payload: Record<string, unknown>,
  task_id = "t1",
): EventRow => ({ id, ts: id, type, payload, task_id });

describe("transcript row keys", () => {
  it("every row has a unique key that survives streaming updates", () => {
    const events = [
      ev(1, "user.message", { text: "Fix the add function" }),
      ev(2, "model.stream", { text: "Look", message_id: "m1" }),
      ev(3, "tool.started", { tool: "read_file", call_id: "c1" }),
      ev(4, "model.stream", { text: "ing", message_id: "m1" }),
      ev(5, "tool.completed", {
        tool: "read_file",
        call_id: "c1",
        success: true,
      }),
      ev(6, "model.delta", { text: "Looking done", message_id: "m1" }),
    ];
    let state = emptyTranscript();
    const keys: Record<string, string> = {};
    for (const event of events) {
      state = applyEvent(state, event);
      const seen = state.items.map((item) => item.key);
      expect(seen.every(Boolean)).toBe(true);
      expect(new Set(seen).size).toBe(seen.length);
      for (const item of state.items)
        keys[`${item.kind}:${item.text.slice(0, 4)}`] ??= item.key!;
    }
    const answer = state.items.find((item) => item.kind === "agent")!;
    expect(answer.text).toBe("Looking done");
    // The answer kept the key its first fragment got.
    expect(answer.key).toBe(keys["agent:Look"]);
    const tool = state.items.find((item) => item.kind === "tool")!;
    expect(tool.key).toBe(keys["tool:"]);
    expect(tool.key).toBe("t:t1:event:3");
    expect(state.items[0].key).toBe("e:1:0");
  });

  it("replaying the same events yields the same keys", () => {
    const events = Array.from({ length: 50 }, (_, i) =>
      ev(i + 1, i % 3 ? "model.delta" : "user.message", {
        text: `row ${i}`,
      }),
    );
    const keys = (items: { key?: string }[]) => items.map((item) => item.key);
    expect(keys(replay(events).items)).toEqual(keys(replay(events).items));
  });

  it("a row inserted before the end still gets a key", () => {
    let state = replay([
      ev(1, "user.message", { text: "First" }),
      ev(2, "limit.reached", { vendor: "codex" }),
      ev(3, "model.delta", { text: "Stopped here" }),
    ]);
    state = applyEvent(
      state,
      ev(4, "limit.fallback", {
        ok: false,
        mode: "ask",
        from: "Codex",
        reason: "Choose a model",
      }),
    );
    const keys = state.items.map((item) => item.key);
    expect(keys.every(Boolean)).toBe(true);
    expect(new Set(keys).size).toBe(keys.length);
  });
});
