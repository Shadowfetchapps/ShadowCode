import { test, expect, type Page } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { installFakeBackend } from "./fakeBackend";
import { installFakeTools } from "./fakeTools";
import { expectEditorText } from "./editorHelpers";

// Every test drives the production UI build against the deterministic fake
// engine; nothing here talks to a real vendor CLI or model.
test.beforeEach(async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  (page as Page & { errors?: string[] }).errors = errors;
  await page.addInitScript(installFakeBackend, { stepMs: 90 });
  await page.addInitScript(installFakeTools, {});
  await page.goto("/");
  await expect(
    page.getByRole("heading", { name: "What should we work on?" }),
  ).toBeVisible();
});
test.afterEach(async ({ page }) => {
  expect((page as Page & { errors?: string[] }).errors).toEqual([]);
});

const trigger = (page: Page) =>
  page.getByRole("button", { name: /Model for this task/ });
const prompt = (page: Page) =>
  page.getByRole("textbox", { name: "Message ShadowCode" });
const send = (page: Page) => page.getByRole("button", { name: "Send task" });
async function openCompare(page: Page) {
  const more = page.locator("details.composer-more");
  if (!(await more.evaluate((element) => (element as HTMLDetailsElement).open)))
    await more.locator("summary").click();
  return more.getByRole("button", { name: "Compare", exact: true });
}
const fakeLog = (page: Page) =>
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

async function chooseBySearch(page: Page, text: string) {
  await trigger(page).click();
  const search = page.getByRole("combobox", { name: "Search models" });
  await expect(search).toBeFocused();
  await search.fill(text);
  await search.press("Enter");
  await expect(page.getByRole("listbox")).toHaveCount(0);
}

async function runLocalTask(page: Page, text: string) {
  await chooseBySearch(page, "qwen3:14b");
  await prompt(page).fill(text);
  await send(page).click();
  await expect(
    page.getByRole("region", { name: "Task summary" }).last(),
  ).toBeVisible({
    timeout: 15000,
  });
}

test("picks a subscription row with the keyboard", async ({ page }) => {
  await expect(trigger(page)).toContainText("Choose a model");
  await prompt(page).fill("Explain the build");
  await expect(send(page)).toBeDisabled();
  await page.getByRole("button", { name: "Choose a model" }).click();
  await expect(
    page.getByRole("group", { name: "Subscriptions" }),
  ).toBeVisible();
  await expect(
    page.getByRole("group", { name: "On this computer" }),
  ).toBeVisible();
  const search = page.getByRole("combobox", { name: "Search models" });
  // Details for the active row open with the right arrow.
  await search.press("ArrowRight");
  await expect(page.locator(".unified-picker-details")).toContainText(
    "Weekly · 2% left",
  );
  await search.press("ArrowLeft");
  await search.press("Home");
  await search.press("Enter");
  await expect(trigger(page)).toContainText("Codex · GPT-6-Astra");
  await expect(trigger(page)).toContainText("Cloud");
  await expect(trigger(page)).toBeFocused();
  await expect(send(page)).toBeEnabled();
  const log = await fakeLog(page);
  expect(
    log.some(
      (r) =>
        r.path === "/api/sessions/s1/target" &&
        r.body.target_id === "cli:codex:gpt-6-astra",
    ),
  ).toBe(true);
  // The choice belongs to the conversation and survives a reload.
  await page.reload();
  await expect(trigger(page)).toContainText("Codex · GPT-6-Astra");
});

test("picks a local row; web and permission controls follow the row", async ({
  page,
}) => {
  await expect(
    page.getByRole("button", { name: "Web lookups for this task" }),
  ).toHaveCount(0);
  await chooseBySearch(page, "qwen3:14b");
  await expect(trigger(page)).toHaveAttribute(
    "aria-label",
    /qwen3:14b · This computer/,
  );
  await expect(trigger(page)).toHaveText("Localqwen3:14b");
  const web = page.getByRole("button", { name: "Web lookups for this task" });
  await expect(web).toHaveAttribute("aria-pressed", "false");
  await web.click();
  await expect(web).toHaveAttribute("aria-pressed", "true");
  await page
    .getByRole("button", { name: /Permissions: Ask before actions/ })
    .click();
  await page.getByRole("radio", { name: /Allow project edits/ }).click();
  await expect(
    page.getByRole("button", { name: /Permissions: Allow project edits/ }),
  ).toBeVisible();
  const log = await fakeLog(page);
  expect(
    log.find((r) => r.path === "/api/config" && r.method === "PUT")?.body
      .values,
  ).toEqual({ permissions: { mode: "allow_edits" } });
});

test("runs a task with streamed events and reviews the changes", async ({
  page,
}) => {
  await chooseBySearch(page, "qwen3:14b");
  await prompt(page).fill("Fix the add function");
  await send(page).click();
  const live = page.locator(".working .activity-timeline");
  await expect(live).toBeVisible();
  await expect(live.getByText("Reading project")).toBeVisible();
  const summary = page.getByRole("region", { name: "Task summary" });
  await expect(summary).toBeVisible({ timeout: 15000 });
  await expect(summary).toContainText("src/app.ts");
  await expect(summary).toContainText("+1");
  await expect(summary).toContainText("npm test");
  await expect(summary).toContainText("exit 0");
  // The finished timeline sits above the answer (or in the summary block
  // when the task gave no answer text).
  const timeline = page
    .locator(
      ".msg-with-activity .activity-timeline, .msg-summary .activity-timeline",
    )
    .last();
  for (const step of [
    "Reading project",
    "Editing files",
    "Checks not verified",
  ])
    await expect(timeline.getByText(step, { exact: true })).toBeVisible();
  await expect(timeline.getByText("Finished", { exact: true })).toHaveCount(0);
  await expect(summary).toContainText("Finished");
  // Each step expands to the real tool call and its output.
  // This legacy fixture records a successful command without a configured
  // check receipt. Keep its output available without certifying verification.
  const checks = timeline.locator(".activity-step").filter({
    has: page.getByText("Checks not verified", { exact: true }),
  });
  await expect(checks).toHaveClass(/is-incomplete/);
  await expect(checks.locator("svg.lucide-circle-dashed")).toHaveCount(1);
  await expect(checks.locator("svg.lucide-check")).toHaveCount(0);
  await expect(
    timeline.getByText("Running checks", { exact: true }),
  ).toHaveCount(0);
  await expect(summary).toContainText("Verification not run");
  await checks.getByText("Checks not verified", { exact: true }).click();
  await expect(timeline.getByText("Tests  4 passed (4)")).toBeVisible();
  await summary.getByRole("button", { name: "Review changes" }).click();
  // The task's own changes open full width, file by file.
  const review = page.getByRole("region", { name: "Review changes" });
  await expect(review).toBeVisible();
  await review.getByRole("button", { name: /src\/app\.ts/ }).click();
  await expect(review.locator(".review-diff")).toContainText(
    "export const add = (a, b) => a + b;",
  );
  await page.screenshot({
    path: "test-results/task-complete.png",
    fullPage: true,
  });
});

