import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { CompareLane, CompareRecord } from "../api";

const mocks = vi.hoisted(() => ({
  compare: vi.fn(),
  compares: vi.fn(),
  compareScoreboard: vi.fn(),
  approvals: vi.fn(),
  keepCompare: vi.fn(),
  discardCompare: vi.fn(),
  cancelCompare: vi.fn(),
}));
vi.mock("../api", () => ({ api: mocks }));

import { CompareView, Scoreboard } from "./CompareView";

const lane = (over: Partial<CompareLane> = {}): CompareLane => ({
  model: "cli:codex:astra",
  name: "Codex · GPT-6-Astra",
  session_id: "s2",
  job_id: "j1",
  worktree: "/wt/1",
  worktree_id: "w1",
  branch: "shadowcode/w1",
  base_commit: "b",
  status: "running",
  summary: "",
  changed_files: [],
  changed_files_truncated: false,
  checks: { passed: 0, failed: 0, commands: [] },
  duration_s: 4,
  usage: {},
  error: null,
  removed: false,
  ...over,
});
const files = [
  {
    path: "src/app.test.ts",
    status: "added",
    additions: 12,
    deletions: 0,
    binary: false,
  },
  {
    path: "src/app.ts",
    status: "modified",
    additions: 4,
    deletions: 1,
    binary: false,
  },
];
const finishedCloud = lane({
  status: "completed",
  changed_files: files,
  checks: {
    passed: 1,
    failed: 1,
    commands: [
      { command: "npm test", exit_code: 0, success: true },
      { command: "npm run lint", exit_code: 1, success: false },
    ],
  },
  usage: { total_tokens: 21400 },
  summary: "Fixed add",
});
const localLane = (over: Partial<CompareLane> = {}) =>
  lane({
    model: "local:gguf:qwen",
    name: "qwen3:14b",
    session_id: "s3",
    job_id: "j2",
    worktree: "/wt/2",
    ...over,
  });
const record = (over: Partial<CompareRecord> = {}): CompareRecord => ({
  id: "c1",
  workspace: "/work/demo",
  task: "Fix the add function",
  mode: "code",
  web: false,
  created_at: Date.now() / 1000 - 30,
  finished_at: null,
  state: "running",
  base: { commit: "b", head: "h", included_uncommitted: true },
  lanes: [lane(), localLane()],
  winner: null,
  applied_files: [],
  notes: [],
  ...over,
});

beforeEach(() => {
  for (const fn of Object.values(mocks)) fn.mockReset();
  mocks.compares.mockResolvedValue({ compares: [record()] });
  mocks.compareScoreboard.mockResolvedValue({ workspace: "/w", rows: [] });
  mocks.approvals.mockResolvedValue({ approvals: [] });
});
afterEach(() => cleanup());

function view(over: Partial<Parameters<typeof CompareView>[0]> = {}) {
  const props = {
    workspace: "/work/demo",
    compareId: "c1",
    targets: [],
    pollMs: 20,
    onSelect: vi.fn(),
    onClose: vi.fn(),
    onOpenLane: vi.fn(),
    onOpenDiff: vi.fn(),
    onOpenChanges: vi.fn(),
    onApplied: vi.fn(),
    ...over,
  };
  render(<CompareView {...props} />);
  return props;
}
const article = (name: string) =>
  screen.getByRole("article", { name: new RegExp(`^${name}$`) });

