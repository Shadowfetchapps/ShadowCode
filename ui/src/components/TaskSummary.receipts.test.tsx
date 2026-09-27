import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type { EventRow } from "../api";
import { replay } from "../lib/transcript";
import { TaskSummary } from "./TaskSummary";

afterEach(cleanup);
const event = (
  id: number,
  type: string,
  payload: Record<string, unknown>,
  task_id = "one",
): EventRow => ({ id, ts: id, type, payload, task_id });
const complete = (id: number, call_id: string, text: string, task = "one") =>
  event(
    id,
    "tool.completed",
    { tool: "exec", call_id, success: false, output_preview: text },
    task,
  );
function show(calls: EventRow[], callId: string, output_ref?: unknown) {
  const result = replay([
    event(1, "agent.started", { task: "Test" }),
    ...calls,
    event(100, "agent.completed", {
      success: true,
      summary: "Finished",
      verification: {
        status: "not_run",
        commands: [
          {
            command: "npm test",
            exit_code: 1,
            success: false,
            tool_call_id: callId,
            output_ref,
          },
        ],
      },
    }),
  ]);
  render(<TaskSummary activity={result.activity.one} onReview={vi.fn()} />);
}
it("uses the exact completion event when historical call IDs collide", () => {
  show(
    [
      complete(2, "[redacted secret]", "wrong edit output"),
      complete(3, "[redacted secret]", "actual command output"),
    ],
    "[redacted secret]",
    "event:3",
  );
  expect(screen.getByText("actual command output")).toBeTruthy();
  expect(screen.queryByText("wrong edit output")).toBeNull();
});
it("does not guess output from an ambiguous historical call ID", () => {
  show(
    [
      complete(2, "duplicate", "first wrong output"),
      complete(3, "duplicate", "second wrong output"),
    ],
    "duplicate",
  );
  expect(
    screen.getByText(
      "The recorded output could not be matched to this execution receipt.",
    ),
  ).toBeTruthy();
  expect(screen.queryByText("first wrong output")).toBeNull();
});
it.each([
  "event:999",
  "https://example.invalid/output",
  "event:02",
  null,
  123,
  { event: 2 },
])(
  "does not fall back from an unavailable or invalid explicit reference %s",
  (reference) => {
    show([complete(2, "unique", "unrelated output")], "unique", reference);
    expect(
      screen.getByText(
        "The recorded output could not be matched to this execution receipt.",
      ),
    ).toBeTruthy();
  },
);
it("never obtains receipt output from another task", () => {
  show(
    [
      complete(2, "same", "other task output", "two"),
      complete(3, "same", "this task output"),
    ],
    "same",
    "event:2",
  );
  expect(
    screen.getByText(
      "The recorded output could not be matched to this execution receipt.",
    ),
  ).toBeTruthy();
});
it("rejects an explicit reference to a same-task non-command tool", () => {
  show(
    [
      event(2, "tool.completed", {
        tool: "read_file",
        call_id: "read",
        success: true,
        output_preview: "file content is not command output",
      }),
      complete(3, "exec", "actual command output"),
    ],
    "exec",
    "event:2",
  );
  expect(
    screen.getByText(
      "The recorded output could not be matched to this execution receipt.",
    ),
  ).toBeTruthy();
  expect(screen.queryByText("file content is not command output")).toBeNull();
});
it("retains a unique nonredacted legacy call ID fallback", () => {
  show([complete(2, "unique", "legacy output")], "unique");
  expect(screen.getByText("legacy output")).toBeTruthy();
});
