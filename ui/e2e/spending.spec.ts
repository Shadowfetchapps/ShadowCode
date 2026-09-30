import { test, expect, type Page } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { installFakeBackend, type FakeOptions } from "./fakeBackend";

// Spending limits on paid API models, a busy provider, "Try on…", "Resume
// at …" after a plan limit, Run details and the logs folder, against the
// deterministic fake engine. Nothing here reaches a real model.
type Call = { method: string; path: string; body: any };

async function start(page: Page, options: FakeOptions) {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  (page as Page & { errors?: string[] }).errors = errors;
  await page.addInitScript(installFakeBackend, { stepMs: 60, ...options });
  await page.goto("/");
  await expect(
    page.getByRole("heading", { name: "What should we work on?" }),
  ).toBeVisible();
}
test.afterEach(async ({ page }) => {
  expect((page as Page & { errors?: string[] }).errors).toEqual([]);
});

const trigger = (page: Page) =>
  page.getByRole("button", { name: /Model for this task/ });
const prompt = (page: Page) =>
  page.getByRole("textbox", { name: "Message ShadowCode" });
const send = (page: Page) => page.getByRole("button", { name: "Send task" });
const fakeLog = (page: Page) =>
  page.evaluate(
    () =>
      (window as unknown as { __SHADOW_FAKE__: { log: Call[] } })
        .__SHADOW_FAKE__.log,
  );
const posts = async (page: Page, path: string) =>
  (await fakeLog(page)).filter((r) => r.path === path && r.method === "POST");
/** The fake Codex plan resets two hours from now: "tomorrow at …" when the
 * suite runs late in the evening. */
const RESUME_ON_CODEX = /^Resume on Codex (?:tomorrow )?at /;

async function chooseBySearch(page: Page, text: string) {
  await trigger(page).click();
  const search = page.getByRole("combobox", { name: "Search models" });
  await expect(search).toBeFocused();
  await search.fill(text);
  await search.press("Enter");
  await expect(page.getByRole("listbox")).toHaveCount(0);
}

async function accessible(page: Page, name: string) {
  for (const theme of ["light", "dark"]) {
    await page.evaluate(
      (t) => (document.documentElement.dataset.theme = t),
      theme,
    );
    const results = await new AxeBuilder({ page })
      .withTags(["wcag2a", "wcag2aa", "wcag21aa"])
      .analyze();
    expect(results.violations).toEqual([]);
    await page.screenshot({ path: `test-results/${name}-${theme}.png` });
  }
  await page.evaluate(() => (document.documentElement.dataset.theme = "light"));
}

test("a paid task shows its estimate, pauses at the spending limit, and Continue raises it", async ({
  page,
}) => {
  await start(page, { openrouterKey: true, spendLimitOnApi: true });
  await chooseBySearch(page, "Gemini 2.5 Flash");
  await prompt(page).fill("Fix the add function");
  await expect(page.getByText("Next message: about $0.01–$0.03")).toBeVisible();
  await send(page).click();
  const card = page.getByRole("region", { name: "Spending limit reached" });
  await expect(card).toContainText("This task reached its spending limit", {
    timeout: 15000,
  });
  await expect(card).toContainText(
    "It has spent $1.04 on paid models, and the limit for one task is $1.00.",
  );
  // The calm 75% note came first, and the task is waiting, not finished.
  await expect(
    page.getByText(
      "This task has spent $0.80 of its $1.00 limit on paid models.",
    ),
  ).toBeVisible();
  await expect(page.getByRole("region", { name: "Task summary" })).toHaveCount(
    0,
  );
  await accessible(page, "spend-limit");
  await card
    .getByRole("button", { name: "Continue (limit raised to $2.00)" })
    .click();
  await expect(card).toContainText(
    "Continuing. This task's limit is now $2.00.",
  );
  await expect(card.getByRole("button")).toHaveCount(0);
  const summary = page.getByRole("region", { name: "Task summary" });
  await expect(summary).toContainText("Finished", { timeout: 15000 });
  const [answer] = await posts(page, "/api/jobs/j1/spending");
  expect(answer.body).toEqual({ prompt_id: "p-j1", action: "continue" });
  // Run details say exactly what ran.
  await summary.getByText("Run details").click();
  await expect(summary).toContainText("api:openrouter:google/gemini-2.5-flash");
  await expect(summary).toContainText("4f2a9c1b7d3e");
  // Subscriptions and local models show no estimate.
  await chooseBySearch(page, "qwen3:14b");
  await prompt(page).fill("Another change");
  await expect(page.getByText(/^Next message:/)).toHaveCount(0);
});

