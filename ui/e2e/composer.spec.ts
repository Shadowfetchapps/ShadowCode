import { test, expect, type Page } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { installFakeBackend } from "./fakeBackend";

// Composer extras, approvals, message actions, the per-task Review and
// rewinds, against the deterministic fake engine.
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

type Fake = {
  __SHADOW_FAKE__: {
    log: { method: string; path: string; body: any }[];
    state: any;
    requestApproval: (r: object) => { id: string };
  };
};
const prompt = (page: Page) =>
  page.getByRole("textbox", { name: "Message ShadowCode" });
const send = (page: Page) => page.getByRole("button", { name: "Send task" });
const fakeLog = (page: Page) =>
  page.evaluate(() => (window as unknown as Fake).__SHADOW_FAKE__.log);
const fakeFile = (page: Page, path: string) =>
  page.evaluate(
    (p) => (window as unknown as Fake).__SHADOW_FAKE__.state.files[p],
    path,
  );
const jobPosts = async (page: Page) =>
  (await fakeLog(page)).filter(
    (r) => r.path === "/api/jobs" && r.method === "POST",
  );

async function chooseLocal(page: Page) {
  await page.getByRole("button", { name: /Model for this task/ }).click();
  const search = page.getByRole("combobox", { name: "Search models" });
  await search.fill("qwen3:14b");
  await search.press("Enter");
  await expect(page.getByRole("listbox")).toHaveCount(0);
}
async function runTask(page: Page, text: string) {
  await prompt(page).fill(text);
  await send(page).click();
  await expect(
    page.getByRole("region", { name: "Task summary" }).last(),
  ).toBeVisible({ timeout: 15000 });
}

test("@ attaches files, effort and mode reach the engine, ↑ recalls the prompt", async ({
  page,
}) => {
  await chooseLocal(page);
  await prompt(page).pressSequentially("Explain @app");
  const menu = page.getByRole("listbox", { name: "Mentions" });
  await expect(
    menu.getByRole("option", { name: /src\/app\.ts/ }),
  ).toBeVisible();
  await prompt(page).press("Enter");
  await expect(prompt(page)).toHaveValue("Explain @src/app.ts ");
  await expect(
    page.getByRole("list", { name: "Mentioned files and folders" }),
  ).toContainText("src/app.ts");
  await page.getByLabel("Reasoning effort").selectOption("high");
  await page.getByRole("radio", { name: "Plan" }).click();
  await send(page).click();
  await expect(page.getByRole("region", { name: "Task summary" })).toBeVisible({
    timeout: 15000,
  });
  const [post] = await jobPosts(page);
  expect(post.body).toMatchObject({
    task: "Explain @src/app.ts",
    mentions: [{ path: "src/app.ts", kind: "file" }],
    effort: "high",
    purpose: "planner",
  });
  // The effort is remembered for this model.
  await page.reload();
  await expect(page.getByLabel("Reasoning effort")).toHaveValue("high");
  await prompt(page).press("ArrowUp");
  await expect(prompt(page)).toHaveValue("Explain @src/app.ts");
  await prompt(page).press("ArrowDown");
  await expect(prompt(page)).toHaveValue("");
});

test("inspects bounded @-attached context and discloses opaque provider context", async ({
  page,
}) => {
  await prompt(page).pressSequentially("Explain @app");
  await expect(
    page.getByRole("option", { name: /src\/app\.ts/ }),
  ).toBeVisible();
  await prompt(page).press("Enter");

  await page.getByRole("button", { name: "Inspect attached context" }).click();
  const inventory = page.getByRole("dialog", {
    name: "Attached context inventory",
  });
  await expect(inventory).toContainText("Included · src/app.ts");
  await expect(inventory).toContainText(/Lines 1–\d+ · \d+ of \d+ bytes/);
  await expect(inventory).toContainText(
    /About \d+ tokens estimated · \d+ attached bytes/,
  );
  await expect(inventory).toContainText(
    "Files are read again when the task starts",
  );
  await expect(inventory).toContainText(
    "ShadowCode cannot enumerate that provider-managed context",
  );
  expect(
    (await fakeLog(page)).some(
      (entry) => entry.path === "/api/workspace/context-preview",
    ),
  ).toBe(true);

  await inventory
    .getByRole("button", { name: "Refresh context preview" })
    .click();
  await expect(inventory).toContainText("Previewed");
});

test("edit & resend forks just before the message, the original stays", async ({
  page,
}) => {
  await chooseLocal(page);
  await runTask(page, "Fix the add function");
  const bubble = page.locator(".msg-user").first();
  await bubble.hover();
  await bubble.getByRole("button", { name: "Edit and resend" }).click();
  await page.getByLabel("Edited message").fill("Fix add and sub");
  await page.getByRole("button", { name: "Send edited message" }).click();
  await expect(page.locator(".msg-user .bubble").first()).toHaveText(
    "Fix add and sub",
  );
  await expect(page.getByText("Fix the add function")).toHaveCount(0);
  const log = await fakeLog(page);
  const fork = log.find((r) => r.path.endsWith("/fork"));
  expect(fork?.body).toMatchObject({ before: true });
  const posts = await jobPosts(page);
  expect(posts.at(-1)?.body).toMatchObject({
    task: "Fix add and sub",
    session_id: "s2",
  });
  // The original conversation is kept.
  const sessions = await page.evaluate(() =>
    (window as unknown as Fake).__SHADOW_FAKE__.state.sessions.map(
      (s: { id: string }) => s.id,
    ),
  );
  expect(sessions).toEqual(expect.arrayContaining(["s1", "s2"]));
});

