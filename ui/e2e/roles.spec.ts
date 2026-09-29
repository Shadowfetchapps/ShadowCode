import { test, expect, type Page } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { installFakeBackend } from "./fakeBackend";

// Roles: a model or vendor CLI per role, Plan → Implement → Review from the
// composer's More menu, consent before a local conversation's work reaches
// cloud roles, role cards and the roles summary, and Settings › Roles.
test.beforeEach(async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  (page as Page & { errors?: string[] }).errors = errors;
  await page.addInitScript(installFakeBackend, { stepMs: 40 });
  await page.goto("/");
  await expect(
    page.getByRole("heading", { name: "What should we work on?" }),
  ).toBeVisible();
});
test.afterEach(async ({ page }) => {
  expect((page as Page & { errors?: string[] }).errors).toEqual([]);
});

type Fake = {
  __SHADOW_FAKE__: {
    log: { method: string; path: string; body: any }[];
    state: any;
  };
};
const prompt = (page: Page) =>
  page.getByRole("textbox", { name: "Message ShadowCode" });
const send = (page: Page) => page.getByRole("button", { name: "Send task" });
const jobPosts = (page: Page) =>
  page.evaluate(() =>
    (window as unknown as Fake).__SHADOW_FAKE__.log.filter(
      (r) => r.path === "/api/jobs" && r.method === "POST",
    ),
  );
const axeClean = async (page: Page, include?: string) => {
  const builder = new AxeBuilder({ page }).withTags([
    "wcag2a",
    "wcag2aa",
    "wcag21aa",
  ]);
  if (include) builder.include(include);
  expect((await builder.analyze()).violations).toEqual([]);
};

async function chooseLocal(page: Page) {
  await page.getByRole("button", { name: /Model for this task/ }).click();
  const search = page.getByRole("combobox", { name: "Search models" });
  await search.fill("qwen3:14b");
  await search.press("Enter");
  await expect(page.getByRole("listbox")).toHaveCount(0);
}
async function openMore(page: Page) {
  const more = page.locator("details.composer-more");
  if ((await more.getAttribute("open")) === null)
    await more.locator("summary").click();
  await expect(more).toHaveAttribute("open", "");
  return more;
}

test("a local conversation runs Claude Code, Codex and a local reviewer as roles after consent", async ({
  page,
}) => {
  await chooseLocal(page);
  const more = await openMore(page);
  const roles = more.getByRole("group", { name: "Roles" });
  await expect(roles).toContainText(
    "Off: each message runs on the model you picked.",
  );
  // A preset turns Plan → Implement → Review on (and closes More).
  await roles
    .getByRole("button", {
      name: "Claude Code plans, Codex implements, local reviews",
    })
    .click();
  await openMore(page);
  await expect(
    roles.getByRole("switch", { name: /Plan → Implement → Review/ }),
  ).toBeChecked();
  await expect(roles).toContainText(
    "Plan: Claude Code · Implement: Codex · Review: qwen3:14b",
  );
  await expect(roles).toContainText(
    "Claude Code and Codex run in the cloud. ShadowCode asks before this conversation's work leaves this computer.",
  );
  await axeClean(page, "details.composer-more");
  await page.keyboard.press("Escape");
  const trigger = page.locator("details.composer-more > summary");
  await expect(trigger).toContainText("Roles");
  await expect(trigger).toHaveAttribute(
    "aria-label",
    /Plan → Implement → Review on/,
  );

  await prompt(page).fill("Fix the add function");
  await send(page).click();
  const consent = page.getByRole("dialog", {
    name: "Send to a cloud provider?",
  });
  await expect(
    consent.getByRole("heading", { name: "Send to Claude Code and Codex?" }),
  ).toBeVisible();
  const cloud = consent.getByRole("list", { name: "Cloud roles" });
  await expect(cloud).toContainText("Plan role Claude Code");
  await expect(cloud).toContainText("Implement role Codex");
  await axeClean(page, ".consent-dialog");
  await consent.getByRole("button", { name: "Send" }).click();

  // Role cards: who did what, on which model, at what cost.
  const cards = page.locator(".role-card");
  await expect(cards).toHaveCount(3, { timeout: 15000 });
  await expect(cards.nth(0)).toContainText("Plan");
  await expect(cards.nth(0)).toContainText("Claude Code");
  await expect(cards.nth(0)).toContainText("Subscription");
  await expect(cards.nth(1)).toContainText("Codex");
  await expect(cards.nth(1)).toContainText("worktree · 1 file · applied");
  await expect(cards.nth(2)).toContainText("qwen3:14b");
  await expect(cards.nth(2)).toContainText("$0 · local");
  await expect(cards.nth(2)).toContainText("Ready to apply");
  await expect(
    page.getByText(
      "Plan → Implement → Review · Claude Code → Codex → qwen3:14b",
    ),
  ).toBeVisible();
  const summary = page.getByRole("region", { name: "Task summary" }).last();
  await expect(summary).toBeVisible({ timeout: 15000 });
  const list = summary.getByRole("list", { name: "Roles" });
  await expect(list).toContainText("Plan Claude Code Done · Subscription");
  await expect(list).toContainText(
    "Implement Codex Done · 1 file (+1 −1) · Subscription",
  );
  await expect(list).toContainText(
    "Review qwen3:14b Ready to apply · $0 · local",
  );
  await expect(summary).toContainText(
    "The changes were applied to the project.",
  );
  await expect(summary).toContainText("src/app.ts");
  await cards.nth(1).getByRole("button").first().click();
  await expect(cards.nth(1)).toContainText("These changes were applied");
  await axeClean(page, ".role-card");
  await page.screenshot({ path: "test-results/roles-cards.png" });
  // Role cards fit a narrow window without sideways scrolling.
  await page.setViewportSize({ width: 520, height: 900 });
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
  const head = await cards
    .nth(1)
    .locator(".subagent-head")
    .evaluate((el) => el.scrollWidth <= el.clientWidth + 1);
  expect(head).toBe(true);
  await page.setViewportSize({ width: 1440, height: 1000 });

  const posts = await jobPosts(page);
  expect(posts).toHaveLength(2);
  expect(posts[0].body.roles).toBe(true);
  expect(posts[0].body.handoff_consent).toBeUndefined();
  expect(posts[1].body.roles).toBe(true);
  expect(posts[1].body.handoff_consent).toBe(true);

  // Allowed providers are remembered: the next task asks nothing.
  await prompt(page).fill("Now add a test");
  await send(page).click();
  await expect(page.locator(".role-card")).toHaveCount(6, { timeout: 15000 });
  await expect(consent).toHaveCount(0);
});

