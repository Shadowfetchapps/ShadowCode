import { afterEach, expect, it, vi } from "vitest";
import { act, renderHook, waitFor } from "@testing-library/react";
import { useSecondOpinions } from "./useSecondOpinions";
import { opinionApi, type SecondOpinion } from "../lib/secondOpinion";

const bus = vi.hoisted(() => ({
  handlers: [] as ((payload: unknown) => void)[],
}));
vi.mock("../lib/transport", async (original) => {
  const real = await original<typeof import("../lib/transport")>();
  return {
    ...real,
    isNative: () => true,
    listen: vi.fn(async (_event: string, handler: (p: unknown) => void) => {
      bus.handlers.push(handler);
      return () => {
        bus.handlers = bus.handlers.filter((h) => h !== handler);
      };
    }),
  };
});
vi.mock("../lib/secondOpinion", async (original) => {
  const real = await original<typeof import("../lib/secondOpinion")>();
  return {
    ...real,
    opinionApi: {
      list: vi.fn(),
      start: vi.fn(),
      cancel: vi.fn(),
      setFinding: vi.fn(),
      fix: vi.fn(),
    },
  };
});

afterEach(() => {
  vi.resetAllMocks();
  bus.handlers = [];
});

const record = (over: Partial<SecondOpinion> = {}) =>
  ({
    id: "o1",
    kind: "review",
    status: "completed",
    created_at: 1,
    findings: [],
    ...over,
  }) as SecondOpinion;

it("reads again when the engine says a second opinion changed", async () => {
  vi.mocked(opinionApi.list).mockResolvedValue({
    workspace: "/w",
    second_opinions: [],
  });
  const toast = vi.fn();
  const scope = { workspace: "/w", session_id: "s1" };
  const { result } = renderHook(() => useSecondOpinions(scope, toast));
  await waitFor(() => expect(result.current.loaded).toBe(true));
  await waitFor(() => expect(bus.handlers).toHaveLength(1));
  vi.mocked(opinionApi.list).mockResolvedValue({
    workspace: "/w",
    second_opinions: [record()],
  });
  // Stream noise does not read; the second-opinion wake-up does.
  act(() => bus.handlers[0]({ type: "model.delta", session_id: "s1" }));
  expect(opinionApi.list).toHaveBeenCalledTimes(1);
  act(() =>
    bus.handlers[0]({ type: "second_opinion.updated", session_id: "s1" }),
  );
  await waitFor(() => expect(result.current.items).toHaveLength(1));
  expect(opinionApi.list).toHaveBeenLastCalledWith(scope);
});

it("a fix that needs consent waits for Send and then queues", async () => {
  vi.mocked(opinionApi.list).mockResolvedValue({
    workspace: "/w",
    second_opinions: [record()],
  });
  vi.mocked(opinionApi.fix)
    .mockResolvedValueOnce({
      consent: { needs_consent: true, handoff: { to: "Codex" } },
    })
    .mockResolvedValueOnce({
      value: {
        second_opinion: record({
          findings: [
            {
              id: "f1",
              file: "a.ts",
              severity: "high",
              title: "t",
              explanation: "e",
              suggested_fix: "",
              status: "fixing",
            },
          ],
        }),
        job: { id: "j2", session_id: "s1", status: "queued" },
      },
    });
  const { result } = renderHook(() =>
    useSecondOpinions({ workspace: "/w" }, vi.fn()),
  );
  await waitFor(() => expect(result.current.loaded).toBe(true));
  let queued: Promise<unknown> = Promise.resolve();
  act(() => {
    queued = result.current.fix("o1", "f1");
  });
  await waitFor(() => expect(result.current.consent).not.toBeNull());
  expect(opinionApi.fix).toHaveBeenCalledWith("o1", "f1", false);
  act(() => result.current.consent!.send());
  await expect(queued).resolves.toEqual({
    id: "j2",
    session_id: "s1",
    status: "queued",
  });
  expect(opinionApi.fix).toHaveBeenLastCalledWith("o1", "f1", true);
  await waitFor(() =>
    expect(result.current.items[0].findings[0]?.status).toBe("fixing"),
  );
  expect(result.current.consent).toBeNull();
});
