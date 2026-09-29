import { afterEach, expect, it, vi } from "vitest";
import type { PickerTarget } from "./picker";
import {
  continuePrompt,
  findingsForHunk,
  newRange,
  opinionApi,
  opinionStatus,
  reviewerChoices,
  suggestReviewer,
  unplaced,
  usageText,
  type Finding,
  type SecondOpinion,
} from "./secondOpinion";

const transport = vi.hoisted(() => ({ request: vi.fn() }));
vi.mock("./transport", async (original) => {
  const real = await original<typeof import("./transport")>();
  return { ...real, request: transport.request };
});

afterEach(() => vi.resetAllMocks());

const row = (
  id: string,
  provider: string,
  over: Partial<PickerTarget> = {},
): PickerTarget => ({
  id,
  provider,
  group: provider === "llamacpp" ? "local" : "subscriptions",
  name: id,
  inference: provider === "llamacpp" ? "local" : "cloud",
  availability: "ready",
  ...over,
});
const targets = [
  row("cli:claude", "cli:claude"),
  row("cli:claude:opus", "cli:claude"),
  row("cli:codex", "cli:codex"),
  row("cli:cursor", "cli:cursor", { availability: "sign_in" }),
  row("api:openrouter:x", "openrouter", { group: "api" }),
  row("api:openrouter:y", "openrouter", { group: "api", featured: true }),
  row("local:gguf:qwen", "llamacpp"),
];

it("offers ready rows, featured API-key rows, and only local ones offline", () => {
  const ids = (list: PickerTarget[]) => list.map((t) => t.id);
  expect(ids(reviewerChoices(targets, { offline: false }))).toEqual([
    "cli:claude",
    "cli:claude:opus",
    "cli:codex",
    "api:openrouter:y",
    "local:gguf:qwen",
  ]);
  expect(
    ids(reviewerChoices(targets, { offline: false, keep: "api:openrouter:x" })),
  ).toContain("api:openrouter:x");
  expect(ids(reviewerChoices(targets, { offline: true }))).toEqual([
    "local:gguf:qwen",
  ]);
});

it("suggests another model than the writer, remembering the project's choice", () => {
  const base = { offline: false, localOnly: false };
  // Another provider first, not just another Claude model.
  expect(suggestReviewer(targets, { ...base, writer: "cli:claude" })).toBe(
    "cli:codex",
  );
  // The project's last choice, unless it wrote the change.
  expect(
    suggestReviewer(targets, {
      ...base,
      writer: "cli:claude",
      remembered: "local:gguf:qwen",
    }),
  ).toBe("local:gguf:qwen");
  expect(
    suggestReviewer(targets, {
      ...base,
      writer: "cli:codex",
      remembered: "cli:codex",
    }),
  ).toBe("cli:claude");
  // A remembered model that is no longer ready is skipped.
  expect(
    suggestReviewer(targets, {
      ...base,
      writer: "cli:claude",
      remembered: "cli:cursor",
    }),
  ).toBe("cli:codex");
});

it("preselects only local models for local work and offline mode", () => {
  expect(
    suggestReviewer(targets, {
      writer: "local:gguf:other",
      remembered: "cli:codex",
      offline: false,
      localOnly: true,
    }),
  ).toBe("local:gguf:qwen");
  // No other local model: nothing is preselected; a cloud model stays an
  // explicit choice.
  expect(
    suggestReviewer(targets, {
      writer: "local:gguf:qwen",
      offline: false,
      localOnly: true,
    }),
  ).toBe("");
  expect(
    suggestReviewer(targets, {
      writer: "cli:codex",
      offline: true,
      localOnly: false,
    }),
  ).toBe("local:gguf:qwen");
});

const finding = (over: Partial<Finding>): Finding => ({
  id: "f1",
  file: "src/a.ts",
  severity: "high",
  title: "t",
  explanation: "e",
  suggested_fix: "",
  status: "open",
  ...over,
});