test("roles stay off for Ask, and Settings › Roles chooses a model per role", async ({
  page,
}) => {
  await chooseLocal(page);
  await page.getByRole("radio", { name: "Ask" }).click();
  const more = await openMore(page);
  const roles = more.getByRole("group", { name: "Roles" });
  await roles
    .getByRole("switch", { name: /Plan → Implement → Review/ })
    .check();
  await expect(roles).toContainText("Ask answers on the model you picked.");
  await page.keyboard.press("Escape");
  await prompt(page).fill("Where is add defined?");
  await send(page).click();
  await expect(
    page.getByRole("region", { name: "Task summary" }).last(),
  ).toBeVisible({ timeout: 15000 });
  const posts = await jobPosts(page);
  expect(posts[0].body.roles).toBeUndefined();
  await expect(page.locator(".role-card")).toHaveCount(0);

  // Settings › Roles from the More menu.
  await openMore(page);
  await more
    .getByRole("button", { name: "Choose a model for each role…" })
    .click();
  const settings = page.getByRole("dialog", { name: "Settings" });
  await expect(settings.getByRole("heading", { name: "Roles" })).toBeVisible();
  const review = settings.getByLabel(/^Review/);
  await expect(review).toHaveValue("");
  await review.selectOption("skip");
  await expect(review).toHaveValue("skip");
  const plan = settings.getByLabel(/^Plan/);
  await plan.selectOption("cli:cursor:auto");
  await expect(settings).toContainText(
    "Cursor · Auto · vendor CLI · cloud · Subscription",
  );
  await expect(settings).toContainText(
    "Runs in the cloud: ShadowCode asks before this conversation's work goes to it.",
  );
  // The implement role cannot be skipped.
  await expect(
    settings.getByLabel(/^Implement/).locator("option[value=skip]"),
  ).toHaveCount(0);
  await settings
    .getByRole("button", { name: /Everything on this computer/ })
    .click();
  await expect(
    settings.getByRole("button", { name: /Everything on this computer/ }),
  ).toHaveAttribute("aria-pressed", "true");
  await expect(plan).toHaveValue("local:gguf:qwen");
  await axeClean(page, ".roles-page");
  await page.screenshot({ path: "test-results/roles-settings.png" });
  const saved = await page.evaluate(
    () => (window as unknown as Fake).__SHADOW_FAKE__.state.roles,
  );
  expect(saved).toMatchObject({
    pipeline: true,
    plan: "local:gguf:qwen",
    implement: "local:gguf:qwen",
    review: "local:gguf:qwen",
    preset: "all-local",
  });
});

test("offline, cloud roles are refused with the reason", async ({ page }) => {
  // The later init script replaces the default fake.
  await page.addInitScript(installFakeBackend, {
    stepMs: 40,
    network: "offline",
    roles: { pipeline: true, plan: "cli:codex", review: "skip" },
  });
  await page.reload();
  await chooseLocal(page);
  const more = await openMore(page);
  await expect(more.getByRole("group", { name: "Roles" })).toContainText(
    "Offline mode: the plan role uses Codex",
  );
  await page.keyboard.press("Escape");
  await prompt(page).fill("Fix it");
  await send(page).click();
  await expect(
    page.getByText(/Offline mode: the plan role uses Codex/).first(),
  ).toBeVisible();
  await expect(page.locator(".role-card")).toHaveCount(0);
});
