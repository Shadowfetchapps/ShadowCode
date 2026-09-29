import {
  act,
  cleanup,
  fireEvent,
  render,
  renderHook,
  screen,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  taskCheckpoint: vi.fn(),
  rewindTask: vi.fn(),
  undoRewind: vi.fn(),
}));
vi.mock("../api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../api")>();
  return { ...actual, api: { ...actual.api, ...mocks } };
});

import { RewindDialog } from "./RewindDialog";
import { useRewind } from "../hooks/useRewind";

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("rewinding a subscription turn you edited during", () => {
  it("keeps your saves, marks unreported changes, and rewinds shared files only on request", async () => {
    const confirm = vi.fn();
    render(
      <RewindDialog
        paths={["agent.txt", "shell.txt"]}
        kept={[
          { path: "both.txt", reason: "edited_by_you_and_agent" },
          { path: "mine.txt", reason: "saved_by_you" },
        ]}
        unreported={["shell.txt"]}
        onConfirm={confirm}
        onCancel={vi.fn()}
      />,
    );
    const changing = screen.getByRole("list", {
      name: "Files that will change",
    });
    expect(changing.textContent).toContain("agent.txt");
    expect(changing.textContent).toContain(
      "shell.txt · not reported by the agent",
    );
    expect(changing.textContent).not.toContain("mine.txt");
    const kept = screen.getByRole("list", {
      name: "Files that stay as you saved them",
    });
    expect(kept.textContent).toContain("mine.txt · only you changed it");
    expect(kept.textContent).toContain("both.txt · the agent edited it too");
    await act(async () =>
      fireEvent.click(screen.getByRole("button", { name: "Rewind 2 files" })),
    );
    expect(confirm).toHaveBeenLastCalledWith(false);
    fireEvent.click(
      screen.getByRole("checkbox", {
        name: /Also rewind the file the agent edited too/,
      }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Rewind 3 files" }));
    expect(confirm).toHaveBeenLastCalledWith(true);
  });

  it("asks with the kept files and passes the choice to the rewind", async () => {
    mocks.taskCheckpoint.mockResolvedValue({
      rewindable: true,
      checkpoint: {
        changes: 2,
        paths: ["agent.txt", "both.txt"],
        rewind_paths: [],
        kept: [
          { path: "both.txt", reason: "edited_by_you_and_agent" },
          { path: "mine.txt", reason: "saved_by_you" },
        ],
        unreported: [],
      },
    });
    mocks.rewindTask.mockResolvedValue({
      ok: true,
      restored: ["both.txt"],
      undo_id: null,
    });
    const toast = vi.fn();
    const { result } = renderHook(() =>
      useRewind({ busy: false, refresh: async () => {}, toast }),
    );
    await act(() => result.current.ask("t1"));
    // Only a shared file is left: the dialog still opens so it can be chosen.
    expect(result.current.asking).toEqual({
      taskId: "t1",
      paths: [],
      kept: [
        { path: "both.txt", reason: "edited_by_you_and_agent" },
        { path: "mine.txt", reason: "saved_by_you" },
      ],
    });
    await act(() => result.current.confirm(true));
    expect(mocks.rewindTask).toHaveBeenCalledWith("t1", true);
  });

  it("says there is nothing left when only your own saves remain", async () => {
    mocks.taskCheckpoint.mockResolvedValue({
      rewindable: false,
      checkpoint: {
        changes: 1,
        paths: ["agent.txt"],
        rewind_paths: [],
        kept: [{ path: "mine.txt", reason: "saved_by_you" }],
        unreported: [],
      },
    });
    const toast = vi.fn();
    const { result } = renderHook(() =>
      useRewind({ busy: false, refresh: async () => {}, toast }),
    );
    await act(() => result.current.ask("t1"));
    expect(result.current.asking).toBeNull();
    expect(toast).toHaveBeenCalledWith(
      "This task has no file changes left to rewind.",
      "info",
    );
  });
});
