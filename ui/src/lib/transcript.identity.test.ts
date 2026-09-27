import { expect, it } from "vitest";
import type { EventRow } from "../api";
import { replay } from "./transcript";
const event = (
  id: number,
  type: string,
  payload: Record<string, unknown>,
): EventRow => ({ id, ts: id, type, payload, task_id: "one" });
it("preserves ambiguous historical completions without assigning them to the wrong path", () => {
  const result = replay([
    event(1, "agent.started", { task: "Read" }),
    event(2, "tool.started", {
      tool: "read_file",
      call_id: "[redacted secret]",
      arguments: { path: "a.txt" },
    }),
    event(3, "tool.started", {
      tool: "read_file",
      call_id: "[redacted secret]",
      arguments: { path: "b.txt" },
    }),
    event(4, "tool.completed", {
      tool: "read_file",
      call_id: "[redacted secret]",
      success: true,
      output_preview: "output B",
    }),
    event(5, "tool.completed", {
      tool: "read_file",
      call_id: "[redacted secret]",
      success: true,
      output_preview: "output A",
    }),
  ]);
  const calls = result.activity.one.calls;
  expect(calls.filter((c) => c.live).map((c) => c.path)).toEqual([
    "a.txt",
    "b.txt",
  ]);
  expect(calls.filter((c) => !c.live).map((c) => c.path)).toEqual([
    undefined,
    undefined,
  ]);
  expect(calls.filter((c) => !c.live).map((c) => c.output)).toEqual([
    "output B",
    "output A",
  ]);
  const cards = result.items.filter((i) => i.kind === "tool");
  expect(new Set(cards.map((c) => c.key)).size).toBe(4);
});
