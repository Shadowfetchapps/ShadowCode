import { test, expect, type Page } from "@playwright/test";
import { installFakeBackend } from "./fakeBackend";

// Help for people new to coding agents: the words ShadowCode uses, and a
// failed task explained in plain words with a next step.
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

const prompt = (page: Page) =>
  page.getByRole("textbox", { name: "Message ShadowCode" });
const fake = <T>(page: Page, run: (fake: any) => T) =>
  page.evaluate(run as never) as Promise<T>;

async function chooseLocal(page: Page) {
  await page.getByRole("button", { name: /Model for this task/ }).click();
  const search = page.getByRole("combobox", { name: "Search models" });
  await search.fill("qwen3:14b");
  await search.press("Enter");
  await expect(page.getByRole("listbox")).toHaveCount(0);
}

test("Help lists the words ShadowCode uses and finds one", async ({ page }) => {
  await page.keyboard.press("Control+k");
  await page.keyboard.type("words you");
  await page.keyboard.press("Enter");
  const help = page.getByRole("dialog", { name: "Help & Shortcuts" });
  await expect(help.getByText("Words you’ll see")).toBeVisible();
  await help.getByRole("searchbox", { name: "Find a word" }).fill("worktree");
  const terms = help.locator(".help-glossary dt");
  await expect(terms).toHaveText(["Worktree"]);
  await expect(help.locator(".help-glossary dd")).toContainText(
    "A second copy of your project",
  );
  await help.getByRole("searchbox", { name: "Find a word" }).fill("zzz");
  await expect(help.getByText("No word matches.")).toBeVisible();
});

test("a failed task says what went wrong and offers the next step", async ({
  page,
}) => {
  await fake(page, () => {
    (window as any).__SHADOW_FAKE__.state.failNext =
      "Model provider returned HTTP 429; provider rate limit reached";
  });
  await chooseLocal(page);
  await prompt(page).fill("Fix the add function");
  await page.getByRole("button", { name: "Send task" }).click();
  const help = page.getByRole("region", { name: "What went wrong" });
  await expect(help).toContainText("The provider is limiting requests");
  // Try again sends the same request; this time it succeeds.
  await help.getByRole("button", { name: "Try again" }).click();
  const sent = () =>
    fake(page, () =>
      (window as any).__SHADOW_FAKE__.log
        .filter((r: any) => r.path === "/api/jobs" && r.method === "POST")
        .map((r: any) => r.body.task),
    );
  await expect
    .poll(sent)
    .toEqual(["Fix the add function", "Fix the add function"]);
  await expect(
    page.getByRole("region", { name: "Task summary" }).last(),
  ).toContainText("Finished", { timeout: 15000 });
});
