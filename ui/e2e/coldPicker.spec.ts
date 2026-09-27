import { test, expect } from "@playwright/test";
import { installFakeBackend } from "./fakeBackend";

type PickerGate = {
  released: boolean;
  fullStarted: number;
  fullCompleted: number;
  cachedCalls: number;
  release: () => void;
};
type FixtureWindow = Window & {
  __SHADOW_PICKER_GATE__: PickerGate;
  __SHADOW_FAKE__: {
    log: { method: string; path: string; body: any }[];
  };
};

// Installed in the same init script as the fake engine so the barrier is
// present before React starts. It holds responses, not a guessed duration.
function holdFullPicker() {
  const scope = window as unknown as FixtureWindow;
  const transport = window.__SHADOW_TEST_TRANSPORT__!;
  const request = transport.request;
  let release!: () => void;
  const pending = new Promise<void>((resolve) => {
    release = resolve;
  });
  const gate: PickerGate = {
    released: false,
    fullStarted: 0,
    fullCompleted: 0,
    cachedCalls: 0,
    release: () => {
      gate.released = true;
      release();
    },
  };
  scope.__SHADOW_PICKER_GATE__ = gate;
  transport.request = async (path, method, body) => {
    const url = new URL(path, location.href);
    if (method !== "GET" || url.pathname !== "/api/picker")
      return request(path, method, body);
    if (url.searchParams.get("cached") === "1") {
      gate.cachedCalls += 1;
      return request(path, method, body);
    }
    gate.fullStarted += 1;
    await pending;
    const result = (await request(path, method, body)) as {
      targets: { id: string; name: string }[];
    };
    // The changed display name proves the final answer reached the UI;
    // selection must stay bound to the exact existing target ID.
    const local = result.targets.find((row) => row.id === "local:gguf:qwen");
    if (!local) throw new Error("fixture local row is missing");
    local.name = "qwen3:14b refreshed";
    gate.fullCompleted += 1;
    return result;
  };
}

test("selects and submits cached local rows before full refresh, then preserves the selection", async ({
  page,
}) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.addInitScript({
    content: `(${installFakeBackend.toString()})({stepMs:90}); (${holdFullPicker.toString()})();`,
  });
  await page.goto("/");
  const trigger = page.getByRole("button", { name: /Model for this task/ });
  const prompt = page.getByRole("textbox", { name: "Message ShadowCode" });
  const send = page.getByRole("button", { name: "Send task" });
  const gate = () =>
    page.evaluate(() => {
      const { released, fullStarted, fullCompleted, cachedCalls } = (
        window as unknown as FixtureWindow
      ).__SHADOW_PICKER_GATE__;
      return { released, fullStarted, fullCompleted, cachedCalls };
    });
  const log = () =>
    page.evaluate(
      () => (window as unknown as FixtureWindow).__SHADOW_FAKE__.log,
    );
  try {
    await expect(trigger).toContainText("Choose a model");
    await expect
      .poll(async () => (await gate()).fullStarted)
      .toBeGreaterThan(0);
    await trigger.click();
    const search = page.getByRole("combobox", { name: "Search models" });
    await search.fill("qwen3:14b");
    await expect(
      page.getByRole("option", { name: /qwen3:14b · This computer/ }),
    ).toBeVisible();
    await search.press("Enter");
    await expect(trigger).toHaveText("Localqwen3:14b");
    await prompt.fill("Inspect the project using the cached local model");
    await expect(send).toBeEnabled();
    expect(await gate()).toMatchObject({ released: false, fullCompleted: 0 });
    expect((await gate()).cachedCalls).toBeGreaterThan(0);
    await send.click();
    await expect
      .poll(async () =>
        (await log()).filter(
          (row) => row.method === "POST" && row.path === "/api/jobs",
        ),
      )
      .toHaveLength(1);
    const submission = (await log()).find(
      (row) => row.method === "POST" && row.path === "/api/jobs",
    );
    expect(submission?.body.model).toBe("local:gguf:qwen");
    expect(await gate()).toMatchObject({ released: false, fullCompleted: 0 });
    await page.evaluate(() =>
      (window as unknown as FixtureWindow).__SHADOW_PICKER_GATE__.release(),
    );
    await expect(trigger).toHaveText("Localqwen3:14b refreshed");
    await expect
      .poll(async () => (await gate()).fullCompleted)
      .toBeGreaterThan(0);
    const selections = (await log()).filter(
      (row) => row.method === "POST" && row.path === "/api/sessions/s1/target",
    );
    expect(selections).toHaveLength(1);
    expect(selections[0].body.target_id).toBe("local:gguf:qwen");
    await expect(
      page.getByRole("region", { name: "Task summary" }),
    ).toBeVisible();
    await expect(trigger).toHaveText("Localqwen3:14b refreshed");
  } finally {
    await page.evaluate(() =>
      (window as unknown as FixtureWindow).__SHADOW_PICKER_GATE__.release(),
    );
  }
  expect(errors).toEqual([]);
});
