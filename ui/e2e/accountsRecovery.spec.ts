import { test, expect } from "@playwright/test";
import { installFakeBackend } from "./fakeBackend";

test("Accounts restores independent login and stopping controls after settings navigation", async ({
  page,
}, testInfo) => {
  await page.addInitScript(installFakeBackend, {});
  await page.addInitScript(() => {
    const fake = (window as any).__SHADOW_FAKE__;
    const progress: Record<string, any> = {
      claude: {
        running: true,
        cancellation_requested: false,
        lines: ["Claude sign-in instructions retained"],
        done: null,
      },
      grok: {
        running: true,
        cancellation_requested: false,
        lines: ["Grok sign-in instructions retained"],
        done: null,
      },
    };
    Object.assign(fake.state.vendors.grok, {
      state: "not_logged_in",
      availability: "sign_in",
      availability_label: "Sign in",
    });
    const bridge = window.__SHADOW_TEST_TRANSPORT__!;
    const original = bridge.request;
    bridge.request = async (path, method, body) => {
      const match =
        /^\/api\/accounts\/(claude|grok)\/(login|cancel-login)$/.exec(path);
      if (!match) return original(path, method, body);
      fake.log.push({ path, method, body });
      if (match[2] === "cancel-login") {
        progress[match[1]].cancellation_requested = true;
        return { ok: true };
      }
      return structuredClone(progress[match[1]]);
    };
  });
  await page.goto("/");
  await expect(
    page.getByRole("textbox", { name: "Message ShadowCode" }),
  ).toBeVisible();
  await page.keyboard.press("Control+,");
  const settings = page.getByRole("dialog", { name: "Settings" });
  const claude = settings.getByRole("article", { name: "Claude Code" });
  const grok = settings.getByRole("article", { name: "Grok" });
  await expect(
    claude.getByText("Claude sign-in instructions retained"),
  ).toBeVisible();
  await expect(
    grok.getByText("Grok sign-in instructions retained"),
  ).toBeVisible();
  await claude.getByRole("button", { name: "Cancel sign-in" }).click();
  await expect(claude.getByText("Stopping sign-in…")).toBeVisible();
  await settings
    .getByRole("button", { name: "Appearance", exact: true })
    .click();
  await expect(claude).toHaveCount(0);
  await settings.getByRole("button", { name: "Accounts", exact: true }).click();
  await expect(claude.getByText("Stopping sign-in…")).toBeVisible();
  await expect(
    claude.getByRole("button", { name: "Cancel sign-in" }),
  ).toBeDisabled();
  await expect(
    grok.getByRole("button", { name: "Cancel sign-in" }),
  ).toBeEnabled();
  await expect(
    grok.getByText("Grok sign-in instructions retained"),
  ).toBeVisible();
  await claude.getByText("Stopping sign-in…").scrollIntoViewIfNeeded();
  await page.screenshot({
    path: testInfo.outputPath("accounts-sign-in-recovered.png"),
  });
  const posts = await page.evaluate(() =>
    (window as any).__SHADOW_FAKE__.log.filter(
      (r: { method: string; path: string }) =>
        r.method === "POST" &&
        /\/accounts\/.*\/(connect|cancel-login)$/.test(r.path),
    ),
  );
  expect(posts).toEqual([
    { method: "POST", path: "/api/accounts/claude/cancel-login", body: {} },
  ]);
});
