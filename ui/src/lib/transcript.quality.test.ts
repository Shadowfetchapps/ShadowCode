import { expect, it } from "vitest";
import { compactedNote, replay, tryOnFor } from "./transcript";
import { tryOnRequest } from "../hooks/useTaskActions";
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

it("closes a paused stuck card once the task goes on or ends", () => {
  const stuck = event(2, "agent.stuck", {
    job_id: "j1",
    kind: "same_failure",
    text: "The agent seems stuck.",
    paused: true,
  });
  const card = (events: EventRow[]) =>
    replay(events).items.find((i) => i.kind === "stuck");
  const start = event(1, "agent.started", { task: "Fix it" });
  expect(card([start, stuck])).toMatchObject({ paused: true });
  expect(card([start, stuck])).not.toHaveProperty("resolved");
  expect(
    card([start, stuck, event(3, "tool.started", { tool: "exec" })]),
  ).toMatchObject({ resolved: "continued" });
  expect(
    card([start, stuck, event(3, "agent.completed", { success: true })]),
  ).toMatchObject({ resolved: "ended" });
  // A subagent's card, or one for a task nobody could answer for, never
  // waited.
  expect(
    card([
      start,
      event(2, "agent.stuck", { job_id: "j1", text: "Stuck.", paused: false }),
    ]),
  ).toMatchObject({ paused: false });
});

it("keeps a task's files, scope and mode for Try on…", () => {
  const state = replay([
    event(1, "user.message", {
      text: "Fix the parser",
      mentions: [{ path: "src/parser.rs", kind: "file" }],
      only_change: true,
    }),
    event(2, "agent.started", { task: "Fix the parser", mode: "plan" }),
    event(3, "agent.completed", { success: false, cancelled: true }),
  ]);
  const tryOn = tryOnFor(state, "one");
  expect(tryOn).toMatchObject({
    request: "Fix the parser",
    mentions: [{ path: "src/parser.rs", kind: "file" }],
    onlyChange: true,
    mode: "plan",
  });
  expect(
    tryOnRequest(tryOn, "local:qwen", {
      workspace: "/w",
      sessionId: "s1",
      queue: false,
    }),
  ).toMatchObject({
    model: "local:qwen",
    session_id: "s1",
    purpose: "planner",
    mentions: [{ path: "src/parser.rs", kind: "file" }],
    only_change: true,
  });
  // Without mentions nothing is scoped.
  const plain = tryOnRequest(
    { taskId: "one", from: "Codex", request: "Hi" },
    "local:qwen",
    { queue: false },
  );
  expect(plain.purpose).toBe("coder");
  expect(plain).not.toHaveProperty("mentions");
  expect(plain).not.toHaveProperty("only_change");
});
