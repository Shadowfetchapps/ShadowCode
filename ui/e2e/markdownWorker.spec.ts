import { test, expect } from "@playwright/test";
import { readFileSync } from "node:fs";
import { installFakeBackend } from "./fakeBackend";
import { installFakePerformance } from "./fakePerformance";
import { observeMarkdownWorker } from "./observeMarkdownWorker";

const desktopCsp = JSON.parse(
  readFileSync("../src-tauri/tauri.conf.json", "utf8"),
).app.security.csp as string;
const remoteCsp = readFileSync(
  "../native/core/src/remote/http.rs",
  "utf8",
).match(/const HTML_CSP: &str = "([^"]+)";/)?.[1];
if (!remoteCsp) throw new Error("Remote HTML CSP was not found");

for (const [mode, csp] of [
  ["desktop policy worker", desktopCsp],
  ["remote policy fallback", remoteCsp],
] as const) {
  test(`${mode} preserves complete Markdown, safe links and code copy after final replacement`, async ({
    page,
    context,
  }) => {
    const errors: string[] = [];
    page.on("pageerror", (error) => errors.push(error.message));
    await context.grantPermissions(["clipboard-read", "clipboard-write"]);
    // Exercise the checked-in policies in Chromium. Native WebKit/Wayland still
    // requires a separate qualification; no production policy is relaxed here.
    await page.route("**/", async (route) => {
      const response = await route.fetch();
      await route.fulfill({
        response,
        headers: { ...response.headers(), "content-security-policy": csp },
      });
    });
    await page.addInitScript(installFakeBackend);
    await page.addInitScript(observeMarkdownWorker);
    const prefix =
      "# Worker semantics\n\n[Reference][later]\n\n" +
      "A recorded paragraph.\n\n".repeat(500) +
      "```ts\nconst n = 1;\n\n";
    await page.addInitScript(installFakePerformance, {
      initialReplyText: prefix,
    });
    await page.addInitScript(() =>
      (window as any).__SHADOW_PERFORMANCE__.prepareStream(),
    );
    await page.goto("/");
    await page.locator('.task-link[data-session-id="s1"]').click();
    await page.evaluate(() => {
      (window as any).__SHADOW_PERFORMANCE__.startStream();
      (window as any).__SHADOW_PERFORMANCE__.stopStream();
    });
    await expect(
      page.getByRole("heading", { name: "Worker semantics" }),
    ).toBeVisible();
    if (mode === "desktop policy worker") {
      await expect
        .poll(() =>
          page.evaluate(
            () => (window as any).__SHADOW_MARKDOWN_WORKERS__.trees,
          ),
        )
        .toBeGreaterThan(0);
    }
    const completed =
      prefix +
      "console.log(n);\n```\n\n[later]: https://example.com\n\n| Left | Right |\n| --- | --- |\n| A | B |\n\nNote[^n].\n\n[^n]: footnote\n\n[Unsafe](javascript:alert(1))\n\n![remote](https://example.com/image.png)\n\n<script>unsafe()</script>\n\n&copy; &NotEqualTilde;";
    await page.evaluate(
      (text) => (window as any).__SHADOW_PERFORMANCE__.finishStream(text),
      completed,
    );
    const answer = page
      .locator(".msg-agent")
      .filter({ has: page.getByRole("heading", { name: "Worker semantics" }) });
    await expect(
      answer.getByRole("link", { name: "Reference", exact: true }),
    ).toHaveAttribute("href", "https://example.com");
    await expect(
      answer.getByRole("columnheader", { name: "Left", exact: true }),
    ).toBeVisible();
    await expect(
      answer.getByRole("cell", { name: "B", exact: true }),
    ).toBeVisible();
    await expect(answer).toContainText("footnote");
    await expect(
      answer.getByRole("link", { name: "Unsafe", exact: true }),
    ).toHaveCount(0);
    await expect(answer.locator("img, script")).toHaveCount(0);
    await expect(answer).toContainText("<script>unsafe()</script>");
    await expect(answer).toContainText("© ≂̸");
    await answer.getByRole("button", { name: "Copy code" }).click();
    expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(
      "const n = 1;\n\nconsole.log(n);\n",
    );
    await answer
      .getByRole("button", { name: "Copy answer", exact: true })
      .click();
    expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(
      completed,
    );
    const workers = await page.evaluate(
      () => (window as any).__SHADOW_MARKDOWN_WORKERS__,
    );
    expect(workers.maxConcurrent).toBe(1);
    if (mode === "desktop policy worker") {
      expect(workers.created - workers.terminated).toBe(1);
      expect(workers.trees).toBeGreaterThanOrEqual(2);
      expect(workers.errors).toBe(0);
    } else {
      expect(workers.created - workers.terminated).toBe(0);
      expect(workers.trees).toBe(0);
      expect(workers.errors).toBe(1);
    }
    expect(errors).toEqual([]);
  });
}
