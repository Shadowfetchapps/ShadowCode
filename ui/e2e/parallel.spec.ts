import { test, expect, type Page } from "@playwright/test";
import { installFakeBackend } from "./fakeBackend";

// Parallel tasks: a task in a new worktree beside the main checkout, sidebar
// badges and unread state, conversation shortcuts and the context & cost
// chip. Everything runs against the deterministic fake engine.
test.beforeEach(async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  (page as Page & { errors?: string[] }).errors = errors;
  await page.addInitScript(installFakeBackend, { stepMs: 90 });
  await page.goto("/");
  await expect(
    page.getByRole("heading", { name: "What should we work on?" }),
  ).toBeVisible();
});
test.afterEach(async ({ page }) => {
  expect((page as Page & { errors?: string[] }).errors).toEqual([]);
});

const prompt = (page: Page) =>
  page.getByRole("textbox", { name: "Message ShadowCode" });
const fake = <T>(page: Page, run: (fake: any) => T) =>
  page.evaluate(run as never) as Promise<T>;
const fakeLog = (page: Page) =>
  page.evaluate(
    () =>
      (
        window as unknown as {
          __SHADOW_FAKE__: {
            log: { method: string; path: string; body: any }[];
          };
        }
      ).__SHADOW_FAKE__.log,
  );

async function chooseLocal(page: Page) {
  await page.getByRole("button", { name: /Model for this task/ }).click();
  const search = page.getByRole("combobox", { name: "Search models" });
  await search.fill("qwen3:14b");
  await search.press("Enter");
  await expect(page.getByRole("listbox")).toHaveCount(0);
}

test("a paused task keeps new conversations in the same checkout queued", async ({
  page,
}) => {
  await fake(page, () => {
    (window as any).__SHADOW_FAKE__.state.held = ["s1"];
  });
  await chooseLocal(page);
  await prompt(page).fill("Hold this task in the main checkout");
  await page.getByRole("button", { name: "Send task" }).click();
  await expect(
    page.getByRole("button", { name: "Pause task", exact: true }),
  ).toBeVisible();
  // Inject a saved paused snapshot; the ordinary reader must discover it.
  await fake(page, () => {
    const job = (window as any).__SHADOW_FAKE__.state.jobs.find(
      (j: any) => j.session_id === "s1",
    );
    job.status = "paused";
  });
  await expect(
    page.getByRole("button", { name: "Resume task", exact: true }),
  ).toBeVisible();
  await page.locator(".new-task").click();
  await expect(page.getByRole("button", { name: "Stop task" })).toHaveCount(0);
  await prompt(page).fill("Run after the paused task");
  await page
    .getByRole("button", { name: "Queue follow-up", exact: true })
    .click();
  const submissions = (await fakeLog(page)).filter(
    (r) => r.path === "/api/jobs" && r.method === "POST",
  );
  expect(submissions.at(-1)?.body).toMatchObject({
    task: "Run after the paused task",
    queue: true,
  });
});

test("runs a second task in a new worktree beside a running one and applies it", async ({
  page,
}) => {
  // The main checkout's task stays running until released.
  await fake(page, () => {
    (window as any).__SHADOW_FAKE__.state.held = ["s1"];
  });
  await chooseLocal(page);
  await prompt(page).fill("Fix add in the main checkout");
  await page.getByRole("button", { name: "Send task" }).click();
  // While it runs, Send would queue; the worktree runs now instead.
  const now = page.getByRole("button", {
    name: "Run now in a new worktree instead of queueing",
  });
  await expect(now).toBeVisible();
  await prompt(page).fill("Add a regression test");
  await prompt(page).press("Control+Shift+Enter");
  const bar = page.getByRole("region", {
    name: "This conversation runs in its own worktree",
  });
  await expect(bar).toContainText("main checkout is free");
  const started = (await fakeLog(page)).find(
    (r) => r.path === "/api/jobs" && r.method === "POST" && r.body?.worktree,
  );
  expect(started?.body.session_id).toBeUndefined();
  expect(started?.body.queue).toBeUndefined();
  // Both run at once.
  const running = await fake(page, () =>
    (window as any).__SHADOW_FAKE__.state.jobs
      .filter((j: any) => ["queued", "running"].includes(j.status))
      .map((j: any) => j.session_id),
  );
  expect(running.sort()).toEqual(["s1", "w1"]);
  // Listed under the project, never as a project of its own.
  const sidebar = page.locator(".sidebar");
  await expect(sidebar.getByTitle(/^Add a regression test/)).toBeVisible();
  await expect(sidebar).not.toContainText("checkouts");
  await expect(bar).toContainText("1 file changed", { timeout: 15000 });
  // The main checkout's task is still running while this one is done.
  await expect(page.locator('[data-session-id="s1"]')).toHaveAttribute(
    "data-badge",
    "running",
  );
  await fake(page, () => {
    (window as any).__SHADOW_FAKE__.state.held = [];
  });
  // It finishes out of view: unread.
  await expect(page.locator('[data-session-id="s1"]')).toHaveAttribute(
    "data-badge",
    "unread",
    { timeout: 15000 },
  );
  await bar.getByRole("button", { name: /Apply to project/ }).click();
  await expect(page.getByText(/Applied 1 file to the project/)).toBeVisible();
  await expect(bar).toHaveCount(0);
  expect(
    (await fakeLog(page)).some((r) =>
      /^\/api\/worktree-tasks\/[0-9a-f]+\/apply$/.test(r.path),
    ),
  ).toBe(true);
  // The conversation now belongs to the project, where a new worktree can
  // be started again.
  await expect(
    page.getByRole("button", { name: "Run in a new worktree" }),
  ).toBeVisible();
});