test("approvals appear as soon as the engine asks, without polling", async ({
  page,
}) => {
  const reads = async () =>
    (await fakeLog(page)).filter((r) =>
      /^\/api\/(feed|approvals|jobs)\b/.test(r.path),
    ).length;
  await page.waitForTimeout(500);
  const idle = await reads();
  // Nothing polls approvals or jobs while the window is idle.
  await page.waitForTimeout(3000);
  expect(await reads()).toBe(idle);
  const asked = Date.now();
  await page.evaluate(() =>
    (
      window as unknown as {
        __SHADOW_FAKE__: { requestApproval: (r: object) => void };
      }
    ).__SHADOW_FAKE__.requestApproval({
      command: "npm run lint",
      reason: "Check the style",
    }),
  );
  const card = page.locator(".approval");
  await expect(card).toContainText("npm run lint", { timeout: 1000 });
  expect(Date.now() - asked).toBeLessThan(1000);
  await card.getByRole("button", { name: "Allow" }).click();
  await expect(card).toHaveCount(0, { timeout: 1000 });
  const decided = (await fakeLog(page)).find(
    (r) => r.method === "POST" && r.path.startsWith("/api/approvals/"),
  );
  expect(decided?.body).toMatchObject({ decision: "approve" });
});

test("drawer tabs keep the terminal, the open file and the commit message", async ({
  page,
}) => {
  await page.getByRole("button", { name: "Review changes" }).click();
  const drawer = page.getByRole("complementary", { name: "Drawer" });
  const tab = (name: string) =>
    drawer.locator(".drawer-tabs").getByRole("button", { name });
  await drawer.getByPlaceholder("Commit message").fill("Fix the add function");
  await tab("Terminal").click();
  const shell = drawer.getByRole("group", { name: "Terminal 1" });
  await shell.click();
  await page.keyboard.type("ls");
  await page.keyboard.press("Enter");
  await expect(shell).toContainText("ls: ran in /work/demo");
  await page.keyboard.type("echo half-typed");
  await tab("Files").click();
  await drawer.getByRole("button", { name: /README\.md/ }).click();
  await expectEditorText(
    drawer.getByRole("textbox", { name: "Edit README.md" }),
    "# Demo\n",
  );
  await tab("Changes").click();
  await expect(drawer.getByPlaceholder("Commit message")).toHaveValue(
    "Fix the add function",
  );
  await tab("Terminal").click();
  await expect(shell).toContainText("ls: ran in /work/demo");
  await expect(shell).toContainText("echo half-typed");
  await tab("Files").click();
  await expectEditorText(
    drawer.getByRole("textbox", { name: "Edit README.md" }),
    "# Demo\n",
  );
  // Closing and reopening the drawer keeps them too.
  await drawer.getByRole("button", { name: "Close drawer" }).click();
  await page.getByRole("button", { name: "Review changes" }).click();
  await expect(drawer.getByPlaceholder("Commit message")).toHaveValue(
    "Fix the add function",
  );
});

test("keeps an unsaved file draft and reviews an agent edit before saving", async ({
  page,
}) => {
  await page.getByRole("button", { name: "Review changes" }).click();
  const drawer = page.getByRole("complementary", { name: "Drawer" });
  const tab = (name: string) =>
    drawer.locator(".drawer-tabs").getByRole("button", { name });
  await tab("Files").click();
  await drawer.getByRole("button", { name: "README.md", exact: true }).click();
  const editor = drawer.getByRole("textbox", { name: "Edit README.md" });
  await expectEditorText(editor, "# Demo\n");
  await editor.fill("# My draft\n");
  await editor.press("Escape");
  await expect(drawer.getByRole("button", { name: "Diff" })).toBeFocused();
  await expect(drawer).toBeVisible();
  await tab("Changes").click();
  await tab("Files").click();
  await expectEditorText(editor, "# My draft\n");
  await page.evaluate(() => {
    (window as any).__SHADOW_FAKE__.state.files["README.md"] = "# Agent edit\n";
  });
  const conflict = drawer.getByRole("group", { name: "File conflict" });
  await expect(conflict).toBeVisible({ timeout: 7000 });
  await expectEditorText(editor, "# My draft\n");
  await expect(
    drawer.getByRole("button", { name: "Save", exact: true }),
  ).toBeDisabled();
  await editor.press("Control+s");
  expect(
    (await fakeLog(page)).filter(
      (entry) =>
        entry.method === "PUT" &&
        entry.path.split("?")[0] === "/api/workspace/file",
    ),
  ).toEqual([]);
  expect(
    await page.evaluate(
      () => (window as any).__SHADOW_FAKE__.state.files["README.md"],
    ),
  ).toBe("# Agent edit\n");
  await conflict.getByText("Show current disk version").click();
  await expect(conflict).toContainText("# Agent edit");
  await conflict
    .getByRole("button", { name: "Use disk revision as save base" })
    .click();
  await drawer.getByRole("button", { name: "Save", exact: true }).click();
  await expect(drawer.getByText("Saved", { exact: true })).toBeVisible();
  const saved = await page.evaluate(
    () => (window as any).__SHADOW_FAKE__.state.files["README.md"],
  );
  expect(saved).toBe("# My draft\n");
});

test("indents and outdents selected file lines in the rendered editor", async ({
  page,
}) => {
  await page.getByRole("button", { name: "Review changes" }).click();
  const drawer = page.getByRole("complementary", { name: "Drawer" });
  await drawer
    .locator(".drawer-tabs")
    .getByRole("button", { name: "Files" })
    .click();
  await drawer.getByRole("button", { name: "README.md", exact: true }).click();
  const editor = drawer.getByRole("textbox", { name: "Edit README.md" });
  await editor.fill("one\ntwo\nthree\n");
  // Select from column 1 of the first line through the start of line 3
  // using the real editor keymap, rather than a textarea-only DOM API.
  await editor.press("Control+Home");
  await editor.press("ArrowRight");
  await editor.press("Shift+ArrowDown");
  await editor.press("Shift+ArrowDown");
  await editor.press("Shift+ArrowLeft");
  await editor.press("Tab");
  await expectEditorText(editor, "  one\n  two\nthree\n");
  await editor.press("Shift+Tab");
  await expectEditorText(editor, "one\ntwo\nthree\n");
});

