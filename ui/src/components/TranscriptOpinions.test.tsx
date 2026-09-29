import { afterEach, expect, it, vi } from "vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { createRef } from "react";
import { TranscriptRows, type RowActions } from "./shell/TranscriptRows";
import {
  OpinionContext,
  type ConversationOpinions,
} from "./TranscriptOpinions";
import type { ChatItem } from "./cards";
import type { PickerTarget } from "../lib/picker";
import { opinionApi, type SecondOpinion } from "../lib/secondOpinion";
import type { SecondOpinions } from "../hooks/useSecondOpinions";

vi.mock("./Markdown", () => ({
  Markdown: ({ children }: { children: string }) => <p>{children}</p>,
}));
vi.mock("../lib/secondOpinion", async (original) => {
  const real = await original<typeof import("../lib/secondOpinion")>();
  return {
    ...real,
    opinionApi: { ...real.opinionApi, options: vi.fn() },
  };
});

afterEach(() => {
  cleanup();
  vi.resetAllMocks();
});

const actions: RowActions = {
  onToggleTool: vi.fn(),
  diffStats: vi.fn(async () => ({})),
  onReview: vi.fn(),
  onRewind: vi.fn(),
  onContinue: vi.fn(),
  onChooseModel: vi.fn(),
  onOpenLocal: vi.fn(),
  onFork: vi.fn(),
  onEditResend: vi.fn(),
  onRetry: vi.fn(),
  onCopy: vi.fn(),
};
const items: ChatItem[] = [
  { kind: "user", text: "Fix add", taskId: "t1", key: "u1" },
  { kind: "agent", text: "Fixed add.", taskId: "t1", key: "a1", eventId: 3 },
  { kind: "user", text: "Now subtract", taskId: "t2", key: "u2" },
  { kind: "agent", text: "Added subtract.", taskId: "t2", key: "a2" },
];
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
    id: "cli:codex",
    provider: "cli:codex",
    group: "subscriptions",
    name: "Codex · GPT",
    inference: "cloud",
    availability: "ready",
  },
];
const opinion: SecondOpinion = {
  id: "o1",
  kind: "ask",
  workspace: "/w",
  source: "task",
  task_id: "t1",
  session_id: "s1",
  question: "Is it right?",
  reviewer: { model: "cli:codex", label: "Codex", local: false },
  writer: { model: "cli:claude", label: "Claude Code", local: false },
  same_model: false,
  consented: false,
  job_id: "j",
  review_session: "rs",
  review_task: "rt",
  status: "completed",
  created_at: 5,
  diff_hash: "h",
  files: [],
  omitted: [],
  diff: [],
  truncated: false,
  context_chars: 100,
  summary: "Yes, but test the overflow.",
  findings: [],
  format_note: "",
  error: "",
  model_name: "Codex",
  reviewer_changed: [],
};

function context(
  over: Partial<ConversationOpinions> = {},
): ConversationOpinions {
  return {
    targets,
    opinions: {
      items: [opinion],
      cancel: vi.fn(),
    } as unknown as SecondOpinions,
    ask: vi.fn(async () => undefined),
    actions: { fix: vi.fn(), setFinding: vi.fn() },
    onContinue: vi.fn(),
    onShowInReview: vi.fn(),
    onOpenWork: vi.fn(),
    ...over,
  };
}

function view(value: ConversationOpinions | null) {
  const scrollRef = createRef<HTMLDivElement>();
  const rows = (
    <div ref={scrollRef}>
      <TranscriptRows
        items={items}
        activity={{}}
        activeTaskId=""
        queuedTaskIds={new Set()}
        fallback={null}
        locked={false}
        forkDisabled={false}
        actions={actions}
        scrollRef={scrollRef}
        resetKey="s1:0"
      />
    </div>
  );
  return render(
    value ? (
      <OpinionContext.Provider value={value}>{rows}</OpinionContext.Provider>
    ) : (
      rows
    ),
  );
}

it("without a conversation there is no Ask another model", () => {
  view(null);
  expect(
    screen.queryByRole("button", { name: "Ask another model" }),
  ).toBeNull();
  expect(screen.getAllByRole("button", { name: "Copy answer" })).toHaveLength(
    2,
  );
});

it("a second opinion card follows the task it is about", () => {
  const value = context();
  view(value);
  const card = screen.getByRole("region", {
    name: "Second opinion from Codex",
  });
  expect(card.textContent).toContain("Yes, but test the overflow.");
  expect(card.textContent).toContain("Is it right?");
  // After task 1's answer, before task 2's request.
  const first = screen.getByText("Fixed add.");
  const next = screen.getByText("Now subtract");
  expect(
    first.compareDocumentPosition(card) & Node.DOCUMENT_POSITION_FOLLOWING,
  ).toBeTruthy();
  expect(
    card.compareDocumentPosition(next) & Node.DOCUMENT_POSITION_FOLLOWING,
  ).toBeTruthy();
  fireEvent.click(
    within(card).getByRole("button", { name: "Continue with Codex" }),
  );
  expect(value.onContinue).toHaveBeenCalledWith(opinion);
  fireEvent.click(
    within(card).getByRole("button", { name: "What the reviewer read" }),
  );
  expect(value.onOpenWork).toHaveBeenCalledWith(opinion);
});

it("asks another model about an answer, suggesting one that did not write it", async () => {
  vi.mocked(opinionApi.options).mockResolvedValue({
    workspace: "/w",
    prefs: { model: null, before_commit: false },
    offline: false,
    writer: { model: "cli:claude", label: "Claude Code", local: false },
    local_only: false,
  });
  const value = context({
    opinions: { items: [], cancel: vi.fn() } as unknown as SecondOpinions,
  });
  view(value);
  const [ask] = screen.getAllByRole("button", { name: "Ask another model" });
  fireEvent.click(ask);
  const form = await screen.findByRole("form", { name: "Ask another model" });
  expect(opinionApi.options).toHaveBeenCalledWith({ task_id: "t1" });
  const select = within(form).getByRole("combobox") as HTMLSelectElement;
  await waitFor(() => expect(select.value).toBe("cli:codex"));
  fireEvent.change(within(form).getByRole("textbox"), {
    target: { value: "Any edge cases?" },
  });
  fireEvent.click(within(form).getByRole("button", { name: "Ask" }));
  expect(value.ask).toHaveBeenCalledWith("t1", "cli:codex", "Any edge cases?");
  await waitFor(() =>
    expect(
      screen.queryByRole("form", { name: "Ask another model" }),
    ).toBeNull(),
  );
});
