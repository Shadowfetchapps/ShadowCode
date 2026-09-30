import { afterEach, expect, it, vi } from "vitest";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
} from "@testing-library/react";
import { createRef } from "react";
import {
  ROW_WINDOW,
  TranscriptRows,
  transcriptRows,
  type RowActions,
} from "./TranscriptRows";
import { Elapsed } from "../Elapsed";
import type { ChatItem } from "../cards";
import { emptyActivity } from "../../lib/activity";

const markdown = vi.hoisted(() => ({ renders: {} as Record<string, number> }));
vi.mock("../Markdown", () => ({
  Markdown: ({ children }: { children: string }) => {
    markdown.renders[children] = (markdown.renders[children] || 0) + 1;
    return <p>{children}</p>;
  },
}));

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

it("an event re-renders only the rows it changed", () => {
  markdown.renders = {};
  const first: ChatItem = { kind: "agent", text: "First answer", key: "m:a" };
  const second: ChatItem = { kind: "agent", text: "Second", key: "m:b" };
  const { rerender } = render(rows([first, second]));
  rerender(rows([first, second, ...notes(1)]));
  rerender(rows([first, { ...second, text: "Second answer" }, ...notes(1)]));
  expect(markdown.renders["First answer"]).toBe(1);
  expect(markdown.renders["Second"]).toBe(1);
  expect(markdown.renders["Second answer"]).toBe(1);
});

const actions: RowActions = {
  onToggleTool: vi.fn(),
  diffStats: vi.fn(async () => ({})),
  onReview: vi.fn(),
  onRewind: vi.fn(),
  onContinue: vi.fn(),
  onChooseModel: vi.fn(),
  onTryOn: vi.fn(),
  onSpendDecision: vi.fn(async () => {}),
  onScheduleResume: vi.fn(),
  onCancelResume: vi.fn(),
  onResumeNow: vi.fn(),
  onOpenLocal: vi.fn(),
  onFork: vi.fn(),
  onEditResend: vi.fn(),
  onRetry: vi.fn(),
  onCopy: vi.fn(),
};
const notes = (count: number): ChatItem[] =>
  Array.from({ length: count }, (_, i) => ({
    kind: "note",
    text: `Note ${i + 1}`,
    key: `e:${i + 1}:0`,
  }));

function rows(items: ChatItem[], resetKey = "s1:0", forkDisabled = false) {
  const scrollRef = createRef<HTMLDivElement>();
  return (
    <div ref={scrollRef}>
      <TranscriptRows
        items={items}
        activity={{}}
        activeTaskId=""
        queuedTaskIds={new Set()}
        fallback={null}
        locked={false}
        forkDisabled={forkDisabled}
        actions={actions}
        scrollRef={scrollRef}
        resetKey={resetKey}
      />
    </div>
  );
}

it("renders the latest rows of a long transcript and reveals earlier ones", () => {
  const items = notes(ROW_WINDOW + 40);
  const { rerender } = render(rows(items));
  expect(screen.queryByText("Note 40")).toBeNull();
  expect(screen.getByText("Note 41")).toBeTruthy();
  expect(screen.getByText(`Note ${ROW_WINDOW + 40}`)).toBeTruthy();
  fireEvent.click(
    screen.getByRole("button", { name: "Show 40 earlier items" }),
  );
  expect(screen.getByText("Note 1")).toBeTruthy();
  expect(screen.queryByRole("button", { name: /earlier items/ })).toBeNull();
  // Another conversation starts from its latest rows again.
  rerender(rows(items, "s2:0"));
  expect(screen.queryByText("Note 1")).toBeNull();
});

it("a failed task's Try again waits while a request is being sent", () => {
  const failed: ChatItem = {
    kind: "agent",
    who: "Needs attention",
    text: "Model provider returned HTTP 529; provider overloaded",
    request: "Fix the add function",
    key: "m:f",
  };
  const { rerender } = render(rows([failed], "s1:0", true));
  const retry = () =>
    screen.getByRole("button", { name: "Try again" }) as HTMLButtonElement;
  expect(retry().disabled).toBe(true);
  rerender(rows([failed], "s1:0", false));
  fireEvent.click(retry());
  expect(actions.onRetry).toHaveBeenCalledWith("Fix the add function");
});

it("short transcripts render every row without paging", () => {
  render(rows(notes(12)));
  expect(screen.getByText("Note 1")).toBeTruthy();
  expect(screen.queryByRole("button", { name: /earlier items/ })).toBeNull();
});

it("leaves out queued prompts and plain tool calls, keeps stranded work", () => {
  const stranded = {
    ...emptyActivity("t2"),
    calls: [
      {
        callId: "c1",
        tool: "read_file",
        label: "Read a.ts",
        path: "a.ts",
      },
    ],
  } as unknown as ReturnType<typeof emptyActivity>;
  const items: ChatItem[] = [
    { kind: "user", text: "Queued follow-up", taskId: "t3", key: "a" },
    { kind: "user", text: "Stopped task", taskId: "t2", key: "b" },
    { kind: "tool", tool: "read_file", text: "", taskId: "t2", key: "c" },
  ];
  const result = transcriptRows(
    items,
    { t2: stranded },
    "",
    undefined,
    new Set(["t3"]),
  );
  expect(result.map((row) => row.key)).toEqual(["b", "c"]);
  expect(result[1].stranded).toBe(true);
  // The live task's prompt shows even while the job list says queued.
  expect(transcriptRows(items, {}, "t3", "t3", new Set(["t3"]))[0].queued).toBe(
    false,
  );
});

it("the elapsed timer ticks on its own", () => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date(100_000));
  render(<Elapsed since={95} />);
  expect(screen.getByText("5s")).toBeTruthy();
  act(() => {
    vi.advanceTimersByTime(2000);
  });
  expect(screen.getByText("7s")).toBeTruthy();
});