test("recovers an unsaved editor draft after window reload and clears it after save", async ({
  page,
}) => {
  await page.getByRole("button", { name: "Review changes" }).click();
  const drawer = page.getByRole("complementary", { name: "Drawer" });
  const files = drawer
    .locator(".drawer-tabs")
    .getByRole("button", { name: "Files" });
  await files.click();
  await drawer.getByRole("button", { name: "README.md", exact: true }).click();
  const editor = drawer.getByRole("textbox", { name: "Edit README.md" });
  await editor.fill("# Draft after restart");
  await expect(
    drawer.getByText("Recovery copy saved on this computer"),
  ).toBeVisible();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          JSON.parse(
            sessionStorage.getItem("shadow-fake-editor-drafts") || "{}",
          )["README.md"]?.draft,
      ),
    )
    .toBe("# Draft after restart");
  page.once("dialog", (dialog) => void dialog.accept());
  await page.reload();
  await page.evaluate(() => {
    (window as any).__SHADOW_FAKE__.state.files["README.md"] =
      "# Changed while closed\n";
  });
  await page.getByRole("button", { name: "Review changes" }).click();
  await files.click();
  await drawer.getByRole("button", { name: "Open README.md, unsaved" }).click();
  await expectEditorText(editor, "# Draft after restart");
  const conflict = drawer.getByRole("group", { name: "File conflict" });
  await expect(conflict).toBeVisible();
  await expect(
    drawer.getByRole("button", { name: "Save", exact: true }),
  ).toBeDisabled();
  await conflict
    .getByRole("button", { name: "Use disk revision as save base" })
    .click();
  await drawer.getByRole("button", { name: "Save", exact: true }).click();
  await expect(drawer.getByText("Saved", { exact: true })).toBeVisible();
  await expect
    .poll(() =>
      page.evaluate(() =>
        Object.keys(
          JSON.parse(
            sessionStorage.getItem("shadow-fake-editor-drafts") || "{}",
          ),
        ),
      ),
    )
    .toEqual([]);
});

test("waits for an in-flight recovery write before discarding its draft", async ({
  page,
}) => {
  await page.getByRole("button", { name: "Review changes" }).click();
  const drawer = page.getByRole("complementary", { name: "Drawer" });
  await drawer
    .locator(".drawer-tabs")
    .getByRole("button", { name: "Files" })
    .click();
  await drawer.getByRole("button", { name: "README.md", exact: true }).click();
  await page.evaluate(() => {
    const bridge = window.__SHADOW_TEST_TRANSPORT__!;
    const request = bridge.request;
    bridge.request = async (path, method, body) => {
      if (path.startsWith("/api/workspace/editor-draft?") && method === "PUT")
        await new Promise((resolve) => setTimeout(resolve, 700));
      return request(path, method, body);
    };
  });
  const editor = drawer.getByRole("textbox", { name: "Edit README.md" });
  await editor.fill("# Temporary draft");
  await expect(drawer.getByText("Saving recovery copy…")).toBeVisible();
  await drawer.getByRole("button", { name: "Discard draft" }).click();
  await expect(
    drawer.getByRole("button", { name: "Discarding…" }),
  ).toBeVisible();
  await expectEditorText(editor, "# Temporary draft");
  await expectEditorText(editor, "# Demo\n");
  await expect
    .poll(() =>
      page.evaluate(() =>
        Object.keys(
          JSON.parse(
            sessionStorage.getItem("shadow-fake-editor-drafts") || "{}",
          ),
        ),
      ),
    )
    .toEqual([]);
  await page.reload();
  await page.getByRole("button", { name: "Review changes" }).click();
  await drawer
    .locator(".drawer-tabs")
    .getByRole("button", { name: "Files" })
    .click();
  await expect(
    drawer.getByRole("button", { name: "Open README.md, unsaved" }),
  ).toHaveCount(0);
});

test("coalesces superseded recovery drafts behind an in-flight write", async ({
  page,
}) => {
  await page.getByRole("button", { name: "Review changes" }).click();
  const drawer = page.getByRole("complementary", { name: "Drawer" });
  await drawer
    .locator(".drawer-tabs")
    .getByRole("button", { name: "Files" })
    .click();
  await drawer.getByRole("button", { name: "README.md", exact: true }).click();
  await page.evaluate(() => {
    const bridge = window.__SHADOW_TEST_TRANSPORT__!;
    const request = bridge.request;
    (window as any).__recoveryWrites = [] as string[];
    bridge.request = async (path, method, body) => {
      if (path.startsWith("/api/workspace/editor-draft?") && method === "PUT") {
        (window as any).__recoveryWrites.push(
          (body as { draft: string }).draft,
        );
        if ((window as any).__recoveryWrites.length === 1)
          await new Promise((resolve) => setTimeout(resolve, 700));
      }
      return request(path, method, body);
    };
  });
  const editor = drawer.getByRole("textbox", { name: "Edit README.md" });
  await editor.fill("# First draft");
  await expect
    .poll(() => page.evaluate(() => (window as any).__recoveryWrites.length))
    .toBe(1);
  await editor.fill("# Superseded draft");
  await editor.fill("# Final draft");
  await expect(
    drawer.getByText("Recovery copy saved on this computer"),
  ).toBeVisible();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          JSON.parse(
            sessionStorage.getItem("shadow-fake-editor-drafts") || "{}",
          )["README.md"]?.draft,
      ),
    )
    .toBe("# Final draft");
  expect(await page.evaluate(() => (window as any).__recoveryWrites)).toEqual([
    "# First draft",
    "# Final draft",
  ]);
});

test("starts a fresh recovery revision after saving a file and editing again", async ({
  page,
}) => {
  await page.getByRole("button", { name: "Review changes" }).click();
  const drawer = page.getByRole("complementary", { name: "Drawer" });
  await drawer
    .locator(".drawer-tabs")
    .getByRole("button", { name: "Files" })
    .click();
  await drawer.getByRole("button", { name: "README.md", exact: true }).click();
  const editor = drawer.getByRole("textbox", { name: "Edit README.md" });
  const record = () =>
    page.evaluate(
      () =>
        JSON.parse(sessionStorage.getItem("shadow-fake-editor-drafts") || "{}")[
          "README.md"
        ],
    );
  await editor.fill("# First saved edit");
  await expect
    .poll(async () => (await record())?.draft)
    .toBe("# First saved edit");
  await drawer.getByRole("button", { name: "Save", exact: true }).click();
  await expect(drawer.getByText("Saved", { exact: true })).toBeVisible();
  await expect.poll(record).toBeUndefined();
  await editor.fill("# Second unsaved edit");
  await expect
    .poll(async () => (await record())?.draft)
    .toBe("# Second unsaved edit");
  await expect(
    drawer.getByRole("group", { name: "Saved draft conflict" }),
  ).toHaveCount(0);
  await expect(
    drawer.getByText("Recovery copy saved on this computer"),
  ).toBeVisible();
});

