import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type { SubagentRun } from "../lib/subagents";
import { SubagentCard } from "./SubagentCard";
import { RoleSummary } from "./RoleSummary";
import { ConsentDialog } from "./ConsentDialog";
import { TaskSummary } from "./TaskSummary";
import { emptyActivity } from "../lib/activity";

afterEach(cleanup);

const run = (over: Partial<SubagentRun> = {}): SubagentRun => ({
  runId: "r1",
  agent: "general",
  description: "Implement",
  prompt: "You are the implement role…",
  mode: "write",
  model: "Codex",
  status: "completed",
  summary: "Changed note.txt",
  jobId: "j1",
  sessionId: "child",
  files: [{ path: "note.txt", status: "modified", additions: 1, deletions: 1 }],
  filesTruncated: false,
  binaryFiles: [],
  patch: true,
  applied: false,
  notes: [],
  steps: 1,
  tokens: 45,
  durationS: 3,
  role: "implement",
  runner: "vendor",
  route: "cloud",
  cost: "subscription",
  ...over,
});

it("shows who did what: the role, its model, cost and status", () => {
  const onOpen = vi.fn();
  render(<SubagentCard run={run()} onOpen={onOpen} />);
  const head = screen.getByRole("button", { name: /Implement/ });
  expect(head.textContent).toContain("Codex");
  expect(head.textContent).toContain("worktree · 1 file · Subscription");
  expect(screen.getByRole("status").textContent).toContain("Done");
  fireEvent.click(head);
  expect(
    screen.getByText(
      /applied to the project with your usual edit approval after the review/,
    ),
  ).toBeTruthy();
  expect(screen.getByText(/vendor CLI · cloud/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: /Open transcript/ }));
  expect(onOpen).toHaveBeenCalledWith("child");
});

it("shows a review role's verdict and a local role's cost", () => {
  render(
    <SubagentCard
      run={run({
        role: "review",
        agent: "review",
        mode: "read-only",
        files: [],
        patch: false,
        model: "Qwen3 14B",
        runner: "shadowcode",
        route: "local",
        cost: "local",
        verdict: "needs_changes",
        summary: "A bug on line 3.\nVerdict: needs changes",
      })}
    />,
  );
  const head = screen.getByRole("button", { name: /Review/ });
  expect(head.textContent).toContain("read-only · $0 · local");
  expect(screen.getByText("Needs changes")).toBeTruthy();
});

it("keeps plain subagent cards and names a vendor runner", () => {
  render(
    <SubagentCard
      run={run({
        role: undefined,
        agent: "auditor",
        mode: "read-only",
        files: [],
      })}
    />,
  );
  const head = screen.getByRole("button", { name: /@auditor/ });
  expect(head.textContent).toContain("Codex · read-only · Subscription");
});

it("summarises the roles on the task summary card", () => {
  const roles = {
    stages: [
      {
        role: "plan" as const,
        name: "Claude Code",
        status: "completed",
        runner: "vendor",
        route: "cloud",
        cost: "subscription",
        files: 0,
        additions: 0,
        deletions: 0,
      },
      {
        role: "implement" as const,
        name: "Codex",
        status: "completed",
        runner: "vendor",
        route: "cloud",
        cost: "subscription",
        files: 2,
        additions: 5,
        deletions: 1,
      },
      {
        role: "review" as const,
        name: "Qwen3 14B",
        status: "completed",
        runner: "shadowcode",
        route: "local",
        cost: "local",
        files: 0,
        additions: 0,
        deletions: 0,
        verdict: "ready",
      },
    ],
    applied: true,
    note: "The changes were applied to the project.",
  };
  render(<RoleSummary roles={roles} />);
  const list = screen.getByRole("list", { name: "Roles" });
  expect(list.textContent).toContain("Plan Claude Code Done · Subscription");
  expect(list.textContent).toContain(
    "Implement Codex Done · 2 files (+5 −1) · Subscription",
  );
  expect(list.textContent).toContain(
    "Review Qwen3 14B Ready to apply · $0 · local",
  );
  expect(
    screen.getByText("The changes were applied to the project."),
  ).toBeTruthy();
  cleanup();
  // A plan-only task changed nothing: the quiet summary still names roles.
  render(
    <TaskSummary
      activity={{
        ...emptyActivity("t1"),
        roles: { ...roles, stages: roles.stages.slice(0, 1), note: "" },
        finished: { success: true, cancelled: false, summary: "Plan" },
      }}
      onReview={() => {}}
    />,
  );
  expect(screen.getByRole("list", { name: "Roles" }).textContent).toContain(
    "Claude Code",
  );
});

it("asks before a local conversation's work reaches cloud roles", () => {
  const onSend = vi.fn();
  render(
    <ConsentDialog
      request={{
        needs_consent: true,
        handoff: {
          from: "Qwen3 14B",
          to: "Claude Code and Codex",
          excerpt_chars: 1200,
          roles: [
            { role: "plan", label: "Plan", name: "Claude Code" },
            { role: "implement", label: "Implement", name: "Codex" },
          ],
        },
      }}
      attachments={[]}
      onSend={onSend}
      onCancel={() => {}}
    />,
  );
  expect(
    screen.getByRole("heading", { name: "Send to Claude Code and Codex?" }),
  ).toBeTruthy();
  const list = screen.getByRole("list", { name: "Cloud roles" });
  expect(list.textContent).toContain("Plan role Claude Code");
  expect(list.textContent).toContain("Implement role Codex");
  expect(screen.getByText(/This conversation runs on Qwen3 14B/)).toBeTruthy();
  expect(screen.getByText(/A summary of this conversation/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  expect(onSend).toHaveBeenCalledOnce();
});