it("places findings next to the hunk whose new lines hold them", () => {
  expect(newRange("@@ -1,3 +10,4 @@")).toEqual({ start: 10, end: 13 });
  expect(newRange("@@ -1 +7 @@ fn x")).toEqual({ start: 7, end: 7 });
  expect(newRange("@@ -3,2 +2,0 @@")).toEqual({ start: 2, end: 2 });
  const list = [
    finding({ id: "f1", line: 11 }),
    finding({ id: "f2", line: 40, hunk: "@@ -1,3 +10,4 @@" }),
    finding({ id: "f3", line: 99 }),
    finding({ id: "f4", line: null }),
    finding({ id: "f5", file: "src/b.ts", line: 11 }),
  ];
  const here = findingsForHunk(list, "src/a.ts", "@@ -1,3 +10,4 @@");
  expect(here.map((f) => f.id)).toEqual(["f1", "f2"]);
  expect(
    unplaced(list, "src/a.ts", ["@@ -1,3 +10,4 @@"]).map((f) => f.id),
  ).toEqual(["f3", "f4"]);
});

const opinion = (over: Partial<SecondOpinion>): SecondOpinion => ({
  id: "o1",
  kind: "review",
  workspace: "/w",
  source: "staged",
  question: "",
  reviewer: { model: "cli:codex", label: "Codex", local: false },
  same_model: false,
  consented: false,
  job_id: "j",
  review_session: "s",
  review_task: "t",
  status: "completed",
  created_at: 1,
  diff_hash: "h",
  files: [],
  omitted: [],
  diff: [],
  truncated: false,
  context_chars: 10,
  summary: "",
  findings: [],
  format_note: "",
  error: "",
  model_name: "Codex",
  reviewer_changed: [],
  ...over,
});

it("words status, usage and cost plainly", () => {
  expect(opinionStatus(opinion({ status: "queued" }))).toBe(
    "Waiting for the running task to finish",
  );
  expect(opinionStatus(opinion({ status: "running" }))).toBe("Reviewing…");
  expect(opinionStatus(opinion({}))).toBe("No problems found");
  expect(opinionStatus(opinion({ format_note: "odd" }))).toBe(
    "No findings read",
  );
  expect(
    opinionStatus(opinion({ findings: [finding({}), finding({ id: "f2" })] })),
  ).toBe("2 findings");
  expect(opinionStatus(opinion({ kind: "ask" }))).toBe("Done");
  expect(opinionStatus(opinion({ status: "failed" }))).toBe("Failed");
  expect(
    usageText(
      opinion({ usage: { total_tokens: 1234, cost_usd: 0.021 } as never }),
    ),
  ).toBe("1,234 tokens · $0.0210");
  expect(usageText(opinion({ usage: { total_tokens: 5 } as never }))).toBe(
    "5 tokens · cost not reported (plan allowance)",
  );
  expect(
    usageText(
      opinion({
        reviewer: { model: "local:gguf:q", label: "Qwen", local: true },
        usage: { total_tokens: 9, cost_usd: 0 } as never,
      }),
    ),
  ).toBe("9 tokens · no cost (this computer)");
});

it("drafts the continuation with the second opinion quoted", () => {
  const text = continuePrompt(opinion({ summary: "It is wrong.\nUse a map." }));
  expect(text).toContain("> It is wrong.\n> Use a map.");
  expect(text).toMatch(/^Here is your second opinion/);
});

it("turns a 409 consent answer into a consent request, not an error", async () => {
  const body = {
    ok: false,
    status: 409,
    needs_consent: true,
    handoff: { to: "Codex", purpose: "second_opinion", excerpt_chars: 300 },
  };
  transport.request.mockRejectedValueOnce(
    Object.assign(new (await import("./transport")).ApiError("409", body)),
  );
  const refused = await opinionApi.start({
    kind: "review",
    source: "staged",
    model: "cli:codex",
  });
  expect("consent" in refused && refused.consent.handoff.to).toBe("Codex");
  transport.request.mockResolvedValueOnce(body);
  const again = await opinionApi.start({
    kind: "review",
    source: "staged",
    model: "cli:codex",
  });
  expect("consent" in again).toBe(true);
  transport.request.mockResolvedValueOnce(opinion({}));
  const started = await opinionApi.start({
    kind: "review",
    source: "staged",
    model: "cli:codex",
    consent: true,
  });
  expect("second_opinion" in started && started.second_opinion.id).toBe("o1");
  expect(transport.request).toHaveBeenLastCalledWith(
    "/api/second-opinions",
    "POST",
    { kind: "review", source: "staged", model: "cli:codex", consent: true },
  );
});