test("reviews a competing window's recovery draft before choosing either version", async ({
  page,
}) => {
  await page.getByRole("button", { name: "Review changes" }).click();
  const drawer = page.getByRole("complementary", { name: "Drawer" });
  await drawer
    .locator(".drawer-tabs")
    .getByRole("button", { name: "Files" })
    .click();
  await drawer.getByRole("button", { name: "README.md", exact: true }).click();
  const editor = drawer.getByRole("textbox", { name: "Edit README.md" });
  const savedDraft = () =>
    page.evaluate(
      () =>
        JSON.parse(sessionStorage.getItem("shadow-fake-editor-drafts") || "{}")[
          "README.md"
        ]?.draft,
    );
  const replaceSavedCopy = async (draft: string, revision: string) => {
    await page.evaluate(
      ({ draft, revision }) => {
        const records = JSON.parse(
          sessionStorage.getItem("shadow-fake-editor-drafts") || "{}",
        );
        records["README.md"] = { ...records["README.md"], draft, revision };
        sessionStorage.setItem(
          "shadow-fake-editor-drafts",
          JSON.stringify(records),
        );
      },
      { draft, revision },
    );
  };
  await editor.fill("# First local draft");
  await expect.poll(savedDraft).toBe("# First local draft");
  await replaceSavedCopy("# Other window draft", "a".repeat(32));
  await editor.fill("# Keep my draft");
  const conflict = drawer.getByRole("group", { name: "Saved draft conflict" });
  await expect(conflict).toBeVisible();
  await conflict.getByText("Show other window's saved draft").click();
  await expect(conflict).toContainText("# Other window draft");
  await replaceSavedCopy("# Changed again while reviewing", "c".repeat(32));
  await conflict
    .getByRole("button", { name: "Keep this window's draft" })
    .click();
  await expect(conflict).toContainText("# Changed again while reviewing");
  expect(await savedDraft()).toBe("# Changed again while reviewing");
  await conflict
    .getByRole("button", { name: "Keep this window's draft" })
    .click();
  await expect(conflict).toHaveCount(0);
  await expect.poll(savedDraft).toBe("# Keep my draft");
  await replaceSavedCopy("# Newer saved draft", "b".repeat(32));
  await editor.fill("# My later edit");
  await expect(conflict).toBeVisible();
  await conflict.getByRole("button", { name: "Use other saved draft" }).click();
  await expectEditorText(editor, "# Newer saved draft");
  await expect(conflict).toHaveCount(0);
  expect(await savedDraft()).toBe("# Newer saved draft");
});

test("shows and retries a failed recovery cleanup after file save", async ({
  page,
}) => {
  await page.getByRole("button", { name: "Review changes" }).click();
  const drawer = page.getByRole("complementary", { name: "Drawer" });
  await drawer
    .locator(".drawer-tabs")
    .getByRole("button", { name: "Files" })
    .click();
  await drawer.getByRole("button", { name: "README.md", exact: true }).click();
  await page.evaluate(() => {
    const bridge = window.__SHADOW_TEST_TRANSPORT__!;
    const request = bridge.request;
    let refusals = 2;
    bridge.request = async (path, method, body) => {
      if (
        path.startsWith("/api/workspace/editor-draft?") &&
        method === "DELETE" &&
        refusals > 0
      ) {
        refusals -= 1;
        throw new Error("Fixture draft cleanup refusal");
      }
      return request(path, method, body);
    };
  });
  await drawer
    .getByRole("textbox", { name: "Edit README.md" })
    .fill("# Reviewed save");
  await expect(
    drawer.getByText("Recovery copy saved on this computer"),
  ).toBeVisible();
  await drawer.getByRole("button", { name: "Save", exact: true }).click();
  const retry = drawer.getByRole("button", { name: "Retry recovery cleanup" });
  await expect(retry).toBeVisible();
  await expect(drawer.getByRole("alert")).toContainText(
    "Fixture draft cleanup refusal",
  );
  await drawer.getByRole("button", { name: "Close file" }).click();
  await expect(
    drawer.getByRole("textbox", { name: "Edit README.md" }),
  ).toBeVisible();
  await expect(drawer.getByText(/The file stayed open/)).toBeVisible();
  await retry.click();
  await expect(retry).toHaveCount(0);
  await expect
    .poll(() =>
      page.evaluate(() =>
        Object.keys(
          JSON.parse(
            sessionStorage.getItem("shadow-fake-editor-drafts") || "{}",
          ),
        ),
      ),
    )
    .toEqual([]);
});

test("asks for consent before sending local context to a cloud row", async ({
  page,
}) => {
  await runLocalTask(page, "Start on this computer");
  await trigger(page).click();
  await page.getByRole("option", { name: /Codex · GPT-6-Astra/ }).click();
  await prompt(page).fill("Continue in the cloud");
  await send(page).click();
  const dialog = page.getByRole("dialog", {
    name: "Send to a cloud provider?",
  });
  await expect(dialog).toBeVisible();
  await expect(dialog).toContainText("Send to Codex · GPT-6-Astra?");
  await expect(dialog).toContainText("2,400 characters");
  await expect(
    new AxeBuilder({ page })
      .include('[role="dialog"]')
      .withTags(["wcag2a", "wcag2aa"])
      .analyze(),
  ).resolves.toMatchObject({ violations: [] });
  await dialog.getByRole("button", { name: "Send" }).click();
  await expect(dialog).toHaveCount(0);
  await expect(page.getByRole("region", { name: "Task summary" })).toHaveCount(
    2,
    {
      timeout: 15000,
    },
  );
  const posts = (await fakeLog(page)).filter(
    (r) => r.path === "/api/jobs" && r.method === "POST",
  );
  expect(
    posts.map((r) => [r.body.model, Boolean(r.body.handoff_consent)]),
  ).toEqual([
    ["local:gguf:qwen", false],
    ["cli:codex:gpt-6-astra", false],
    ["cli:codex:gpt-6-astra", true],
  ]);
});

test("a Sign in row opens Accounts and Connect streams the official login", async ({
  page,
}) => {
  await trigger(page).click();
  await page.getByRole("option", { name: /Claude Code · Default/ }).click();
  const settings = page.getByRole("dialog", { name: "Settings" });
  await expect(settings).toBeVisible();
  const claude = settings.getByRole("article", { name: "Claude Code" });
  const connect = claude.getByRole("button", { name: "Connect" });
  await expect(connect).toBeFocused();
  await connect.click();
  await expect(
    claude.getByRole("link", { name: /claude\.ai\/oauth/ }),
  ).toBeVisible();
  await expect(claude.locator("code.device-code")).toHaveText("WXYZ-1234");
  await expect(claude.getByText("Signed in.")).toBeVisible({ timeout: 10000 });
  await expect(claude.getByText("Ready", { exact: true })).toBeVisible();
  await settings.getByRole("button", { name: "Close" }).last().click();
  await trigger(page).click();
  await expect(
    page.getByRole("option", { name: /Claude Code · Default/ }),
  ).toContainText("Ready");
});

