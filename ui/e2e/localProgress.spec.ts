import { test, expect } from "@playwright/test";
import { installFakeBackend } from "./fakeBackend";

test("local startup reaches the live conversation before agent.started", async ({
  page,
}) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.addInitScript(installFakeBackend, {
    stepMs: 600,
    localStartup: true,
  });
  await page.goto("/");
  await page.getByRole("button", { name: /Model for this task/ }).click();
  const search = page.getByRole("combobox", { name: "Search models" });
  await search.fill("qwen3:14b");
  await search.press("Enter");
  await page
    .getByRole("textbox", { name: "Message ShadowCode" })
    .fill("Read this project");
  await page.getByRole("button", { name: "Send task" }).click();
  const live = page.locator(".working");
  await expect(live).toContainText("Preparing local model");
  await expect(live).toContainText("Waiting for local runtime");
  await expect(live).toContainText("Loading local model");
  await page.screenshot({ path: "../../local-progress-browser.png" });
  await expect(live).not.toContainText("Loading local model");
  await expect(live).toContainText("Reading project", { timeout: 10000 });
  expect(errors).toEqual([]);
});

test("timing details stay within the completed conversation", async ({
  page,
}) => {
  await page.addInitScript(installFakeBackend, {
    stepMs: 30,
    taskTimings: {
      schema_version: 1,
      complete: true,
      total_seconds: 12.4,
      queue_seconds: 4,
      active_seconds: 8.4,
      preparation_seconds: 3,
      runtime_wait_seconds: 1,
      model_load_seconds: 1.8,
      model_reused: false,
      model_requests_seconds: 2.5,
      model_requests: 2,
      first_text_seconds: 0.15,
      first_text_request: 2,
      tool_batches_seconds: 2,
      final_checks_seconds: 0.5,
      check_process_seconds: 1.2,
    },
  });
  await page.goto("/");
  await page.getByRole("button", { name: /Model for this task/ }).click();
  const search = page.getByRole("combobox", { name: "Search models" });
  await search.fill("qwen3:14b");
  await search.press("Enter");
  await page
    .getByRole("textbox", { name: "Message ShadowCode" })
    .fill("Fix add and run checks");
  await page.getByRole("button", { name: "Send task" }).click();
  const summary = page.getByRole("region", { name: "Task summary" });
  await summary.getByText("Timing details").click();
  await expect(summary.getByText("Configured check processes")).toBeVisible();
  await expect(summary.getByText("0.15s")).toBeVisible();
  await summary.locator(".task-timings").scrollIntoViewIfNeeded();
  await page.screenshot({ path: "../../task-timing-browser.png" });
  const details = summary.locator(".task-timings");
  expect(await details.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(
    true,
  );
});
