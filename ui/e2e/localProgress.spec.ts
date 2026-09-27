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
