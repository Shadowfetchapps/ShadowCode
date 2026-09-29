import { test, expect, type Page } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { installFakeBackend } from "./fakeBackend";

// Settings › Rules & skills and the Health page's skill checker, against
// the fake engine: one rulebook for every agent.
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

type Call = { method: string; path: string; body: Record<string, unknown> };
const calls = (page: Page, prefix: string) =>
  page.evaluate(
    (p) =>
      (
        (window as unknown as { __SHADOW_FAKE__: { log: Call[] } })
          .__SHADOW_FAKE__.log as Call[]
      ).filter((c) => c.path.startsWith(p) && c.method !== "GET"),
    prefix,
  );

async function openRules(page: Page) {
  await page.keyboard.press("Control+,");
  const settings = page.getByRole("dialog", { name: "Settings" });
  await settings
    .getByRole("button", { name: "Rules & skills", exact: true })
    .click();
  await expect(
    settings.getByRole("heading", { name: "Rules & skills" }),
  ).toBeVisible();
  return settings;
}

test("edits profile rules, switches items and shows what each agent reads", async ({
  page,
}) => {
  const settings = await openRules(page);
  await expect(
    settings.getByText("/home/tester/.config/shadowcode/profile", {
      exact: true,
    }),
  ).toBeVisible();
  // Edit the profile AGENTS.md.
  const editor = settings.getByLabel(
    "AGENTS.md in your profile, sent to every agent",
  );
  await expect(editor).toHaveValue("Answer in plain language.\n");
  await editor.fill("Prefer small, reviewed commits.\n");
  await settings.getByRole("button", { name: "Save rules" }).click();
  await expect(page.getByText("Rules saved")).toBeVisible();
  await expect(
    settings.getByRole("button", { name: "Save rules" }),
  ).toBeDisabled();
  const saves = await calls(page, "/api/rules/profile");
  expect(saves).toEqual([
    {
      method: "PUT",
      path: "/api/rules/profile",
      body: {
        content: "Prefer small, reviewed commits.\n",
        expected_hash: "rh1",
      },
    },
  ]);

  // What each agent reads: Codex reads AGENTS.md itself.
  const agent = settings.getByLabel("Agent", { exact: true });
  await agent.selectOption("codex");
  const codex = settings.getByRole("list", { name: "What Codex reads" });
  await expect(codex.getByText("Codex reads this file itself")).toBeVisible();
  await expect(settings.getByText(/developerInstructions/)).toBeVisible();

  // Switch the project's release skill off for this project.
  const project = settings.getByRole("list", { name: "From this project" });
  const release = project.getByRole("checkbox", {
    name: "Use Skill: release",
  });
  await release.uncheck();
  await expect(release).not.toBeChecked();
  expect(await calls(page, "/api/rules/items")).toEqual([
    {
      method: "POST",
      path: "/api/rules/items",
      body: {
        id: "project:.shadow/skills/release.md",
        enabled: false,
        workspace: "/work/demo",
      },
    },
  ]);
  await expect(codex.getByText("Switched off")).toBeVisible();

  // Sending to vendor CLIs can be turned off.
  await settings
    .getByRole("checkbox", { name: /Send rules and skills to Claude Code/ })
    .uncheck();
  await expect(
    settings.getByText(/Sending rules to vendor agents is off/),
  ).toBeVisible();
  await expect(
    settings.getByText("Nothing is sent to this agent."),
  ).toBeVisible();
  await page.screenshot({ path: "test-results/rules-settings.png" });
  const results = await new AxeBuilder({ page })
    .withTags(["wcag2a", "wcag2aa", "wcag21aa"])
    .include('[role="dialog"]')
    .analyze();
  expect(results.violations).toEqual([]);
});

test("installs starters, imports from Git and exports only on request", async ({
  page,
}) => {
  const settings = await openRules(page);
  // Starter skills are installed only when picked.
  const install = settings.getByRole("button", { name: "Install selected" });
  await expect(install).toBeDisabled();
  await settings.getByRole("checkbox", { name: /Careful review/ }).check();
  await install.click();
  await expect(page.getByText("Starter skills installed")).toBeVisible();
  await expect(
    settings
      .getByRole("list", { name: "From your profile" })
      .getByText("Skill: careful-review"),
  ).toBeVisible();

  // Git import: a refused address says why; an https address imports.
  const address = settings.getByLabel("Repository address");
  await address.fill("file:///etc");
  await settings.getByRole("button", { name: "Import", exact: true }).click();
  await expect(page.getByText(/Use an https:\/\/ or SSH/)).toBeVisible();
  await address.fill("https://github.com/team/agent-rules.git");
  await settings.getByRole("button", { name: "Import", exact: true }).click();
  const imports = settings.getByRole("list", { name: "Imported profiles" });
  await expect(imports.getByText("agent-rules", { exact: true })).toBeVisible();
  await expect(
    imports.getByText(/Commit 4f2a9c1e0b · Team rules/),
  ).toBeVisible();
  await imports.getByRole("button", { name: "Update" }).click();
  await expect(page.getByText("agent-rules is up to date")).toBeVisible();
  // Removing asks first.
  await imports.getByRole("button", { name: "Remove" }).click();
  const confirm = page.getByRole("dialog", { name: "Remove agent-rules?" });
  await confirm.getByRole("button", { name: "Remove" }).click();
  await expect(imports).toHaveCount(0);

  // Nothing is exported until the user asks.
  expect(await calls(page, "/api/rules/export")).toEqual([]);
  await expect(
    settings.getByText("a file already exists here", { exact: false }),
  ).toBeVisible();
  await settings.getByRole("button", { name: "Use in Claude Code" }).click();
  await expect(
    settings.getByRole("button", { name: "Stop using in Claude Code" }),
  ).toBeVisible();
  await settings
    .getByRole("button", { name: "Stop using in Claude Code" })
    .click();
  await expect(
    settings.getByRole("button", { name: "Use in Claude Code" }),
  ).toBeVisible();
  expect((await calls(page, "/api/rules/export")).map((c) => c.method)).toEqual(
    ["POST", "DELETE"],
  );
  // Open folder goes through the desktop, which names the folder itself.
  await settings.getByRole("button", { name: "Open folder" }).click();
  const invoked = await page.evaluate(() =>
    (
      (window as unknown as { __SHADOW_FAKE__: { log: Call[] } })
        .__SHADOW_FAKE__.log as Call[]
    ).filter((c) => c.method === "INVOKE" && c.path === "open_rules_folder"),
  );
  expect(invoked).toHaveLength(1);
  expect(invoked[0].body).toBeUndefined();
});

test("the Health page's skill checker reports without editing", async ({
  page,
}) => {
  await page.keyboard.press("Control+,");
  const settings = page.getByRole("dialog", { name: "Settings" });
  await settings.getByRole("button", { name: "Advanced", exact: true }).click();
  await settings
    .getByRole("tablist", { name: "Advanced sections" })
    .getByRole("tab", { name: "Health" })
    .click();
  const findings = settings.getByRole("list", {
    name: "Skill checker findings",
  });
  await expect(findings.getByText(/has no SKILL.md/)).toBeVisible();
  await expect(findings.getByText(/turns off approvals/)).toBeVisible();
  await expect(
    settings.getByText("4 files checked · 1 errors · 1 warnings · 0 notes"),
  ).toBeVisible();
  await settings.getByRole("button", { name: "Check again" }).click();
  await expect(findings).toBeVisible();
  // Report only: nothing was written.
  expect(await calls(page, "/api/rules")).toEqual([]);
});
