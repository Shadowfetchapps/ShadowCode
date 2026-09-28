import { test, expect, type Page } from "@playwright/test";
import { installFakeBackend, type FakeOptions } from "./fakeBackend";

// A new user with no subscription, no API key and no local model: the first
// run offers a free model for this computer, an OpenRouter key or a
// subscription; the download runs in the window and the model is selected
// when it is ready. Downloads and progress come from the fake engine.
async function start(page: Page, options: FakeOptions = {}) {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  (page as Page & { errors?: string[] }).errors = errors;
  await page.addInitScript(installFakeBackend, {
    stepMs: 60,
    firstRun: true,
    ...options,
  });
  await page.goto("/");
}
test.afterEach(async ({ page }) => {
  expect((page as Page & { errors?: string[] }).errors).toEqual([]);
});

type FakeWindow = {
  __SHADOW_FAKE__: {
    log: { method: string; path: string; body: any }[];
    state: any;
  };
};
const downloadPosts = (page: Page) =>
  page.evaluate(() =>
    (window as unknown as FakeWindow).__SHADOW_FAKE__.log.filter(
      (r) =>
        r.method === "POST" &&
        r.path.startsWith("/api/local-models/downloads/"),
    ),
  );

async function trustAndOpen(page: Page) {
  const welcome = page.getByRole("dialog", { name: "Welcome to ShadowCode" });
  await expect(welcome.getByLabel("Project folder")).toHaveValue("/work/demo");
  await welcome.getByRole("button", { name: "Trust and open" }).click();
  const step = page.getByRole("dialog", { name: "Choose a model" });
  await expect(
    step.getByRole("heading", { name: "Choose how ShadowCode thinks" }),
  ).toBeVisible();
  return step;
}

test("no accounts: download the recommended free model and start with it", async ({
  page,
}) => {
  await start(page, { downloadSteps: 3 });
  const step = await trustAndOpen(page);
  const download = step.getByRole("radio", {
    name: /Download a free model to run on this computer/,
  });
  await expect(download).toBeChecked();
  await expect(step).toContainText(
    "Gemma 4 E4B by Google: 4.8 GB, about 14 minutes on a 50 Mbit/s connection. Runs on your graphics card.",
  );
  await expect(step).toContainText(
    "Picked for this computer: Intel Arc A750 (8.0 GB) · 16 GB memory.",
  );
  await expect(
    step.getByRole("radio", { name: /Use an OpenRouter key/ }),
  ).toBeVisible();
  await expect(
    step.getByRole("radio", { name: /Sign in to a subscription/ }),
  ).toBeVisible();
  await page.screenshot({ path: "test-results/first-run-model-step.png" });
  expect(await downloadPosts(page)).toEqual([]);

  await step.getByRole("button", { name: "Download Gemma 4 E4B" }).click();
  await expect(step).toBeHidden();
  // The empty conversation follows the download…
  await expect(
    page.getByText(
      "Downloading Gemma 4 E4B. It is selected here as soon as it is ready.",
    ),
  ).toBeVisible();
  const row = page.getByRole("article", { name: "Gemma 4 E4B" });
  await expect(row.getByRole("button", { name: "Pause" })).toBeVisible();
  await page.screenshot({ path: "test-results/first-run-downloading.png" });
  // …and selects the model when it is ready.
  await expect(
    page.getByText("Gemma 4 E4B is ready. Ask it anything about this project."),
  ).toBeVisible({ timeout: 15000 });
  await expect(
    page.getByRole("button", {
      name: "Model for this task: Gemma 4 E4B · This computer",
    }),
  ).toBeVisible();
  await expect(page.getByText("ShadowCode needs a model to work")).toHaveCount(
    0,
  );
  expect((await downloadPosts(page)).map((r) => r.path)).toEqual([
    "/api/local-models/downloads/start",
  ]);
  // It is a ready local row in the picker.
  await page.getByRole("button", { name: /Model for this task/ }).click();
  await expect(
    page.locator(".unified-picker-row", { hasText: "Gemma 4 E4B" }),
  ).toBeVisible();
});

test("no accounts: OpenRouter and subscriptions go to Accounts; the welcome keeps the choices", async ({
  page,
}) => {
  await start(page);
  const step = await trustAndOpen(page);
  await step.getByRole("radio", { name: /Use an OpenRouter key/ }).click();
  await step.getByRole("button", { name: "Add an OpenRouter key" }).click();
  const settings = page.getByRole("dialog", { name: "Settings" });
  await expect(settings.getByLabel("OpenRouter API key")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(settings).toBeHidden();
  // Still no model: the empty conversation offers the same three choices.
  await expect(
    page.getByText(
      "ShadowCode needs a model to work. Pick one of these to start.",
    ),
  ).toBeVisible();
  await page.getByRole("radio", { name: /Sign in to a subscription/ }).click();
  await page.getByRole("button", { name: "Choose a subscription" }).click();
  await expect(settings.getByRole("article", { name: "Codex" })).toBeVisible();
  await page.keyboard.press("Escape");
  await page.getByRole("button", { name: "See other free models" }).click();
  await expect(
    settings.getByRole("heading", { name: "Download a free model" }),
  ).toBeVisible();
  expect(await downloadPosts(page)).toEqual([]);
});

test("Settings › Local models: pause, resume after a dropped connection, cancel, and offline", async ({
  page,
}) => {
  await start(page, { firstRun: false, downloadSteps: 3, downloadDrops: true });
  await expect(
    page.getByRole("textbox", { name: "Message ShadowCode" }),
  ).toBeVisible();
  await page.keyboard.press("Control+,");
  const settings = page.getByRole("dialog", { name: "Settings" });
  await settings
    .getByRole("button", { name: "Local models", exact: true })
    .click();
  const granite = settings.getByRole("article", { name: "Granite 4.2 3B" });
  await granite.getByRole("button", { name: "Download (2.1 GB)" }).click();
  await expect(granite.getByRole("alert")).toContainText(
    "The connection dropped at 0.7 GB of 2.1 GB. Choose Resume to continue.",
  );
  await expect(granite.getByRole("status")).toHaveText(
    "Stopped at 713 MB of 2.1 GB",
  );
  await granite.getByRole("button", { name: "Resume" }).click();
  await granite.getByRole("button", { name: "Pause" }).click();
  await expect(granite.getByRole("status")).toContainText("Paused at");
  await granite.getByRole("button", { name: "Cancel download" }).click();
  await expect(
    granite.getByRole("button", { name: "Download (2.1 GB)" }),
  ).toBeEnabled();
  await page.screenshot({ path: "test-results/local-models-downloads.png" });

  // Offline mode: nothing can start, and the page says why.
  await page.evaluate(() => {
    (window as unknown as FakeWindow).__SHADOW_FAKE__.state.config.network = {
      mode: "offline",
    };
  });
  await settings
    .getByRole("button", { name: "Appearance", exact: true })
    .click();
  await settings
    .getByRole("button", { name: "Local models", exact: true })
    .click();
  await expect(
    settings.getByText(
      "Offline mode is on, so nothing can be downloaded. Switch to Online in Settings › Permissions & network.",
    ),
  ).toBeVisible();
  await expect(
    settings
      .getByRole("article", { name: "Granite 4.2 3B" })
      .getByRole("button", { name: "Download (2.1 GB)" }),
  ).toBeDisabled();
  const starts = (await downloadPosts(page)).filter((r) =>
    r.path.endsWith("/start"),
  );
  expect(starts).toHaveLength(2);
});
