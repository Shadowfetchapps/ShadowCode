import { afterEach, beforeEach, expect, it, vi } from "vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { BranchPanel } from "./BranchPanel";
import { api } from "../api";
import { forgeApi, type GitOverview } from "../lib/forge";
import { opinionApi, type SecondOpinion } from "../lib/secondOpinion";
import type { PickerTarget } from "../lib/picker";
import { useDrawerMemory } from "../hooks/useDrawerMemory";

vi.mock("../api", () => ({
  api: { gitAdd: vi.fn(), gitCommit: vi.fn() },
}));
vi.mock("../lib/forge", async (original) => {
  const real = await original<typeof import("../lib/forge")>();
  return {
    ...real,
    forgeApi: {
      overview: vi.fn(),
      branch: vi.fn(),
      suggest: vi.fn(),
      push: vi.fn(),
      prStatus: vi.fn(),
      createPr: vi.fn(),
      checks: vi.fn(),
    },
  };
});
vi.mock("../lib/secondOpinion", async (original) => {
  const real = await original<typeof import("../lib/secondOpinion")>();
  return {
    ...real,
    opinionApi: {
      list: vi.fn(),
      get: vi.fn(),
      start: vi.fn(),
      cancel: vi.fn(),
      setFinding: vi.fn(),
      fix: vi.fn(),
      options: vi.fn(),
      current: vi.fn(),
      savePrefs: vi.fn(),
    },
  };
});
vi.mock("./Markdown", () => ({
  Markdown: ({ children }: { children: string }) => <p>{children}</p>,
}));

