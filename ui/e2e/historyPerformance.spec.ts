import { test, expect } from "@playwright/test";
import { installFakeBackend } from "./fakeBackend";
import { installFakePerformance } from "./fakePerformance";

test("10,000 saved messages remain accessible through bounded history pages", async ({
  page,
}) => {
  test.setTimeout(120_000);
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.addInitScript(installFakeBackend);
  await page.addInitScript(installFakePerformance, { messages: 10_000 });
  await page.goto("/");
  await page.locator('.task-link[data-session-id="s1"]').click();
  const rows = page.locator(".msg-user .bubble, .msg-agent .markdown");
  await expect(rows).toHaveCount(128);
  await expect(rows.last()).toContainText("Saved message 10000.");
  const seen = new Set<number>();
  let pages = 0;
  const older = page.getByRole("button", {
    name: "Older messages",
    exact: true,
  });
  while (true) {
    const ids = await rows.evaluateAll((nodes) =>
      nodes.map((node) =>
        Number(node.textContent?.match(/Saved message (\d+)/)?.[1]),
      ),
    );
    expect(ids.length).toBeLessThanOrEqual(128);
    expect(ids.every((id) => id > 0)).toBe(true);
    ids.forEach((id) => seen.add(id));
    pages++;
    if (await older.isDisabled()) break;
    await older.click();
    await expect(rows.last()).toContainText(
      `Saved message ${String(ids[0] - 1).padStart(5, "0")}.`,
    );
  }
  expect(pages).toBe(79);
  expect(seen.size).toBe(10_000);
  expect(seen.has(1)).toBe(true);
  await page
    .getByRole("button", { name: "Newer messages", exact: true })
    .click();
  await expect(rows).toHaveCount(128);
  await expect(rows.first()).toContainText("Saved message 00017.");
  await page
    .getByRole("button", { name: "Latest messages", exact: true })
    .click();
  await expect(rows.last()).toContainText("Saved message 10000.");
  expect(
    await page.evaluate(() =>
      (window as any).__SHADOW_PERFORMANCE__.reads.every(
        (read: any) => read.events <= 128,
      ),
    ),
  ).toBe(true);
  expect(errors).toEqual([]);
});
