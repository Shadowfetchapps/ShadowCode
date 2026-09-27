import { afterEach, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { TaskTimingDetails } from "./TaskTimingDetails";
import { TaskSummary } from "./TaskSummary";
import { parseTimings } from "../lib/timing";
import { replay } from "../lib/transcript";
import type { TaskTimings } from "../api";

afterEach(cleanup);
const timing: TaskTimings = {
  schema_version: 1,
  complete: true,
  total_seconds: 21,
  queue_seconds: 10,
  active_seconds: 11,
  preparation_seconds: 5,
  runtime_wait_seconds: 3,
  model_load_seconds: 1.5,
  model_reused: false,
  model_requests: 2,
  model_requests_seconds: 2,
  first_text_seconds: 0.15,
  first_text_request: 2,
  tool_batches_seconds: 1,
  check_process_seconds: 0.7,
  final_checks_seconds: 0.2,
};
it("renders measured durations and explains overlap and first-text limits", () => {
  render(<TaskTimingDetails timings={timing} />);
  expect(screen.getByText("Model load · within preparation")).toBeTruthy();
  expect(screen.getByText("1.50s")).toBeTruthy();
  expect(screen.getByText("Loaded for this task")).toBeTruthy();
  expect(screen.getByText("First text · request 2")).toBeTruthy();
  expect(screen.getByText(/It is not time to first token/)).toBeTruthy();
  expect(
    screen.getByText(/excludes approval waits and file fingerprinting/),
  ).toBeTruthy();
});
it("does not invent missing or invalid observations for old and subscription tasks", () => {
  const { rerender, container } = render(<TaskTimingDetails />);
  expect(container.textContent).toBe("");
  rerender(
    <TaskTimingDetails
      timings={{
        schema_version: 1,
        complete: true,
        total_seconds: 2,
        queue_seconds: 0,
        model_requests: 0,
      }}
    />,
  );
  expect(screen.queryByText(/First text/)).toBeNull();
  expect(screen.queryByText(/Model requests/)).toBeNull();
  rerender(
    <TaskTimingDetails
      timings={{ ...timing, model_reused: true, model_load_seconds: null }}
    />,
  );
  expect(screen.getByText("Already loaded")).toBeTruthy();
  expect(screen.queryByText("Model load · within preparation")).toBeNull();
  expect(parseTimings({ ...timing, total_seconds: NaN })).toBeUndefined();
  expect(
    parseTimings({ ...timing, model_load_seconds: -1 })?.model_load_seconds,
  ).toBeNull();
});
it("replays the durable timing receipt in the existing task summary", () => {
  const state = replay([
    {
      id: 1,
      ts: 100,
      task_id: "t",
      type: "agent.completed",
      payload: { success: true, summary: "Done", timings: timing },
    },
  ]);
  render(<TaskSummary activity={state.activity.t} onReview={() => {}} />);
  expect(screen.getByText("Timing details")).toBeTruthy();
  expect(screen.getByText("21.0s")).toBeTruthy();
});
it("keeps incomplete measurements visibly partial", () => {
  render(
    <TaskTimingDetails
      timings={{ ...timing, complete: false, first_text_seconds: null }}
    />,
  );
  expect(screen.getByText("Timing details · partial")).toBeTruthy();
  expect(screen.getByText(/No text was observed/)).toBeTruthy();
});
