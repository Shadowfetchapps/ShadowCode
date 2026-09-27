import { afterEach, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { ActivityTimeline } from "./ActivityTimeline";
import { emptyActivity } from "../lib/activity";

afterEach(cleanup);

it("renders local startup and gives task controls precedence over it", () => {
  const activity = { ...emptyActivity("task"), localPhase: "loading" as const };
  const { rerender } = render(
    <ActivityTimeline activity={activity} jobStatus="running" />,
  );
  expect(screen.getByText("Loading local model")).toBeTruthy();
  rerender(
    <ActivityTimeline activity={activity} jobStatus="queued" localQueued />,
  );
  expect(screen.getByText("Queued for local model")).toBeTruthy();
  expect(screen.queryByText("Loading local model")).toBeNull();
  rerender(
    <ActivityTimeline
      activity={activity}
      jobStatus="cancelling"
      pendingApprovals={1}
    />,
  );
  expect(screen.getByText("Stopping…")).toBeTruthy();
  expect(screen.queryByText("Waiting for approval")).toBeNull();
  rerender(<ActivityTimeline activity={activity} jobStatus="paused" />);
  expect(screen.getByText("Paused")).toBeTruthy();
  rerender(
    <ActivityTimeline
      activity={activity}
      jobStatus="running"
      pendingApprovals={1}
    />,
  );
  expect(screen.getByText("Waiting for approval")).toBeTruthy();
  expect(screen.queryByText("Loading local model")).toBeNull();
});

it("does not show a green success icon or done label for cancelled verification", () => {
  const activity = {
    ...emptyActivity("task"),
    verification: {
      status: "cancelled",
      commands: [
        {
          command: "npm test",
          kind: "configured_check",
          state: "cancelled",
          success: false,
          exit_code: null,
        },
      ],
    },
    finished: { success: false, cancelled: true, summary: "Stopped" },
  };
  const { container } = render(
    <ActivityTimeline activity={activity} withSummary />,
  );
  const row = container.querySelector(".activity-step")!;
  expect(row.className).toContain("is-incomplete");
  expect(row.querySelector(".lucide-check")).toBeNull();
  expect(screen.getByText("Checks cancelled")).toBeTruthy();
  expect(screen.getByText("not verified or incomplete")).toBeTruthy();
  expect(screen.queryByText("done")).toBeNull();
});

it("labels a tool interrupted by final completion as outcome unknown", () => {
  const activity = {
    ...emptyActivity("task"),
    calls: [
      {
        callId: "pending",
        tool: "exec",
        step: "commands" as const,
        label: "exec",
        command: "sleep 60",
        live: false,
      },
    ],
    finished: { success: false, cancelled: true, summary: "Stopped" },
  };
  const { container } = render(
    <ActivityTimeline activity={activity} withSummary />,
  );
  expect(container.querySelector(".lucide-check")).toBeNull();
  expect(screen.getByText("outcome unknown")).toBeTruthy();
  expect(screen.queryByText("done")).toBeNull();
});

it.each([
  ["stale", "Checks stale"],
  ["skipped", "Checks incomplete"],
  ["incomplete", "Checks incomplete"],
  ["unavailable", "Checks unavailable"],
  ["not_run", "Checks not run"],
  ["vendor_owned", "Vendor checks"],
])(
  "renders %s verification without a success or failure icon",
  (status, label) => {
    const activity = {
      ...emptyActivity("task"),
      calls: [
        {
          callId: "check",
          tool: "exec",
          step: "testing" as const,
          label: "exec",
          command: "npm test",
          live: false,
          ok: true,
        },
      ],
      verification: {
        status,
        commands: [
          {
            command: "npm test",
            kind: "configured_check",
            state: status,
            success: false,
            exit_code: 0,
          },
        ],
      },
      finished: { success: true, cancelled: false, summary: "Done" },
    };
    const { container } = render(
      <ActivityTimeline activity={activity} withSummary />,
    );
    expect(screen.getByText(label)).toBeTruthy();
    expect(container.querySelector(".lucide-check")).toBeNull();
    expect(container.querySelector(".lucide-circle-alert")).toBeNull();
    expect(container.querySelector(".lucide-circle-dashed")).toBeTruthy();
    // The recorded successful subprocess still truthfully appears completed.
    expect(screen.getByText("done")).toBeTruthy();
  },
);

it("renders failed commands as failed while keeping completed reads green", () => {
  const activity = {
    ...emptyActivity("task"),
    calls: [
      {
        callId: "read",
        tool: "read_file",
        step: "reading" as const,
        label: "read_file",
        live: false,
        ok: true,
      },
      {
        callId: "command",
        tool: "exec",
        step: "commands" as const,
        label: "exec",
        live: false,
        ok: false,
      },
    ],
    finished: { success: false, cancelled: false, summary: "Failed" },
  };
  const { container } = render(
    <ActivityTimeline activity={activity} withSummary />,
  );
  const read = container.querySelector(".activity-step.is-done")!;
  const command = container.querySelector(".activity-step.is-failed")!;
  expect(read.textContent).toContain("Reading project");
  expect(read.querySelector(".lucide-check")).toBeTruthy();
  expect(command.textContent).toContain("Running commands");
  expect(command.querySelector(".lucide-check")).toBeNull();
  expect(command.querySelector(".lucide-circle-alert")).toBeTruthy();
});
