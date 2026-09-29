import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { PickerTarget } from "../lib/picker";
import type { SecondOpinion } from "../lib/secondOpinion";

const mocks = vi.hoisted(() => ({
  review: vi.fn(),
  reviewFile: vi.fn(),
  reviewUndo: vi.fn(),
}));
vi.mock("../api", () => ({ api: mocks }));
const opinions = vi.hoisted(() => ({
  list: vi.fn(),
  start: vi.fn(),
  cancel: vi.fn(),
  setFinding: vi.fn(),
  fix: vi.fn(),
  options: vi.fn(),
  current: vi.fn(),
  savePrefs: vi.fn(),
}));
vi.mock("../lib/secondOpinion", async (original) => {
  const real = await original<typeof import("../lib/secondOpinion")>();
  return { ...real, opinionApi: opinions };
});
vi.mock("./Markdown", () => ({
  Markdown: ({ children }: { children: string }) => <p>{children}</p>,
}));

import { ReviewView } from "./ReviewView";

const targets: PickerTarget[] = [
  {
    id: "cli:claude",
    provider: "cli:claude",
    group: "subscriptions",
    name: "Claude Code · Sonnet",
    inference: "cloud",
    availability: "ready",
  },
  {
    id: "local:gguf:qwen",
    provider: "llamacpp",
    group: "local",
    name: "Qwen · This computer",
    inference: "local",
    availability: "ready",
  },
];
const hunk = (line: number, from: string, to: string) => ({
  id: `h${line}`,
  header: `@@ -${line},1 +${line},1 @@`,
  lines: [
    { kind: "del", text: from },
    { kind: "add", text: to },
  ],
});
const record = (over: Partial<SecondOpinion> = {}): SecondOpinion => ({
  id: "o1",
  kind: "review",
  workspace: "/w",
  source: "task",
  task_id: "t1",
  session_id: "s1",
  question: "",
  reviewer: { model: "local:gguf:qwen", label: "Qwen", local: true },
  writer: { model: "cli:claude", label: "Claude Code", local: false },
  same_model: false,
  consented: false,
  job_id: "j",
  review_session: "rs",
  review_task: "rt",
  status: "completed",
  created_at: 1,
  diff_hash: "task-1",
  files: ["src/app.ts"],
  omitted: [],
  diff: [],
  truncated: false,
  context_chars: 100,
  summary: "Mostly fine.",
  findings: [
    {
      id: "f1",
      file: "src/app.ts",
      line: 12,
      severity: "medium",
      title: "V should stay 1",
      explanation: "Callers rely on V = 1.",
      suggested_fix: "Keep V = 1.",
      status: "open",
    },
    {
      id: "f2",
      file: "",
      severity: "low",
      title: "No test covers add",
      explanation: "Add a test.",
      suggested_fix: "",
      status: "open",
    },
  ],
  format_note: "",
  error: "",
  model_name: "Qwen",
  reviewer_changed: [],
  usage: { total_tokens: 800, cost_usd: 0 },
  ...over,
});

beforeEach(() => {
  localStorage.clear();
  mocks.review.mockResolvedValue({
    task_id: "t1",
    session_id: "s1",
    workspace: "/w",
    busy: false,
    files: [
      {
        path: "src/app.ts",
        status: "modified",
        source: "checkpoint",
        added: 2,
        removed: 2,
        binary: false,
      },
    ],
  });
  mocks.reviewFile.mockResolvedValue({
    path: "src/app.ts",
    status: "modified",
    source: "checkpoint",
    added: 2,
    removed: 2,
    binary: false,
    hash: "h",
    hunks: [hunk(1, "a - b", "a + b"), hunk(12, "alpha beta", "gamma delta")],
  });
  opinions.options.mockResolvedValue({
    workspace: "/w",
    prefs: { model: null, before_commit: false },
    offline: false,
    writer: { model: "cli:claude", label: "Claude Code", local: false },
    local_only: false,
  });
  opinions.current.mockResolvedValue({
    hash: "task-1",
    files: ["src/app.ts"],
    omitted: [],
    truncated: false,
  });
});
afterEach(() => {
  cleanup();
  vi.resetAllMocks();
});

function view() {
  const toast = vi.fn();
  render(
    <ReviewView
      taskId="t1"
      busy={false}
      onClose={vi.fn()}
      toast={toast}
      refresh={async () => {}}
      onAskAgent={vi.fn()}
      memory={{} as never}
      onMemory={vi.fn()}
      targets={targets}
    />,
  );
  return toast;
}

it("reviews a task with another model than the one that wrote it", async () => {
  opinions.list.mockResolvedValue({ workspace: "/w", second_opinions: [] });
  opinions.start.mockResolvedValue({
    second_opinion: record({ status: "running", findings: [], summary: "" }),
  });
  view();
  const panel = await screen.findByRole("region", { name: "Second opinion" });
  const select = within(panel).getByRole("combobox", {
    name: "Reviewer model",
  }) as HTMLSelectElement;
  await waitFor(() => expect(select.value).toBe("local:gguf:qwen"));
  fireEvent.click(
    within(panel).getByRole("button", { name: "Review with another model" }),
  );
  await waitFor(() =>
    expect(opinions.start).toHaveBeenCalledWith({
      kind: "review",
      source: "task",
      task_id: "t1",
      model: "local:gguf:qwen",
    }),
  );
  expect(await within(panel).findByText("Reviewing…")).toBeTruthy();
});

it("shows findings next to their hunks and queues a fix", async () => {
  opinions.list.mockResolvedValue({
    workspace: "/w",
    second_opinions: [record()],
  });
  opinions.fix.mockResolvedValue({
    value: {
      second_opinion: record(),
      job: { id: "j2", session_id: "s1", status: "queued" },
    },
  });
  const toast = view();
  await screen.findByText("gamma delta");
  // The file list counts the file's open findings.
  expect(
    screen.getByLabelText("1 open finding", { selector: ".opinion-count" }),
  ).toBeTruthy();
  const finding = screen.getByRole("article", {
    name: "Medium: V should stay 1",
  });
  // It sits after the second hunk, not the first.
  const second = screen.getByText("gamma delta");
  const first = screen.getByText("a + b");
  expect(
    second.compareDocumentPosition(finding) & Node.DOCUMENT_POSITION_FOLLOWING,
  ).toBeTruthy();
  expect(
    first.compareDocumentPosition(finding) & Node.DOCUMENT_POSITION_FOLLOWING,
  ).toBeTruthy();
  // A finding about no file shows with the review's summary.
  const panel = screen.getByRole("region", { name: "Second opinion" });
  expect(within(panel).getByText("No test covers add")).toBeTruthy();
  expect(within(panel).getByText("Mostly fine.")).toBeTruthy();
  fireEvent.click(
    within(finding).getByRole("button", { name: "Ask the agent to fix this" }),
  );
  await waitFor(() =>
    expect(toast).toHaveBeenCalledWith(
      "Fix queued in this conversation.",
      "ok",
    ),
  );
  expect(opinions.fix).toHaveBeenCalledWith("o1", "f1", false);
});

it("says when the task's changes moved on since the review", async () => {
  opinions.list.mockResolvedValue({
    workspace: "/w",
    second_opinions: [record()],
  });
  opinions.current.mockResolvedValue({
    hash: "task-2",
    files: ["src/app.ts"],
    omitted: [],
    truncated: false,
  });
  view();
  expect(await screen.findByText(/The changes are different now/)).toBeTruthy();
});
