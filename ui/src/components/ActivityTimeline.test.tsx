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
