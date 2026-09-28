import { test, expect } from "@playwright/test";
import { installFakeBackend } from "./fakeBackend";

test("a check preserves the draft, requires approval, and finishes without model selection", async ({
  page,
}, testInfo) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.addInitScript(installFakeBackend, {
    completedTask: true,
    stepMs: 40,
  });
  await page.goto("/");
  const composer = page.getByRole("textbox", { name: "Message ShadowCode" });
  await expect(composer).toBeVisible();
  const draft = "Preserve this draft while I check the existing changes";
  await composer.fill(draft);
  const picker = page.getByRole("button", { name: /Model for this task/ });
  await expect(picker).toContainText("Choose a model");
  await page.getByRole("button", { name: "Run a check…", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Run a check", exact: true });
  await expect(dialog.getByText("/work/demo", { exact: true })).toBeVisible();
  await dialog.getByRole("textbox", { name: "Check command" }).fill("npm test");
  await page.screenshot({ path: testInfo.outputPath("run-check-dialog.png") });
  await dialog.getByRole("button", { name: "Run check", exact: true }).click();
  await expect(dialog).not.toBeVisible();
  await expect(
    page.getByRole("button", { name: "Allow", exact: true }),
  ).toBeVisible();
  await expect(composer).toHaveValue(draft);
  await expect(
    page.getByText("Fixture check output: 4 cases passed.", { exact: true }),
  ).toHaveCount(0);
  await page.getByRole("button", { name: "Allow", exact: true }).click();
  const summaries = page.getByRole("region", { name: "Task summary" });
  await expect(summaries).toHaveCount(2);
  const result = summaries.last();
  await expect(result.getByText("passed", { exact: true })).toBeVisible();
  await result.getByText("Execution receipt", { exact: true }).click();
  await expect(
    result.getByText(/Fixture check output: 4 cases passed\./),
  ).toBeVisible();
  await expect(composer).toHaveValue(draft);
  await expect(picker).toContainText("Choose a model");
  const posts = await page.evaluate(() =>
    (window as any).__SHADOW_FAKE__.log.filter(
      (request: { method: string; path: string }) =>
        request.method === "POST" &&
        ["/api/jobs", "/api/jobs/test"].includes(request.path),
    ),
  );
  expect(posts).toEqual([
    {
      method: "POST",
      path: "/api/jobs/test",
      body: {
        workspace: "/work/demo",
        session_id: "s1",
        command: "npm test",
        timeout: 300,
        queue: false,
      },
    },
  ]);
  const terminal = await page.evaluate(() =>
    (window as any).__SHADOW_FAKE__.state.jobs.at(-1),
  );
  expect(terminal).toMatchObject({
    status: "completed",
    mode: "command",
    model: "native command",
    session_id: "s1",
    workspace: "/work/demo",
  });
  await page.screenshot({ path: testInfo.outputPath("run-check-result.png") });
  expect(errors).toEqual([]);
});
