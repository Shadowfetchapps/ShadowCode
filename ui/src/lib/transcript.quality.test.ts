import { expect, it } from "vitest";
import { compactedNote, replay } from "./transcript";
import type { EventRow } from "../api";

const event = (
  id: number,
  type: string,
  payload: Record<string, unknown>,
): EventRow => ({
  id,
  ts: id,
  type,
  payload,
  task_id: "one",
});

it("turns agent-quality events into cards and notes", () => {
  const items = replay([
    event(1, "agent.started", { task: "Fix it" }),
    event(2, "tool_call.repaired", { from: "text", count: 1 }),
    event(3, "agent.stuck", {
      job_id: "j1",
      kind: "same_failure",
      text: "The agent seems stuck.",
    }),
    event(4, "scope.outside", { job_id: "j1", paths: ["README.md"] }),
    event(5, "task.flags", {
      count: 1,
      text: "Heads up: this task skipped or focused a test in tests/a.rs.",
    }),
  ]).items;
  expect(items.find((i) => i.kind === "stuck")).toMatchObject({
    kind: "stuck",
    jobId: "j1",
    text: "The agent seems stuck.",
  });
  const notes = items
    .filter((i) => i.kind === "note")
    .map((i) => ("text" in i ? i.text : ""));
  expect(notes).toEqual([
    "The model wrote its tool call as text; ShadowCode read it and ran it",
    "Changed outside the files you chose: README.md. Review can undo them",
    "Heads up: this task skipped or focused a test in tests/a.rs.",
  ]);
});

it("says what compaction kept", () => {
  expect(compactedNote({ omitted_messages: 12 })).toBe(
    "Context compacted; 12 earlier messages omitted",
  );
  expect(
    compactedNote({
      omitted_messages: 12,
      requested: true,
      pinned: 2,
      rules_reapplied: 1,
    }),
  ).toBe(
    "Conversation shortened as you asked; 12 earlier messages summarized; kept 2 pinned answers and re-applied 1 folder rule",
  );
});
