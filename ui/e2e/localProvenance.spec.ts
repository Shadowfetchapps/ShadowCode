import { test, expect } from "@playwright/test";
import { installFakeBackend } from "./fakeBackend";

test("local provenance stays in the task summary and wraps exact fingerprints", async ({
  page,
}) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.addInitScript(installFakeBackend, {
    stepMs: 20,
    localStartup: true,
    localRuntimeReceipt: {
      preparation_seconds: 1,
      automatic_cpu_fallback_allowed: false,
      runtime: {
        backend: "vulkan",
        context_tokens: 8192,
        cpu_fallback: false,
        provenance: {
          schema: 1,
          identity_kind: "filesystem_metadata",
          files: {
            model: {
              path: "/home/user/models/long-project-name/installed-model/weights.gguf",
              bytes: 3500000000,
            },
            runtime: { path: "/runtime/llama-server", bytes: 1000000 },
          },
          model: {
            architecture: "qwen3",
            gguf_version: 3,
            quantization: { file_type: 18 },
            header_sha256: "ab".repeat(32),
            header_bytes: 10000,
            chat_template: { sha256: "cd".repeat(32), bytes: 70000 },
          },
          runtime: {
            reported_version: "fixture-version",
            reported_generation_defaults: {
              temperature: 0.8,
              top_p: 0.95,
              top_k: 40,
              seed: 4294967295,
            },
          },
          context: { requested_tokens: 8192, reported_tokens: 8192 },
          gpu: {
            requested_mode: "all",
            launch_mode: "all",
            reported_backend: "vulkan",
          },
        },
      },
      request_policy: {
        sampling_source: "runtime_defaults",
        sampling_overrides: {},
        max_tokens_policy: "context_budget_per_request",
        chat_template_kwargs: { enable_thinking: false },
      },
    },
  });
  await page.goto("/");
  await page.getByRole("button", { name: /Model for this task/ }).click();
  const search = page.getByRole("combobox", { name: "Search models" });
  await search.fill("qwen3:14b");
  await search.press("Enter");
  await page
    .getByRole("textbox", { name: "Message ShadowCode" })
    .fill("Fix add and run checks");
  await page.getByRole("button", { name: "Send task" }).click();
  const summary = page.getByRole("region", { name: "Task summary" });
  await summary.getByText("Local model details").click();
  const details = summary.locator(".local-model-details");
  await expect(details.getByText("fixture-version")).toBeVisible();
  await expect(details.getByText("ab".repeat(32))).toBeVisible();
  await expect(details.getByText(/not full weight hashes/)).toBeVisible();
  expect(await details.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(
    true,
  );
  await page.setViewportSize({ width: 1100, height: 900 });
  await details.scrollIntoViewIfNeeded();
  expect(await details.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(
    true,
  );
  await page.screenshot({ path: "../../model-provenance-browser.png" });
  expect(errors).toEqual([]);
});
