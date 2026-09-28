import { renderHook } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { api, type CheckJobRequest, type Job } from "../api";
import { useRunCheck } from "./useRunCheck";

afterEach(() => vi.restoreAllMocks());
const request: CheckJobRequest = {
  workspace: "/project",
  session_id: "session",
  command: "npm test",
  timeout: 300,
  queue: false,
};
const job: Job = {
  id: "check",
  session_id: "session",
  workspace: "/project",
  event_cursor: 0,
  started_at: 1,
  status: "running",
  mode: "command",
};
function setup() {
  const context = {
    workspace: "/project",
    sessionId: "session",
    selectedRef: { current: "session" },
    selection: { current: 1 },
    submittingRef: { current: false },
    locked: false,
    setSubmitting: vi.fn(),
    start: vi.fn(),
    pin: vi.fn(),
    refresh: vi.fn().mockResolvedValue(undefined),
  };
  const view = renderHook(() => useRunCheck(context));
  return { context, ...view };
}

it("starts a native check without model generation or composer changes", async () => {
  const start = vi.spyOn(api, "startTestJob").mockResolvedValue(job);
  const generate = vi.spyOn(api, "startJob");
  const { result, context } = setup();
  await result.current(request);
  expect(start).toHaveBeenCalledWith(request);
  expect(generate).not.toHaveBeenCalled();
  expect(context.start).toHaveBeenCalledWith(job);
  expect(context.pin).toHaveBeenCalledOnce();
  expect(context.submittingRef.current).toBe(false);
});

it("refuses duplicate submissions while the initial command is being accepted", async () => {
  let accept!: (value: Job) => void;
  const start = vi.spyOn(api, "startTestJob").mockImplementation(
    () =>
      new Promise((resolve) => {
        accept = resolve;
      }),
  );
  const { result, context } = setup();
  const first = result.current(request);
  await expect(result.current(request)).rejects.toThrow("Wait for");
  expect(start).toHaveBeenCalledTimes(1);
  accept(job);
  await first;
  expect(context.submittingRef.current).toBe(false);
});

it("rejects an old dialog's project/session before submitting", async () => {
  const start = vi.spyOn(api, "startTestJob");
  const { result } = setup();
  await expect(
    result.current({ ...request, workspace: "/other" }),
  ).rejects.toThrow("changed");
  await expect(
    result.current({ ...request, session_id: "other" }),
  ).rejects.toThrow("changed");
  expect(start).not.toHaveBeenCalled();
});

it("does not attach a delayed job after navigating away and back", async () => {
  let accept!: (value: Job) => void;
  vi.spyOn(api, "startTestJob").mockImplementation(
    () =>
      new Promise((resolve) => {
        accept = resolve;
      }),
  );
  const { result, context } = setup();
  const pending = result.current(request);
  context.selection.current += 2;
  accept(job);
  await pending;
  expect(context.start).not.toHaveBeenCalled();
  expect(context.pin).not.toHaveBeenCalled();
  expect(context.refresh).toHaveBeenCalledOnce();
});

it("does not turn a refresh failure into a retryable command failure", async () => {
  vi.spyOn(api, "startTestJob").mockResolvedValue(job);
  const { result, context } = setup();
  context.refresh.mockRejectedValue(new Error("feed unavailable"));
  await expect(result.current(request)).resolves.toBeUndefined();
  expect(context.start).toHaveBeenCalledWith(job);
});

it("releases the shared submission guard after a refused command", async () => {
  vi.spyOn(api, "startTestJob").mockRejectedValue(
    new Error("Project is not trusted"),
  );
  const { result, context } = setup();
  await expect(result.current(request)).rejects.toThrow("not trusted");
  expect(context.start).not.toHaveBeenCalled();
  expect(context.submittingRef.current).toBe(false);
  expect(context.setSubmitting).toHaveBeenLastCalledWith(false);
});