test("a conflicting apply lists the files and keeps the worktree; discard removes it", async ({
  page,
}) => {
  await fake(page, () => {
    (window as any).__SHADOW_FAKE__.state.worktreeConflict = true;
  });
  await chooseLocal(page);
  await prompt(page).fill("Try something risky");
  await page.getByRole("button", { name: "Run in a new worktree" }).click();
  const bar = page.getByRole("region", {
    name: "This conversation runs in its own worktree",
  });
  await expect(bar).toContainText("1 file changed", { timeout: 15000 });
  await bar.getByRole("button", { name: /Apply to project/ }).click();
  await expect(bar.getByRole("alert")).toContainText("src/app.ts");
  await bar.getByRole("button", { name: "Discard…" }).click();
  await bar.getByRole("button", { name: "Discard", exact: true }).click();
  await expect(bar).toHaveCount(0);
  await expect(page.getByText(/Discarded\. The worktree/)).toBeVisible();
});

test("badges show approvals, running and unread; shortcuts move between conversations", async ({
  page,
}) => {
  await chooseLocal(page);
  // A second conversation to move to and from.
  await page.keyboard.press("Control+n");
  await expect(page.locator('[data-session-id="s2"]')).toBeVisible();
  await chooseLocal(page);
  await fake(page, () => {
    (window as any).__SHADOW_FAKE__.state.held = ["s2"];
  });
  await prompt(page).fill("Work in the background");
  await page.getByRole("button", { name: "Send task" }).click();
  const s2 = page.locator('[data-session-id="s2"]');
  await expect(s2).toHaveAttribute("data-badge", "running");
  // Alt+↓ opens the next conversation in the sidebar (s2 is listed first).
  await page.keyboard.press("Alt+ArrowDown");
  await expect(page.locator('[data-session-id="s1"]')).toHaveAttribute(
    "aria-current",
    "page",
  );
  await page.keyboard.press("Alt+ArrowUp");
  await expect(s2).toHaveAttribute("aria-current", "page");
  await page.keyboard.press("Alt+ArrowDown");
  await expect(page.locator('[data-session-id="s1"]')).toHaveAttribute(
    "aria-current",
    "page",
  );
  // s2 finishes out of view: unread until opened, and it survives a reload.
  await fake(page, () => {
    (window as any).__SHADOW_FAKE__.state.held = [];
  });
  await expect(s2).toHaveAttribute("data-badge", "unread", { timeout: 15000 });
  // (A reload restarts the fake engine, so read the saved state directly.)
  const saved = await page.evaluate(() =>
    JSON.parse(localStorage.getItem("shadow:unread") || "{}"),
  );
  expect(Object.keys(saved)).toEqual(["s2"]);
  // An approval in another conversation is flagged in the sidebar.
  await fake(page, () =>
    (window as any).__SHADOW_FAKE__.requestApproval({ session_id: "s2" }),
  );
  await expect(page.locator('[data-session-id="s2"]')).toHaveAttribute(
    "data-badge",
    "approval",
  );
  // Ctrl+Tab returns to the last conversation; opening it marks it read.
  await page.locator('[data-session-id="s2"]').click();
  await page.locator('[data-session-id="s1"]').click();
  await page.keyboard.press("Control+Tab");
  await expect(page.locator('[data-session-id="s2"]')).toHaveAttribute(
    "aria-current",
    "page",
  );
  // Right-click menu: rename.
  await page.locator('[data-session-id="s1"]').click({ button: "right" });
  await page.getByRole("menuitem", { name: "Rename" }).click();
  const field = page.getByRole("textbox", { name: /^Rename/ });
  await field.fill("Renamed task");
  await field.press("Enter");
  await expect(page.locator('[data-session-id="s1"]')).toContainText(
    "Renamed task",
  );
});

test("the context & cost chip follows usage and opens a breakdown", async ({
  page,
}) => {
  await chooseLocal(page);
  await prompt(page).fill("Fix add");
  await page.getByRole("button", { name: "Send task" }).click();
  const chip = page.getByRole("button", { name: /Context and cost/ });
  await expect(chip).toContainText("33% · 5.4k / 16k · $0 · local", {
    timeout: 15000,
  });
  await chip.click();
  const breakdown = page.getByRole("dialog", {
    name: "Context and cost breakdown",
  });
  await expect(breakdown).toContainText("Output tokens");
  await expect(breakdown).toContainText("This computer");
  await page.keyboard.press("Escape");
  await expect(breakdown).toHaveCount(0);
});
