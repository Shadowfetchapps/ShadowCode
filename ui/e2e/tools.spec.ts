import { test, expect, type Page } from "@playwright/test";
import { installFakeBackend } from "./fakeBackend";
import { installFakeTools } from "./fakeTools";

// The drawer's Terminal, Git and Tools tabs against the fake engine (with the
// fake terminals and Git routes from fakeTools.ts layered on top).
async function start(page: Page, ghReady = true, secrets = false) {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  (page as Page & { errors?: string[] }).errors = errors;
  await page.addInitScript(installFakeBackend, { stepMs: 90 });
  await page.addInitScript(installFakeTools, { ghReady, secrets });
  await page.goto("/");
  await expect(
    page.getByRole("heading", { name: "What should we work on?" }),
  ).toBeVisible();
}
test.afterEach(async ({ page }) => {
  expect((page as Page & { errors?: string[] }).errors).toEqual([]);
});

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
const drawer = (page: Page) =>
  page.getByRole("complementary", { name: "Drawer" });
const tab = (page: Page, name: string) =>
  drawer(page).locator(".drawer-tabs").getByRole("button", { name });

async function openDrawer(page: Page, name: string) {
  await page.getByRole("button", { name: "Review changes" }).click();
  await tab(page, name).click();
}

test("terminals keep running across tabs, the drawer and several shells", async ({
  page,
}) => {
  await start(page);
  await openDrawer(page, "Terminal");
  const first = drawer(page).getByRole("group", { name: "Terminal 1" });
  await expect(first).toContainText("demo$");
  await first.click();
  await page.keyboard.type("echo hello from one");
  await page.keyboard.press("Enter");
  await expect(first).toContainText("hello from one");
  // A second shell from "+".
  await drawer(page).getByRole("button", { name: "New terminal" }).click();
  const second = drawer(page).getByRole("group", { name: "Terminal 2" });
  await expect(second).toContainText("demo$");
  await second.click();
  await page.keyboard.type("pwd");
  await page.keyboard.press("Enter");
  await expect(second).toContainText("/work/demo");
  // Back to the first: its output is still there.
  await drawer(page)
    .getByRole("button", { name: "Terminal 1", exact: true })
    .click();
  await expect(first).toContainText("hello from one");
  // Another tab, then closing and reopening the drawer, keep both shells.
  await tab(page, "Files").click();
  await tab(page, "Terminal").click();
  await expect(first).toContainText("hello from one");
  await drawer(page).getByRole("button", { name: "Close drawer" }).click();
  await expect(drawer(page)).toHaveCount(0);
  await page.keyboard.press("Control+`");
  await expect(first).toContainText("hello from one");
  // Escape inside the terminal belongs to the shell, not the drawer.
  await first.click();
  await page.keyboard.press("Escape");
  await expect(drawer(page)).toBeVisible();
  const opened = (await fakeLog(page)).filter(
    (r) => r.method === "POST" && r.path === "/api/terminals",
  );
  expect(opened).toHaveLength(2);
  const typed = (await fakeLog(page))
    .filter((r) => r.path.endsWith("/input"))
    .map((r) => r.body.data)
    .join("");
  expect(typed).toContain("echo hello from one\r");
  // Closing a tab ends that shell only.
  await drawer(page).getByRole("button", { name: "Close Terminal 2" }).click();
  await expect(second).toHaveCount(0);
  await expect(first).toBeVisible();
});

test("suggests a commit message, then commits the edited text", async ({
  page,
}) => {
  await start(page);
  await openDrawer(page, "Git");
  const panel = drawer(page);
  await expect(panel.getByText("Up to date with origin/main")).toBeVisible();
  await expect(
    panel.getByRole("button", { name: "Suggest message" }),
  ).toBeDisabled();
  await panel.getByRole("button", { name: "Stage all" }).click();
  await expect(panel.getByText("1 file staged")).toBeVisible();
  await panel.getByRole("button", { name: "Suggest message" }).click();
  const message = panel.getByRole("textbox", { name: "Commit message" });
  await expect(message).toHaveValue(
    "Fix the add function\n\nIt subtracted its arguments.",
  );
  await expect(
    panel.getByText("Drafted by the local model qwen3:14b."),
  ).toBeVisible();
  await message.fill("Fix add\n\nIt subtracted.");
  await panel.getByRole("button", { name: "Commit", exact: true }).click();
  await expect(page.getByText("Committed")).toBeVisible();
  await expect(message).toHaveValue("");
  await expect(panel.getByText("1 to push · origin/main")).toBeVisible();
  const commit = (await fakeLog(page)).find(
    (r) => r.path === "/api/workspace/git/commit",
  );
  expect(commit?.body.message).toBe("Fix add\n\nIt subtracted.");
});

