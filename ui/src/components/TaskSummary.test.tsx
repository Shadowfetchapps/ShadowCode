import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { TaskSummary } from "./TaskSummary";
import { WelcomeBanner } from "./WelcomeBanner";
import { emptyActivity, type TaskActivity } from "../lib/activity";

afterEach(() => cleanup());

const base: TaskActivity = {
  ...emptyActivity("t1"),
  startedAt: 100,
  finishedAt: 175,
  changed: ["src/app.ts", "README.md"],
  verification: {
    status: "last_command_failed",
    commands: [
      { command: "npm test", exit_code: 0, success: true },
      { command: "npm run lint", exit_code: 2, success: false },
    ],
  },
  finished: { success: true, cancelled: false, summary: "Done" },
};

it("lists changed files with diff counts, checks with exit codes and duration", async () => {
  const onReview = vi.fn();
  const diffStat = vi.fn(async (path: string) =>
    path === "src/app.ts" ? { add: 3, del: 1 } : null,
  );
  render(
    <TaskSummary
      activity={base}
      onReview={onReview}
      diffStat={diffStat}
      onRewind={vi.fn()}
    />,
  );
  expect(screen.getByText("Finished")).toBeTruthy();
  expect(screen.getByText(/1m 15s/)).toBeTruthy();
  expect(screen.getByText("npm test")).toBeTruthy();
  expect(screen.getByText("exit 0")).toBeTruthy();
  expect(screen.getByText("exit 2")).toBeTruthy();
  await waitFor(() => expect(screen.getByText("+3")).toBeTruthy());
  expect(screen.getByText("−1")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "src/app.ts" }));
  expect(onReview).toHaveBeenCalledWith("src/app.ts");
  onReview.mockClear();
  // Review changes opens the task's first changed file, so a diff shows.
  fireEvent.click(screen.getByRole("button", { name: "Review changes" }));
  expect(onReview).toHaveBeenCalledWith(base.changed[0]);
  expect(screen.getByRole("button", { name: "Rewind" })).toBeTruthy();
});