test("installs the Antigravity agent, signs in with Google and shows its rows Ready", async ({
  page,
}) => {
  await page.keyboard.press("Control+,");
  const settings = page.getByRole("dialog", { name: "Settings" });
  const card = settings.getByRole("article", { name: "Antigravity" });
  await expect(card).toContainText("Setup required");
  await card.getByRole("button", { name: "Install Antigravity agent" }).click();
  const dialog = page.getByRole("dialog", {
    name: "Install the Antigravity agent",
  });
  await expect(dialog).toContainText("334 MB");
  await expect(dialog).toContainText("about 1.1 GB");
  await expect(dialog).toContainText("dl.google.com");
  await expect(dialog).toContainText(
    "/home/user/.local/share/shadowcode/antigravity-acp/1.2.1",
  );
  await expect(
    new AxeBuilder({ page })
      .include('[role="dialog"]')
      .withTags(["wcag2a", "wcag2aa", "wcag21aa"])
      .analyze(),
  ).resolves.toMatchObject({ violations: [] });
  await page.screenshot({
    path: "test-results/antigravity-install-dialog.png",
  });
  await dialog.getByRole("button", { name: "Install", exact: true }).click();
  await expect(dialog).toHaveCount(0);

  const progress = card.getByRole("progressbar", {
    name: "Installing the Antigravity agent",
  });
  await expect(progress).toBeVisible();
  await expect(card).toContainText("Downloading 120 MB of 334 MB");
  await expect(progress).toHaveAttribute("aria-valuenow", "36");
  await card.screenshot({
    path: "test-results/antigravity-install-progress.png",
  });
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
  }
  await page.evaluate(() => (document.documentElement.dataset.theme = "light"));

  const connect = card.getByRole("button", { name: "Connect" });
  await expect(connect).toBeVisible({ timeout: 10000 });
  await expect(card.getByText("Sign in", { exact: true })).toBeVisible();
  await expect(progress).toHaveCount(0);
  await connect.click();
  await expect(
    card.getByRole("link", { name: /accounts\.google\.com/ }),
  ).toBeVisible();
  await expect(card.getByText("Signed in.")).toBeVisible({ timeout: 10000 });
  await expect(card.getByText("Ready", { exact: true })).toBeVisible();
  await expect(card.getByText("2 models available")).toBeVisible();
  await expect(
    card.getByRole("button", { name: "Remove agent" }),
  ).toBeVisible();
  await settings.getByRole("button", { name: "Close" }).last().click();
  await expect(settings).toHaveCount(0);

  await trigger(page).click();
  for (const name of [
    /Antigravity · Gemini 3\.5 Pro/,
    /Antigravity · Gemini 3\.5 Flash/,
  ])
    await expect(page.getByRole("option", { name })).toContainText("Ready");
  const log = await fakeLog(page);
  expect(
    log.filter(
      (r) =>
        r.method === "POST" && r.path === "/api/accounts/antigravity/install",
    ),
  ).toEqual([
    {
      method: "POST",
      path: "/api/accounts/antigravity/install",
      body: { confirm: true },
    },
  ]);
});

test("an OpenRouter API key unlocks per-token models in the picker", async ({
  page,
}) => {
  await trigger(page).click();
  const api = page.getByRole("group", { name: "API keys" });
  await expect(api).toContainText("Billed per token by the provider");
  await expect(
    api.getByRole("option", { name: "Show all 40 OpenRouter models" }),
  ).toBeVisible();
  const keyless = api.getByRole("option", { name: /Qwen: Qwen3 Coder/ });
  await expect(keyless).toContainText("Add API key");
  await keyless.click();
  const settings = page.getByRole("dialog", { name: "Settings" });
  const card = settings.getByRole("article", { name: "OpenRouter" });
  const input = card.getByLabel("OpenRouter API key");
  await expect(input).toBeFocused();
  await input.fill("sk-or-nope");
  await card.getByRole("button", { name: "Save" }).click();
  await expect(card.getByRole("alert")).toHaveText(
    "OpenRouter rejected this key (401)",
  );
  await input.fill("sk-or-valid");
  await card.getByRole("button", { name: "Save" }).click();
  await expect(card.getByText("Key: sk-or-v1-a1b…9f2")).toBeVisible();
  await expect(card.getByText(/40 models \(30 support tools\)/)).toBeVisible();
  expect(await page.content()).not.toContain("sk-or-valid");
  await expect(
    new AxeBuilder({ page })
      .include('[role="dialog"]')
      .withTags(["wcag2a", "wcag2aa", "wcag21aa"])
      .analyze(),
  ).resolves.toMatchObject({ violations: [] });
  await card.screenshot({ path: "test-results/openrouter-card.png" });
  await settings.getByRole("button", { name: "Close" }).last().click();
  await expect(settings).toHaveCount(0);

  await trigger(page).click();
  await expect(
    page
      .getByRole("group", { name: "API keys" })
      .getByRole("option", { name: /Qwen: Qwen3 Coder/ }),
  ).toContainText("Ready");
  await page.screenshot({ path: "test-results/picker-openrouter.png" });
  const search = page.getByRole("combobox", { name: "Search models" });
  await search.fill("qwen coder");
  await expect(page.getByRole("option")).toHaveCount(1);
  await search.press("Enter");
  await expect(trigger(page)).toContainText("Qwen: Qwen3 Coder");
  await expect(trigger(page)).toContainText("API key");
  // OpenRouter runs on ShadowCode's own loop, so the Web toggle applies.
  const web = page.getByRole("button", { name: "Web lookups for this task" });
  await expect(web).toHaveAttribute("aria-pressed", "false");
  await web.click();
  await expect(web).toHaveAttribute("aria-pressed", "true");
  await prompt(page).fill("Fix the add function");
  await send(page).click();
  await expect(page.getByRole("region", { name: "Task summary" })).toBeVisible({
    timeout: 15000,
  });
  const posts = (await fakeLog(page)).filter(
    (r) => r.path === "/api/jobs" && r.method === "POST",
  );
  expect(posts.map((r) => r.body.model)).toEqual([
    "api:openrouter:qwen/qwen3-coder",
  ]);
  expect(posts[0].body.web).toBe(true);
});

test("loads a local model from Settings", async ({ page }) => {
  await page.keyboard.press("Control+,");
  const settings = page.getByRole("dialog", { name: "Settings" });
  await settings.getByRole("button", { name: "Local models" }).click();
  await expect(settings.getByText(/Ready · Vulkan/)).toBeVisible();
  await expect(settings.getByText("No model loaded")).toBeVisible();
  const qwen = settings.getByRole("article", { name: "qwen3:14b" });
  await qwen.getByRole("button", { name: "Load" }).click();
  await expect(settings.getByText(/qwen3:14b · Vulkan0/)).toBeVisible();
  await expect(qwen.getByRole("button", { name: "Unload" })).toBeVisible();
  const gptoss = settings.getByRole("article", { name: "gpt-oss:20b" });
  await expect(gptoss).toContainText("unknown model architecture: gptoss");
  await expect(
    new AxeBuilder({ page })
      .include('[role="dialog"]')
      .withTags(["wcag2a", "wcag2aa"])
      .analyze(),
  ).resolves.toMatchObject({ violations: [] });
});

test("light and dark themes pass accessibility checks, picker open", async ({
  page,
}) => {
  for (const theme of ["light", "dark"]) {
    await page.evaluate(
      (t) => (document.documentElement.dataset.theme = t),
      theme,
    );
    await trigger(page).click();
    const results = await new AxeBuilder({ page })
      .withTags(["wcag2a", "wcag2aa", "wcag21aa"])
      .analyze();
    expect(results.violations).toEqual([]);
    await page.screenshot({ path: `test-results/picker-${theme}.png` });
    await page.keyboard.press("Escape");
    await expect(page.getByRole("listbox")).toHaveCount(0);
  }
});

