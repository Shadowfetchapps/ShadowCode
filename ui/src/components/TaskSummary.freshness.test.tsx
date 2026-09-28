import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { api } from "../api";
import { useDrawerMemory } from "../hooks/useDrawerMemory";
import { isNative, listen } from "../lib/transport";
import { notifyWorkspaceFilesChanged } from "../lib/workspaceChanges";
import { emptyActivity } from "../lib/activity";
import { createVerificationReader } from "../lib/verificationRefresh";
import { FileEditor } from "./FileEditor";
import { TaskSummary } from "./TaskSummary";

vi.mock("../lib/transport", () => ({
  isNative: vi.fn(() => false),
  transportKind: () => "none",
  listen: vi.fn(),
}));

vi.mock("../api", () => ({
  api: {
    files: vi.fn(),
    file: vi.fn(),
    fileRevision: vi.fn(),
    saveFile: vi.fn(),
  },
}));

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.resetAllMocks();
  vi.unstubAllGlobals();
});

const command = {
  command: "npm test",
  exit_code: 0,
  success: true,
  kind: "configured_check",
  state: "passed",
  attemptId: "check-1",
};
const activity = {
  ...emptyActivity("task-1"),
  finished: { success: true, cancelled: false, summary: "Done" },
  verification: { status: "passed", commands: [command] },
};
const passed = {
  status: "passed",
  commands: [{ ...command, attempt_id: "check-1" }],
};
const stale = {
  status: "stale",
  commands: [
    { ...command, attempt_id: "check-1", state: "stale", success: false },
  ],
};

function Editor() {
  const { memory, update, discardFileDraft, resolveFileDraftConflict } =
    useDrawerMemory("/project");
  return (
    <FileEditor
      workspace="/project"
      memory={memory}
      onMemory={update}
      onDiscardFileDraft={discardFileDraft}
      onResolveFileDraftConflict={resolveFileDraftConflict}
      onShowDiff={() => {}}
      toast={() => {}}
    />
  );
}

it("removes a visible passed verdict after saving in the editor without refocusing", async () => {
  vi.stubGlobal("IntersectionObserver", undefined);
  let saved = false;
  vi.mocked(api.files).mockResolvedValue({
    entries: [{ name: "source.txt", path: "source.txt", type: "file" }],
    workspace: "/project",
    path: ".",
    parent: "",
  });
  // Mixed line endings select the accessible plain editor, not CodeMirror.
  vi.mocked(api.file).mockResolvedValue({
    path: "source.txt",
    content: "one\r\ntwo\n",
    hash: "a".repeat(64),
    bytes: 9,
    truncated: false,
  });
  vi.mocked(api.fileRevision).mockImplementation(async () => ({
    path: "source.txt",
    hash: (saved ? "b" : "a").repeat(64),
    bytes: 9,
  }));
  vi.mocked(api.saveFile).mockImplementation(async () => {
    saved = true;
    return { path: "source.txt", hash: "b".repeat(64), bytes: 12 };
  });
  const read = vi.fn(async () => (saved ? stale : passed));
  render(
    <>
      <TaskSummary
        workspace="/project"
        sessionId="session-1"
        activity={activity}
        readVerification={read}
        onReview={() => {}}
      />
      <Editor />
    </>,
  );
  await screen.findByText("Configured checks passed");
  fireEvent.click(await screen.findByRole("button", { name: "source.txt" }));
  const editor = await screen.findByRole("textbox", {
    name: "Edit source.txt",
  });
  fireEvent.change(editor, { target: { value: "changed\ntwo\n" } });
  fireEvent.keyDown(editor, { key: "s", ctrlKey: true });
  await waitFor(() => expect(api.saveFile).toHaveBeenCalledOnce());
  await screen.findByText("Checks are stale — files changed");
  expect(screen.queryByText("Configured checks passed")).toBeNull();
  expect(read).toHaveBeenCalledTimes(2);
});