test("Stop ends the task cleanly and Try on continues it on a model you pick", async ({
  page,
}) => {
  await start(page, { openrouterKey: true, spendLimitOnApi: true });
  await chooseBySearch(page, "Gemini 2.5 Flash");
  await prompt(page).fill("Fix the add function");
  await send(page).click();
  const card = page.getByRole("region", { name: "Spending limit reached" });
  await card.getByRole("button", { name: "Stop" }).click({ timeout: 15000 });
  await expect(card).toContainText(
    "Stopped at your spending limit. Changes made so far are kept.",
  );
  const summary = page.getByRole("region", { name: "Task summary" });
  await expect(summary).toContainText("Stopped", { timeout: 15000 });
  await summary.getByRole("button", { name: "Try on…" }).click();
  const search = page.getByRole("combobox", { name: "Search models" });
  await expect(search).toBeFocused();
  await expect(
    page.getByText("Pick a model to continue the stopped task on"),
  ).toBeVisible();
  await search.fill("qwen3:14b");
  await search.press("Enter");
  await expect(summary).toHaveCount(2, { timeout: 15000 });
  const jobs = await posts(page, "/api/jobs");
  expect(jobs.map((r) => r.body.model)).toEqual([
    "api:openrouter:google/gemini-2.5-flash",
    "local:gguf:qwen",
  ]);
  expect(jobs[1].body.task).toBe(
    "Continue where Google: Gemini 2.5 Flash stopped. The request was:\n\nFix the add function",
  );
  expect(jobs[1].body.session_id).toBe(jobs[0].body.session_id);
});

test("a busy provider shows its retries, then says it answered", async ({
  page,
}) => {
  await start(page, { openrouterKey: true, retryOnApi: true });
  await chooseBySearch(page, "Gemini 2.5 Flash");
  await prompt(page).fill("Fix the add function");
  await send(page).click();
  await expect(
    page.getByText("Provider busy, retrying (2 of 5) in 4 s…"),
  ).toBeVisible({ timeout: 15000 });
  await expect(
    page.getByText("The provider was busy; it answered after 2 retries."),
  ).toBeVisible({ timeout: 15000 });
  await expect(
    page.getByRole("region", { name: "Task summary" }),
  ).toContainText("Finished", { timeout: 15000 });
});

test("after a plan limit, Resume at the reset time can be scheduled and cancelled", async ({
  page,
}) => {
  await start(page, { limitOnCodex: true });
  await page.evaluate(() => {
    const fake = (window as unknown as { __SHADOW_FAKE__: { state: any } })
      .__SHADOW_FAKE__.state;
    fake.config.limits = { on_limit: "ask", fallback_model: "" };
  });
  await chooseBySearch(page, "GPT-6-Astra");
  await prompt(page).fill("Fix the add function");
  await send(page).click();
  const card = page.getByRole("region", { name: "Plan limit reached" });
  await expect(card).toContainText("Codex reached its plan limit.", {
    timeout: 15000,
  });
  await expect(card.getByRole("button", { name: "Try on…" })).toBeVisible();
  const resume = card.getByRole("button", { name: RESUME_ON_CODEX });
  await resume.click();
  const note = page.getByRole("status", { name: "Scheduled resume" });
  await expect(note).toContainText(
    /Will resume on Codex (?:tomorrow )?at .+, when its plan limit resets\./,
  );
  await expect(resume).toHaveCount(0);
  const [scheduled] = await posts(
    page,
    `/api/sessions/${(await posts(page, "/api/jobs"))[0].body.session_id}/scheduled-resume`,
  );
  expect(scheduled.body).toEqual({ job_id: "j1" });
  await accessible(page, "resume-scheduled");
  await note.getByRole("button", { name: "Cancel resume" }).click();
  await expect(note).toContainText("The resume on Codex was cancelled.");
  await expect(
    card.getByRole("button", { name: RESUME_ON_CODEX }),
  ).toBeVisible();
});

test("Settings keep the spending limits and About opens the logs folder", async ({
  page,
}) => {
  await start(page, {});
  await page
    .getByRole("complementary", { name: "Projects and tasks" })
    .getByRole("button", { name: "Settings" })
    .click();
  const settings = page.getByRole("dialog", { name: "Settings" });
  const limits = settings.getByRole("article", { name: "Spending limits" });
  await expect(limits).toContainText(
    "Today so far: about $0.42 (estimated) of $10.00",
  );
  await limits
    .getByRole("textbox", { name: /one task spends more than/ })
    .fill("2.50");
  await limits.getByRole("checkbox", { name: /all tasks together/ }).uncheck();
  await limits.getByRole("button", { name: "Save limits" }).click();
  await expect
    .poll(async () =>
      (await fakeLog(page)).find(
        (r) => r.method === "PUT" && r.path === "/api/config",
      ),
    )
    .toMatchObject({
      body: { values: { spending: { task_usd: 2.5, daily_usd: null } } },
    });
  await settings.getByRole("button", { name: "About" }).click();
  await settings.getByRole("button", { name: "Open logs folder" }).click();
  const invoked = (await fakeLog(page)).filter(
    (c) => c.method === "INVOKE" && c.path === "open_logs_folder",
  );
  expect(invoked).toHaveLength(1);
  expect(invoked[0].body).toBeUndefined();
});
