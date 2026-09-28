import { test, expect, type Page } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { installFakeBackend, type FakeOptions } from "./fakeBackend";

// Settings › About and the status-bar update notice (fake engine).
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

async function openAbout(page: Page) {
  await page
    .getByRole("complementary", { name: "Projects and tasks" })
    .getByRole("button", { name: "Settings" })
    .click();
  const settings = page.getByRole("dialog", { name: "Settings" });
  await settings.getByRole("button", { name: "About" }).click();
  return settings;
}

test("no notice without a newer release; About shows the facts", async ({
  page,
}) => {
  await start(page, {});
  const status = page.locator("footer.statusline");
  await expect(status).toContainText("v0.33.0");
  await expect(
    status.getByRole("button", { name: /Update available/ }),
  ).toHaveCount(0);
  const settings = await openAbout(page);
  await expect(
    settings.getByRole("heading", { name: "About ShadowCode" }),
  ).toBeVisible();
  await expect(settings.getByText("e15c4480e65d")).toBeVisible();
  await expect(settings.getByText("Apache License 2.0")).toBeVisible();
  await expect(
    settings.getByText(/originally created by Shadowfetch/).first(),
  ).toBeVisible();
  await expect(
    settings.getByRole("link", { name: "Release notes" }),
  ).toHaveAttribute(
    "href",
    "https://github.com/Shadowfetchapps/ShadowCode/releases/tag/v0.33.0",
  );
  await expect(settings.getByRole("status")).toHaveText(
    "Not checked yet. ShadowCode checks once a day.",
  );
  await settings.getByRole("button", { name: "Check now" }).click();
  await expect(settings.getByRole("status")).toHaveText(
    "You have the latest version (checked just now).",
  );
  // Turning the daily check off saves updates.check.
  await settings.getByLabel("Check for updates once a day").uncheck();
  await expect(
    settings.getByLabel("Check for updates once a day"),
  ).not.toBeChecked();
  const saved = await page.evaluate(
    () => (window as any).__SHADOW_FAKE__.state.config.updates,
  );
  expect(saved).toEqual({ check: false });
});

test("a newer release shows a quiet notice that opens About with the steps", async ({
  page,
}) => {
  await start(page, { updateAvailable: "appimage" });
  const notice = page
    .locator("footer.statusline")
    .getByRole("button", { name: "Update available: 0.34.0" });
  await expect(notice).toBeVisible();
  for (const theme of ["light", "dark"]) {
    await page.evaluate(
      (t) => (document.documentElement.dataset.theme = t),
      theme,
    );
    const results = await new AxeBuilder({ page })
      .include("footer.statusline")
      .withTags(["wcag2a", "wcag2aa", "wcag21aa"])
      .analyze();
    expect(results.violations).toEqual([]);
  }
  await notice.click();
  const settings = page.getByRole("dialog", { name: "Settings" });
  const card = settings.getByRole("region", { name: "ShadowCode 0.34.0" });
  await expect(card).toBeVisible();
  await expect(
    card.getByRole("link", { name: "Release notes" }),
  ).toHaveAttribute(
    "href",
    "https://github.com/Shadowfetchapps/ShadowCode/releases/tag/v0.34.0",
  );
  await expect(card).toContainText("install-appimage.sh");
  await expect(card).toContainText("RELEASE-AUTH.sig");
  // The notice and the About page meet WCAG AA in both themes.
  for (const theme of ["light", "dark"]) {
    await page.evaluate(
      (t) => (document.documentElement.dataset.theme = t),
      theme,
    );
    const results = await new AxeBuilder({ page })
      .include('[role="dialog"]')
      .withTags(["wcag2a", "wcag2aa", "wcag21aa"])
      .analyze();
    expect(results.violations).toEqual([]);
    await page.screenshot({ path: `test-results/about-update-${theme}.png` });
  }
  await page.evaluate(() => (document.documentElement.dataset.theme = "light"));
  // Hiding it clears the status bar until a newer version appears.
  await card
    .getByRole("button", { name: "Hide the notice until the next version" })
    .click();
  await expect(
    card.getByRole("button", {
      name: "Hide the notice until the next version",
    }),
  ).toHaveCount(0);
  await page.keyboard.press("Escape");
  await expect(settings).toHaveCount(0);
  await expect(
    page
      .locator("footer.statusline")
      .getByRole("button", { name: /Update available/ }),
  ).toHaveCount(0);
});

test("Debian installs are told to use the package manager", async ({
  page,
}) => {
  await start(page, { updateAvailable: "deb" });
  await page
    .locator("footer.statusline")
    .getByRole("button", { name: "Update available: 0.34.0" })
    .click();
  const card = page
    .getByRole("dialog", { name: "Settings" })
    .getByRole("region", { name: "ShadowCode 0.34.0" });
  await expect(card).toContainText("Update through your package manager");
  await expect(card.getByRole("button", { name: "Copy command" })).toHaveCount(
    0,
  );
  await expect(
    card.getByRole("link", { name: "Install instructions" }),
  ).toHaveAttribute(
    "href",
    "https://github.com/Shadowfetchapps/ShadowCode/blob/v0.34.0/README.md#debian-package",
  );
});