describe("CompareView", () => {
  it("reports retained copies and retries cleanup without keeping again", async () => {
    const pending = record({
      state: "applied",
      winner: finishedCloud.model,
      lanes: [finishedCloud],
      cleanup_pending: true,
    });
    mocks.compare.mockResolvedValue(pending);
    mocks.discardCompare.mockResolvedValue({
      ...pending,
      cleanup_pending: false,
      lanes: [{ ...finishedCloud, removed: true }],
    });
    view({ pollMs: 60000 });
    const retry = await screen.findByRole("button", { name: "Retry cleanup" });
    expect(screen.getByText(/Some copies are retained/)).toBeTruthy();
    fireEvent.click(retry);
    await waitFor(() =>
      expect(mocks.discardCompare).toHaveBeenCalledWith("c1"),
    );
    await waitFor(() =>
      expect(
        screen.queryByRole("button", { name: "Retry cleanup" }),
      ).toBeNull(),
    );
    expect(mocks.keepCompare).not.toHaveBeenCalled();
  });

  it("polls running lanes until they finish and shows their results", async () => {
    let finished = false;
    mocks.compare.mockImplementation(async () =>
      finished
        ? record({
            state: "done",
            lanes: [finishedCloud, localLane({ status: "completed" })],
          })
        : record(),
    );
    const props = view();
    const cloud = await screen.findByRole("article", {
      name: "Codex · GPT-6-Astra",
    });
    expect(cloud.textContent).toContain("Working…");
    expect(cloud.textContent).toContain("No changes yet");
    expect(
      within(cloud).getByRole("button", { name: "Keep this one" }),
    ).toHaveProperty("disabled", true);
    expect(screen.getByText("Includes your uncommitted work")).toBeTruthy();
    expect(screen.getByRole("button", { name: /Stop/ })).toBeTruthy();
    finished = true;
    await waitFor(() =>
      expect(article("Codex · GPT-6-Astra").textContent).toContain("Finished"),
    );
    const done = article("Codex · GPT-6-Astra");
    expect(done.textContent).toContain("src/app.test.ts");
    expect(done.textContent).toContain("+12");
    expect(done.textContent).toContain("1 passed · 1 failed");
    expect(done.textContent).toContain("npm run lint failed (exit 1)");
    expect(done.textContent).toContain("21.4k tokens");
    expect(done.querySelector(".inference-badge")?.textContent).toBe("Cloud");
    expect(
      article("qwen3:14b").querySelector(".inference-badge")?.textContent,
    ).toBe("Local");
    expect(screen.queryByRole("button", { name: /Stop/ })).toBeNull();
    fireEvent.click(within(done).getByRole("button", { name: /src\/app.ts/ }));
    expect(props.onOpenDiff).toHaveBeenCalledWith(
      expect.objectContaining({ id: "c1" }),
      expect.objectContaining({ model: "cli:codex:astra" }),
      "src/app.ts",
    );
    fireEvent.click(
      within(done).getByRole("button", { name: /Open conversation/ }),
    );
    expect(props.onOpenLane).toHaveBeenCalled();
    // Polling stops once the comparison is done.
    const calls = mocks.compare.mock.calls.length;
    await new Promise((r) => setTimeout(r, 80));
    expect(mocks.compare.mock.calls.length).toBe(calls);
    // Finishing refreshes the list and scoreboard.
    expect(mocks.compareScoreboard.mock.calls.length).toBeGreaterThan(1);
  });

  it("shows a lane waiting for approval with a way to answer it", async () => {
    mocks.compare.mockResolvedValue(record());
    mocks.approvals.mockResolvedValue({
      approvals: [{ id: "a1", session_id: "s2", command: "npm install" }],
    });
    const props = view();
    await waitFor(() =>
      expect(article("Codex · GPT-6-Astra").textContent).toContain(
        "Waiting for approval",
      ),
    );
    expect(article("qwen3:14b").textContent).toContain("Working…");
    fireEvent.click(
      within(article("Codex · GPT-6-Astra")).getByRole("button", {
        name: "Answer in conversation",
      }),
    );
    expect(props.onOpenLane).toHaveBeenCalledWith(
      expect.objectContaining({ id: "c1" }),
      expect.objectContaining({ session_id: "s2" }),
    );
  });

  it("keeps a lane after confirming and points to Changes", async () => {
    const done = record({
      state: "done",
      lanes: [finishedCloud, localLane({ status: "completed" })],
    });
    mocks.compare.mockResolvedValue(done);
    mocks.keepCompare.mockResolvedValue({
      ...done,
      state: "applied",
      winner: "cli:codex:astra",
      applied_files: ["src/app.test.ts", "src/app.ts"],
      lanes: done.lanes.map((l) => ({ ...l, removed: true })),
    });
    const props = view();
    await waitFor(() =>
      expect(
        within(article("Codex · GPT-6-Astra")).getByRole("button", {
          name: "Keep this one",
        }),
      ).toHaveProperty("disabled", false),
    );
    fireEvent.click(
      within(article("Codex · GPT-6-Astra")).getByRole("button", {
        name: "Keep this one",
      }),
    );
    const confirm = screen.getByRole("dialog", {
      name: "Keep Codex · GPT-6-Astra's result",
    });
    expect(confirm.textContent).toContain(
      "Apply Codex · GPT-6-Astra’s changes to your project as uncommitted changes and remove the other copies?",
    );
    fireEvent.click(
      within(confirm).getByRole("button", { name: "Keep Codex · GPT-6-Astra" }),
    );
    await waitFor(() =>
      expect(mocks.keepCompare).toHaveBeenCalledWith(
        "c1",
        "cli:codex:astra",
        false,
      ),
    );
    const applied = await screen.findByText(/Applied 2 files from/);
    expect(applied.textContent).toContain("review them in");
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(props.onApplied).toHaveBeenCalled();
    expect(article("Codex · GPT-6-Astra").textContent).toContain("Kept");
    fireEvent.click(screen.getByRole("button", { name: "Open Changes" }));
    expect(props.onOpenChanges).toHaveBeenCalledWith();
    // The kept files are the project's now; the other copy is gone.
    fireEvent.click(
      within(article("Codex · GPT-6-Astra")).getByRole("button", {
        name: /src\/app.ts/,
      }),
    );
    expect(props.onOpenChanges).toHaveBeenLastCalledWith("src/app.ts");
    expect(
      within(article("qwen3:14b")).getByRole("button", {
        name: /Open conversation/,
      }),
    ).toHaveProperty("disabled", true);
    expect(screen.queryByRole("button", { name: /Discard all/ })).toBeNull();
  });

  it("names the files of a conflicting Keep and keeps the comparison open", async () => {
    const done = record({
      state: "done",
      lanes: [finishedCloud, localLane({ status: "completed" })],
    });
    mocks.compare.mockResolvedValue(done);
    mocks.keepCompare.mockRejectedValue(
      new Error(
        "Codex · GPT-6-Astra's changes no longer apply: the project changed since the comparison started in src/app.ts, src/app.test.ts. Nothing was changed and every lane is kept; update or revert those files, then keep again.",
      ),
    );
    view();
    await waitFor(() =>
      expect(
        within(article("Codex · GPT-6-Astra")).getByRole("button", {
          name: "Keep this one",
        }),
      ).toHaveProperty("disabled", false),
    );
    fireEvent.click(
      within(article("Codex · GPT-6-Astra")).getByRole("button", {
        name: "Keep this one",
      }),
    );
    fireEvent.click(
      within(screen.getByRole("dialog")).getByRole("button", {
        name: "Keep Codex · GPT-6-Astra",
      }),
    );
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain(
      "Codex · GPT-6-Astra's changes no longer apply.",
    );
    const list = within(alert).getByRole("list", { name: "Conflicting files" });
    expect(
      within(list)
        .getAllByRole("listitem")
        .map((li) => li.textContent),
    ).toEqual(["src/app.ts", "src/app.test.ts"]);
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(
      within(article("Codex · GPT-6-Astra")).getByRole("button", {
        name: "Keep this one",
      }),
    ).toHaveProperty("disabled", false);
    expect(screen.getByRole("button", { name: /Discard all/ })).toBeTruthy();
  });

  it("stops running lanes and discards after confirming", async () => {
    mocks.compare.mockResolvedValue(record());
    mocks.cancelCompare.mockResolvedValue(
      record({
        lanes: [lane({ status: "cancelling" }), localLane()],
      }),
    );
    mocks.discardCompare.mockResolvedValue(
      record({
        state: "discarded",
        lanes: [
          lane({ status: "cancelled", removed: true }),
          localLane({ status: "cancelled", removed: true }),
        ],
      }),
    );
    view({ pollMs: 5000 });
    fireEvent.click(await screen.findByRole("button", { name: /Stop/ }));
    await waitFor(() => expect(mocks.cancelCompare).toHaveBeenCalledWith("c1"));
    await waitFor(() =>
      expect(article("Codex · GPT-6-Astra").textContent).toContain("Stopping…"),
    );
    fireEvent.click(screen.getByRole("button", { name: /Discard all/ }));
    const confirm = screen.getByRole("dialog", {
      name: "Discard this comparison",
    });
    expect(confirm.textContent).toContain("Your project is not changed.");
    fireEvent.click(
      within(confirm).getByRole("button", { name: "Discard all" }),
    );
    await waitFor(() =>
      expect(mocks.discardCompare).toHaveBeenCalledWith("c1"),
    );
    expect(
      await screen.findByText(/This comparison was discarded/),
    ).toBeTruthy();
    expect(screen.queryByRole("button", { name: /Keep this one/ })).toBeNull();
  });

  it("shows the newest comparison by default and an empty project", async () => {
    mocks.compare.mockResolvedValue(record());
    const props = view({ compareId: "" });
    await screen.findByRole("article", { name: "Codex · GPT-6-Astra" });
    expect(mocks.compare).toHaveBeenCalledWith("c1");
    fireEvent.click(
      screen.getByRole("button", { name: "Back to conversation" }),
    );
    expect(props.onClose).toHaveBeenCalled();
    cleanup();
    mocks.compares.mockResolvedValue({ compares: [] });
    view({ compareId: "" });
    expect(
      await screen.findByText("No comparisons in this project yet."),
    ).toBeTruthy();
  });
});

