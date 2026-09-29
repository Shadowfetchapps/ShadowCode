import { afterEach, expect, it, vi } from "vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  within,
} from "@testing-library/react";
import {
  AskAnotherModel,
  FindingItem,
  OpinionCard,
  ReviewedChanges,
  ReviewerSelect,
  type FindingActions,
} from "./SecondOpinion";
import type { PickerTarget } from "../lib/picker";
import type { Finding, SecondOpinion } from "../lib/secondOpinion";

vi.mock("./Markdown", () => ({
  Markdown: ({ children }: { children: string }) => <p>{children}</p>,
}));

afterEach(cleanup);

const targets: PickerTarget[] = [
  {
    id: "cli:codex",
    provider: "cli:codex",
    group: "subscriptions",
    name: "Codex · GPT",
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

const finding = (over: Partial<Finding> = {}): Finding => ({
  id: "f1",
  file: "src/app.ts",
  line: 2,
  severity: "high",
  title: "Subtracts instead of adding",
  explanation: "add returns a - b.",
  suggested_fix: "Return a + b.",
  status: "open",
  ...over,
});

const opinion = (over: Partial<SecondOpinion> = {}): SecondOpinion => ({
  id: "o1",
  kind: "review",
  workspace: "/w",
  source: "staged",
  question: "",
  reviewer: { model: "cli:codex", label: "Codex", local: false },
  writer: { model: "local:gguf:qwen", label: "Qwen", local: true },
  same_model: false,
  consented: true,
  job_id: "j1",
  review_session: "rs",
  review_task: "rt",
  status: "completed",
  created_at: 1,
  diff_hash: "h",
  files: ["src/app.ts"],
  omitted: [],
  diff: [
    {
      path: "src/app.ts",
      status: "modified",
      binary: false,
      diff: "@@ -1,3 +1,3 @@\n export const x = 1;\n-export const add = (a, b) => a + b;\n+export const add = (a, b) => a - b;\n",
    },
  ],
  truncated: false,
  context_chars: 400,
  summary: "One bug.",
  findings: [finding()],
  format_note: "",
  error: "",
  usage: { total_tokens: 2500, cost_usd: 0.012 },
  model_name: "Codex",
  reviewer_changed: [],
  ...over,
});

const actions = () => ({
  fix: vi.fn<FindingActions["fix"]>(),
  setFinding: vi.fn<FindingActions["setFinding"]>(),
});

it("a finding says where, how bad and what to change, with its actions", () => {
  const onFix = vi.fn();
  const onDismiss = vi.fn();
  render(
    <FindingItem finding={finding()} onFix={onFix} onDismiss={onDismiss} />,
  );
  const item = screen.getByRole("article", {
    name: "High: Subtracts instead of adding",
  });
  expect(item.textContent).toContain("src/app.ts, line 2");
  expect(item.textContent).toContain("add returns a - b.");
  expect(item.textContent).toContain("Return a + b.");
  fireEvent.click(
    within(item).getByRole("button", { name: "Ask the agent to fix this" }),
  );
  fireEvent.click(within(item).getByRole("button", { name: "Dismiss" }));
  expect(onFix).toHaveBeenCalledOnce();
  expect(onDismiss).toHaveBeenCalledOnce();
});

it("a dismissed finding folds away and can come back; a queued fix says so", () => {
  const onRestore = vi.fn();
  const { rerender } = render(
    <FindingItem
      finding={finding({ status: "dismissed" })}
      onRestore={onRestore}
    />,
  );
  expect(screen.queryByText("add returns a - b.")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Show again" }));
  expect(onRestore).toHaveBeenCalledOnce();
  rerender(<FindingItem finding={finding({ status: "fixing" })} />);
  expect(screen.getByRole("status").textContent).toContain(
    "Fix queued in the conversation",
  );
  expect(
    screen.queryByRole("button", { name: "Ask the agent to fix this" }),
  ).toBeNull();
});

it("the reviewer picker groups models and warns about the writer and offline mode", () => {
  const onChange = vi.fn();
  const { rerender } = render(
    <ReviewerSelect
      targets={targets}
      value="local:gguf:qwen"
      onChange={onChange}
      offline={false}
      writer={{ model: "local:gguf:qwen", label: "Qwen", local: true }}
    />,
  );
  const select = screen.getByRole("combobox", { name: "Reviewer model" });
  expect(
    within(select).getByRole("option", {
      name: "Qwen · This computer (wrote this)",
    }),
  ).toBeTruthy();
  expect(screen.getByText(/model that wrote the change/)).toBeTruthy();
  fireEvent.change(select, { target: { value: "cli:codex" } });
  expect(onChange).toHaveBeenCalledWith("cli:codex");
  rerender(
    <ReviewerSelect
      targets={targets}
      value="cli:codex"
      onChange={onChange}
      offline={false}
      writer={{ model: "local:gguf:qwen", label: "Qwen", local: true }}
    />,
  );
  expect(
    screen.getByText(/Reviewing on a cloud model asks first/),
  ).toBeTruthy();
  rerender(
    <ReviewerSelect
      targets={targets}
      value=""
      onChange={onChange}
      offline={true}
    />,
  );
  expect(
    within(select).queryByRole("option", { name: "Codex · GPT" }),
  ).toBeNull();
  expect(
    screen.getByText(/Offline: only models on this computer/),
  ).toBeTruthy();
});

it("the Git tab shows each finding under the hunk it points at", () => {
  const acts = actions();
  render(
    <ReviewedChanges
      opinion={opinion({
        findings: [
          finding(),
          finding({
            id: "f2",
            file: "",
            line: null,
            severity: "low",
            title: "No tests",
          }),
        ],
      })}
      actions={acts}
    />,
  );
  const file = screen.getByRole("region", { name: "Findings in src/app.ts" });
  expect(file.querySelector(".diff-hunk")?.textContent).toContain("a - b");
  expect(within(file).getByText("Subtracts instead of adding")).toBeTruthy();
  const general = screen.getByRole("region", { name: "General findings" });
  expect(within(general).getByText("No tests")).toBeTruthy();
  fireEvent.click(
    within(file).getByRole("button", { name: "Ask the agent to fix this" }),
  );
  expect(acts.fix).toHaveBeenCalledWith(
    expect.objectContaining({ id: "o1" }),
    expect.objectContaining({ id: "f1" }),
  );
  fireEvent.click(within(general).getByRole("button", { name: "Dismiss" }));
  expect(acts.setFinding).toHaveBeenCalledWith(
    expect.objectContaining({ id: "o1" }),
    expect.objectContaining({ id: "f2" }),
    "dismissed",
  );
});

it("the transcript card is labelled, shows model and cost, and can continue", () => {
  const onContinue = vi.fn();
  const onCancel = vi.fn();
  const { rerender } = render(
    <OpinionCard
      opinion={opinion({ kind: "ask", status: "running", findings: [] })}
      actions={actions()}
      onCancel={onCancel}
      onContinue={onContinue}
    />,
  );
  const card = screen.getByRole("region", {
    name: "Second opinion from Codex",
  });
  expect(card.textContent).toContain("Thinking…");
  fireEvent.click(within(card).getByRole("button", { name: "Stop" }));
  expect(onCancel).toHaveBeenCalledOnce();
  expect(within(card).queryByRole("button", { name: /Continue/ })).toBeNull();
  rerender(
    <OpinionCard
      opinion={opinion({
        kind: "ask",
        summary: "The answer misses the empty case.",
        findings: [],
      })}
      actions={actions()}
      onCancel={onCancel}
      onContinue={onContinue}
    />,
  );
  expect(card.textContent).toContain("The answer misses the empty case.");
  expect(card.textContent).toContain("2,500 tokens · $0.0120");
  expect(card.textContent).toContain("read-only");
  fireEvent.click(
    within(card).getByRole("button", { name: "Continue with Codex" }),
  );
  expect(onContinue).toHaveBeenCalledOnce();
});

it("a failed or oddly formed review explains itself", () => {
  render(
    <OpinionCard
      opinion={opinion({
        status: "completed",
        findings: [],
        summary: "Hmm.",
        format_note:
          "The reviewer did not list findings in the requested form.",
        omitted: [".env"],
        reviewer_changed: ["src/app.ts"],
        truncated: true,
        same_model: true,
      })}
      actions={actions()}
      onCancel={vi.fn()}
    />,
  );
  const card = screen.getByRole("region", { name: "Review by Codex" });
  for (const text of [
    "No findings read",
    "requested form",
    "Not sent: .env",
    "asked only to read",
    "Only part of the changes fit",
    "less independent",
  ])
    expect(card.textContent).toContain(text);
});

it("asking another model sends the chosen model and question", () => {
  const onAsk = vi.fn();
  const onClose = vi.fn();
  render(
    <AskAnotherModel
      targets={targets}
      initial="cli:codex"
      offline={false}
      onAsk={onAsk}
      onClose={onClose}
    />,
  );
  const form = screen.getByRole("form", { name: "Ask another model" });
  fireEvent.change(within(form).getByRole("textbox"), {
    target: { value: "Is the cache safe?" },
  });
  fireEvent.click(within(form).getByRole("button", { name: "Ask" }));
  expect(onAsk).toHaveBeenCalledWith("cli:codex", "Is the cache safe?");
  fireEvent.keyDown(form, { key: "Escape" });
  expect(onClose).toHaveBeenCalledOnce();
});
