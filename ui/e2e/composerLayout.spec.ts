import { test, expect } from "@playwright/test";
import { installFakeBackend } from "./fakeBackend";
import { installFakeTools } from "./fakeTools";

for (const model of ["qwen3:14b", "Codex · GPT-6-Astra"]) {
  test(`composer controls remain inside their panel without overlapping: ${model}`, async ({
    page,
  }) => {
    await page.addInitScript(installFakeBackend, { stepMs: 90 });
    await page.addInitScript(installFakeTools, {});
    await page.goto("/");
    await expect(
      page.getByRole("heading", { name: "What should we work on?" }),
    ).toBeVisible();
    await page.getByRole("button", { name: /Model for this task/ }).click();
    const search = page.getByRole("combobox", { name: "Search models" });
    await search.fill(model);
    await search.press("Enter");
    await expect(page.getByRole("listbox")).toHaveCount(0);
    await page
      .getByRole("textbox", { name: "Message ShadowCode" })
      .fill("Explain this project");

    for (const width of [1360, 1440, 1280, 1024, 900, 760, 520]) {
      await page.setViewportSize({ width, height: 1000 });
      const controls = page.locator(
        ".composer-footer button:visible, .composer-footer select:visible",
      );
      await expect
        .poll(
          async () => {
            const panel = await page.locator("form.composer").boundingBox();
            if (!panel) return ["missing composer"];
            const boxes = await controls.evaluateAll((elements) =>
              elements.map((element) => {
                const r = element.getBoundingClientRect();
                return {
                  name:
                    element.getAttribute("aria-label") ||
                    element.textContent?.trim(),
                  x: r.x,
                  y: r.y,
                  right: r.right,
                  bottom: r.bottom,
                };
              }),
            );
            const problems: string[] = [];
            for (const [index, box] of boxes.entries()) {
              if (
                box.x < panel.x - 1 ||
                box.right > panel.x + panel.width + 1 ||
                box.y < panel.y - 1 ||
                box.bottom > panel.y + panel.height + 1
              )
                problems.push(`outside panel: ${box.name}`);
              for (const other of boxes.slice(index + 1)) {
                if (
                  Math.min(box.right, other.right) - Math.max(box.x, other.x) >
                    1 &&
                  Math.min(box.bottom, other.bottom) -
                    Math.max(box.y, other.y) >
                    1
                )
                  problems.push(`overlap: ${box.name} / ${other.name}`);
              }
            }
            return problems;
          },
          { message: `Composer layout at ${width}px` },
        )
        .toEqual([]);
      await expect(
        page.getByRole("button", { name: "Send task" }),
      ).toBeInViewport();
      if (width === 1360 || width === 520)
        await page.screenshot({
          path: `test-results/composer-${model.startsWith("qwen") ? "local" : "subscription"}-${width}.png`,
        });
    }
  });
}