test("approval cards show the diff; Allow for this task and Deny with note answer the engine", async ({
  page,
}) => {
  await page.evaluate(() =>
    (window as unknown as Fake).__SHADOW_FAKE__.requestApproval({
      tool: "edit_file",
      command: "edit_file",
      reason: "Edit src/app.ts",
      arguments: { path: "src/app.ts" },
      preview: {
        kind: "files",
        files: [
          {
            path: "src/app.ts",
            status: "modified",
            diff: "@@ -1,2 +1,2 @@\n-export const add = (a, b) => a - b;\n+export const add = (a, b) => a + b;\n export const sub = (a, b) => a - b;\n",
            added: 1,
            removed: 1,
            truncated: false,
            binary: false,
          },
        ],
      },
      grant: "file edits",
      note: true,
    }),
  );
  const card = page.locator(".approval");
  await expect(card).toContainText("Edited");
  await expect(card.locator(".diff-del")).toContainText("a - b");
  await expect(card.locator(".diff-add")).toContainText("a + b");
  const axe = await new AxeBuilder({ page }).include(".approval").analyze();
  expect(axe.violations).toEqual([]);
  await card.getByRole("button", { name: "Allow for this task" }).click();
  await expect(card).toHaveCount(0);
  let decided = (await fakeLog(page)).filter((r) =>
    r.path.startsWith("/api/approvals/"),
  );
  expect(decided.at(-1)?.body).toMatchObject({
    decision: "approve",
    scope: "task",
  });

  await page.evaluate(() =>
    (window as unknown as Fake).__SHADOW_FAKE__.requestApproval({
      command: "rm -rf build",
      reason: "Run a shell command",
      preview: { kind: "command", command: "rm -rf build", cwd: "/work/demo" },
      grant: "",
      note: true,
    }),
  );
  await expect(page.locator(".approval")).toContainText("Runs in /work/demo");
  await expect(
    page.getByRole("button", { name: "Allow for this task" }),
  ).toHaveCount(0);
  await page.getByRole("button", { name: "Deny with note…" }).click();
  await page
    .getByLabel("Tell the agent why, or what to do instead")
    .fill("Use make clean");
  await page.getByRole("button", { name: "Deny and send note" }).click();
  await expect(page.locator(".approval")).toHaveCount(0);
  decided = (await fakeLog(page)).filter((r) =>
    r.path.startsWith("/api/approvals/"),
  );
  expect(decided.at(-1)?.body).toMatchObject({
    decision: "deny",
    note: "Use make clean",
  });
});

test("the Review view lists the task's files and undoes one hunk", async ({
  page,
}) => {
  await chooseLocal(page);
  await runTask(page, "Fix the add function");
  await page
    .getByRole("region", { name: "Task summary" })
    .getByRole("button", { name: "Review changes" })
    .click();
  const review = page.getByRole("region", { name: "Review changes" });
  await expect(review).toBeVisible();
  await expect(
    review.getByRole("navigation", { name: "Changed files" }),
  ).toContainText("src/app.ts");
  const undo = review.getByRole("button", { name: /^Undo change / });
  await expect(undo).toHaveCount(2);
  await review.getByRole("button", { name: "Split" }).click();
  await expect(review.locator(".diff-split")).toHaveCount(2);
  await undo.first().click();
  await expect(undo).toHaveCount(1);
  const text = await fakeFile(page, "src/app.ts");
  expect(text).toContain("add = (a, b) => a - b");
  expect(text).toContain("VERSION = 2");
  await review.getByRole("button", { name: /^Keep change / }).click();
  await expect(review.getByText("Kept")).toBeVisible();
  const axe = await new AxeBuilder({ page }).include(".review-view").analyze();
  expect(axe.violations).toEqual([]);
  // Git staging is one tab away.
  await review.getByRole("tab", { name: "Git" }).click();
  await expect(review.getByPlaceholder("Commit message")).toBeVisible();
  await review.getByRole("button", { name: "Back to conversation" }).click();
  await expect(prompt(page)).toBeVisible();
});

test("rewind asks first, marks the transcript, and can be undone", async ({
  page,
}) => {
  await chooseLocal(page);
  await runTask(page, "Fix the add function");
  await page
    .getByRole("region", { name: "Task summary" })
    .getByRole("button", { name: "Rewind" })
    .click();
  const dialog = page.getByRole("dialog", {
    name: "Rewind this task's changes?",
  });
  await expect(
    dialog.getByRole("list", { name: "Files that will change" }),
  ).toContainText("src/app.ts");
  await dialog.getByRole("button", { name: "Rewind 1 file" }).click();
  await expect(dialog).toHaveCount(0);
  await expect(
    page.getByRole("separator", { name: "Rewound to here · 1 file restored" }),
  ).toBeVisible();
  expect(await fakeFile(page, "src/app.ts")).toContain("a - b;");
  const toast = page.locator(".toast", { hasText: "Restored 1 file." });
  await toast.getByRole("button", { name: "Undo" }).click();
  await expect(
    page.getByText("Rewind undone · 1 file put back as they were"),
  ).toBeVisible();
  expect(await fakeFile(page, "src/app.ts")).toContain("a + b;");
});
