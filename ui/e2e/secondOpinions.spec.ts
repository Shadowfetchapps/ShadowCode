import { test, expect, type Page } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { installFakeBackend } from "./fakeBackend";
import { installFakeTools } from "./fakeTools";
import { installFakeSecondOpinions } from "./fakeSecondOpinions";

// Second opinions against the fake engine: a review of the staged changes
// before a commit, a review of one task in the Review view, and another
// model's view of an answer. Nothing here reaches a real model.
test.beforeEach(async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  (page as Page & { errors?: string[] }).errors = errors;
  await page.addInitScript(installFakeBackend, { stepMs: 60 });
  await page.addInitScript(installFakeTools, {});
  await page.addInitScript(installFakeSecondOpinions);
  await page.goto("/");
  await expect(
    page.getByRole("heading", { name: "What should we work on?" }),
  ).toBeVisible();
});
test.afterEach(async ({ page }) => {
  expect((page as Page & { errors?: string[] }).errors).toEqual([]);
});

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
const drawer = (page: Page) =>
  page.getByRole("complementary", { name: "Drawer" });

/** A finished task on the local model: it changed src/app.ts. */
async function runLocalTask(page: Page, text = "Fix the add function") {
  await page.getByRole("button", { name: /Model for this task/ }).click();
  const search = page.getByRole("combobox", { name: "Search models" });
  await search.fill("qwen3:14b");
  await search.press("Enter");
  await page.getByRole("textbox", { name: "Message ShadowCode" }).fill(text);
  await page.getByRole("button", { name: "Send task" }).click();
  await expect(
    page.getByRole("region", { name: "Task summary" }).last(),
  ).toBeVisible({ timeout: 15000 });
}

test("reviews the staged changes before a commit, with consent for a cloud reviewer", async ({
  page,
}) => {
  await runLocalTask(page);
  await page
    .locator("header.top")
    .getByRole("button", { name: "Review changes" })
    .click();
  await drawer(page)
    .locator(".drawer-tabs")
    .getByRole("button", { name: "Git" })
    .click();
  const panel = drawer(page);
  await panel.getByRole("button", { name: "Stage all" }).click();
  await expect(panel.getByText(/1 file staged/)).toBeVisible();
  const section = panel.getByRole("region", { name: "Review before commit" });
  // The only other ready model on this computer wrote the change, so no
  // cloud reviewer is chosen for local work without asking.
  const reviewer = section.getByRole("combobox", { name: "Reviewer model" });
  await expect(reviewer).toHaveValue("");
  await section
    .getByRole("checkbox", { name: "Review before every commit" })
    .check();
  await reviewer.selectOption({ label: "Codex · GPT-6-Astra" });
  await panel.getByRole("textbox", { name: "Commit message" }).fill("Fix add");
  await panel.getByRole("button", { name: "Review and commit" }).click();
  // Local work goes to a cloud reviewer only after the consent dialog.
  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText("This second opinion goes to a cloud");
  await expect(dialog).toContainText("The work ran on qwen3:14b");
  await expect(
    new AxeBuilder({ page })
      .include('[role="dialog"]')
      .withTags(["wcag2a", "wcag2aa"])
      .analyze(),
  ).resolves.toMatchObject({ violations: [] });
  await dialog.getByRole("button", { name: "Send" }).click();
  await expect(dialog).toHaveCount(0);
  await expect(
    panel.getByText(/The commit waits until you have seen the findings/),
  ).toBeVisible();
  // The findings arrive next to the reviewed hunk; nothing was committed.
  const finding = section.getByRole("article", {
    name: "High: Negative numbers are not covered",
  });
  await expect(finding).toBeVisible({ timeout: 5000 });
  await expect(
    section.getByRole("region", { name: "Findings in src/math.js" }),
  ).toContainText("return a + b;");
  await expect(section).toContainText("3,100 tokens");
  await expect(section).toContainText("read-only");
  let log = await fakeLog(page);
  expect(log.some((r) => r.path === "/api/workspace/git/commit")).toBe(false);
  expect(
    log.find((r) => r.method === "POST" && r.path === "/api/second-opinions")
      ?.body,
  ).toMatchObject({ kind: "review", source: "staged", consent: true });
  await expect(
    new AxeBuilder({ page })
      .include('[aria-label="Review before commit"]')
      .withTags(["wcag2a", "wcag2aa"])
      .analyze(),
  ).resolves.toMatchObject({ violations: [] });
  await page.screenshot({ path: "test-results/second-opinion-git.png" });
  // Open findings: the commit is still one click away, labelled plainly.
  await expect(panel.getByText(/found 2 open findings/)).toBeVisible();
  await expect(
    panel.getByRole("button", { name: "Commit anyway" }),
  ).toBeEnabled();
  // Dismiss one, ask the agent to fix the other in the conversation.
  const general = section.getByRole("article", {
    name: "Low: No test covers the change",
  });
  await general.getByRole("button", { name: "Dismiss" }).click();
  await expect(
    general.getByRole("button", { name: "Show again" }),
  ).toBeVisible();
  await finding
    .getByRole("button", { name: "Ask the agent to fix this" })
    .click();
  await expect(
    finding.getByText("Fix queued in the conversation"),
  ).toBeVisible();
  await expect(
    page.getByText("Fix queued in this conversation."),
  ).toBeVisible();
  log = await fakeLog(page);
  const queued = log.filter(
    (r) => r.method === "POST" && r.path === "/api/jobs",
  );
  expect(queued.at(-1)?.body.task).toContain(
    "found a problem in src/math.js at line 2: Negative numbers are not covered",
  );
  expect(queued.at(-1)?.body.session_id).toBe("s1");
  // Nothing is left open; the commit goes ahead once the fix task (which
  // holds the project) has finished.
  await expect(
    panel.getByText("The review is done. Commit when you are ready."),
  ).toBeVisible();
  const commit = panel.getByRole("button", { name: "Commit", exact: true });
  await expect(commit).toBeEnabled({ timeout: 15000 });
  await commit.click();
  await expect(page.getByText("Committed")).toBeVisible();
  log = await fakeLog(page);
  expect(
    log.find((r) => r.path === "/api/workspace/git/commit")?.body.message,
  ).toBe("Fix add");
});