it("states vendor-owned checks and unverified claims honestly", () => {
  render(
    <TaskSummary
      activity={{
        ...base,
        changed: [],
        verification: {
          status: "vendor_owned",
          commands: [],
          note: "The vendor CLI owns verification; ShadowCode does not claim a harness verdict.",
        },
      }}
      onReview={vi.fn()}
    />,
  );
  // A finished answer that changed nothing gets one quiet line, with no
  // Review or Rewind actions.
  const quiet = screen.getByRole("region", { name: "Task summary" });
  expect(quiet.className).toContain("is-quiet");
  expect(quiet.textContent).toContain("Finished");
  expect(quiet.textContent).toContain("No files were changed.");
  expect(screen.queryByRole("button", { name: "Review changes" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Rewind" })).toBeNull();
  cleanup();
  render(
    <TaskSummary
      activity={{
        ...base,
        changed: ["src/app.ts"],
        verification: {
          status: "vendor_owned",
          commands: [],
          note: "The vendor CLI owns verification; ShadowCode does not claim a harness verdict.",
        },
      }}
      onReview={vi.fn()}
    />,
  );
  expect(screen.getByText(/vendor CLI owns verification/)).toBeTruthy();
  expect(screen.getByRole("button", { name: "Review changes" })).toBeTruthy();
  cleanup();
  render(
    <TaskSummary
      activity={{
        ...base,
        verification: {
          status: "not_run",
          commands: [],
          presentedAs: "unverified",
        },
      }}
      onReview={vi.fn()}
    />,
  );
  expect(screen.getByText("Verification not run.")).toBeTruthy();
  expect(
    screen.getByText(/claims results that no recorded check confirms/),
  ).toBeTruthy();
});

it("the quiet card keeps Run a check in an action row, not stretched across the card", () => {
  const stopped: TaskActivity = {
    ...base,
    changed: [],
    verification: { status: "not_run", commands: [] },
    finished: { success: false, cancelled: true, summary: "Stopped" },
  };
  render(
    <TaskSummary
      activity={stopped}
      onReview={vi.fn()}
      runCheck={{ workspace: "/w", sessionId: "s1", onRun: vi.fn() }}
    />,
  );
  const quiet = screen.getByRole("region", { name: "Task summary" });
  expect(quiet.className).toContain("is-quiet");
  const check = screen.getByRole("button", { name: "Run a check…" });
  // A bare button would be a grid item of the card and fill its width.
  expect(check.parentElement?.className).toContain("task-summary-actions");
  expect(check.parentElement).not.toBe(quiet);
});

it("a plan limit is a warning, quiet when nothing changed", () => {
  const limited: TaskActivity = {
    ...base,
    changed: [],
    verification: { status: "vendor_owned", commands: [] },
    finished: {
      success: false,
      cancelled: false,
      summary: "Codex plan limit reached",
      limitReached: "Codex",
    },
  };
  render(<TaskSummary activity={limited} onReview={vi.fn()} />);
  const quiet = screen.getByRole("region", { name: "Task summary" });
  expect(quiet.className).toContain("is-quiet");
  expect(quiet.className).toContain("is-warn");
  expect(quiet.className).not.toContain("is-bad");
  expect(quiet.textContent).toContain("Plan limit reached");
  expect(quiet.textContent).not.toContain("Finished with problems");
  expect(quiet.textContent).toContain("No files were changed.");
  cleanup();
  // With work done before the limit, the full card keeps its warning.
  render(
    <TaskSummary
      activity={{ ...limited, changed: ["src/app.ts"] }}
      onReview={vi.fn()}
      onRewind={vi.fn()}
    />,
  );
  const card = screen.getByRole("region", { name: "Task summary" });
  expect(card.className).toContain("is-warn");
  expect(card.className).not.toContain("is-bad");
  expect(card.querySelector("header strong")?.textContent).toBe(
    "Plan limit reached",
  );
  expect(screen.getByRole("button", { name: "Review changes" })).toBeTruthy();
  cleanup();
  // An ordinary failure is still one.
  render(
    <TaskSummary
      activity={{
        ...limited,
        finished: { success: false, cancelled: false, summary: "Broke" },
      }}
      onReview={vi.fn()}
    />,
  );
  const failed = screen.getByRole("region", { name: "Task summary" });
  expect(failed.className).toContain("is-bad");
  expect(failed.textContent).toContain("Finished with problems");
});

it("welcome state offers at most three suggestions", () => {
  const onSelect = vi.fn();
  render(<WelcomeBanner onSelect={onSelect} />);
  const chips = screen.getAllByRole("button");
  expect(chips).toHaveLength(3);
  fireEvent.click(chips[0]);
  expect(onSelect).toHaveBeenCalled();
});

it("reassesses visible receipts on focus and never falls back to a green result on error", async () => {
  vi.stubGlobal("IntersectionObserver", undefined);
  const receipt = {
    command: "npm test",
    exit_code: 0,
    success: true,
    kind: "configured_check",
    state: "passed",
    attemptId: "job-1",
  };
  const wire = { ...receipt, attempt_id: "job-1" };
  const read = vi
    .fn()
    .mockResolvedValueOnce({ status: "passed", commands: [wire] })
    .mockResolvedValueOnce({
      status: "stale",
      commands: [{ ...wire, state: "stale", success: false }],
    })
    .mockRejectedValueOnce(new Error("Cannot assess"));
  render(
    <TaskSummary
      activity={{
        ...base,
        verification: { status: "passed", commands: [receipt] },
      }}
      onReview={vi.fn()}
      readVerification={read}
    />,
  );
  await waitFor(() =>
    expect(screen.getByText("Configured checks passed")).toBeTruthy(),
  );
  fireEvent(window, new Event("focus"));
  await waitFor(() =>
    expect(screen.getByText("Checks are stale — files changed")).toBeTruthy(),
  );
  expect(read).toHaveBeenCalledWith("job-1", expect.any(AbortSignal));
  fireEvent.click(
    screen.getByRole("button", { name: "Refresh check evidence" }),
  );
  await waitFor(() =>
    expect(screen.getByText("Current verification unavailable")).toBeTruthy(),
  );
  expect(screen.queryByText("Configured checks passed")).toBeNull();
  vi.unstubAllGlobals();
});

it("does not reuse a freshness result when the displayed attempt changes", async () => {
  vi.stubGlobal("IntersectionObserver", undefined);
  const command = {
    command: "npm test",
    exit_code: 0,
    success: true,
    kind: "configured_check",
    state: "passed",
    attemptId: "first",
  };
  const read = vi
    .fn()
    .mockResolvedValueOnce({
      status: "passed",
      commands: [{ ...command, attempt_id: "first" }],
    })
    .mockResolvedValueOnce({
      status: "stale",
      commands: [
        { ...command, attempt_id: "second", state: "stale", success: false },
      ],
    });
  const shown = render(
    <TaskSummary
      activity={{
        ...base,
        verification: { status: "passed", commands: [command] },
      }}
      onReview={vi.fn()}
      readVerification={read}
    />,
  );
  await waitFor(() =>
    expect(screen.getByText("Configured checks passed")).toBeTruthy(),
  );
  shown.rerender(
    <TaskSummary
      activity={{
        ...base,
        taskId: "second-task",
        verification: {
          status: "passed",
          commands: [{ ...command, attemptId: "second" }],
        },
      }}
      onReview={vi.fn()}
      readVerification={read}
    />,
  );
  await waitFor(() =>
    expect(screen.getByText("Checks are stale — files changed")).toBeTruthy(),
  );
  expect(read).toHaveBeenLastCalledWith("second", expect.any(AbortSignal));
  expect(screen.queryByText("Configured checks passed")).toBeNull();
  vi.unstubAllGlobals();
});