test("creates a branch and a draft pull request, then shows its checks", async ({
  page,
}) => {
  await start(page);
  await openDrawer(page, "Git");
  const panel = drawer(page);
  const name = panel.getByRole("textbox", { name: "New branch name" });
  await name.fill("fix add");
  await expect(panel.getByText(/cannot contain spaces/)).toBeVisible();
  await expect(
    panel.getByRole("button", { name: "Create branch" }),
  ).toBeDisabled();
  await name.fill("fix/add");
  await panel.getByRole("button", { name: "Create branch" }).click();
  await expect(panel.getByText("fix/add", { exact: true })).toBeVisible();
  await panel.getByRole("button", { name: "Stage all" }).click();
  await panel
    .getByRole("textbox", { name: "Commit message" })
    .fill("Fix the add function");
  await panel.getByRole("button", { name: "Commit", exact: true }).click();
  await expect(panel.getByText(/Not on origin yet/)).toBeVisible();
  await panel
    .getByRole("button", { name: "Suggest title and description" })
    .click();
  await expect(
    panel.getByRole("textbox", { name: "Pull request title" }),
  ).toHaveValue("Fix the add function");
  await expect(
    panel.getByRole("textbox", { name: "Pull request description" }),
  ).toHaveValue(/## Changes/);
  await panel
    .getByRole("combobox", { name: "Base branch" })
    .selectOption("develop");
  await panel.getByRole("checkbox", { name: "Draft" }).check();
  await panel.getByRole("button", { name: "Create pull request" }).click();
  await expect(
    page.getByText("Pushed the branch and opened the pull request"),
  ).toBeVisible();
  const link = panel.getByRole("link", { name: /#7 Fix the add function/ });
  await expect(link).toHaveAttribute(
    "href",
    "https://github.com/octo/demo/pull/7",
  );
  await expect(panel.getByText("· draft")).toBeVisible();
  await expect(panel.getByText("Checks are running")).toBeVisible();
  await expect(panel.getByRole("link", { name: "build" })).toBeVisible();
  const created = (await fakeLog(page)).find(
    (r) => r.method === "POST" && r.path === "/api/git/pr",
  );
  expect(created?.body).toMatchObject({
    title: "Fix the add function",
    base: "develop",
    draft: true,
  });
  await panel.getByRole("button", { name: "Refresh checks" }).click();
  await expect
    .poll(
      async () =>
        (await fakeLog(page)).filter((r) =>
          r.path.startsWith("/api/git/pr/checks"),
        ).length,
    )
    .toBeGreaterThan(1);
});

test("without a signed-in gh, explains sign-in and opens the compare page", async ({
  page,
}) => {
  await start(page, false);
  await openDrawer(page, "Git");
  const panel = drawer(page);
  await panel.getByRole("textbox", { name: "New branch name" }).fill("fix/add");
  await panel.getByRole("button", { name: "Create branch" }).click();
  await expect(panel.getByText(/installed but not signed in/)).toBeVisible();
  await expect(
    panel.getByText("gh auth login --hostname github.com"),
  ).toBeVisible();
  await expect(
    panel.getByRole("button", { name: "Create pull request" }),
  ).toHaveCount(0);
  await panel
    .getByRole("button", { name: /Open compare page in browser/ })
    .click();
  await expect
    .poll(async () =>
      (await fakeLog(page)).find(
        (r) => r.method === "INVOKE" && r.path === "open_external",
      ),
    )
    .toMatchObject({
      body: {
        url: "https://github.com/octo/demo/compare/main...fix/add?expand=1",
      },
    });
  // "Open the Terminal" jumps to a shell to run the sign-in command.
  await panel.getByRole("button", { name: "Open the Terminal" }).click();
  await expect(
    drawer(page).getByRole("group", { name: "Terminal 1" }),
  ).toBeVisible();
});

test("goals, processes and worktrees moved from Settings to the drawer", async ({
  page,
}) => {
  await start(page);
  await page.keyboard.press("Control+,");
  const settings = page.getByRole("dialog", { name: "Settings" });
  await settings.getByRole("button", { name: "Advanced", exact: true }).click();
  const sections = settings.getByRole("tablist", { name: "Advanced sections" });
  for (const kept of [
    "Skills",
    "Health",
    "MCP",
    "Plugins",
    "Hooks",
    "Guardian",
    "Vendor tools",
  ])
    await expect(sections.getByRole("tab", { name: kept })).toBeVisible();
  for (const moved of ["Goals", "Background", "Worktrees"])
    await expect(sections.getByRole("tab", { name: moved })).toHaveCount(0);
  // Permissions: per-runner prose is folded away and the raw key is gone.
  await settings
    .getByRole("button", { name: "Permissions & network", exact: true })
    .click();
  const codex = settings.getByText("How Codex applies this");
  await expect(codex).toBeVisible();
  await expect(
    settings.getByText(/Codex sandbox: workspace-write/),
  ).toBeHidden();
  await codex.click();
  await expect(
    settings.getByText(/Codex sandbox: workspace-write/),
  ).toBeVisible();
  await expect(settings).not.toContainText(/\bnative\b/);
  await page.keyboard.press("Escape");

  // The palette still reaches them, now in the drawer's Tools tab.
  await page.keyboard.press("Control+k");
  await page.keyboard.type("Worktrees");
  await page.keyboard.press("Enter");
  const tools = drawer(page).getByRole("region", { name: "Tools" });
  await expect(
    tools.getByRole("button", { name: "Worktrees" }),
  ).toHaveAttribute("aria-pressed", "true");
  await tools.getByRole("button", { name: "Goals" }).click();
  await expect(tools.getByText("Ship the login page")).toBeVisible();
  await tools.getByRole("button", { name: "Processes" }).click();
  await expect(
    tools.getByText(/Development servers and watchers/),
  ).toBeVisible();
  // The Tools tab remembers the last view.
  await tab(page, "Files").click();
  await tab(page, "Tools").click();
  await expect(
    tools.getByRole("button", { name: "Processes" }),
  ).toHaveAttribute("aria-pressed", "true");
});

test("a staged secret is shown before the commit and the push, and goes only when chosen", async ({
  page,
}) => {
  await start(page, true, true);
  await openDrawer(page, "Git");
  const panel = drawer(page);
  await panel.getByRole("button", { name: "Stage all" }).click();
  await panel
    .getByRole("textbox", { name: "Commit message" })
    .fill("Add config");
  await panel.getByRole("button", { name: "Commit", exact: true }).click();
  const dialog = page.getByRole("dialog", {
    name: "This commit may contain a secret",
  });
  await expect(dialog).toContainText("src/config.js");
  await expect(dialog).toContainText("Line 2: looks like a GitHub token");
  await expect(dialog).not.toContainText("ghp_aB3");
  await dialog.getByRole("button", { name: "Commit anyway" }).click();
  await expect(page.getByText("Committed")).toBeVisible();
  const commits = (await fakeLog(page)).filter(
    (r) => r.path === "/api/workspace/git/commit",
  );
  expect(commits.map((c) => Boolean(c.body.allow_secrets))).toEqual([
    false,
    true,
  ]);

  await panel.getByRole("button", { name: "Push" }).click();
  const push = page.getByRole("dialog", {
    name: "These commits may contain a secret",
  });
  await expect(push).toContainText("in commit 01234567");
  await push.getByRole("button", { name: "Push anyway" }).click();
  await expect(page.getByText("Pushed main to origin")).toBeVisible();
});

test("the Worktrees tab saves what each new worktree task gets", async ({
  page,
}) => {
  await start(page);
  await page.keyboard.press("Control+k");
  await page.keyboard.type("Worktrees");
  await page.keyboard.press("Enter");
  const setup = drawer(page).getByRole("region", {
    name: "Setup for new worktrees",
  });
  await expect(setup).toBeVisible();
  await setup
    .getByRole("button", { name: "Use suggestions for this project" })
    .click();
  await expect(
    setup.getByRole("textbox", { name: "Setup commands (one per line)" }),
  ).toHaveValue("npm ci");
  await setup
    .getByRole("textbox", { name: "Teardown commands (one per line)" })
    .fill("docker compose down");
  await setup.getByRole("textbox", { name: "Ports" }).fill("4000-4099");
  await setup.getByRole("button", { name: "Save setup" }).click();
  await expect(page.getByText("Worktree setup saved")).toBeVisible();
  const saved = (await fakeLog(page)).find(
    (r) => r.path === "/api/worktree-tasks/setup" && r.method === "POST",
  );
  expect(saved?.body.setup).toEqual({
    copy: [".env"],
    setup: ["npm ci"],
    teardown: ["docker compose down"],
    port_start: 4000,
    port_end: 4099,
  });
  // Saved: the suggestion is no longer offered over it.
  await expect(
    setup.getByRole("button", { name: "Use suggestions for this project" }),
  ).toHaveCount(0);
});
