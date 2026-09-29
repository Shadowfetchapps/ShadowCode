import { test, expect, type Page } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { installFakeBackend } from "./fakeBackend";
import { installFakeTools } from "./fakeTools";
import { installFakeVoice } from "./fakeVoice";
import { installFakeSettings } from "./fakeSettings";

// Consumer polish: every Settings page and drawer panel renders its content
// in place (no stretched rows, no clipped labels, no raw error prefixes) and
// stays free of accessibility violations in both themes.
test.beforeEach(async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  (page as Page & { errors?: string[] }).errors = errors;
  await page.addInitScript(installFakeBackend, { stepMs: 60 });
  await page.addInitScript(installFakeTools, {});
  await page.addInitScript(installFakeVoice, { installed: true });
  await page.addInitScript(installFakeSettings);
  await page.goto("/");
  await expect(
    page.getByRole("heading", { name: "What should we work on?" }),
  ).toBeVisible();
});
test.afterEach(async ({ page }) => {
  expect((page as Page & { errors?: string[] }).errors).toEqual([]);
});

const SECTIONS = [
  "Accounts",
  "Local models",
  "Code intelligence",
  "Voice",
  "Permissions & network",
  "Appearance",
  "Remote access",
  "Your data",
  "Advanced",
  "About",
];
const ADVANCED = [
  "Skills",
  "Health",
  "MCP",
  "Plugins",
  "Hooks",
  "Guardian",
  "Vendor tools",
];
const slug = (text: string) => text.toLowerCase().replace(/\W+/g, "-");

async function axeClean(page: Page, include?: string) {
  let builder = new AxeBuilder({ page }).withTags([
    "wcag2a",
    "wcag2aa",
    "wcag21aa",
  ]);
  if (include) builder = builder.include(include);
  const results = await builder.analyze();
  expect(results.violations).toEqual([]);
}

test("every Settings page shows its content from the top, light and dark", async ({
  page,
}) => {
  // Two themes × sixteen pages, each with an axe pass.
  test.setTimeout(150_000);
  for (const theme of ["light", "dark"]) {
    await page.evaluate(
      (t) => (document.documentElement.dataset.theme = t),
      theme,
    );
    await page.keyboard.press("Control+,");
    const dialog = page.getByRole("dialog", { name: "Settings" });
    for (const section of SECTIONS) {
      await dialog.getByRole("button", { name: section, exact: true }).click();
      const body = dialog.locator(".settings-body");
      await expect(body.getByText(/^(Reading|Loading) .*…$/)).toHaveCount(0);
      // No raw class prefix and no load error on any page.
      await expect(body).not.toContainText("ApiError");
      await expect(body.locator(".load-error")).toHaveCount(0);
      if (section !== "Advanced") {
        await axeClean(page, '[role="dialog"]');
        if (theme === "light")
          await page.screenshot({
            path: `test-results/settings-${slug(section)}.png`,
          });
        continue;
      }
      const tabs = dialog.getByRole("tablist", { name: "Advanced sections" });
      for (const name of ADVANCED) {
        await tabs.getByRole("tab", { name }).click();
        await expect(tabs.getByRole("tab", { name })).toHaveAttribute(
          "aria-selected",
          "true",
        );
        await expect(body).not.toContainText("ApiError");
        // A short tab (an error, a small panel) must not stretch the tab
        // bar: it stays one row tall at the top of the page.
        const bar = await tabs.boundingBox();
        const heading = await body.locator("h3").first().boundingBox();
        expect(bar).not.toBeNull();
        expect(heading).not.toBeNull();
        expect(bar!.height).toBeLessThan(60);
        expect(bar!.y - heading!.y).toBeLessThan(60);
        await axeClean(page, '[role="dialog"]');
        if (theme === "light" && ["Health", "MCP"].includes(name))
          await page.screenshot({
            path: `test-results/settings-advanced-${slug(name)}.png`,
          });
      }
    }
    await page.keyboard.press("Escape");
    await expect(dialog).toHaveCount(0);
  }
});

test("a page that cannot load says why and offers Try again", async ({
  page,
}) => {
  // The engine fails Code intelligence once; Try again recovers.
  await page.evaluate(() => {
    const w = window as unknown as {
      __SHADOW_TEST_TRANSPORT__: {
        request: (p: string, m: string, b: unknown) => Promise<unknown>;
      };
    };
    const bridge = w.__SHADOW_TEST_TRANSPORT__;
    const request = bridge.request.bind(bridge);
    let failed = false;
    bridge.request = async (path, method, body) => {
      if (path === "/api/code-intel/status" && !failed) {
        failed = true;
        throw new Error("The engine is busy. Try again in a moment.");
      }
      return request(path, method, body);
    };
  });
  await page.keyboard.press("Control+,");
  const dialog = page.getByRole("dialog", { name: "Settings" });
  await dialog
    .getByRole("button", { name: "Code intelligence", exact: true })
    .click();
  const failure = dialog.locator(".load-error");
  await expect(failure).toHaveText(
    /^The engine is busy\. Try again in a moment\.Try again$/,
  );
  // The message sits under the page title, not in the middle of the page.
  const title = await dialog.locator(".settings-page h3").boundingBox();
  const box = await failure.boundingBox();
  expect(box && title && box.y - title.y).toBeLessThan(60);
  await axeClean(page, '[role="dialog"]');
  await failure.getByRole("button", { name: "Try again" }).click();
  await expect(failure).toHaveCount(0);
  await expect(dialog.getByText("TypeScript and JavaScript")).toBeVisible();
});

