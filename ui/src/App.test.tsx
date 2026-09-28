import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import App from "./App";
import { installFakeBackend } from "../e2e/fakeBackend";

let fake: ReturnType<typeof installFakeBackend>;

beforeEach(() => {
  localStorage.clear();
  vi.stubEnv("VITE_SHADOW_TEST_TRANSPORT", "1");
  fake = installFakeBackend({ stepMs: 5 });
});
afterEach(() => {
  cleanup();
  vi.unstubAllEnvs();
  vi.unstubAllGlobals();
  delete window.__SHADOW_TEST_TRANSPORT__;
});

const trigger = () =>
  screen.getByRole("button", { name: /Model for this task/ });
const send = () =>
  screen.getByRole("button", { name: /Send task|Queue follow-up/ });
const prompt = () =>
  screen.getByRole("textbox", { name: "Message ShadowCode" });

async function boot() {
  render(<App />);
  await screen.findByRole("textbox", { name: "Message ShadowCode" });
  await waitFor(() =>
    expect(trigger().textContent).toContain("Choose a model"),
  );
}

async function choose(name: RegExp) {
  fireEvent.click(trigger());
  fireEvent.click(await screen.findByRole("option", { name }));
  await waitFor(() => expect(screen.queryByRole("listbox")).toBeNull());
}

it("runs an approved check without a model and preserves the unsent composer draft", async () => {
  // Happy DOM has no layout visibility observations; assess the mounted card.
  vi.stubGlobal("IntersectionObserver", undefined);
  fake = installFakeBackend({ stepMs: 5, completedTask: true });
  await boot();
  const draft = "Keep this unsent implementation request";
  fireEvent.change(prompt(), { target: { value: draft } });
  expect(trigger().textContent).toContain("Choose a model");
  fireEvent.click(await screen.findByRole("button", { name: "Run a check…" }));
  const dialog = screen.getByRole("dialog", { name: "Run a check" });
  fireEvent.change(
    within(dialog).getByRole("textbox", { name: "Check command" }),
    { target: { value: "npm test" } },
  );
  fireEvent.click(within(dialog).getByRole("button", { name: "Run check" }));
  await waitFor(() =>
    expect(screen.queryByRole("dialog", { name: "Run a check" })).toBeNull(),
  );
  const allow = await screen.findByRole("button", {
    name: "Allow",
  });
  expect(prompt()).toHaveProperty("value", draft);
  expect(fake.state.jobs.at(-1).status).toBe("running");
  expect(
    screen.queryByText("Fixture check output: 4 cases passed."),
  ).toBeNull();
  fireEvent.click(allow);
  await waitFor(() => expect(fake.state.jobs.at(-1).status).toBe("completed"));
  await waitFor(() =>
    expect(
      screen.getAllByRole("region", { name: "Task summary" }),
    ).toHaveLength(2),
  );
  const summary = screen
    .getAllByRole("region", { name: "Task summary" })
    .at(-1)!;
  await waitFor(() => expect(within(summary).getByText("passed")).toBeTruthy());
  expect(
    within(summary).getByText(/Fixture check output: 4 cases passed\./),
  ).toBeTruthy();
  expect(prompt()).toHaveProperty("value", draft);
  expect(trigger().textContent).toContain("Choose a model");
  expect(
    fake.log
      .filter(
        (request) =>
          request.method === "POST" && request.path === "/api/jobs/test",
      )
      .map((request) => request.body),
  ).toEqual([
    {
      workspace: "/work/demo",
      session_id: "s1",
      command: "npm test",
      timeout: 300,
      queue: false,
    },
  ]);
  expect(
    fake.log.some(
      (request) => request.method === "POST" && request.path === "/api/jobs",
    ),
  ).toBe(false);
});

