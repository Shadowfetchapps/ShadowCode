import { afterEach, beforeEach, expect, it, vi } from "vitest";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { BranchPanel } from "./BranchPanel";
import { api } from "../api";
import { forgeApi, type GitOverview, type PrStatus } from "../lib/forge";
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

const feature: GitOverview = {
  repo: true,
  branch: "feature/login",
  upstream: "origin/feature/login",
  ahead: 1,
  behind: 0,
  staged: 1,
  changed: 1,
  branches: [
    { name: "feature/login", upstream: "origin/feature/login", current: true },
    { name: "main", upstream: "origin/main", current: false },
  ],
  remote: "origin",
  bases: ["main", "develop"],
  default_base: "main",
};
const ready: PrStatus = {
  remote: "origin",
  provider: "github",
  cli: {
    name: "gh",
    installed: true,
    authenticated: true,
    detail: "Logged in to github.com account octo",
    login_command: "gh auth login --hostname github.com",
    install_url: "https://cli.github.com",
  },
  base: "main",
  compare_url:
    "https://github.com/octo/demo/compare/main...feature/login?expand=1",
  pr: null,
};

function mount(busy = false) {
  const toast = vi.fn();
  function Harness() {
    const memory = useDrawerMemory("/work/demo");
    return (
      <BranchPanel
        busy={busy}
        toast={toast}
        memory={memory.memory}
        onMemory={memory.update}
        onOpenTerminal={vi.fn()}
      />
    );
  }
  render(<Harness />);
  return { toast };
}

beforeEach(() => {
  vi.mocked(forgeApi.overview).mockResolvedValue(feature);
  vi.mocked(forgeApi.prStatus).mockResolvedValue(ready);
});
afterEach(() => {
  cleanup();
  vi.resetAllMocks();
});

it("suggests an editable message, then commits it", async () => {
  vi.mocked(forgeApi.suggest).mockResolvedValue({
    kind: "commit",
    source: "model",
    model: "qwen3-coder",
    note: "",
    message: "Add the login page\n\nUsers can sign in.",
  });
  vi.mocked(api.gitCommit).mockResolvedValue({ ok: true });
  const { toast } = mount();
  await screen.findByText("feature/login");
  expect(screen.getByText("1 to push · origin/feature/login")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Suggest message" }));
  await waitFor(() => expect(forgeApi.suggest).toHaveBeenCalledWith("commit"));
  const box = screen.getByRole("textbox", {
    name: "Commit message",
  }) as HTMLTextAreaElement;
  await waitFor(() => expect(box.value).toContain("Add the login page"));
  expect(screen.getByText(/Drafted by qwen3-coder/)).toBeTruthy();
  fireEvent.change(box, { target: { value: "Add login" } });
  fireEvent.click(screen.getByRole("button", { name: "Commit" }));
  await waitFor(() =>
    expect(api.gitCommit).toHaveBeenCalledWith("Add login", {}),
  );
  expect(toast).toHaveBeenCalledWith("Committed", "ok");
});

it("validates new branch names before asking the engine", async () => {
  mount();
  await screen.findByText("feature/login");
  const name = screen.getByRole("textbox", { name: "New branch name" });
  fireEvent.change(name, { target: { value: "bad name" } });
  expect(screen.getByText(/cannot contain spaces/)).toBeTruthy();
  expect(
    (screen.getByRole("button", { name: "Create branch" }) as HTMLButtonElement)
      .disabled,
  ).toBe(true);
  vi.mocked(forgeApi.branch).mockResolvedValue({ ok: true, branch: "fix/a" });
  fireEvent.change(name, { target: { value: "fix/a" } });
  fireEvent.click(screen.getByRole("button", { name: "Create branch" }));
  await waitFor(() =>
    expect(forgeApi.branch).toHaveBeenCalledWith("fix/a", true),
  );
});

it("opens a draft pull request and shows its checks", async () => {
  vi.mocked(forgeApi.createPr).mockResolvedValue({
    ok: true,
    url: "https://github.com/octo/demo/pull/7",
    number: 7,
    provider: "github",
    pushed: true,
    branch: "feature/login",
    base: "develop",
    draft: true,
  });
  vi.mocked(forgeApi.checks).mockResolvedValue({
    supported: true,
    checks: [
      {
        name: "build",
        bucket: "pass",
        workflow: "CI",
        link: "https://github.com/octo/demo/actions/runs/1",
      },
      { name: "test", bucket: "pending", workflow: "CI" },
    ],
    summary: { pass: 1, pending: 1 },
    overall: "pending",
    url: "https://github.com/octo/demo/pull/7/checks",
  });
  const { toast } = mount();
  await screen.findByRole("button", { name: "Create pull request" });
  fireEvent.change(
    screen.getByRole("textbox", { name: "Pull request title" }),
    {
      target: { value: "Add login" },
    },
  );
  fireEvent.change(screen.getByRole("combobox", { name: "Base branch" }), {
    target: { value: "develop" },
  });
  fireEvent.click(screen.getByRole("checkbox", { name: "Draft" }));
  fireEvent.click(screen.getByRole("button", { name: "Create pull request" }));
  await waitFor(() =>
    expect(forgeApi.createPr).toHaveBeenCalledWith({
      title: "Add login",
      body: "",
      base: "develop",
      draft: true,
    }),
  );
  const link = await screen.findByRole("link", { name: /#7 Add login/ });
  expect(link.getAttribute("href")).toBe("https://github.com/octo/demo/pull/7");
  expect(toast).toHaveBeenCalledWith(
    "Pushed the branch and opened the pull request",
    "ok",
  );
  await screen.findByText("Checks are running");
  expect(forgeApi.checks).toHaveBeenCalledWith(7, "origin");
  expect(screen.getByRole("link", { name: "build" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Refresh checks" }));
  await waitFor(() => expect(forgeApi.checks).toHaveBeenCalledTimes(2));
});

it("explains how to sign in and offers the compare page when gh is not ready", async () => {
  vi.mocked(forgeApi.prStatus).mockResolvedValue({
    ...ready,
    cli: { ...ready.cli, authenticated: false, detail: "" },
  });
  mount();
  await screen.findByText(/installed but not signed in/);
  expect(screen.getByText("gh auth login --hostname github.com")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Create pull request" })).toBe(
    null,
  );
  expect(
    screen.getByRole("button", { name: /Open compare page in browser/ }),
  ).toBeTruthy();
  vi.mocked(forgeApi.prStatus).mockResolvedValue({
    ...ready,
    cli: { ...ready.cli, installed: false, authenticated: false },
  });
  cleanup();
  mount();
  await screen.findByText(/install the GitHub CLI \(gh\)/);
  expect(
    screen.getByRole("link", { name: "https://cli.github.com" }),
  ).toBeTruthy();
});

it("waits for the agent before switching branches or committing", async () => {
  mount(true);
  await screen.findByText(/wait until the agent finishes/);
  expect(
    (screen.getByRole("button", { name: "Commit" }) as HTMLButtonElement)
      .disabled,
  ).toBe(true);
  expect(
    (
      screen.getByRole("combobox", {
        name: "Switch to branch",
      }) as HTMLSelectElement
    ).disabled,
  ).toBe(true);
  // Push still works while the agent runs.
  expect(
    (screen.getByRole("button", { name: "Push" }) as HTMLButtonElement)
      .disabled,
  ).toBe(false);
  await act(async () => undefined);
});
