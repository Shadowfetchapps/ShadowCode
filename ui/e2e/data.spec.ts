import { test, expect, type Page } from "@playwright/test";
import { installFakeBackend } from "./fakeBackend";

// Settings › Your data in the desktop window (fake engine): back up, preview
// and schedule a restore, cancel it, schedule a reset, check and repair.
test.beforeEach(async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  (page as Page & { errors?: string[] }).errors = errors;
  await page.addInitScript(installFakeBackend, { stepMs: 60 });
  await page.goto("/");
  await expect(
    page.getByRole("heading", { name: "What should we work on?" }),
  ).toBeVisible();
});
test.afterEach(async ({ page }) => {
  expect((page as Page & { errors?: string[] }).errors).toEqual([]);
});

const log = (page: Page) =>
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

test("backs up, schedules a restore and a reset, and repairs", async ({
  page,
}) => {
  await page.keyboard.press("Control+,");
  const settings = page.getByRole("dialog", { name: "Settings" });
  await settings
    .getByRole("button", { name: "Your data", exact: true })
    .click();
  await expect(
    settings.getByRole("heading", { name: "Your data", exact: true }),
  ).toBeVisible();
  await expect(
    settings.getByText("/home/dev/.config/shadow-agent"),
  ).toBeVisible();

  // A backup with API keys warns first.
  await settings
    .getByLabel("Include API keys and remote-access pairing")
    .check();
  await expect(settings.getByRole("note")).toContainText(
    "Anyone who gets this backup can use your API keys",
  );
  await settings.getByRole("button", { name: "Back up now" }).click();
  const backups = settings.getByRole("list", { name: "Backups" });
  await expect(backups.getByRole("listitem")).toHaveCount(2);
  await expect(backups).toContainText("includes API keys");
  await page.screenshot({ path: "test-results/settings-your-data.png" });

  // Restore: preview, schedule, cancel.
  await backups
    .getByRole("button", { name: /^Restore the backup from/ })
    .last()
    .click();
  const restore = page.getByRole("dialog", { name: "Restore this backup?" });
  await expect(restore).toContainText("12 conversations");
  await restore.getByRole("button", { name: "Restore at next start" }).click();
  await expect(
    settings.getByText(/A restore from .* is scheduled/),
  ).toBeVisible();
  await settings.getByRole("button", { name: "Cancel the restore" }).click();
  await expect(
    settings.getByText(/A restore from .* is scheduled/),
  ).toHaveCount(0);

  // Reset asks first and deletes nothing.
  await settings.getByRole("button", { name: "Reset ShadowCode…" }).click();
  const reset = page.getByRole("dialog", { name: "Reset ShadowCode?" });
  await expect(reset).toContainText("Nothing is deleted");
  await reset.getByRole("button", { name: "Reset at next start" }).click();
  await expect(
    settings.getByText("A reset is scheduled.", { exact: false }),
  ).toBeVisible();

  await settings.getByRole("button", { name: "Check and repair" }).click();
  await expect(
    settings.getByRole("list", { name: "Check and repair results" }),
  ).toContainText("Database integrity");

  const requests = await log(page);
  expect(
    requests.find((r) => r.path === "/api/data/backups" && r.method === "POST")
      ?.body,
  ).toEqual({ include_secrets: true, folder: "" });
  expect(requests.find((r) => r.path === "/api/data/reset")?.body).toEqual({
    confirm: "reset",
  });
  expect(
    requests.some(
      (r) => r.path === "/api/data/pending" && r.method === "DELETE",
    ),
  ).toBe(true);
});