test("at 520 px Settings section names wrap instead of being cut off", async ({
  page,
}) => {
  await page.setViewportSize({ width: 520, height: 820 });
  await page.keyboard.press("Control+,");
  const nav = page.getByRole("navigation", { name: "Settings sections" });
  await expect(nav).toBeVisible();
  const clipped = await nav
    .locator("button")
    .evaluateAll((buttons) =>
      buttons
        .filter((b) => b.scrollWidth > b.clientWidth + 1)
        .map((b) => b.textContent),
    );
  expect(clipped).toEqual([]);
  await expect(
    nav.getByRole("button", { name: "Permissions & network" }),
  ).toBeVisible();
  await page.screenshot({ path: "test-results/settings-520.png" });
});

test("the drawer keeps Close on its tab row and the Tasks panel is keyboard friendly", async ({
  page,
}) => {
  await page.getByRole("button", { name: "Review changes" }).click();
  const drawer = page.getByRole("complementary", { name: "Drawer" });
  const close = drawer.getByRole("button", { name: "Close drawer" });
  const first = drawer.locator(".drawer-tab-list button").first();
  const closeBox = await close.boundingBox();
  const firstBox = await first.boundingBox();
  // Close sits on the first row of tabs, at the right edge.
  expect(
    closeBox && firstBox && Math.abs(closeBox.y - firstBox.y),
  ).toBeLessThan(8);
  // At the default width all seven tabs fit on that one row.
  const rows = await drawer
    .locator(".drawer-tab-list button")
    .evaluateAll(
      (buttons) =>
        new Set(buttons.map((b) => Math.round(b.getBoundingClientRect().top)))
          .size,
    );
  expect(rows).toBe(1);
  await drawer.getByRole("button", { name: "Tasks", exact: true }).click();
  const search = drawer.getByRole("textbox", { name: "Search tasks" });
  await expect(search).toBeVisible();
  await expect(drawer).not.toContainText("session");
  await axeClean(page, ".drawer");
  await page.screenshot({ path: "test-results/drawer-tasks.png" });
  // Opening a row and its actions are separate buttons; Tab reaches the
  // actions of a row that is not the open one.
  await drawer.getByRole("button", { name: "New task", exact: true }).click();
  await expect(drawer.locator(".item")).toHaveCount(2);
  const other = drawer.locator(".item:not(.active)");
  await other.locator(".item-open").focus();
  await page.keyboard.press("Tab");
  await expect(other.getByRole("button", { name: /^Rename/ })).toBeFocused();
  // Delete asks in the app's own dialog, then removes the task.
  await other.getByRole("button", { name: /^Delete/ }).click();
  const confirm = page.getByRole("dialog", { name: "Delete this task?" });
  await expect(confirm).toBeVisible();
  await axeClean(page, '[role="dialog"]');
  await confirm.getByRole("button", { name: "Delete" }).click();
  await expect(confirm).toHaveCount(0);
  await expect(drawer.locator(".item")).toHaveCount(1);
  await expect(page.locator(".toast").last()).toContainText("Task deleted");
});

test("Processes and Worktrees fields match the app's other fields", async ({
  page,
}) => {
  await page.keyboard.press("Control+k");
  await page.keyboard.type("Goals and milestones");
  await page.keyboard.press("Enter");
  const tools = page.getByRole("region", { name: "Tools" });
  const goal = tools.getByRole("textbox", { name: "Goal instruction" });
  const style = (el: Element) => {
    const s = getComputedStyle(el);
    return [s.backgroundColor, s.borderRadius, s.borderStyle].join(" ");
  };
  const reference = await goal.evaluate(style);
  await tools.getByRole("button", { name: "Processes" }).click();
  expect(await tools.getByLabel("Background command").evaluate(style)).toBe(
    reference,
  );
  await expect(tools.getByText("No processes yet")).toBeVisible();
  await tools.getByRole("button", { name: "Worktrees" }).click();
  expect(
    await tools.getByLabel("Starting commit or branch").evaluate(style),
  ).toBe(reference);
});

test("the More menu says why its options are unavailable", async ({ page }) => {
  const more = page.locator("details.composer-more");
  await more.locator("summary").click();
  await expect(more.locator(".composer-more-menu")).toContainText(
    "Type a task first to run it in a new worktree or compare models on it.",
  );
  await axeClean(page, ".composer-more-menu");
  await page.screenshot({ path: "test-results/composer-more-empty.png" });
});