function deferred() {
  let resolve!: (value: unknown) => void;
  const promise = new Promise<unknown>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

it("discards a pre-edit result and coalesces changes during an unresolved read", async () => {
  vi.stubGlobal("IntersectionObserver", undefined);
  const beforeEdit = deferred();
  const afterEdit = deferred();
  const read = vi
    .fn()
    .mockReturnValueOnce(beforeEdit.promise)
    .mockReturnValueOnce(afterEdit.promise);
  render(
    <TaskSummary
      workspace="/project"
      sessionId="session-1"
      activity={activity}
      readVerification={read}
      onReview={() => {}}
    />,
  );
  expect(read).toHaveBeenCalledOnce();
  act(() => {
    notifyWorkspaceFilesChanged("/project");
    notifyWorkspaceFilesChanged("/project");
    notifyWorkspaceFilesChanged("/project");
  });
  expect(read).toHaveBeenCalledOnce();
  await act(async () => beforeEdit.resolve(passed));
  expect(read).toHaveBeenCalledTimes(2);
  expect(screen.queryByText("Configured checks passed")).toBeNull();
  await act(async () => afterEdit.resolve(stale));
  expect(screen.getByText("Checks are stale — files changed")).toBeTruthy();
});

it("clears green immediately, ignores other projects, and scopes pending results to the displayed identity", async () => {
  vi.stubGlobal("IntersectionObserver", undefined);
  const oldProject = deferred();
  const newProject = deferred();
  const read = vi
    .fn()
    .mockResolvedValueOnce(passed)
    .mockReturnValueOnce(oldProject.promise)
    .mockReturnValueOnce(newProject.promise);
  const shown = render(
    <TaskSummary
      workspace="/project"
      sessionId="session-1"
      activity={activity}
      readVerification={read}
      onReview={() => {}}
    />,
  );
  await screen.findByText("Configured checks passed");
  act(() => notifyWorkspaceFilesChanged("/unrelated"));
  expect(read).toHaveBeenCalledOnce();
  expect(screen.getByText("Configured checks passed")).toBeTruthy();
  act(() => notifyWorkspaceFilesChanged("/project"));
  expect(screen.queryByText("Configured checks passed")).toBeNull();
  shown.rerender(
    <TaskSummary
      workspace="/other"
      sessionId="session-2"
      activity={activity}
      readVerification={read}
      onReview={() => {}}
    />,
  );
  await act(async () => oldProject.resolve(passed));
  expect(screen.queryByText("Configured checks passed")).toBeNull();
  act(() => notifyWorkspaceFilesChanged("/project"));
  expect(read).toHaveBeenCalledTimes(3);
  await act(async () => newProject.resolve(stale));
  expect(screen.getByText("Checks are stale — files changed")).toBeTruthy();
});

it("defers hidden receipt reads until visible and removes subscriptions on unmount", async () => {
  let intersect!: (entries: { isIntersecting: boolean }[]) => void;
  const disconnect = vi.fn();
  vi.stubGlobal(
    "IntersectionObserver",
    class {
      constructor(callback: typeof intersect) {
        intersect = callback;
      }
      observe() {}
      disconnect = disconnect;
    },
  );
  const read = vi
    .fn()
    .mockResolvedValueOnce(passed)
    .mockResolvedValueOnce(stale);
  const shown = render(
    <TaskSummary
      workspace="/project"
      activity={activity}
      readVerification={read}
      onReview={() => {}}
    />,
  );
  act(() => notifyWorkspaceFilesChanged("/project"));
  expect(read).not.toHaveBeenCalled();
  act(() => intersect([{ isIntersecting: true }]));
  await screen.findByText("Configured checks passed");
  act(() => {
    intersect([{ isIntersecting: false }]);
    notifyWorkspaceFilesChanged("/project");
  });
  expect(read).toHaveBeenCalledOnce();
  expect(screen.queryByText("Configured checks passed")).toBeNull();
  act(() => intersect([{ isIntersecting: true }]));
  await screen.findByText("Checks are stale — files changed");
  shown.unmount();
  act(() => notifyWorkspaceFilesChanged("/project"));
  expect(read).toHaveBeenCalledTimes(2);
  expect(disconnect).toHaveBeenCalledOnce();
});

it("uses scoped engine mutation wake-ups without reading on token events", async () => {
  vi.stubGlobal("IntersectionObserver", undefined);
  vi.mocked(isNative).mockReturnValue(true);
  let wake!: (payload: unknown) => void;
  const stop = vi.fn();
  vi.mocked(listen).mockImplementation(async (_channel, handler) => {
    wake = handler;
    return stop;
  });
  const read = vi.fn().mockResolvedValueOnce(passed).mockResolvedValue(stale);
  const shown = render(
    <TaskSummary
      workspace="/project"
      sessionId="session-1"
      activity={activity}
      readVerification={read}
      onReview={() => {}}
    />,
  );
  await screen.findByText("Configured checks passed");
  act(() => {
    for (let index = 0; index < 100; index++)
      wake({ type: "agent.delta", session_id: "session-1" });
    wake({ type: "files.changed", session_id: "unrelated-session" });
  });
  expect(read).toHaveBeenCalledOnce();
  act(() => wake({ type: "files.changed", session_id: "session-1" }));
  await screen.findByText("Checks are stale — files changed");
  expect(read).toHaveBeenCalledTimes(2);
  shown.unmount();
  expect(stop).toHaveBeenCalledOnce();
});

it("reassesses an external change without an event and never piles polls onto a slow read", async () => {
  vi.useFakeTimers();
  vi.stubGlobal("IntersectionObserver", undefined);
  const slow = deferred();
  const read = vi
    .fn()
    .mockResolvedValueOnce(passed)
    .mockReturnValueOnce(slow.promise)
    .mockResolvedValue(stale);
  const shown = render(
    <TaskSummary
      workspace="/project"
      activity={activity}
      readVerification={read}
      onReview={() => {}}
    />,
  );
  await act(async () => {});
  expect(screen.getByText("Configured checks passed")).toBeTruthy();
  await act(async () => {
    await vi.advanceTimersByTimeAsync(5_000);
  });
  expect(read).toHaveBeenCalledTimes(2);
  expect(screen.queryByText("Configured checks passed")).toBeNull();
  await act(async () => {
    await vi.advanceTimersByTimeAsync(60_000);
  });
  expect(read).toHaveBeenCalledTimes(2);
  await act(async () => slow.resolve(stale));
  expect(screen.getByText("Checks are stale — files changed")).toBeTruthy();
  expect(read).toHaveBeenCalledTimes(2);
  await act(async () => {
    await vi.advanceTimersByTimeAsync(5_000);
  });
  expect(read).toHaveBeenCalledTimes(3);
  shown.unmount();
  expect(vi.getTimerCount()).toBe(0);
});

it("does not read a hidden document or offscreen card and rejects a read completed after hiding", async () => {
  vi.useFakeTimers();
  let intersect!: (entries: { isIntersecting: boolean }[]) => void;
  vi.stubGlobal(
    "IntersectionObserver",
    class {
      constructor(callback: typeof intersect) {
        intersect = callback;
      }
      observe() {}
      disconnect() {}
    },
  );
  const visibility = vi
    .spyOn(document, "visibilityState", "get")
    .mockReturnValue("hidden");
  const slow = deferred();
  const read = vi
    .fn()
    .mockReturnValueOnce(slow.promise)
    .mockResolvedValue(stale);
  render(
    <TaskSummary
      workspace="/project"
      activity={activity}
      readVerification={read}
      onReview={() => {}}
    />,
  );
  act(() => intersect([{ isIntersecting: true }]));
  await act(async () => {
    await vi.advanceTimersByTimeAsync(10_000);
  });
  expect(read).not.toHaveBeenCalled();
  visibility.mockReturnValue("visible");
  fireEvent(document, new Event("visibilitychange"));
  expect(read).toHaveBeenCalledOnce();
  visibility.mockReturnValue("hidden");
  fireEvent(document, new Event("visibilitychange"));
  expect(read.mock.calls[0][1].aborted).toBe(true);
  await act(async () => slow.resolve(passed));
  expect(screen.queryByText("Configured checks passed")).toBeNull();
  act(() => intersect([{ isIntersecting: false }]));
  visibility.mockReturnValue("visible");
  fireEvent(document, new Event("visibilitychange"));
  await act(async () => {
    await vi.advanceTimersByTimeAsync(20_000);
  });
  expect(read).toHaveBeenCalledOnce();
  await act(async () => intersect([{ isIntersecting: true }]));
  expect(screen.getByText("Checks are stale — files changed")).toBeTruthy();
  visibility.mockRestore();
});

it("coalesces repeated manual refresh while a read is pending", async () => {
  vi.stubGlobal("IntersectionObserver", undefined);
  const slow = deferred();
  const read = vi
    .fn()
    .mockReturnValueOnce(slow.promise)
    .mockResolvedValue(stale);
  render(
    <TaskSummary
      activity={activity}
      readVerification={read}
      onReview={() => {}}
    />,
  );
  for (let i = 0; i < 10; i++)
    fireEvent.click(
      screen.getByRole("button", { name: "Refresh check evidence" }),
    );
  expect(read).toHaveBeenCalledOnce();
  await act(async () => slow.resolve(passed));
  expect(read).toHaveBeenCalledTimes(2);
  expect(screen.getByText("Checks are stale — files changed")).toBeTruthy();
});

it("shares one periodic batch across visible cards and removes queued hidden cards", async () => {
  vi.useFakeTimers();
  const intersections: ((entries: { isIntersecting: boolean }[]) => void)[] =
    [];
  vi.stubGlobal(
    "IntersectionObserver",
    class {
      constructor(callback: (entries: { isIntersecting: boolean }[]) => void) {
        intersections.push(callback);
      }
      observe() {}
      disconnect() {}
    },
  );
  let externalEdit = false;
  const send = vi.fn(async (ids: string[]) => ({
    verifications: Object.fromEntries(
      ids.map((id) => [id, externalEdit ? stale : passed]),
    ),
  }));
  const read = createVerificationReader(send);
  const otherActivity = {
    ...activity,
    taskId: "task-2",
    verification: {
      ...activity.verification,
      commands: [{ ...command, attemptId: "check-2" }],
    },
  };
  const shown = render(
    <>
      <TaskSummary
        activity={activity}
        readVerification={read}
        onReview={() => {}}
      />
      <TaskSummary
        activity={otherActivity}
        readVerification={read}
        onReview={() => {}}
      />
    </>,
  );
  await act(async () => {
    for (const intersect of intersections)
      intersect([{ isIntersecting: true }]);
  });
  expect(send).toHaveBeenCalledOnce();
  expect(send).toHaveBeenCalledWith(["check-1", "check-2"]);
  expect(screen.getAllByText("Configured checks passed")).toHaveLength(2);
  externalEdit = true;
  await act(async () => {
    await vi.advanceTimersByTimeAsync(5_000);
  });
  expect(send).toHaveBeenCalledTimes(2);
  expect(screen.getAllByText("Checks are stale — files changed")).toHaveLength(
    2,
  );
  await act(async () => {
    // A card can leave the viewport after requesting but before dispatch.
    fireEvent.click(
      screen.getAllByRole("button", { name: "Refresh check evidence" })[0],
    );
    intersections[0]([{ isIntersecting: false }]);
  });
  expect(send).toHaveBeenCalledTimes(2);
  await act(async () => {
    await vi.advanceTimersByTimeAsync(5_000);
  });
  expect(send).toHaveBeenLastCalledWith(["check-2"]);
  shown.unmount();
  expect(vi.getTimerCount()).toBe(0);
});