it("keeps a pending approval when the selected conversation is opened again", async () => {
  await boot();
  const pending = fake.requestApproval({
    session_id: "s1",
    task_id: "command-task",
    command: "sleep 45",
    arguments: { command: "sleep 45" },
  });
  const card = () =>
    document.querySelector(`[data-approval-id="${pending.id}"]`);
  await waitFor(() => expect(card()).not.toBeNull());
  const selected = document.querySelector<HTMLButtonElement>(
    'button[data-session-id="s1"][aria-current="page"]',
  )!;
  expect(selected).not.toBeNull();
  const reads = fake.log.filter((r) => r.path.startsWith("/api/feed?")).length;
  // This sends no new engine event and does not advance the 15s backstop.
  await act(async () => fireEvent.click(selected));
  expect(screen.queryByText("Opening task…")).toBeNull();
  expect(fake.state.approvals.map((a: { id: string }) => a.id)).toContain(
    pending.id,
  );
  expect(fake.log.filter((r) => r.path.startsWith("/api/feed?")).length).toBe(
    reads,
  );
  expect(card()).not.toBeNull();
});

it("disables Send until a ready row is chosen and remembers it per conversation", async () => {
  await boot();
  fireEvent.change(prompt(), { target: { value: "Fix the add function" } });
  expect(send()).toHaveProperty("disabled", true);
  expect(screen.getByText("Choose a model to send.")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Choose a model" }));
  const local = screen.getByRole("group", { name: "On this computer" });
  expect(
    within(local).getByRole("option", { name: /qwen3:14b · This computer/ }),
  ).toBeTruthy();
  fireEvent.click(screen.getByRole("option", { name: /Codex · GPT-6-Astra/ }));
  await waitFor(() => expect(send()).toHaveProperty("disabled", false));
  expect(trigger().textContent).toContain("Cloud");
  await waitFor(() =>
    expect(
      fake.log.some(
        (r) =>
          r.path === "/api/sessions/s1/target" &&
          r.body.target_id === "cli:codex:gpt-6-astra",
      ),
    ).toBe(true),
  );
});

it("forces a provider reprobe from an already-open model picker", async () => {
  await boot();
  fireEvent.click(trigger());
  expect(screen.getByRole("listbox")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Refresh models" }));
  await waitFor(() =>
    expect(
      fake.log.some((request) => request.path === "/api/picker?refresh=1"),
    ).toBe(true),
  );
  expect(screen.getByRole("listbox")).toBeTruthy();
});

it("runs a task and shows the event-derived timeline and summary", async () => {
  await boot();
  await choose(/qwen3:14b · This computer/);
  fireEvent.change(prompt(), { target: { value: "Fix the add function" } });
  fireEvent.click(send());
  const summary = await screen.findByRole(
    "region",
    { name: "Task summary" },
    { timeout: 3000 },
  );
  expect(within(summary).getByText("src/app.ts")).toBeTruthy();
  expect(within(summary).getByText("npm test")).toBeTruthy();
  expect(within(summary).getByText("exit 0")).toBeTruthy();
  await waitFor(() => expect(within(summary).getByText("+1")).toBeTruthy());
  const timeline = screen.getAllByLabelText("Agent activity").at(-1)!;
  for (const label of ["Reading project", "Editing files"])
    expect(within(timeline).getByText(label)).toBeTruthy();
  // This fixture reports a successful legacy command, not a configured
  // verification receipt. Its completed process cannot create a green check.
  const checks = within(timeline)
    .getByText("Checks not verified")
    .closest(".activity-step")!;
  expect(checks.className).toBe("activity-step is-incomplete");
  expect(
    within(checks as HTMLElement).getByText("Verification not run"),
  ).toBeTruthy();
  expect(checks.querySelector(".lucide-check")).toBeNull();
  expect(checks.querySelector(".lucide-circle-dashed")).toBeTruthy();
  // The summary card states the outcome; the timeline does not repeat it.
  expect(within(timeline).queryByText("Finished")).toBeNull();
  expect(within(summary).getByText("Finished")).toBeTruthy();
  expect(screen.getByText(/Using .*This computer/)).toBeTruthy();
  fireEvent.click(
    within(summary).getByRole("button", { name: "Review changes" }),
  );
  // A task's changes open the full-width review of that task.
  const review = await screen.findByRole("region", { name: "Review changes" });
  expect(
    await within(review).findByRole("button", { name: /src\/app\.ts/ }),
  ).toBeTruthy();
  fireEvent.click(
    within(review).getByRole("button", { name: "Back to conversation" }),
  );
  expect(screen.queryByRole("region", { name: "Review changes" })).toBeNull();
  const post = fake.log.find(
    (r) => r.path === "/api/jobs" && r.method === "POST",
  );
  expect(post?.body).toMatchObject({ model: "local:gguf:qwen", web: false });
});

it("asks before sending local conversation content to a cloud provider", async () => {
  await boot();
  await choose(/qwen3:14b · This computer/);
  fireEvent.change(prompt(), { target: { value: "First on this computer" } });
  fireEvent.click(send());
  await screen.findByRole(
    "region",
    { name: "Task summary" },
    { timeout: 3000 },
  );
  await choose(/Codex · GPT-6-Astra/);
  fireEvent.change(prompt(), { target: { value: "Continue in the cloud" } });
  await waitFor(() => expect(send()).toHaveProperty("disabled", false));
  fireEvent.click(send());
  const dialog = await screen.findByRole("dialog", {
    name: "Send to a cloud provider?",
  });
  expect(
    within(dialog).getByText(/Send to Codex · GPT-6-Astra\?/),
  ).toBeTruthy();
  expect(within(dialog).getByText(/2,400 characters/)).toBeTruthy();
  fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
  expect(prompt()).toHaveProperty("value", "Continue in the cloud");
  const posts = () =>
    fake.log.filter((r) => r.path === "/api/jobs" && r.method === "POST");
  expect(posts().every((r) => !r.body.handoff_consent)).toBe(true);
  fireEvent.click(send());
  const again = await screen.findByRole("dialog", {
    name: "Send to a cloud provider?",
  });
  fireEvent.click(within(again).getByRole("button", { name: "Send" }));
  await waitFor(() =>
    expect(posts().at(-1)?.body).toMatchObject({
      model: "cli:codex:gpt-6-astra",
      handoff_consent: true,
    }),
  );
});

it("accepts images only for vision rows and re-checks at send time", async () => {
  await boot();
  await choose(/qwen3:14b · This computer/);
  const input = document.querySelector<HTMLInputElement>('input[type="file"]')!;
  const png = new File([new Uint8Array([137, 80, 78, 71])], "shot.png", {
    type: "image/png",
  });
  await act(async () => {
    fireEvent.change(input, { target: { files: [png] } });
  });
  expect(
    await screen.findByText(/qwen3:14b · This computer does not accept images/),
  ).toBeTruthy();
  expect(screen.queryByRole("list", { name: "Attachments" })).toBeNull();
  await choose(/Codex · GPT-6-Astra/);
  await act(async () => {
    fireEvent.change(input, { target: { files: [png] } });
  });
  expect(
    await screen.findByRole("button", { name: "Remove shot.png" }),
  ).toBeTruthy();
  await choose(/qwen3:14b · This computer/);
  expect(
    screen.getByText(
      /does not accept images. Remove the image or choose a model marked Vision/,
    ),
  ).toBeTruthy();
  expect(send()).toHaveProperty("disabled", true);
});

it("stages a model change during a running task for the next message", async () => {
  fake = installFakeBackend({ stepMs: 150 });
  await boot();
  await choose(/qwen3:14b · This computer/);
  fireEvent.change(prompt(), { target: { value: "Slow task" } });
  fireEvent.click(send());
  await screen.findByRole("button", { name: "Stop task" });
  await choose(/Codex · GPT-6-Astra/);
  expect(screen.getByText("Applies to your next message")).toBeTruthy();
});

it("a setup-required Antigravity row opens Accounts at the Antigravity card", async () => {
  await boot();
  fireEvent.click(trigger());
  const row = await screen.findByRole("option", {
    name: /Antigravity · Default/,
  });
  expect(row.textContent).toContain("Setup required");
  fireEvent.click(row);
  expect(
    screen.getByText("Install the Antigravity agent in Settings › Accounts."),
  ).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Open Accounts" }));
  const settings = await screen.findByRole("dialog", { name: "Settings" });
  const card = await within(settings).findByRole("article", {
    name: "Antigravity",
  });
  const install = within(card).getByRole("button", {
    name: "Install Antigravity agent",
  });
  await waitFor(() => expect(document.activeElement).toBe(install));
});

it("Allowance opens from the status bar and its plan-limit choice matches Accounts", async () => {
  await boot();
  const button = await screen.findByRole("button", { name: /^Allowance/ });
  // Codex is low and Cursor is at its limit: the dot warns.
  await waitFor(() =>
    expect(button.querySelector(".allowance-dot")?.className).toContain("warn"),
  );
  fireEvent.click(button);
  const dialog = await screen.findByRole("dialog", { name: "Allowance" });
  const codex = await within(dialog).findByRole("article", { name: "Codex" });
  expect(codex.textContent).toContain("2% left");
  const local = within(dialog).getByRole("article", {
    name: "On this computer",
  });
  expect(
    within(local).getByRole("radio", { name: /Continue on qwen3:14b/ }),
  ).toHaveProperty("checked", true);
  fireEvent.click(within(local).getByRole("radio", { name: /Ask me/ }));
  await waitFor(() =>
    expect(
      fake.log.some(
        (r) =>
          r.path === "/api/config" &&
          r.method === "PUT" &&
          r.body.values?.limits?.on_limit === "ask",
      ),
    ).toBe(true),
  );
  expect(fake.state.config.limits).toEqual({
    on_limit: "ask",
    fallback_model: "",
  });
  // Claude Code needs sign-in: its action opens Accounts at its card.
  fireEvent.click(
    within(dialog).getByRole("button", {
      name: "Open Accounts for Claude Code",
    }),
  );
  const settings = await screen.findByRole("dialog", { name: "Settings" });
  const group = within(settings).getByRole("group", {
    name: "When a plan runs out",
  });
  expect(within(group).getByRole("radio", { name: /Ask me/ })).toHaveProperty(
    "checked",
    true,
  );
  const claude = await within(settings).findByRole("article", {
    name: "Claude Code",
  });
  const connect = within(claude).getByRole("button", { name: "Connect" });
  await waitFor(() => expect(document.activeElement).toBe(connect));
});

it("a Codex plan limit continues on the local model in the same conversation", async () => {
  fake.state.limitOnCodex = true;
  await boot();
  await choose(/Codex · GPT-6-Astra/);
  fireEvent.change(prompt(), { target: { value: "Fix the add function" } });
  fireEvent.click(send());
  expect(
    await screen.findByText(
      "Codex reached its plan limit. Continuing on qwen3:14b on this computer.",
      undefined,
      { timeout: 8000 },
    ),
  ).toBeTruthy();
  // The follow-up reads as ShadowCode's continuation, text kept.
  expect(await screen.findByText("Continued automatically")).toBeTruthy();
  expect(
    screen.getByText(/^Continue where Codex stopped when its plan limit/),
  ).toBeTruthy();
  // The composer follows the conversation onto the local model.
  await waitFor(() => expect(trigger().textContent).toContain("qwen3:14b"), {
    timeout: 8000,
  });
  await waitFor(
    () =>
      expect(
        screen.getAllByRole("region", { name: "Task summary" }),
      ).toHaveLength(2),
    { timeout: 8000 },
  );
  const [limited, done] = screen.getAllByRole("region", {
    name: "Task summary",
  });
  expect(limited.textContent).toContain("Plan limit reached");
  expect(limited.className).not.toContain("is-bad");
  expect(done.textContent).toContain("Finished");
  const posts = fake.log.filter(
    (r) => r.path === "/api/jobs" && r.method === "POST",
  );
  // The engine started the follow-up; the window sent only the first task.
  expect(posts.map((r) => r.body.model)).toEqual(["cli:codex:gpt-6-astra"]);
}, 20000);

it("Compare runs a task on two models in hidden lanes, opens a lane and returns", async () => {
  await boot();
  fireEvent.click(document.querySelector("details.composer-more > summary")!);
  const compare = screen.getByRole("button", { name: "Compare" });
  await waitFor(() =>
    expect(compare.getAttribute("title")).toMatch(/Type a task/),
  );
  expect(compare.getAttribute("aria-disabled")).toBe("true");
  fireEvent.click(compare);
  expect(screen.queryByRole("dialog", { name: "Compare models" })).toBeNull();
  fireEvent.change(prompt(), { target: { value: "Fix the add function" } });
  expect(compare.getAttribute("aria-disabled")).toBe("false");
  fireEvent.click(compare);
  const dialog = await screen.findByRole("dialog", { name: "Compare models" });
  for (const [slot, name] of [
    [1, /qwen3:14b · This computer/],
    [2, /Codex · GPT-6-Astra/],
  ] as const) {
    fireEvent.click(
      within(dialog).getByRole("button", {
        name: new RegExp(`^Model ${slot}:`),
      }),
    );
    fireEvent.click(await within(dialog).findByRole("option", { name }));
  }
  fireEvent.click(
    within(dialog).getByRole("button", { name: "Start comparison" }),
  );
  const view = await screen.findByRole("region", { name: "Comparisons" });
  expect(screen.queryByRole("dialog", { name: "Compare models" })).toBeNull();
  expect(fake.log.find((r) => r.path === "/api/compare")?.body).toMatchObject({
    workspace: "/work/demo",
    task: "Fix the add function",
    models: ["local:gguf:qwen", "cli:codex:gpt-6-astra"],
  });
  // Lane conversations never reach the sidebar.
  const sidebar = screen.getByRole("complementary", {
    name: "Projects and tasks",
  });
  await waitFor(() =>
    expect(fake.state.sessions.some((s: any) => s.compare_id)).toBe(true),
  );
  expect(sidebar.textContent).not.toContain("Compare ·");
  const local = await within(view).findByRole("article", {
    name: "qwen3:14b",
  });
  await waitFor(() => expect(local.textContent).toContain("Finished"), {
    timeout: 5000,
  });
  fireEvent.click(
    within(local).getByRole("button", { name: /Open conversation/ }),
  );
  const banner = await screen.findByText(/Part of a comparison/);
  expect(banner.textContent).toContain("qwen3:14b");
  await waitFor(() =>
    expect(document.querySelector(".top-title")?.textContent).toBe(
      "Compare · qwen3:14b",
    ),
  );
  fireEvent.click(document.querySelector("details.composer-more > summary")!);
  expect(
    screen.getByRole("button", { name: "Compare" }).getAttribute("title"),
  ).toMatch(/one model's copy/);
  // Activating the lane recorded its copy as a project; lists leave it out.
  await waitFor(() =>
    expect(fake.state.projects).toContain(
      "/work/.shadowcode/worktrees/demo-1-1",
    ),
  );
  expect(sidebar.textContent).not.toContain("demo-1-1");
  fireEvent.click(screen.getByRole("button", { name: "Back to comparison" }));
  await screen.findByRole("region", { name: "Comparisons" });
  await waitFor(() =>
    expect(screen.queryByText(/Part of a comparison/)).toBeNull(),
  );
  fireEvent.click(document.querySelector("details.composer-more > summary")!);
  expect(fake.state.selected).toBe("/work/demo");
});

it("offers cached local choices before a held full picker refresh without selecting a model", async () => {
  const bridge = window.__SHADOW_TEST_TRANSPORT__!;
  const request = bridge.request;
  let release!: (value: unknown) => void;
  const held = new Promise<unknown>((resolve) => {
    release = resolve;
  });
  let fullStarted = 0;
  bridge.request = (path, method, body) => {
    if (
      method === "GET" &&
      path.startsWith("/api/picker") &&
      !path.includes("cached=1")
    ) {
      fullStarted += 1;
      return held;
    }
    return request(path, method, body);
  };
  try {
    render(<App />);
    await screen.findByRole("textbox", { name: "Message ShadowCode" });
    await waitFor(() => expect(fullStarted).toBeGreaterThan(0));
    fireEvent.click(trigger());
    expect(
      await screen.findByRole("option", { name: /qwen3:14b · This computer/ }),
    ).toBeTruthy();
    expect(trigger().textContent).toContain("Choose a model");
    fireEvent.change(prompt(), { target: { value: "Inspect the project" } });
    expect(send()).toHaveProperty("disabled", true);
    expect(fake.log.some((r) => r.path === "/api/picker?cached=1")).toBe(true);
    expect(
      fake.log.some((r) => r.path === "/api/jobs" && r.method === "POST"),
    ).toBe(false);
  } finally {
    const response = await request("/api/picker", "GET", null);
    await act(async () => {
      release(response);
      await held;
    });
  }
});