test("settings sections are accessible and trap focus", async ({ page }) => {
  await page.keyboard.press("Control+,");
  const dialog = page.getByRole("dialog", { name: "Settings" });
  for (const section of [
    "Accounts",
    "Local models",
    "Permissions & network",
    "Appearance",
    "Advanced",
  ]) {
    await dialog.getByRole("button", { name: section, exact: true }).click();
    if (section === "Accounts")
      await page.screenshot({ path: "test-results/accounts.png" });
    if (section === "Local models")
      await page.screenshot({ path: "test-results/local-models.png" });
    const results = await new AxeBuilder({ page })
      .include('[role="dialog"]')
      .withTags(["wcag2a", "wcag2aa", "wcag21aa"])
      .analyze();
    expect(results.violations).toEqual([]);
  }
  await dialog.locator("button").last().focus();
  await page.keyboard.press("Tab");
  expect(
    await dialog.evaluate((el) => el.contains(document.activeElement)),
  ).toBe(true);
  // Global shortcuts stay inactive behind a dialog.
  await page.keyboard.press("Control+b");
  await expect(page.locator(".sidebar")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(dialog).toHaveCount(0);
});

test("the window works at its 520 px minimum width", async ({ page }) => {
  await page.setViewportSize({ width: 520, height: 800 });
  await expect(page.locator(".sidebar")).toHaveCount(0);
  await expect(prompt(page)).toBeVisible();
  await expect(trigger(page)).toBeVisible();
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
  await trigger(page).click();
  const menu = page.locator(".unified-picker-menu");
  const box = await menu.boundingBox();
  expect(box && box.x >= 0 && box.x + box.width <= 520).toBe(true);
  await page.screenshot({ path: "test-results/compact.png", fullPage: true });
  await page.keyboard.press("Escape");
  const more = page.locator("details.composer-more");
  await more.locator("summary").click();
  const moreBox = await more.locator(".composer-more-menu").boundingBox();
  expect(moreBox && moreBox.x >= 0 && moreBox.x + moreBox.width <= 520).toBe(
    true,
  );
  await page.keyboard.press("Escape");
});

test("less-frequent composer options stay discoverable and close with Escape", async ({
  page,
}) => {
  await chooseBySearch(page, "qwen3:14b");
  await prompt(page).fill("Fix a bug in the task view");
  await page.screenshot({ path: "test-results/workspace-light.png" });
  const more = page.locator("details.composer-more");
  await expect(more).not.toHaveAttribute("open", "");
  await more.locator("summary").click();
  await expect(more).toHaveAttribute("open", "");
  await expect(
    more.getByRole("button", { name: "Compare", exact: true }),
  ).toBeVisible();
  await expect(more.getByLabel("Reasoning effort")).toBeVisible();
  await expect(
    more.getByRole("button", { name: "Run in a new worktree" }),
  ).toBeVisible();
  await expect(more.locator(".composer-more-heading")).toHaveText(
    "More task options",
  );
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
  await axeClean(page);
  await page.screenshot({ path: "test-results/composer-more.png" });
  await page.keyboard.press("Escape");
  await expect(more).not.toHaveAttribute("open", "");
  await page.evaluate(() => (document.documentElement.dataset.theme = "dark"));
  await page.evaluate(() =>
    (document.activeElement as HTMLElement | null)?.blur(),
  );
  await page.screenshot({ path: "test-results/workspace-dark.png" });
});

test("Allowance shows what is left; in Ask mode a plan limit offers to continue on this computer", async ({
  page,
}) => {
  // A saved OpenRouter key with a credit limit, and a Codex plan that runs out
  // on the next task.
  await page.evaluate(() => {
    const fake = (window as unknown as { __SHADOW_FAKE__: { state: any } })
      .__SHADOW_FAKE__.state;
    fake.openrouter.key = "sk-or-valid";
    fake.openrouter.info = {
      label: "sk-or-v1-a1b…9f2",
      usage: 1.2345,
      limit: 10,
      limit_remaining: 8.7655,
      is_free_tier: false,
    };
    fake.limitOnCodex = true;
  });
  const button = page.getByRole("button", { name: /^Allowance/ });
  await expect(button).toBeVisible();
  await expect(button.locator(".allowance-dot")).toHaveClass(/warn/);
  await button.click();
  const dialog = page.getByRole("dialog", { name: "Allowance" });
  await expect(dialog).toBeVisible();
  const codex = dialog.getByRole("article", { name: "Codex" });
  await expect(codex).toContainText("2% left");
  await expect(codex).toContainText(/Weekly · 2% left · resets in (2h 59m|3h)/);
  await expect(
    codex.getByRole("meter", { name: "Codex remaining" }),
  ).toHaveAttribute("aria-valuenow", "2");
  const openrouter = dialog.getByRole("article", { name: "OpenRouter" });
  await expect(openrouter).toContainText("$8.77 of $10.00 left");
  await expect(
    openrouter.getByRole("meter", { name: "OpenRouter remaining" }),
  ).toHaveAttribute("aria-valuenow", "88");
  await expect(dialog.getByRole("article", { name: "Cursor" })).toContainText(
    "Plan limit reached",
  );
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
    await dialog.screenshot({ path: `test-results/allowance-${theme}.png` });
  }
  await page.evaluate(() => (document.documentElement.dataset.theme = "light"));

  const local = dialog.getByRole("article", { name: "On this computer" });
  await expect(
    local.getByRole("radio", { name: /Continue on qwen3:14b/ }),
  ).toBeChecked();
  await local.getByRole("radio", { name: /Ask me/ }).check();
  await expect
    .poll(async () =>
      (await fakeLog(page)).some(
        (r) =>
          r.method === "PUT" &&
          r.path === "/api/config" &&
          r.body.values?.limits?.on_limit === "ask",
      ),
    )
    .toBe(true);
  await page.keyboard.press("Escape");
  await expect(dialog).toHaveCount(0);
  await expect(button).toBeFocused();

  await chooseBySearch(page, "GPT-6-Astra");
  await prompt(page).fill("Fix the add function");
  await send(page).click();
  const card = page.getByRole("region", { name: "Plan limit reached" });
  await expect(card).toContainText("Codex reached its plan limit.", {
    timeout: 15000,
  });
  const summary = page.getByRole("region", { name: "Task summary" });
  await expect(summary).toContainText("Plan limit reached");
  await expect(summary).not.toHaveClass(/is-bad/);
  const resume = card.getByRole("button", { name: "Continue on qwen3:14b" });
  await expect(resume).toBeVisible();
  await expect(card.getByRole("button", { name: "Try on…" })).toBeVisible();
  await expect(
    card.getByRole("button", { name: /^Resume on Codex (?:tomorrow )?at / }),
  ).toBeVisible();
  for (const theme of ["light", "dark"]) {
    await page.evaluate(
      (t) => (document.documentElement.dataset.theme = t),
      theme,
    );
    const results = await new AxeBuilder({ page })
      .withTags(["wcag2a", "wcag2aa", "wcag21aa"])
      .analyze();
    expect(results.violations).toEqual([]);
    await page.screenshot({ path: `test-results/limit-ask-${theme}.png` });
  }
  await page.evaluate(() => (document.documentElement.dataset.theme = "light"));

  await resume.click();
  await expect(trigger(page)).toContainText("qwen3:14b");
  await expect(page.getByText("Continued after the plan limit")).toBeVisible();
  await expect(summary).toHaveCount(2, { timeout: 15000 });
  await expect(summary.last()).toContainText("Finished");
  await expect(card.getByRole("button")).toHaveCount(0);
  const posts = (await fakeLog(page)).filter(
    (r) => r.path === "/api/jobs" && r.method === "POST",
  );
  expect(posts.map((r) => r.body.model)).toEqual([
    "cli:codex:gpt-6-astra",
    "local:gguf:qwen",
  ]);
  expect(posts[1].body.task).toBe(
    "Continue where Codex stopped when its plan limit was reached. The request was:\n\nFix the add function",
  );
  expect(posts[1].body.session_id).toBe(posts[0].body.session_id);
  await page.screenshot({
    path: "test-results/limit-continued.png",
    fullPage: true,
  });
});