test("reviews one task in the Review view with findings under their hunks", async ({
  page,
}) => {
  await runLocalTask(page);
  await page
    .getByRole("region", { name: "Task summary" })
    .getByRole("button", { name: "Review changes" })
    .click();
  const review = page.getByRole("region", { name: "Review changes" });
  const second = review.getByRole("region", { name: "Second opinion" });
  await second
    .getByRole("combobox", { name: "Reviewer model" })
    .selectOption({ label: "Codex · GPT-6-Astra" });
  await second
    .getByRole("button", { name: "Review with another model" })
    .click();
  await page.getByRole("dialog").getByRole("button", { name: "Send" }).click();
  const finding = review.getByRole("article", {
    name: "Medium: Version bumped without a release note",
  });
  await expect(finding).toBeVisible({ timeout: 5000 });
  // It sits under the VERSION hunk, after the add hunk.
  const hunks = review.locator(".review-hunk");
  await expect(hunks.nth(1)).toContainText("VERSION = 2");
  await expect(hunks.nth(1).getByRole("article")).toHaveCount(1);
  await expect(hunks.nth(0).getByRole("article")).toHaveCount(0);
  await expect(review.getByLabel("1 open finding")).toBeVisible();
  await expect(second).toContainText("No test covers the change");
  await page.screenshot({ path: "test-results/second-opinion-review.png" });
});

test("asks another model about an answer and continues with it", async ({
  page,
}) => {
  await runLocalTask(page);
  const answer = page.locator(".msg-agent").filter({
    hasText: "and ran the tests",
  });
  await answer.hover();
  await answer.getByRole("button", { name: "Ask another model" }).click();
  const form = page.getByRole("form", { name: "Ask another model" });
  await form
    .getByRole("combobox", { name: "Model for the second opinion" })
    .selectOption({ label: "Codex · GPT-6-Astra" });
  await form.getByRole("textbox").fill("Is the fix complete?");
  await form.getByRole("button", { name: "Ask" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Send" }).click();
  const card = page.getByRole("region", {
    name: "Second opinion from Codex · GPT-6-Astra",
  });
  await expect(card).toContainText("nothing tests negative numbers", {
    timeout: 5000,
  });
  await expect(card).toContainText("Is the fix complete?");
  await expect(card).toContainText("3,100 tokens");
  const log = await fakeLog(page);
  expect(
    log.find((r) => r.method === "POST" && r.path === "/api/second-opinions")
      ?.body,
  ).toMatchObject({
    kind: "ask",
    source: "task",
    task_id: "t1",
    model: "cli:codex:gpt-6-astra",
    question: "Is the fix complete?",
    consent: true,
  });
  // No file changed and no turn was added to the conversation.
  expect(
    log.filter((r) => r.path === "/api/jobs" && r.method === "POST"),
  ).toHaveLength(1);
  await card
    .getByRole("button", { name: "Continue with Codex · GPT-6-Astra" })
    .click();
  await expect(
    page.getByRole("textbox", { name: "Message ShadowCode" }),
  ).toHaveValue(/Here is your second opinion on the last answer/);
  await expect(
    page.getByRole("button", { name: /Model for this task/ }),
  ).toContainText("Codex · GPT-6-Astra");
  await page.screenshot({ path: "test-results/second-opinion-card.png" });
});
