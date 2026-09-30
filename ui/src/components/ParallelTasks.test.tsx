import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { ContextChip } from "./ContextChip";
import { Sidebar } from "./Sidebar";
import { WorktreeBar } from "./WorktreeBar";
import type { Session, WorktreeTask } from "../api";

vi.mock("../api", () => ({ api: { sessions: vi.fn() } }));
afterEach(() => {
  cleanup();
  localStorage.clear();
});

it("opens the context and cost breakdown from the chip", () => {
  render(
    <ContextChip
      input={{
        provider: "openrouter",
        local: false,
        budget: { used: 40_000, limit: 100_000 },
        usage: {
          prompt_tokens: 40_000,
          completion_tokens: 900,
          cost_usd: 0.12,
        },
      }}
    />,
  );
  const chip = screen.getByRole("button", { name: /Context and cost/ });
  expect(chip.textContent).toContain("40% · 40k / 100k · $0.12");
  fireEvent.click(chip);
  const breakdown = screen.getByRole("dialog", {
    name: "Context and cost breakdown",
  });
  expect(breakdown.textContent).toContain("Output tokens");
  expect(breakdown.textContent).toContain("None yet");
  fireEvent.keyDown(window, { key: "Escape" });
  expect(screen.queryByRole("dialog")).toBeNull();
});

const rows: Session[] = [
  { id: "a", workspace: "/p", status: "active", updated_at: 3, title: "Alpha" },
  {
    id: "w",
    workspace: "/tree",
    worktree_source: "/p",
    worktree_task: "t1",
    status: "active",
    updated_at: 2,
    title: "In a worktree",
  },
  { id: "b", workspace: "/p", status: "active", updated_at: 1, title: "Beta" },
];

it("shows badges, lists worktree conversations under their project and offers a menu", () => {
  const onAction = vi.fn();
  render(
    <Sidebar
      sessions={rows}
      projects={[]}
      selected="a"
      workspace="/p"
      jobs={[]}
      badges={{ a: "approval", b: "unread" }}
      onSelect={() => undefined}
      onNew={() => undefined}
      onProject={() => undefined}
      onSettings={() => undefined}
      onHide={() => undefined}
      onAction={onAction}
    />,
  );
  // One project group: the worktree folder is not a project of its own.
  expect(screen.queryByText("tree")).toBeNull();
  expect(screen.getByLabelText("Needs your approval")).toBeTruthy();
  expect(screen.getByLabelText("Finished; not opened yet")).toBeTruthy();
  expect(screen.getByLabelText("Runs in its own worktree")).toBeTruthy();
  const beta = screen.getByTitle(/^Beta/);
  fireEvent.contextMenu(beta, { clientX: 10, clientY: 10 });
  fireEvent.click(screen.getByRole("menuitem", { name: "Fork" }));
  expect(onAction).toHaveBeenCalledWith("fork", rows[2]);
  fireEvent.contextMenu(beta, { clientX: 10, clientY: 10 });
  fireEvent.click(screen.getByRole("menuitem", { name: "Delete…" }));
  fireEvent.click(screen.getByRole("menuitem", { name: "Delete" }));
  expect(onAction).toHaveBeenCalledWith("delete", rows[2]);
  fireEvent.contextMenu(beta, { clientX: 10, clientY: 10 });
  fireEvent.click(screen.getByRole("menuitem", { name: "Rename" }));
  const field = screen.getByLabelText("Rename Beta");
  fireEvent.change(field, { target: { value: "Beta two" } });
  fireEvent.keyDown(field, { key: "Enter" });
  expect(onAction).toHaveBeenCalledWith("rename", rows[2], "Beta two");
  fireEvent.contextMenu(screen.getByTitle(/^Beta/), {
    clientX: 10,
    clientY: 10,
  });
  fireEvent.click(screen.getByRole("menuitem", { name: "Pin" }));
  expect(JSON.parse(localStorage.getItem("shadow:pins") || "[]")).toEqual([
    "b",
  ]);
});

const task: WorktreeTask = {
  id: "t1",
  workspace: "/home/u/project",
  session_id: "w",
  worktree: "/data/checkouts/t1",
  branch: "shadowcode/t1",
  base: { commit: "c", head: "h", included_uncommitted: false },
  task: "Fix it",
  created_at: 1,
  state: "done",
  job_id: "j",
  status: "completed",
  changed_files: [
    {
      path: "lib.txt",
      status: "modified",
      additions: 1,
      deletions: 1,
      binary: false,
    },
  ],
  changed_files_truncated: false,
  applied_files: [],
  conflicts: ["lib.txt"],
  conflict_detail: "",
  notes: [],
  removed: false,
};

it("offers apply, keep and discard for a finished worktree task", () => {
  const onAct = vi.fn();
  render(<WorktreeBar task={task} acting="" onAct={onAct} />);
  expect(
    screen.getByText(/1 file changed in its own copy of project/),
  ).toBeTruthy();
  expect(screen.getByRole("alert").textContent).toContain("lib.txt");
  fireEvent.click(screen.getByRole("button", { name: /Apply to project/ }));
  expect(onAct).toHaveBeenLastCalledWith("apply");
  fireEvent.click(screen.getByRole("button", { name: /Keep as branch/ }));
  expect(onAct).toHaveBeenLastCalledWith("keep-branch");
  fireEvent.click(screen.getByRole("button", { name: /Discard…/ }));
  fireEvent.click(screen.getByRole("button", { name: "Discard" }));
  expect(onAct).toHaveBeenLastCalledWith("discard");
  cleanup();
  render(
    <WorktreeBar
      task={{ ...task, state: "running", status: "running", conflicts: [] }}
      acting=""
      onAct={onAct}
    />,
  );
  expect(screen.queryByRole("button", { name: /Apply/ })).toBeNull();
  expect(screen.getByText(/main checkout is free/)).toBeTruthy();
});

it("says why a setup command failed, not that it did not run", () => {
  const setup = (command: object) => ({
    ok: false,
    commands: [
      { command: "npm ci", ok: false, seconds: 600, output: "", ...command },
    ],
  });
  const cases: [object, string][] = [
    [{ exit_code: 1 }, "exited with 1"],
    [{ stopped: "timeout" }, "took too long and was stopped"],
    [{ stopped: "signal", signal: 9 }, "was stopped (signal 9)"],
    [{ stopped: "not_started" }, "could not start"],
  ];
  for (const [command, words] of cases) {
    render(
      <WorktreeBar
        task={{ ...task, conflicts: [], setup: setup(command) }}
        acting=""
        onAct={vi.fn()}
      />,
    );
    const summary = screen.getByText(/Setup did not finish/);
    expect(summary.textContent).toContain(words);
    expect(summary.textContent).not.toContain("did not run");
    cleanup();
  }
});