const axeClean = async (page: Page, include?: string) => {
  const builder = new AxeBuilder({ page }).withTags([
    "wcag2a",
    "wcag2aa",
    "wcag21aa",
  ]);
  if (include) builder.include(include);
  expect((await builder.analyze()).violations).toEqual([]);
};

async function pickSlot(page: Page, slot: number, text: string) {
  const dialog = page.getByRole("dialog", { name: "Compare models" });
  await dialog
    .getByRole("button", { name: new RegExp(`^Model ${slot}:`) })
    .click();
  const search = dialog.getByRole("combobox", { name: "Search models" });
  await expect(search).toBeFocused();
  await search.fill(text);
  await search.press("Enter");
  await expect(dialog.getByRole("listbox")).toHaveCount(0);
}

test("compares a local and a cloud model, keeps one and counts the win", async ({
  page,
}) => {
  const compare = await openCompare(page);
  await expect(compare).toHaveAttribute("aria-disabled", "true");
  await expect(compare).toHaveAttribute("title", /Type a task/);
  await prompt(page).fill("Fix the add function");
  await expect(compare).toHaveAttribute("aria-disabled", "false");
  await compare.click();
  const dialog = page.getByRole("dialog", { name: "Compare models" });
  await expect(dialog).toBeVisible();
  await expect(dialog.locator(".compare-task")).toHaveText(
    "Fix the add function",
  );
  await expect(dialog).toContainText(
    "Each model uses its own plan allowance or OpenRouter credit.",
  );
  await pickSlot(page, 1, "qwen3:14b");
  await pickSlot(page, 2, "GPT-6-Astra");
  await expect(
    dialog.getByRole("button", { name: /^Model 1: qwen3:14b/ }),
  ).toContainText("Local");
  await expect(
    dialog.getByRole("button", { name: /^Model 2: Codex · GPT-6-Astra/ }),
  ).toContainText("Cloud");
  await expect(dialog.locator(".compare-cost")).toContainText(
    "Runs on this computer · No subscription quota",
  );
  await expect(dialog.locator(".compare-cost")).toContainText(
    "Shared plan usage · 2% left",
  );
  for (const theme of ["light", "dark"]) {
    await page.evaluate(
      (t) => (document.documentElement.dataset.theme = t),
      theme,
    );
    await axeClean(page, '[role="dialog"]');
    await dialog.screenshot({
      path: `test-results/compare-dialog-${theme}.png`,
    });
  }
  await page.evaluate(() => (document.documentElement.dataset.theme = "light"));

  await dialog.getByRole("button", { name: "Start comparison" }).click();
  await expect(dialog).toHaveCount(0);
  const post = (await fakeLog(page)).find((r) => r.path === "/api/compare");
  expect(post?.body).toMatchObject({
    task: "Fix the add function",
    models: ["local:gguf:qwen", "cli:codex:gpt-6-astra"],
  });
  const view = page.getByRole("region", { name: "Comparisons" });
  await expect(view).toBeVisible();
  const lanes = view.getByRole("article");
  await expect(lanes).toHaveCount(2);
  const local = view.getByRole("article", { name: "qwen3:14b" });
  const cloud = view.getByRole("article", { name: "Codex · GPT-6-Astra" });
  await expect(local.locator(".inference-badge")).toHaveText("Local");
  await expect(cloud.locator(".inference-badge")).toHaveText("Cloud");
  await expect(cloud).toContainText("Working…");
  await expect(
    cloud.getByRole("button", { name: "Keep this one" }),
  ).toBeDisabled();
  await page.screenshot({ path: "test-results/compare-running.png" });
  // Lane conversations stay out of the sidebar.
  await expect(page.locator(".sidebar")).not.toContainText("Compare ·");

  await expect(local).toContainText("Finished", { timeout: 15000 });
  await expect(cloud).toContainText("Finished", { timeout: 15000 });
  await expect(view).toContainText("Finished · choose a result to keep");
  await expect(cloud).toContainText("src/app.test.ts");
  await expect(cloud).toContainText("+12");
  await expect(cloud.locator(".compare-checks summary")).toHaveText(
    "1 passed · 1 failed",
  );
  await expect(local.locator(".compare-checks summary")).toHaveText("1 passed");
  for (const theme of ["light", "dark"]) {
    await page.evaluate(
      (t) => (document.documentElement.dataset.theme = t),
      theme,
    );
    await axeClean(page);
    await page.screenshot({ path: `test-results/compare-view-${theme}.png` });
  }
  await page.evaluate(() => (document.documentElement.dataset.theme = "light"));

  await cloud.getByRole("button", { name: "Keep this one" }).click();
  const confirm = page.getByRole("dialog", {
    name: "Keep Codex · GPT-6-Astra's result",
  });
  await expect(confirm).toContainText(
    "Apply Codex · GPT-6-Astra’s changes to your project as uncommitted changes and remove the other copies?",
  );
  await confirm
    .getByRole("button", { name: "Keep Codex · GPT-6-Astra" })
    .click();
  await expect(confirm).toHaveCount(0);
  const applied = view.locator(".compare-applied");
  await expect(applied).toContainText(
    "Applied 2 files from Codex · GPT-6-Astra — review them in Changes",
  );
  await expect(cloud).toContainText("Kept");
  const board = view.getByRole("region", { name: "Wins in this project" });
  await expect(
    board.getByRole("row", { name: /Codex · GPT-6-Astra 1 1/ }),
  ).toBeVisible();
  await expect(board.getByRole("row", { name: /qwen3:14b 0 1/ })).toBeVisible();
  await page.screenshot({ path: "test-results/compare-kept.png" });
  await applied.getByRole("button", { name: "Open Changes" }).click();
  const drawer = page.getByRole("complementary", { name: "Drawer" });
  await expect(drawer).toContainText("src/app.test.ts");
  await expect(drawer).toContainText("src/app.ts");
  await expect(
    page.getByRole("button", { name: "Review changes" }).locator(".count"),
  ).toHaveText("2");

  // The narrow window keeps the view without horizontal scrolling.
  await page.getByRole("button", { name: "Close drawer" }).click();
  await page.setViewportSize({ width: 520, height: 800 });
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
  await page.screenshot({
    path: "test-results/compare-520.png",
    fullPage: true,
  });
  // …and so does the dialog with a model list open.
  await page
    .getByRole("button", { name: "Back to conversation", exact: true })
    .click();
  await prompt(page).fill("Tidy the README");
  await (await openCompare(page)).click();
  const again = page.getByRole("dialog", { name: "Compare models" });
  // The last lineup is remembered.
  await expect(
    again.getByRole("button", { name: /^Model 1: qwen3:14b/ }),
  ).toBeVisible();
  await again.getByRole("button", { name: /^Model 2:/ }).click();
  await expect(again.getByRole("listbox")).toBeVisible();
  const box = await again.boundingBox();
  expect(box && box.x >= 0 && box.x + box.width <= 520).toBe(true);
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
  await page.screenshot({ path: "test-results/compare-dialog-520.png" });
  await page.keyboard.press("Escape");
  await expect(again.getByRole("listbox")).toHaveCount(0);
  await page.keyboard.press("Escape");
  await expect(again).toHaveCount(0);
  await page.screenshot({ path: "test-results/compare-composer-520.png" });
});

