import "@testing-library/jest-dom/vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { RunCheck, type RunCheckAction } from "./RunCheck";
import { TaskSummary } from "./TaskSummary";
import { emptyActivity } from "../lib/activity";

afterEach(cleanup);
const action = (onRun = vi.fn(async () => {})): RunCheckAction => ({
  workspace: "/original/project",
  sessionId: "original-session",
  onRun,
});
const open = () =>
  fireEvent.click(screen.getByRole("button", { name: "Run a check…" }));
const command = (value: string) =>
  fireEvent.change(screen.getByRole("textbox", { name: "Check command" }), {
    target: { value },
  });

it("requires explicit command input and submits only a check bound to its original workspace and session", async () => {
  const onRun = vi.fn(async () => {});
  render(<RunCheck action={action(onRun)} />);
  open();
  expect(screen.getByRole("button", { name: "Run check" })).toBeDisabled();
  expect(screen.getByText("/original/project")).toBeTruthy();
  expect(screen.getByText(/without another model turn/)).toBeTruthy();
  command(" npm test ");
  fireEvent.click(screen.getByRole("button", { name: "Run check" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(onRun).toHaveBeenCalledExactlyOnceWith({
    workspace: "/original/project",
    session_id: "original-session",
    command: "npm test",
    timeout: 300,
    queue: false,
  });
});

it("does not replay saved receipt text as an executable command", () => {
  render(
    <TaskSummary
      activity={{
        ...emptyActivity("task"),
        changed: ["a.ts"],
        verification: {
          status: "failed",
          commands: [
            {
              command: "TOKEN=[redacted secret] npm test",
              exit_code: 1,
              success: false,
            },
          ],
        },
        finished: { success: false, cancelled: false, summary: "Failed" },
      }}
      onReview={vi.fn()}
      runCheck={action()}
    />,
  );
  open();
  expect(screen.getByRole("textbox", { name: "Check command" })).toHaveValue(
    "",
  );
});

it("blocks a dialog after navigation instead of redirecting its command", async () => {
  const onRun = vi.fn(async () => {});
  const view = render(<RunCheck action={action(onRun)} />);
  open();
  command("npm test");
  view.rerender(
    <RunCheck
      action={{
        ...action(onRun),
        workspace: "/other",
        sessionId: "other-session",
      }}
    />,
  );
  expect(screen.getByRole("alert")).toHaveTextContent(
    "selected conversation changed",
  );
  expect(screen.getByRole("button", { name: "Run check" })).toBeDisabled();
  fireEvent.submit(screen.getByRole("textbox").closest("form")!);
  expect(onRun).not.toHaveBeenCalled();
  expect(screen.getByText("/original/project")).toBeTruthy();
});

it("prevents duplicate submissions and displays an error without losing command input", async () => {
  let reject!: (error: Error) => void;
  const onRun = vi.fn(
    () =>
      new Promise<void>((_, fail) => {
        reject = fail;
      }),
  );
  render(<RunCheck action={action(onRun)} />);
  open();
  command("cargo test");
  const form = screen.getByRole("textbox").closest("form")!;
  fireEvent.submit(form);
  fireEvent.submit(form);
  expect(onRun).toHaveBeenCalledTimes(1);
  expect(
    screen.getByRole("button", { name: "Starting check…" }),
  ).toBeDisabled();
  reject(new Error("Trust this project first"));
  await waitFor(() =>
    expect(screen.getByRole("alert")).toHaveTextContent(
      "Trust this project first",
    ),
  );
  expect(screen.getByRole("textbox")).toHaveValue("cargo test");
  expect(screen.getByRole("button", { name: "Run check" })).not.toBeDisabled();
});

it("offers a check from a quiet finished task and respects active-work blocking", () => {
  render(
    <TaskSummary
      activity={{
        ...emptyActivity("task"),
        finished: { success: true, cancelled: false, summary: "Done" },
      }}
      onReview={vi.fn()}
      runCheck={{ ...action(), disabled: true }}
    />,
  );
  expect(screen.getByRole("button", { name: "Run a check…" })).toBeDisabled();
});
