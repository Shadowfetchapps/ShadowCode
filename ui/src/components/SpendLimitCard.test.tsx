import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
} from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { SpendLimitCard } from "./SpendLimitCard";
import { ResumeCard } from "./ResumeCard";
import { RunDetails } from "./RunDetails";
import type { ChatItem } from "./cards";

afterEach(() => cleanup());

const item: Extract<ChatItem, { kind: "spend" }> = {
  kind: "spend",
  taskId: "t",
  jobId: "job-1",
  promptId: "p1",
  limitKind: "task",
  title: "This task reached its spending limit",
  text: "It has spent $1.02 on paid models, and the limit for one task is $1.00.",
  continueLabel: "Continue (limit raised to $2.00)",
};

it("asks Continue or Stop, then shows the answer", async () => {
  const onDecide = vi.fn(async () => {});
  render(<SpendLimitCard item={item} onDecide={onDecide} />);
  const card = screen.getByRole("region", { name: "Spending limit reached" });
  expect(card.textContent).toContain("This task reached its spending limit");
  fireEvent.click(
    screen.getByRole("button", { name: "Continue (limit raised to $2.00)" }),
  );
  expect(onDecide).toHaveBeenCalledWith(item, "continue");
  // Both buttons wait while the answer is sent.
  expect(
    (screen.getByRole("button", { name: "Stop" }) as HTMLButtonElement)
      .disabled,
  ).toBe(true);
  await vi.waitFor(() =>
    expect(
      (screen.getByRole("button", { name: "Stop" }) as HTMLButtonElement)
        .disabled,
    ).toBe(false),
  );
  fireEvent.click(screen.getByRole("button", { name: "Stop" }));
  expect(onDecide).toHaveBeenCalledWith(item, "stop");
  cleanup();
  render(
    <SpendLimitCard
      item={{
        ...item,
        resolved: "stop",
        outcome: "Stopped at your spending limit.",
      }}
      onDecide={onDecide}
    />,
  );
  expect(screen.queryAllByRole("button")).toHaveLength(0);
  expect(screen.getByText("Stopped at your spending limit.")).toBeTruthy();
});

it("lets a scheduled resume be cancelled and a ready one reviewed", () => {
  const resume: Extract<ChatItem, { kind: "resume" }> = {
    kind: "resume",
    resumeId: "r1",
    state: "scheduled",
    at: 1_790_010_000,
    label: "Codex",
    target: "cli:codex",
    text: "Will resume on Codex at 3:40 PM, when its plan limit resets.",
  };
  const onCancel = vi.fn();
  const onResumeNow = vi.fn();
  render(
    <ResumeCard item={resume} onCancel={onCancel} onResumeNow={onResumeNow} />,
  );
  fireEvent.click(screen.getByRole("button", { name: "Cancel resume" }));
  expect(onCancel).toHaveBeenCalled();
  cleanup();
  const ready = {
    ...resume,
    state: "needs_consent" as const,
    task: "Continue…",
  };
  render(
    <ResumeCard item={ready} onCancel={onCancel} onResumeNow={onResumeNow} />,
  );
  fireEvent.click(screen.getByRole("button", { name: "Review and resume" }));
  expect(onResumeNow).toHaveBeenCalledWith(ready);
});

it("words a waiting resume from the time it is shown", () => {
  const at = new Date(2026, 8, 30, 3, 40).getTime() / 1000;
  const resume: Extract<ChatItem, { kind: "resume" }> = {
    kind: "resume",
    resumeId: "r1",
    state: "scheduled",
    at,
    label: "Codex",
    target: "cli:codex",
    // Worded when it was scheduled, the evening before.
    text: "Will resume on Codex tomorrow at 3:40 AM, when its plan limit resets.",
  };
  const afterMidnight = new Date(2026, 8, 30, 1, 0).getTime() / 1000;
  render(
    <ResumeCard
      item={resume}
      onCancel={vi.fn()}
      onResumeNow={vi.fn()}
      now={afterMidnight}
    />,
  );
  const card = screen.getByRole("status", { name: "Scheduled resume" });
  expect(card.textContent).toMatch(/^Will resume on Codex at /);
  expect(card.textContent).not.toContain("tomorrow");
});

it("words a waiting resume again at midnight in a window left open", () => {
  vi.useFakeTimers();
  try {
    // Scheduled at 23:00 for 3:40, and the window stays open.
    vi.setSystemTime(new Date(2026, 8, 29, 23, 0));
    const resume: Extract<ChatItem, { kind: "resume" }> = {
      kind: "resume",
      resumeId: "r1",
      state: "scheduled",
      at: new Date(2026, 8, 30, 3, 40).getTime() / 1000,
      label: "Codex",
      target: "cli:codex",
      text: "Will resume on Codex tomorrow at 3:40 AM, when its plan limit resets.",
    };
    render(
      <ResumeCard item={resume} onCancel={vi.fn()} onResumeNow={vi.fn()} />,
    );
    const card = screen.getByRole("status", { name: "Scheduled resume" });
    expect(card.textContent).toMatch(/^Will resume on Codex tomorrow at /);
    // Nothing else renders the row again; midnight alone rewords it.
    act(() => vi.advanceTimersByTime(61 * 60 * 1000));
    expect(card.textContent).toMatch(/^Will resume on Codex at /);
    expect(card.textContent).not.toContain("tomorrow");
  } finally {
    vi.useRealTimers();
  }
});

it("lists what ran under Run details", () => {
  render(
    <RunDetails
      run={{
        model_id: "cli:codex",
        model: "gpt-5",
        provider: "cli:codex",
        route: "vendor_cli",
        vendor: "Codex",
        vendor_version: "codex-cli 0.158.0",
        effort: "high",
        app_version: "1.0.0",
        settings_hash: "abc123abc123",
        rules_hash: null,
      }}
    />,
  );
  const details = screen.getByText("Run details").closest("details")!;
  expect(details.textContent).toContain("Codex · codex-cli 0.158.0");
  expect(details.textContent).toContain("high");
  expect(details.textContent).toContain("None sent");
  cleanup();
  const { container } = render(<RunDetails run={undefined} />);
  expect(container.textContent).toBe("");
});