test("requires open editor drafts to be saved before Compare snapshots the project", async ({
  page,
}) => {
  await prompt(page).fill("Review the current README");
  await page.getByRole("button", { name: "Review changes" }).click();
  const drawer = page.getByRole("complementary", { name: "Drawer" });
  await drawer
    .locator(".drawer-tabs")
    .getByRole("button", { name: "Files" })
    .click();
  await drawer.getByRole("button", { name: "README.md", exact: true }).click();
  const editor = drawer.getByRole("textbox", { name: "Edit README.md" });
  await expect(editor).toBeVisible();
  await editor.fill("# Unsaved README draft\n");

  await (await openCompare(page)).click();
  const dialog = page.getByRole("dialog", { name: "Compare models" });
  await pickSlot(page, 1, "qwen3:14b");
  await pickSlot(page, 2, "GPT-6-Astra");
  await expect(dialog).toContainText("README.md");
  const start = dialog.getByRole("button", { name: "Start comparison" });
  await expect(start).toBeDisabled();
  expect((await fakeLog(page)).some((r) => r.path === "/api/compare")).toBe(
    false,
  );

  await dialog.getByRole("button", { name: "Close Compare" }).click();
  await drawer.getByRole("button", { name: "Save", exact: true }).click();
  await expect(drawer.getByText("Saved", { exact: true })).toBeVisible();
  await (await openCompare(page)).click();
  const readyDialog = page.getByRole("dialog", { name: "Compare models" });
  await expect(
    readyDialog.getByText(/Save or discard these open drafts/),
  ).toHaveCount(0);
  await expect(
    readyDialog.getByRole("button", { name: "Start comparison" }),
  ).toHaveAttribute("aria-disabled", "false");
});

test("Compare rejects a recovery draft saved by another editor window", async ({
  page,
}) => {
  await page.evaluate(() => {
    // Simulate a recovery record written by another app window after this
    // window loaded its own editor state. No in-memory buffer sees this draft.
    sessionStorage.setItem(
      "shadow-fake-editor-drafts",
      JSON.stringify({
        "src/other-window.ts": {
          path: "src/other-window.ts",
          base: "const value = 1;\n",
          draft: "const value = 2;\n",
          base_hash: "0".repeat(64),
          revision: "1".repeat(32),
          updated_at: 1,
        },
      }),
    );
  });
  await prompt(page).fill("Compare the existing model results");
  await (await openCompare(page)).click();
  const dialog = page.getByRole("dialog", { name: "Compare models" });
  await pickSlot(page, 1, "qwen3:14b");
  await pickSlot(page, 2, "GPT-6-Astra");
  await dialog.getByRole("button", { name: "Start comparison" }).click();
  await expect(dialog.getByRole("alert")).toContainText(
    "Save or discard open editor drafts before comparing: src/other-window.ts",
  );
  expect((await fakeLog(page)).some((row) => row.path === "/api/compare")).toBe(
    true,
  );
  await expect(page.getByRole("region", { name: "Comparisons" })).toHaveCount(
    0,
  );
});

test("a lane's approval is answered in its conversation; a conflicting Keep names the files", async ({
  page,
}) => {
  await page.evaluate(() => {
    const fake = (window as unknown as { __SHADOW_FAKE__: { state: any } })
      .__SHADOW_FAKE__.state;
    fake.compare.approval = true;
    fake.compare.conflict = true;
  });
  await prompt(page).fill("Add a regression test for add");
  await (await openCompare(page)).click();
  await pickSlot(page, 1, "qwen3:14b");
  await pickSlot(page, 2, "GPT-6-Luna");
  await page
    .getByRole("dialog", { name: "Compare models" })
    .getByRole("button", { name: "Start comparison" })
    .click();
  const view = page.getByRole("region", { name: "Comparisons" });
  const cloud = view.getByRole("article", { name: "Codex · GPT-6-Luna" });
  await expect(cloud).toContainText("Waiting for approval", { timeout: 15000 });
  await page.screenshot({ path: "test-results/compare-approval.png" });
  await cloud.getByRole("button", { name: "Answer in conversation" }).click();
  const banner = page.locator(".compare-banner");
  await expect(banner).toContainText("Part of a comparison");
  const approval = page.locator(".approval");
  await expect(approval).toContainText("npm install --save-dev vitest");
  await page.screenshot({ path: "test-results/compare-lane-conversation.png" });
  // The lane's copy is not listed as a project.
  await expect(page.locator(".sidebar")).not.toContainText("demo-1-");
  await approval.getByRole("button", { name: "Allow" }).click();
  await expect(approval).toHaveCount(0);
  await banner.getByRole("button", { name: "Back to comparison" }).click();
  await expect(view).toBeVisible();
  await expect(cloud).toContainText("Finished", { timeout: 15000 });
  await expect(view.getByRole("article", { name: "qwen3:14b" })).toContainText(
    "Finished",
    { timeout: 15000 },
  );

  await cloud.getByRole("button", { name: "Keep this one" }).click();
  await page
    .getByRole("dialog", { name: "Keep Codex · GPT-6-Luna's result" })
    .getByRole("button", { name: "Keep Codex · GPT-6-Luna" })
    .click();
  const conflict = view.locator(".compare-conflict");
  await expect(conflict).toContainText(
    "Codex · GPT-6-Luna's changes no longer apply.",
  );
  await expect(
    conflict.getByRole("list", { name: "Conflicting files" }),
  ).toContainText("src/app.test.ts");
  await expect(view).toContainText("Finished · choose a result to keep");
  await page.screenshot({ path: "test-results/compare-conflict.png" });
  // Resolved: keeping again applies.
  await cloud.getByRole("button", { name: "Keep this one" }).click();
  await page
    .getByRole("dialog", { name: "Keep Codex · GPT-6-Luna's result" })
    .getByRole("button", { name: "Keep Codex · GPT-6-Luna" })
    .click();
  await expect(view.locator(".compare-applied")).toContainText(
    "Applied 2 files",
  );
  await expect(conflict).toHaveCount(0);
});
