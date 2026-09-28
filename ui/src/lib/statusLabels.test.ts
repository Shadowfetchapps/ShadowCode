import { expect, it } from "vitest";
import { goalStatusLabel, processStatusLabel } from "./statusLabels";

it("background processes read in sentence case, never as identifiers", () => {
  expect(processStatusLabel("RUNNING")).toBe("Running");
  expect(processStatusLabel("COMPLETED")).toBe("Finished");
  expect(processStatusLabel("CANCELLED")).toBe("Stopped");
  expect(processStatusLabel("INTERRUPTED")).toBe("Interrupted");
  expect(processStatusLabel("running")).toBe("Running");
  expect(processStatusLabel("WAITING_FOR_PORT")).toBe("Waiting for port");
});

it("goals say whether they are running, started, done or paused", () => {
  expect(goalStatusLabel({ status: "active", running: true })).toBe(
    "Running…",
  );
  expect(goalStatusLabel({ status: "active", progress: 0 })).toBe(
    "Not started",
  );
  expect(goalStatusLabel({ status: "active", progress: 0.5 })).toBe(
    "In progress",
  );
  expect(goalStatusLabel({ status: "completed" })).toBe("Done");
  expect(goalStatusLabel({ status: "paused" })).toBe("Paused");
  expect(goalStatusLabel({ status: "abandoned" })).toBe("Abandoned");
  expect(goalStatusLabel({ status: "blocked" })).toBe("Blocked");
});