describe("Scoreboard", () => {
  it("lists wins and runs per model", () => {
    render(
      <Scoreboard
        rows={[
          {
            model: "cli:codex:astra",
            name: "Codex · GPT-6-Astra",
            wins: 2,
            runs: 3,
          },
          { model: "local:gguf:qwen", name: "qwen3:14b", wins: 1, runs: 3 },
        ]}
      />,
    );
    const table = screen.getByRole("table");
    const rows = within(table).getAllByRole("row");
    expect(rows.map((r) => r.textContent)).toEqual([
      "ModelWinsRuns",
      "Codex · GPT-6-Astra23",
      "qwen3:14b13",
    ]);
    cleanup();
    render(<Scoreboard rows={[]} />);
    expect(screen.getByText("No finished comparisons yet.")).toBeTruthy();
  });
});

it("requires an explicit keep action labeled without current checks for stale evidence", async () => {
  const stale = {
    ...finishedCloud,
    checks: {
      passed: 0,
      failed: 0,
      incomplete: 1,
      commands: [
        { command: "npm test", exit_code: 0, success: false, state: "stale" },
      ],
    },
  };
  const done = record({
    state: "done",
    lanes: [stale, localLane({ status: "completed" })],
  });
  mocks.compare.mockResolvedValue(done);
  mocks.keepCompare.mockResolvedValue({
    ...done,
    state: "applied",
    winner: stale.model,
  });
  view();
  await waitFor(() =>
    expect(article(stale.name).textContent).toContain("stale"),
  );
  fireEvent.click(
    within(article(stale.name)).getByRole("button", { name: "Keep this one" }),
  );
  const dialog = screen.getByRole("dialog", {
    name: `Keep ${stale.name}'s result`,
  });
  expect(dialog.textContent).toContain("without current verification");
  expect(mocks.keepCompare).not.toHaveBeenCalled();
  fireEvent.click(
    within(dialog).getByRole("button", { name: "Keep without current checks" }),
  );
  await waitFor(() =>
    expect(mocks.keepCompare).toHaveBeenCalledWith("c1", stale.model, true),
  );
});