const overview: GitOverview = {
  repo: true,
  branch: "main",
  upstream: "origin/main",
  ahead: 0,
  behind: 0,
  staged: 1,
  changed: 1,
  branches: [{ name: "main", upstream: "origin/main", current: true }],
  remote: null,
};
const targets: PickerTarget[] = [
  {
    id: "local:gguf:qwen",
    provider: "llamacpp",
    group: "local",
    name: "Qwen · This computer",
    inference: "local",
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
const record = (over: Partial<SecondOpinion> = {}): SecondOpinion => ({
  id: "o1",
  kind: "review",
  workspace: "/work/demo",
  source: "staged",
  question: "",
  reviewer: { model: "cli:codex", label: "Codex", local: false },
  writer: { model: "local:gguf:qwen", label: "Qwen", local: true },
  same_model: false,
  consented: false,
  job_id: "j9",
  review_session: "rs",
  review_task: "rt",
  status: "running",
  created_at: 10,
  diff_hash: "staged-1",
  files: ["src/app.ts"],
  omitted: [],
  diff: [
    {
      path: "src/app.ts",
      status: "modified",
      binary: false,
      diff: "@@ -1,1 +1,1 @@\n-a + b\n+a - b\n",
    },
  ],
  truncated: false,
  context_chars: 300,
  summary: "",
  findings: [],
  format_note: "",
  error: "",
  model_name: "Codex",
  reviewer_changed: [],
  ...over,
});
const done = record({
  status: "completed",
  summary: "One real bug.",
  findings: [
    {
      id: "f1",
      file: "src/app.ts",
      line: 1,
      severity: "high",
      title: "Subtracts instead of adding",
      explanation: "a - b",
      suggested_fix: "Use a + b",
      status: "open",
    },
  ],
});

function mount() {
  const toast = vi.fn();
  function Harness() {
    const memory = useDrawerMemory("/work/demo");
    return (
      <BranchPanel
        busy={false}
        toast={toast}
        memory={memory.memory}
        onMemory={memory.update}
        onOpenTerminal={vi.fn()}
        workspace="/work/demo"
        sessionId="s1"
        targets={targets}
      />
    );
  }
  render(<Harness />);
  return { toast };
}

beforeEach(() => {
  vi.mocked(forgeApi.overview).mockResolvedValue(overview);
  vi.mocked(opinionApi.list).mockResolvedValue({
    workspace: "/work/demo",
    second_opinions: [],
  });
  vi.mocked(opinionApi.current).mockResolvedValue({
    hash: "staged-1",
    files: ["src/app.ts"],
    omitted: [],
    truncated: false,
  });
  vi.mocked(opinionApi.options).mockResolvedValue({
    workspace: "/work/demo",
    prefs: { model: null, before_commit: true },
    offline: false,
    writer: { model: "local:gguf:qwen", label: "Qwen", local: true },
    local_only: false,
  });
  vi.mocked(api.gitCommit).mockResolvedValue({ ok: true });
});
afterEach(() => {
  cleanup();
  vi.resetAllMocks();
});

async function typeMessage() {
  await screen.findByText(/1 file staged/);
  fireEvent.change(screen.getByRole("textbox", { name: "Commit message" }), {
    target: { value: "Fix add" },
  });
}

it("reviews before every commit: the commit waits for the findings, then goes ahead", async () => {
  vi.mocked(opinionApi.start).mockResolvedValue({ second_opinion: record() });
  mount();
  await typeMessage();
  // Another model than the one that wrote the change is suggested.
  const section = screen.getByRole("region", { name: "Review before commit" });
  await waitFor(() =>
    expect(
      (
        within(section).getByRole("combobox", {
          name: "Reviewer model",
        }) as HTMLSelectElement
      ).value,
    ).toBe("cli:codex"),
  );
  fireEvent.click(screen.getByRole("button", { name: "Review and commit" }));
  await waitFor(() =>
    expect(opinionApi.start).toHaveBeenCalledWith({
      kind: "review",
      source: "staged",
      workspace: "/work/demo",
      session_id: "s1",
      model: "cli:codex",
    }),
  );
  expect(api.gitCommit).not.toHaveBeenCalled();
  await screen.findByText(/The commit waits until you have seen the findings/);
  expect(
    screen.getByRole("button", { name: "Commit without waiting" }),
  ).toBeTruthy();
  // The review finishes: its findings sit next to the reviewed hunk.
  vi.mocked(opinionApi.list).mockResolvedValue({
    workspace: "/work/demo",
    second_opinions: [done],
  });
  await screen.findByText("Subtracts instead of adding", undefined, {
    timeout: 4000,
  });
  expect(screen.getByText(/found 1 open finding/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Commit anyway" }));
  await waitFor(() => expect(api.gitCommit).toHaveBeenCalledWith("Fix add"));
});

it("commit without waiting stops the running review first", async () => {
  vi.mocked(opinionApi.list).mockResolvedValue({
    workspace: "/work/demo",
    second_opinions: [record()],
  });
  vi.mocked(opinionApi.cancel).mockResolvedValue(
    record({ status: "cancelled" }),
  );
  mount();
  await typeMessage();
  const button = await screen.findByRole("button", {
    name: "Stop review and commit",
  });
  fireEvent.click(button);
  await waitFor(() => expect(api.gitCommit).toHaveBeenCalledWith("Fix add"));
  expect(opinionApi.cancel).toHaveBeenCalledWith("o1");
  expect(opinionApi.start).not.toHaveBeenCalled();
});

it("a fresh review lets the commit through; the choice and setting are saved", async () => {
  vi.mocked(opinionApi.list).mockResolvedValue({
    workspace: "/work/demo",
    second_opinions: [{ ...done, findings: [] }],
  });
  vi.mocked(opinionApi.savePrefs).mockResolvedValue({
    model: "local:gguf:qwen",
    before_commit: false,
  });
  mount();
  await typeMessage();
  await screen.findByText("No problems found");
  fireEvent.change(screen.getByRole("combobox", { name: "Reviewer model" }), {
    target: { value: "local:gguf:qwen" },
  });
  expect(opinionApi.savePrefs).toHaveBeenCalledWith("/work/demo", {
    model: "local:gguf:qwen",
  });
  fireEvent.click(
    screen.getByRole("checkbox", { name: "Review before every commit" }),
  );
  await waitFor(() =>
    expect(opinionApi.savePrefs).toHaveBeenCalledWith("/work/demo", {
      before_commit: false,
    }),
  );
  fireEvent.click(screen.getByRole("button", { name: "Commit" }));
  await waitFor(() => expect(api.gitCommit).toHaveBeenCalledWith("Fix add"));
  expect(opinionApi.start).not.toHaveBeenCalled();
});

it("a cloud reviewer of local work asks first and sends nothing on Cancel", async () => {
  vi.mocked(opinionApi.start).mockResolvedValue({
    consent: {
      needs_consent: true,
      handoff: {
        from: "Qwen",
        to: "Codex",
        excerpt_chars: 1200,
        purpose: "second_opinion",
        files: 1,
      },
    },
  });
  mount();
  await typeMessage();
  const section = screen.getByRole("region", { name: "Review before commit" });
  await waitFor(() =>
    expect(
      within(section).getByRole("button", { name: "Review staged changes" }),
    ).toHaveProperty("disabled", false),
  );
  fireEvent.click(
    within(section).getByRole("button", { name: "Review staged changes" }),
  );
  const dialog = await screen.findByRole("dialog");
  expect(dialog.textContent).toContain("This second opinion goes to a cloud");
  expect(dialog.textContent).toContain("changes to 1 file");
  fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(opinionApi.start).toHaveBeenCalledTimes(1);
  fireEvent.click(
    within(section).getByRole("button", { name: "Review staged changes" }),
  );
  fireEvent.click(
    within(await screen.findByRole("dialog")).getByRole("button", {
      name: "Send",
    }),
  );
  await waitFor(() =>
    expect(opinionApi.start).toHaveBeenLastCalledWith(
      expect.objectContaining({ consent: true, model: "cli:codex" }),
    ),
  );
});
