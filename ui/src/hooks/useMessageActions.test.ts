import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  forkSession: vi.fn(),
  taskCheckpoint: vi.fn(),
}));
vi.mock("../api", () => ({ api: mocks }));

import { useMessageActions } from "./useMessageActions";
import type { ChatItem } from "../components/cards";

const items: ChatItem[] = [
  { kind: "user", text: "First", taskId: "t1", eventId: 1, key: "a" },
  { kind: "agent", text: "Done", taskId: "t1", key: "b" },
  { kind: "user", text: "Second", taskId: "t2", eventId: 5, key: "c" },
  { kind: "agent", text: "Done", taskId: "t2", key: "d" },
];

function setup(busy = false, roles = false) {
  const submittingRef = { current: false };
  const deps = {
    openSession: vi.fn(async () => {}),
    // Like the real one: sending starts at once, before the engine answers.
    startTask: vi.fn(async () => {
      submittingRef.current = true;
    }),
    rewindNow: vi.fn(async (task: string) => [`${task}.txt`]),
    toast: vi.fn(),
  };
  const { result } = renderHook(() =>
    useMessageActions({
      items,
      sessionId: "s1",
      workspace: "/w",
      model: "local:gguf:qwen",
      roles,
      busy,
      queueing: false,
      submittingRef,
      ...deps,
    }),
  );
  return { actions: result.current, submittingRef, ...deps };
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.forkSession.mockResolvedValue({
    fork: { id: "s2", title: "Edited" },
    original: { id: "s1" },
    original_intact: true,
    forked_from_event: 1,
  });
  mocks.taskCheckpoint.mockResolvedValue({
    rewindable: true,
    checkpoint: { changes: 1, paths: ["x"] },
  });
});

describe("useMessageActions", () => {
  it("resends as Plan → Implement → Review while roles are on", async () => {
    const plain = setup();
    await act(() => plain.actions.retry("Second"));
    expect(plain.startTask).toHaveBeenCalledWith(
      expect.not.objectContaining({ roles: true }),
      null,
    );
    const roles = setup(false, true);
    await act(() => roles.actions.retry("Second"));
    expect(roles.startTask).toHaveBeenCalledWith(
      expect.objectContaining({ task: "Second", roles: true }),
      null,
    );
  });

  it("edits and resends in a fork made just before the message", async () => {
    const { actions, openSession, startTask, rewindNow } = setup();
    await act(() =>
      actions.editResend(
        items[0] as Extract<ChatItem, { kind: "user" }>,
        "First, edited",
        false,
      ),
    );
    expect(mocks.forkSession).toHaveBeenCalledWith("s1", 1, undefined, true);
    expect(openSession).toHaveBeenCalledWith("s2");
    expect(startTask).toHaveBeenCalledWith(
      expect.objectContaining({
        task: "First, edited",
        session_id: "s2",
        model: "local:gguf:qwen",
      }),
      null,
    );
    expect(rewindNow).not.toHaveBeenCalled();
  });

  it("undoes file changes from the message on, newest task first", async () => {
    const { actions, rewindNow } = setup();
    await act(() =>
      actions.editResend(
        items[0] as Extract<ChatItem, { kind: "user" }>,
        "Again",
        true,
      ),
    );
    expect(rewindNow.mock.calls.map((c) => c[0])).toEqual(["t2", "t1"]);
    expect(mocks.forkSession).toHaveBeenCalled();
  });

  it("stops before forking when a rewind fails, and waits while busy", async () => {
    const failing = setup();
    failing.rewindNow.mockResolvedValueOnce(null as never);
    await act(() =>
      failing.actions.editResend(
        items[2] as Extract<ChatItem, { kind: "user" }>,
        "x",
        true,
      ),
    );
    expect(mocks.forkSession).not.toHaveBeenCalled();
    const busy = setup(true);
    await act(() =>
      busy.actions.editResend(
        items[2] as Extract<ChatItem, { kind: "user" }>,
        "x",
        false,
      ),
    );
    expect(mocks.forkSession).not.toHaveBeenCalled();
    expect(busy.toast).toHaveBeenCalled();
  });

  it("retries in the same conversation", async () => {
    const { actions, startTask } = setup();
    await act(() => actions.retry("Second"));
    expect(startTask).toHaveBeenCalledWith(
      expect.objectContaining({ task: "Second", session_id: "s1" }),
      null,
    );
  });

  it("sends a retry once when it is clicked twice", async () => {
    const { actions, startTask } = setup();
    await act(() =>
      Promise.all([actions.retry("Second"), actions.retry("Second")]),
    );
    expect(startTask).toHaveBeenCalledTimes(1);
  });
});
